use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use tokio::sync::RwLock;
use uuid::Uuid;

use apex_core::domain::models::*;
use apex_core::ports::execution::ExecutionPort;
use apex_core::ports::market_data::AdapterHealth;

/// Default slippage in basis points
const DEFAULT_SLIPPAGE_BPS: f64 = 2.0;
/// Default commission in basis points
const DEFAULT_COMMISSION_BPS: f64 = 3.0;
/// Default initial margin on short positions, as a fraction of their notional (50%).
pub const DEFAULT_SHORT_MARGIN_PCT: f64 = 0.5;

/// Internal state for the paper trading engine
#[derive(Debug, Clone)]
pub struct PaperState {
    pub cash: f64,
    pub initial_capital: f64,
    pub orders: HashMap<String, Order>,
    pub positions: HashMap<String, Position>,
    pub realized_pnl: f64,
    pub order_counter: u64,
    /// Stop-limit orders stay triggered until filled or cancelled.
    pub triggered_stops: HashSet<String>,
}

impl PaperState {
    fn new(initial_capital: f64) -> Self {
        Self {
            cash: initial_capital,
            initial_capital,
            orders: HashMap::new(),
            positions: HashMap::new(),
            realized_pnl: 0.0,
            order_counter: 0,
            triggered_stops: HashSet::new(),
        }
    }
}

/// Paper trading adapter — always available, simulates fills locally
pub struct PaperTradingAdapter {
    #[allow(dead_code)]
    initial_capital: f64,
    currency: String,
    slippage_bps: f64,
    commission_bps: f64,
    /// Initial margin required on short notional (0.5 = 50%). Opening or growing a short is
    /// rejected when account equity after the fill would not cover it.
    short_margin_pct: f64,
    state: Arc<RwLock<PaperState>>,
    quote_cache: Arc<DashMap<String, Quote>>,
}

impl PaperTradingAdapter {
    /// Create a new paper trading adapter with defaults
    pub fn new() -> Self {
        Self::with_config(
            1_000_000.0,
            "INR".into(),
            DEFAULT_SLIPPAGE_BPS,
            DEFAULT_COMMISSION_BPS,
        )
    }

    /// Create a new paper trading adapter with custom configuration
    pub fn with_config(
        initial_capital: f64,
        currency: String,
        slippage_bps: f64,
        commission_bps: f64,
    ) -> Self {
        Self {
            initial_capital,
            currency,
            slippage_bps,
            commission_bps,
            short_margin_pct: DEFAULT_SHORT_MARGIN_PCT,
            state: Arc::new(RwLock::new(PaperState::new(initial_capital))),
            quote_cache: Arc::new(DashMap::new()),
        }
    }

    /// Set the initial margin required on short positions (fraction of notional, clamped to
    /// 5%..=500%). A non-finite value keeps the default.
    pub fn with_short_margin_pct(mut self, pct: f64) -> Self {
        self.short_margin_pct = if pct.is_finite() {
            pct.clamp(0.05, 5.0)
        } else {
            DEFAULT_SHORT_MARGIN_PCT
        };
        self
    }

    /// Initial margin fraction required on short notional.
    pub fn short_margin_pct(&self) -> f64 {
        self.short_margin_pct
    }

    /// Update the quote cache (called by the market data aggregator)
    pub fn update_quote(&self, quote: &Quote) {
        if !Self::fresh_quote(quote) {
            return;
        }
        let mut entry = self
            .quote_cache
            .entry(quote.symbol.0.clone())
            .or_insert_with(|| quote.clone());
        if quote.updated_at > entry.updated_at {
            *entry = quote.clone();
        }
    }

    fn fresh_quote(quote: &Quote) -> bool {
        let age = Utc::now()
            .signed_duration_since(quote.updated_at)
            .num_seconds();
        (-5..=60).contains(&age) && quote.last.is_finite() && quote.last > 0.0
    }

    fn stop_crossed(order: &Order, price: f64) -> bool {
        match (order.side.clone(), order.stop_price) {
            (OrderSide::Buy, Some(stop)) => price >= stop,
            (OrderSide::Sell, Some(stop)) => price <= stop,
            _ => false,
        }
    }

    fn limit_crossed(order: &Order, price: f64) -> bool {
        match (order.side.clone(), order.price) {
            (OrderSide::Buy, Some(limit)) => price <= limit,
            (OrderSide::Sell, Some(limit)) => price >= limit,
            _ => false,
        }
    }

    fn pending_price(&self, order: &Order, quote: &Quote, stop_triggered: bool) -> Option<f64> {
        let eligible = match &order.order_type {
            OrderType::Limit => Self::limit_crossed(order, quote.last),
            OrderType::Stop => Self::stop_crossed(order, quote.last),
            OrderType::StopLimit => stop_triggered && Self::limit_crossed(order, quote.last),
            _ => false,
        };
        if !eligible {
            return None;
        }
        let slipped = self.calculate_fill_price(quote.last, &order.side);
        Some(
            if matches!(&order.order_type, OrderType::Limit | OrderType::StopLimit) {
                let limit = order.price?;
                match &order.side {
                    OrderSide::Buy => slipped.min(limit),
                    OrderSide::Sell => slipped.max(limit),
                }
            } else {
                slipped
            },
        )
    }

    fn marked_position(&self, position: &Position) -> Position {
        let mut marked = position.clone();
        if let Some(quote) = self.quote_cache.get(&position.symbol.0) {
            let change = match position.side {
                OrderSide::Buy => quote.last - position.avg_price,
                OrderSide::Sell => position.avg_price - quote.last,
            };
            marked.pnl = change * position.quantity;
            marked.pnl_pct = if position.avg_price > 0.0 {
                change / position.avg_price * 100.0
            } else {
                0.0
            };
        }
        marked
    }

    /// Get the quote cache (for external access)
    pub fn quote_cache(&self) -> Arc<DashMap<String, Quote>> {
        self.quote_cache.clone()
    }

    /// Calculate fill price with slippage
    fn calculate_fill_price(&self, last_price: f64, side: &OrderSide) -> f64 {
        let slippage = last_price * (self.slippage_bps / 10_000.0);
        match side {
            OrderSide::Buy => last_price + slippage,
            OrderSide::Sell => last_price - slippage,
        }
    }

    /// Calculate commission for a trade
    fn calculate_commission(&self, value: f64) -> f64 {
        value * (self.commission_bps / 10_000.0)
    }

    /// Process a market order fill immediately
    async fn fill_market_order(&self, order_id: &str, request: &NewOrderRequest) -> Result<()> {
        let symbol_key = &request.symbol.0;
        let quote = self
            .quote_cache
            .get(symbol_key)
            .map(|q| q.clone())
            .ok_or_else(|| anyhow!("No quote available for symbol {}", symbol_key))?;

        if !Self::fresh_quote(&quote) {
            return Err(anyhow!(
                "No fresh market price for {}; paper order was not placed",
                symbol_key
            ));
        }
        let fill_price = self.calculate_fill_price(quote.last, &request.side);
        let mut state = self.state.write().await;
        self.fill_locked(&mut state, order_id, request, fill_price)
    }

    /// Mark a position: the latest cached quote, else its average price.
    fn mark_price(&self, position: &Position) -> f64 {
        self.quote_cache
            .get(&position.symbol.0)
            .map(|q| q.last)
            .filter(|p| p.is_finite() && *p > 0.0)
            .unwrap_or(position.avg_price)
    }

    /// Long cost, long value and short value of a set of positions, all at current marks.
    fn exposure<'a>(&self, positions: impl Iterator<Item = &'a Position>) -> Exposure {
        let mut exposure = Exposure::default();
        for position in positions {
            let mark = self.mark_price(position);
            match position.side {
                OrderSide::Buy => {
                    exposure.long_cost += position.quantity * position.avg_price;
                    exposure.long_value += position.quantity * mark;
                }
                OrderSide::Sell => exposure.short_value += position.quantity * mark,
            }
        }
        exposure
    }

    fn reject_locked(state: &mut PaperState, order_id: &str, reason: String) -> Result<()> {
        if let Some(order) = state.orders.get_mut(order_id) {
            order.status = OrderStatus::Rejected;
            order.updated_at = Utc::now();
            order.rejection_reason = Some(reason.clone());
        }
        Err(anyhow!(reason))
    }

    fn fill_locked(
        &self,
        state: &mut PaperState,
        order_id: &str,
        request: &NewOrderRequest,
        fill_price: f64,
    ) -> Result<()> {
        let trade_value = fill_price * request.quantity;
        let commission = self.calculate_commission(trade_value);
        let symbol_key = request.symbol.0.clone();
        let net = net_fill(
            state.positions.get(&symbol_key),
            &request.symbol,
            &request.side,
            request.quantity,
            fill_price,
        );
        let cash_after = match request.side {
            OrderSide::Buy => state.cash - trade_value - commission,
            OrderSide::Sell => state.cash + trade_value - commission,
        };

        // Funds: only the part of a buy that opens or adds to a long must be paid for out of
        // cash. A buy that only covers a short is a closing order and is always allowed (cash may
        // dip below zero as a margin debit when the market has run against the short).
        if request.side == OrderSide::Buy && net.opened_long > 0.0 && cash_after < 0.0 {
            return Self::reject_locked(
                state,
                order_id,
                format!(
                    "Insufficient funds: need {:.2}, have {:.2}",
                    trade_value + commission,
                    state.cash
                ),
            );
        }

        // Margin: opening, adding to, or flipping through zero into a short needs the account
        // equity after the fill to cover the initial margin on the whole short book.
        if net.opened_short > 0.0 {
            let others = state
                .positions
                .iter()
                .filter(|(key, _)| **key != symbol_key)
                .map(|(_, p)| p);
            let mut exposure = self.exposure(others);
            if let Some(position) = &net.position {
                let one = self.exposure(std::iter::once(position));
                exposure.long_cost += one.long_cost;
                exposure.long_value += one.long_value;
                exposure.short_value += one.short_value;
            }
            let equity_after = cash_after + exposure.long_value - exposure.short_value;
            let required = self.short_margin_pct * exposure.short_value;
            if equity_after < required {
                return Self::reject_locked(
                    state,
                    order_id,
                    format!(
                        "Insufficient margin to short {} {}: equity after the order would be {:.2} but short margin ({:.0}% of {:.2} short value) needs {:.2}",
                        request.quantity,
                        request.symbol.0,
                        equity_after,
                        self.short_margin_pct * 100.0,
                        exposure.short_value,
                        required
                    ),
                );
            }
        }

        // Execute the fill
        state.cash = cash_after;

        // Fees are realized expenses on both entry and exit.
        state.realized_pnl += net.realized - commission;

        // Update order status
        if let Some(order) = state.orders.get_mut(order_id) {
            order.status = OrderStatus::Filled;
            order.filled_qty = request.quantity;
            order.avg_price = fill_price;
            order.updated_at = Utc::now();
        }

        // Update position, marking unrealized P&L against the last price.
        match net.position {
            Some(mut position) => {
                if let Some(quote) = self.quote_cache.get(&symbol_key) {
                    let change = match position.side {
                        OrderSide::Buy => quote.last - position.avg_price,
                        OrderSide::Sell => position.avg_price - quote.last,
                    };
                    position.pnl = change * position.quantity;
                    if position.avg_price > 0.0 {
                        position.pnl_pct = change / position.avg_price * 100.0;
                    }
                }
                state.positions.insert(symbol_key, position);
            }
            None => {
                state.positions.remove(&symbol_key);
            }
        }
        state.triggered_stops.remove(order_id);

        Ok(())
    }
}

/// Long and short book totals at current marks.
#[derive(Debug, Default, Clone, Copy)]
struct Exposure {
    /// What the longs were bought for (quantity x average price).
    long_cost: f64,
    /// Longs at the current mark.
    long_value: f64,
    /// Shorts at the current mark: the liability to buy them back.
    short_value: f64,
}

/// Result of netting one fill into a symbol's position.
#[derive(Debug, Clone)]
struct Netting {
    /// The position after the fill, `None` when flat.
    position: Option<Position>,
    /// Gross trading P&L realized by the part of the fill that closed existing exposure.
    /// Commission is accounted for separately.
    realized: f64,
    /// Quantity of the fill that opened or added to a long.
    opened_long: f64,
    /// Quantity of the fill that opened or added to a short (including the part of a sell
    /// that flipped a long through zero).
    opened_short: f64,
}

/// Net a fill into a position. A buy against a short covers it, a sell against a long reduces
/// it, and a fill larger than the open position closes it and opens the rest on the other side.
fn net_fill(
    existing: Option<&Position>,
    symbol: &Symbol,
    side: &OrderSide,
    quantity: f64,
    price: f64,
) -> Netting {
    let opened = |qty: f64, avg: f64| Position {
        symbol: symbol.clone(),
        quantity: qty,
        avg_price: avg,
        side: side.clone(),
        pnl: 0.0,
        pnl_pct: 0.0,
        broker_id: "paper".into(),
    };
    let (opened_long, opened_short) = match side {
        OrderSide::Buy => (quantity, 0.0),
        OrderSide::Sell => (0.0, quantity),
    };
    let Some(pos) = existing.filter(|p| p.quantity > 0.0) else {
        return Netting {
            position: Some(opened(quantity, price)),
            realized: 0.0,
            opened_long,
            opened_short,
        };
    };
    if pos.side == *side {
        // Adding to the position: blend the average price.
        let total = pos.quantity + quantity;
        let mut next = pos.clone();
        next.avg_price = (pos.avg_price * pos.quantity + price * quantity) / total;
        next.quantity = total;
        return Netting {
            position: Some(next),
            realized: 0.0,
            opened_long,
            opened_short,
        };
    }
    // Opposite side: the closing part earns (long) or pays (short) the price difference.
    let closing = quantity.min(pos.quantity);
    let realized = match pos.side {
        OrderSide::Buy => (price - pos.avg_price) * closing,
        OrderSide::Sell => (pos.avg_price - price) * closing,
    };
    let remaining = quantity - pos.quantity;
    if remaining > 1e-12 {
        // Flipped through zero: the rest opens a fresh position at the fill price.
        Netting {
            position: Some(opened(remaining, price)),
            realized,
            opened_long: if *side == OrderSide::Buy { remaining } else { 0.0 },
            opened_short: if *side == OrderSide::Sell { remaining } else { 0.0 },
        }
    } else if pos.quantity - quantity > 1e-12 {
        let mut next = pos.clone();
        next.quantity = pos.quantity - quantity;
        Netting {
            position: Some(next),
            realized,
            opened_long: 0.0,
            opened_short: 0.0,
        }
    } else {
        Netting {
            position: None,
            realized,
            opened_long: 0.0,
            opened_short: 0.0,
        }
    }
}

impl Default for PaperTradingAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExecutionPort for PaperTradingAdapter {
    async fn place_order(&self, request: &NewOrderRequest) -> Result<OrderId> {
        if !request.quantity.is_finite() || request.quantity <= 0.0 {
            return Err(anyhow!("Quantity must be positive"));
        }
        let valid_price = |p: Option<f64>| p.is_some_and(|v| v.is_finite() && v > 0.0);
        match &request.order_type {
            OrderType::Market => {}
            OrderType::Limit if !valid_price(request.price) => {
                return Err(anyhow!("Limit price must be positive"))
            }
            OrderType::Stop if !valid_price(request.stop_price) => {
                return Err(anyhow!("Stop price must be positive"))
            }
            OrderType::StopLimit
                if !valid_price(request.stop_price) || !valid_price(request.price) =>
            {
                return Err(anyhow!("Stop and limit prices must be positive"))
            }
            OrderType::TrailingStop => {
                return Err(anyhow!("Paper trailing stop orders are not supported"))
            }
            _ => {}
        }
        let mut state = self.state.write().await;
        state.order_counter += 1;
        let order_id = format!("PAPER-{}", Uuid::new_v4());

        let order = Order {
            id: OrderId(order_id.clone()),
            symbol: request.symbol.clone(),
            side: request.side.clone(),
            order_type: request.order_type.clone(),
            quantity: request.quantity,
            price: request.price,
            stop_price: request.stop_price,
            status: if request.order_type == OrderType::Market {
                OrderStatus::Pending
            } else {
                OrderStatus::Open
            },
            filled_qty: 0.0,
            avg_price: 0.0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            broker_id: "paper".into(),
            source: request.tag.clone().unwrap_or_else(|| "manual".into()),
            rejection_reason: None,
        };

        state.orders.insert(order_id.clone(), order);
        drop(state); // Release write lock before fill

        if request.order_type == OrderType::Market {
            if let Err(error) = self.fill_market_order(&order_id, request).await {
                // A rejected submission must not leave a phantom pending order.
                self.state.write().await.orders.remove(&order_id);
                return Err(error);
            }
        }

        Ok(OrderId(order_id))
    }

    async fn cancel_order(&self, order_id: &OrderId) -> Result<()> {
        let mut state = self.state.write().await;
        let order = state
            .orders
            .get_mut(&order_id.0)
            .ok_or_else(|| anyhow!("Order {} not found", order_id.0))?;

        match order.status {
            OrderStatus::Pending | OrderStatus::Open => {
                order.status = OrderStatus::Cancelled;
                order.updated_at = Utc::now();
                state.triggered_stops.remove(&order_id.0);
                Ok(())
            }
            _ => Err(anyhow!("Cannot cancel order in {:?} status", order.status)),
        }
    }

    async fn modify_order(&self, order_id: &OrderId, params: &ModifyParams) -> Result<()> {
        if params.quantity.is_some_and(|q| !q.is_finite() || q <= 0.0)
            || params.price.is_some_and(|p| !p.is_finite() || p <= 0.0)
            || params
                .stop_price
                .is_some_and(|p| !p.is_finite() || p <= 0.0)
        {
            return Err(anyhow!("Quantity and prices must be positive"));
        }
        let mut state = self.state.write().await;
        let order = state
            .orders
            .get_mut(&order_id.0)
            .ok_or_else(|| anyhow!("Order {} not found", order_id.0))?;

        match order.status {
            OrderStatus::Pending | OrderStatus::Open => {
                if let Some(qty) = params.quantity {
                    order.quantity = qty;
                }
                if let Some(price) = params.price {
                    order.price = Some(price);
                }
                if let Some(stop_price) = params.stop_price {
                    order.stop_price = Some(stop_price);
                }
                order.updated_at = Utc::now();
                Ok(())
            }
            _ => Err(anyhow!("Cannot modify order in {:?} status", order.status)),
        }
    }

    async fn get_order_status(&self, order_id: &OrderId) -> Result<Order> {
        let state = self.state.read().await;
        state
            .orders
            .get(&order_id.0)
            .cloned()
            .ok_or_else(|| anyhow!("Order {} not found", order_id.0))
    }

    async fn get_positions(&self) -> Result<Vec<Position>> {
        let state = self.state.read().await;
        Ok(state
            .positions
            .values()
            .map(|p| self.marked_position(p))
            .collect())
    }

    async fn get_account_balance(&self) -> Result<AccountBalance> {
        let state = self.state.read().await;
        let unrealized_pnl: f64 = state
            .positions
            .values()
            .map(|p| self.marked_position(p).pnl)
            .sum();
        let exposure = self.exposure(state.positions.values());
        // Equity: a short's sale proceeds are already in cash, so it is subtracted at its
        // current price (what it costs to buy back) rather than added as a position value.
        let total_value = state.cash + exposure.long_value - exposure.short_value;
        let short_margin = self.short_margin_pct * exposure.short_value;

        Ok(AccountBalance {
            total_value,
            cash: state.cash,
            margin_used: exposure.long_cost + short_margin,
            // What can still be opened: new longs are paid from cash and new shorts need equity
            // beyond the margin already held, so the smaller of the two bounds both.
            margin_available: state.cash.min(total_value - short_margin).max(0.0),
            unrealized_pnl,
            realized_pnl: state.realized_pnl,
            currency: self.currency.clone(),
        })
    }

    async fn eligible_orders(&self, quote: &Quote) -> Result<Vec<(Order, f64)>> {
        if !Self::fresh_quote(quote) {
            return Ok(Vec::new());
        }
        if self
            .quote_cache
            .get(&quote.symbol.0)
            .is_some_and(|q| q.updated_at > quote.updated_at)
        {
            return Ok(Vec::new());
        }
        self.update_quote(quote);
        let mut state = self.state.write().await;
        let mut orders: Vec<Order> = state
            .orders
            .values()
            .filter(|o| o.symbol == quote.symbol && o.status == OrderStatus::Open)
            .cloned()
            .collect();
        orders.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.0.cmp(&b.id.0))
        });
        let mut eligible = Vec::new();
        for order in orders {
            if order.order_type == OrderType::StopLimit && Self::stop_crossed(&order, quote.last) {
                state.triggered_stops.insert(order.id.0.clone());
            }
            let triggered = state.triggered_stops.contains(&order.id.0);
            if let Some(price) = self.pending_price(&order, quote, triggered) {
                eligible.push((order, price));
            }
        }
        Ok(eligible)
    }

    async fn resolve_order(
        &self,
        id: &OrderId,
        quote: &Quote,
        rejection_reason: Option<&str>,
    ) -> Result<Option<Order>> {
        if !Self::fresh_quote(quote)
            || self
                .quote_cache
                .get(&quote.symbol.0)
                .is_some_and(|q| q.updated_at > quote.updated_at)
        {
            return Ok(None);
        }
        let mut state = self.state.write().await;
        let Some(order) = state.orders.get(&id.0).cloned() else {
            return Ok(None);
        };
        if order.symbol != quote.symbol || order.status != OrderStatus::Open {
            return Ok(None);
        }
        let Some(price) = self.pending_price(&order, quote, state.triggered_stops.contains(&id.0))
        else {
            return Ok(None);
        };
        if let Some(reason) = rejection_reason {
            let order = state.orders.get_mut(&id.0).unwrap();
            order.status = OrderStatus::Rejected;
            order.updated_at = Utc::now();
            order.rejection_reason = Some(reason.to_owned());
        } else {
            let request = NewOrderRequest {
                symbol: order.symbol.clone(),
                side: order.side.clone(),
                order_type: order.order_type.clone(),
                quantity: order.quantity,
                price: order.price,
                stop_price: order.stop_price,
                tag: Some(order.source.clone()),
                broker_params: None,
            };
            // A resting order can become unaffordable while it waits. Mark it
            // rejected and publish that status rather than silently dropping it.
            let _ = self.fill_locked(&mut state, &id.0, &request, price);
        }
        state.triggered_stops.remove(&id.0);
        Ok(state.orders.get(&id.0).cloned())
    }

    fn latest_quote(&self, symbol: &Symbol) -> Option<Quote> {
        self.quote_cache
            .get(&symbol.0)
            .map(|q| q.clone())
            .filter(Self::fresh_quote)
    }

    fn broker_id(&self) -> &'static str {
        "paper"
    }

    fn supported_order_types(&self) -> &[OrderType] {
        &[
            OrderType::Market,
            OrderType::Limit,
            OrderType::Stop,
            OrderType::StopLimit,
        ]
    }

    fn health(&self) -> AdapterHealth {
        AdapterHealth::Healthy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_quote(symbol: &str, last: f64) -> Quote {
        Quote {
            symbol: Symbol(symbol.into()),
            bid: last - 0.05,
            ask: last + 0.05,
            last,
            open: last,
            high: last + 1.0,
            low: last - 1.0,
            volume: 10000,
            change_pct: 0.0,
            vwap: last,
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_paper_trading_default_creation() {
        let adapter = PaperTradingAdapter::new();
        assert_eq!(adapter.broker_id(), "paper");
        assert_eq!(adapter.health(), AdapterHealth::Healthy);
        assert!(adapter.supported_order_types().contains(&OrderType::Market));
    }

    #[tokio::test]
    async fn test_place_market_buy_order() {
        let adapter = PaperTradingAdapter::new();
        adapter.update_quote(&create_test_quote("AAPL", 150.0));

        let request = NewOrderRequest {
            symbol: Symbol("AAPL".into()),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: 10.0,
            price: None,
            stop_price: None,
            tag: None,
            broker_params: None,
        };

        let order_id = adapter.place_order(&request).await.unwrap();
        let order = adapter.get_order_status(&order_id).await.unwrap();
        assert_eq!(order.status, OrderStatus::Filled);
        assert_eq!(order.filled_qty, 10.0);
        assert!(order.avg_price > 0.0);

        let positions = adapter.get_positions().await.unwrap();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].symbol, Symbol("AAPL".into()));
        assert_eq!(positions[0].quantity, 10.0);
    }

    #[tokio::test]
    async fn test_place_market_sell_order() {
        let adapter = PaperTradingAdapter::new();
        adapter.update_quote(&create_test_quote("AAPL", 150.0));

        let buy_req = NewOrderRequest {
            symbol: Symbol("AAPL".into()),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: 10.0,
            price: None,
            stop_price: None,
            tag: None,
            broker_params: None,
        };
        adapter.place_order(&buy_req).await.unwrap();

        let sell_req = NewOrderRequest {
            symbol: Symbol("AAPL".into()),
            side: OrderSide::Sell,
            order_type: OrderType::Market,
            quantity: 10.0,
            price: None,
            stop_price: None,
            tag: None,
            broker_params: None,
        };
        adapter.place_order(&sell_req).await.unwrap();

        let positions = adapter.get_positions().await.unwrap();
        assert_eq!(positions.len(), 0);
    }

    #[tokio::test]
    async fn test_account_balance_after_trade() {
        let adapter = PaperTradingAdapter::with_config(100_000.0, "USD".into(), 0.0, 0.0);
        adapter.update_quote(&create_test_quote("AAPL", 100.0));

        let request = NewOrderRequest {
            symbol: Symbol("AAPL".into()),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: 10.0,
            price: None,
            stop_price: None,
            tag: None,
            broker_params: None,
        };
        adapter.place_order(&request).await.unwrap();

        let balance = adapter.get_account_balance().await.unwrap();
        assert_eq!(balance.currency, "USD");
        assert!((balance.cash - 99_000.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn invalid_and_unsupported_paper_orders_fail_without_creating_orders() {
        let adapter = PaperTradingAdapter::new();
        for order_type in [
            OrderType::Limit,
            OrderType::Stop,
            OrderType::StopLimit,
            OrderType::TrailingStop,
        ] {
            let request = NewOrderRequest {
                symbol: Symbol("AAPL".into()),
                side: OrderSide::Buy,
                order_type,
                quantity: 10.0,
                price: None,
                stop_price: None,
                tag: None,
                broker_params: None,
            };
            assert!(adapter.place_order(&request).await.is_err());
        }
        assert!(adapter.state.read().await.orders.is_empty());
        assert!(!adapter
            .supported_order_types()
            .contains(&OrderType::TrailingStop));
    }

    #[tokio::test]
    async fn previously_open_paper_order_can_still_be_cancelled() {
        let adapter = PaperTradingAdapter::new();
        let now = Utc::now();
        let order = Order {
            id: OrderId("legacy-open".into()),
            symbol: Symbol("AAPL".into()),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: 1.0,
            price: Some(100.0),
            stop_price: None,
            status: OrderStatus::Open,
            filled_qty: 0.0,
            avg_price: 0.0,
            created_at: now,
            updated_at: now,
            broker_id: "paper".into(),
            source: "manual".into(),
            rejection_reason: None,
        };
        let id = order.id.clone();
        adapter
            .state
            .write()
            .await
            .orders
            .insert(id.0.clone(), order);
        adapter.cancel_order(&id).await.unwrap();
        assert_eq!(
            adapter.get_order_status(&id).await.unwrap().status,
            OrderStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn market_orders_require_fresh_quotes_and_leave_no_phantom_orders() {
        let adapter = PaperTradingAdapter::new();
        let request = NewOrderRequest {
            symbol: Symbol("AAPL".into()),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: 1.0,
            price: None,
            stop_price: None,
            tag: None,
            broker_params: None,
        };
        for offset in [-65, 10] {
            let mut quote = create_test_quote("AAPL", 150.0);
            quote.updated_at = Utc::now() + chrono::Duration::seconds(offset);
            adapter.update_quote(&quote);
            assert!(adapter.place_order(&request).await.is_err());
            assert!(adapter.state.read().await.orders.is_empty());
        }
        adapter.update_quote(&create_test_quote("AAPL", 150.0));
        assert!(adapter.place_order(&request).await.is_ok());
    }

    #[tokio::test]
    async fn test_slippage_applied() {
        let adapter = PaperTradingAdapter::with_config(1_000_000.0, "USD".into(), 10.0, 0.0);
        adapter.update_quote(&create_test_quote("AAPL", 100.0));

        let request = NewOrderRequest {
            symbol: Symbol("AAPL".into()),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: 1.0,
            price: None,
            stop_price: None,
            tag: None,
            broker_params: None,
        };

        let order_id = adapter.place_order(&request).await.unwrap();
        let order = adapter.get_order_status(&order_id).await.unwrap();
        assert!((order.avg_price - 100.10).abs() < 0.001);
    }

    #[tokio::test]
    async fn test_insufficient_funds_rejected() {
        let adapter = PaperTradingAdapter::with_config(100.0, "USD".into(), 0.0, 0.0);
        adapter.update_quote(&create_test_quote("AAPL", 150.0));

        let request = NewOrderRequest {
            symbol: Symbol("AAPL".into()),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: 10.0,
            price: None,
            stop_price: None,
            tag: None,
            broker_params: None,
        };

        let result = adapter.place_order(&request).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_paper_always_healthy() {
        let adapter = PaperTradingAdapter::new();
        assert_eq!(adapter.health(), AdapterHealth::Healthy);
    }

    // ---- Short selling accounting ----

    /// Overwrite the cached quote (tests move the market without waiting on timestamps).
    fn set_price(adapter: &PaperTradingAdapter, symbol: &str, last: f64) {
        adapter
            .quote_cache
            .insert(symbol.to_string(), create_test_quote(symbol, last));
    }

    async fn market(
        adapter: &PaperTradingAdapter,
        symbol: &str,
        side: OrderSide,
        quantity: f64,
    ) -> Result<OrderId> {
        adapter
            .place_order(&NewOrderRequest {
                symbol: Symbol(symbol.into()),
                side,
                order_type: OrderType::Market,
                quantity,
                price: None,
                stop_price: None,
                tag: None,
                broker_params: None,
            })
            .await
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    fn zero_cost(capital: f64) -> PaperTradingAdapter {
        PaperTradingAdapter::with_config(capital, "USD".into(), 0.0, 0.0)
    }

    #[tokio::test]
    async fn open_short_credits_cash_but_not_equity() {
        let adapter = zero_cost(100_000.0);
        set_price(&adapter, "AAPL", 100.0);
        market(&adapter, "AAPL", OrderSide::Sell, 10.0).await.unwrap();

        let positions = adapter.get_positions().await.unwrap();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].side, OrderSide::Sell);
        assert_eq!(positions[0].quantity, 10.0);
        let balance = adapter.get_account_balance().await.unwrap();
        // The 1000 of proceeds is cash, but it is owed back: equity is unchanged.
        assert!(near(balance.cash, 101_000.0));
        assert!(near(balance.total_value, 100_000.0), "equity was {}", balance.total_value);
        assert!(near(balance.unrealized_pnl, 0.0));
        assert!(near(balance.realized_pnl, 0.0));
        // 50% of the 1000 notional is held as margin.
        assert!(near(balance.margin_used, 500.0));
    }

    #[tokio::test]
    async fn short_equity_follows_the_price_both_ways() {
        let adapter = zero_cost(100_000.0);
        set_price(&adapter, "AAPL", 100.0);
        market(&adapter, "AAPL", OrderSide::Sell, 10.0).await.unwrap();

        set_price(&adapter, "AAPL", 90.0);
        let balance = adapter.get_account_balance().await.unwrap();
        assert!(near(balance.unrealized_pnl, 100.0));
        assert!(near(balance.total_value, 100_100.0));
        assert!(near(balance.margin_used, 450.0));

        set_price(&adapter, "AAPL", 120.0);
        let balance = adapter.get_account_balance().await.unwrap();
        assert!(near(balance.unrealized_pnl, -200.0));
        assert!(near(balance.total_value, 99_800.0));
        let position = &adapter.get_positions().await.unwrap()[0];
        assert!(near(position.pnl, -200.0));
        assert!(near(position.pnl_pct, -20.0));
    }

    #[tokio::test]
    async fn equity_without_a_quote_marks_at_average_price() {
        let adapter = zero_cost(100_000.0);
        set_price(&adapter, "AAPL", 100.0);
        market(&adapter, "AAPL", OrderSide::Sell, 10.0).await.unwrap();
        adapter.quote_cache.clear();
        let balance = adapter.get_account_balance().await.unwrap();
        assert!(near(balance.total_value, 100_000.0));
        assert!(near(balance.unrealized_pnl, 0.0));
    }

    #[tokio::test]
    async fn add_to_short_blends_the_average_price() {
        let adapter = zero_cost(100_000.0);
        set_price(&adapter, "AAPL", 100.0);
        market(&adapter, "AAPL", OrderSide::Sell, 10.0).await.unwrap();
        set_price(&adapter, "AAPL", 110.0);
        market(&adapter, "AAPL", OrderSide::Sell, 10.0).await.unwrap();

        let position = adapter.get_positions().await.unwrap().remove(0);
        assert_eq!(position.side, OrderSide::Sell);
        assert_eq!(position.quantity, 20.0);
        assert!(near(position.avg_price, 105.0));
        let balance = adapter.get_account_balance().await.unwrap();
        assert!(near(balance.cash, 100_000.0 + 1000.0 + 1100.0));
        assert!(near(balance.realized_pnl, 0.0));
        // Short 20 at 105 average, marked at 110: -100.
        assert!(near(balance.unrealized_pnl, -100.0));
        assert!(near(balance.total_value, 99_900.0));
    }

    #[tokio::test]
    async fn partial_cover_realizes_part_and_keeps_the_rest_short() {
        let adapter = zero_cost(100_000.0);
        set_price(&adapter, "AAPL", 100.0);
        market(&adapter, "AAPL", OrderSide::Sell, 10.0).await.unwrap();
        set_price(&adapter, "AAPL", 90.0);
        market(&adapter, "AAPL", OrderSide::Buy, 4.0).await.unwrap();

        let position = adapter.get_positions().await.unwrap().remove(0);
        assert_eq!(position.side, OrderSide::Sell);
        assert_eq!(position.quantity, 6.0);
        assert!(near(position.avg_price, 100.0));
        let balance = adapter.get_account_balance().await.unwrap();
        assert!(near(balance.realized_pnl, 40.0));
        assert!(near(balance.unrealized_pnl, 60.0));
        assert!(near(balance.cash, 100_000.0 + 1000.0 - 360.0));
        assert!(near(balance.total_value, 100_100.0));
    }

    #[tokio::test]
    async fn full_cover_closes_the_short_and_equity_equals_cash() {
        let adapter = zero_cost(100_000.0);
        set_price(&adapter, "AAPL", 100.0);
        market(&adapter, "AAPL", OrderSide::Sell, 10.0).await.unwrap();
        set_price(&adapter, "AAPL", 90.0);
        market(&adapter, "AAPL", OrderSide::Buy, 10.0).await.unwrap();

        assert!(adapter.get_positions().await.unwrap().is_empty());
        let balance = adapter.get_account_balance().await.unwrap();
        assert!(near(balance.cash, 100_100.0));
        assert!(near(balance.total_value, 100_100.0));
        assert!(near(balance.realized_pnl, 100.0));
        assert!(near(balance.unrealized_pnl, 0.0));
        assert!(near(balance.margin_used, 0.0));
    }

    #[tokio::test]
    async fn losing_cover_realizes_a_loss() {
        let adapter = zero_cost(100_000.0);
        set_price(&adapter, "AAPL", 100.0);
        market(&adapter, "AAPL", OrderSide::Sell, 10.0).await.unwrap();
        set_price(&adapter, "AAPL", 108.0);
        market(&adapter, "AAPL", OrderSide::Buy, 10.0).await.unwrap();
        let balance = adapter.get_account_balance().await.unwrap();
        assert!(near(balance.realized_pnl, -80.0));
        assert!(near(balance.total_value, 99_920.0));
    }

    #[tokio::test]
    async fn buy_through_zero_covers_the_short_and_opens_a_long() {
        let adapter = zero_cost(100_000.0);
        set_price(&adapter, "AAPL", 100.0);
        market(&adapter, "AAPL", OrderSide::Sell, 10.0).await.unwrap();
        set_price(&adapter, "AAPL", 90.0);
        market(&adapter, "AAPL", OrderSide::Buy, 15.0).await.unwrap();

        let position = adapter.get_positions().await.unwrap().remove(0);
        assert_eq!(position.side, OrderSide::Buy);
        assert_eq!(position.quantity, 5.0);
        assert!(near(position.avg_price, 90.0));
        let balance = adapter.get_account_balance().await.unwrap();
        assert!(near(balance.realized_pnl, 100.0));
        assert!(near(balance.cash, 100_000.0 + 1000.0 - 1350.0));
        assert!(near(balance.total_value, 100_100.0));
        assert!(near(balance.unrealized_pnl, 0.0));
    }

    #[tokio::test]
    async fn sell_through_zero_closes_the_long_and_opens_a_short() {
        let adapter = zero_cost(100_000.0);
        set_price(&adapter, "AAPL", 100.0);
        market(&adapter, "AAPL", OrderSide::Buy, 10.0).await.unwrap();
        set_price(&adapter, "AAPL", 110.0);
        market(&adapter, "AAPL", OrderSide::Sell, 15.0).await.unwrap();

        let position = adapter.get_positions().await.unwrap().remove(0);
        assert_eq!(position.side, OrderSide::Sell);
        assert_eq!(position.quantity, 5.0);
        assert!(near(position.avg_price, 110.0));
        let balance = adapter.get_account_balance().await.unwrap();
        assert!(near(balance.realized_pnl, 100.0));
        assert!(near(balance.cash, 100_000.0 - 1000.0 + 1650.0));
        assert!(near(balance.total_value, 100_100.0));
    }

    #[tokio::test]
    async fn commission_is_charged_on_short_entry_and_cover() {
        let adapter = PaperTradingAdapter::with_config(100_000.0, "USD".into(), 0.0, 10.0);
        set_price(&adapter, "AAPL", 100.0);
        market(&adapter, "AAPL", OrderSide::Sell, 10.0).await.unwrap();
        market(&adapter, "AAPL", OrderSide::Buy, 10.0).await.unwrap();
        let balance = adapter.get_account_balance().await.unwrap();
        // 10 bps of 1000 on each leg.
        assert!(near(balance.realized_pnl, -2.0));
        assert!(near(balance.cash, 100_000.0 - 2.0));
        assert!(near(balance.total_value, balance.cash));
    }

    #[tokio::test]
    async fn short_beyond_margin_is_rejected_with_a_reason() {
        // Equity 10_000, 50% margin: at most 20_000 of shorts.
        let adapter = zero_cost(10_000.0);
        set_price(&adapter, "AAPL", 100.0);
        let error = market(&adapter, "AAPL", OrderSide::Sell, 250.0)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("Insufficient margin"), "{error}");
        assert!(adapter.get_positions().await.unwrap().is_empty());
        assert!(adapter.state.read().await.orders.is_empty());
        let balance = adapter.get_account_balance().await.unwrap();
        assert!(near(balance.cash, 10_000.0));

        // Exactly at the limit is allowed.
        market(&adapter, "AAPL", OrderSide::Sell, 200.0).await.unwrap();
        // Any increase now breaches it.
        let error = market(&adapter, "AAPL", OrderSide::Sell, 1.0)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("Insufficient margin"), "{error}");
        assert_eq!(adapter.get_positions().await.unwrap()[0].quantity, 200.0);
    }

    #[tokio::test]
    async fn adding_to_a_short_counts_the_whole_short_book() {
        let adapter = zero_cost(10_000.0);
        set_price(&adapter, "AAPL", 100.0);
        set_price(&adapter, "MSFT", 100.0);
        market(&adapter, "AAPL", OrderSide::Sell, 150.0).await.unwrap();
        // 15_000 already short; another 50 of a different symbol makes 20_000 and fits...
        market(&adapter, "MSFT", OrderSide::Sell, 50.0).await.unwrap();
        // ...one more share does not.
        assert!(market(&adapter, "MSFT", OrderSide::Sell, 1.0).await.is_err());
    }

    #[tokio::test]
    async fn selling_through_zero_into_too_large_a_short_is_rejected() {
        let adapter = zero_cost(10_000.0);
        set_price(&adapter, "AAPL", 100.0);
        market(&adapter, "AAPL", OrderSide::Buy, 50.0).await.unwrap();
        // Closing 50 and shorting 300 more needs 15_000 of margin on 10_000 of equity.
        let error = market(&adapter, "AAPL", OrderSide::Sell, 350.0)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("Insufficient margin"), "{error}");
        let position = adapter.get_positions().await.unwrap().remove(0);
        assert_eq!(position.side, OrderSide::Buy);
        assert_eq!(position.quantity, 50.0);
        // Selling the long itself, or flipping within margin, still works.
        market(&adapter, "AAPL", OrderSide::Sell, 150.0).await.unwrap();
        let position = adapter.get_positions().await.unwrap().remove(0);
        assert_eq!(position.side, OrderSide::Sell);
        assert_eq!(position.quantity, 100.0);
    }

    #[tokio::test]
    async fn covering_is_allowed_even_when_the_account_is_under_water() {
        let adapter = zero_cost(10_000.0);
        set_price(&adapter, "AAPL", 100.0);
        market(&adapter, "AAPL", OrderSide::Sell, 200.0).await.unwrap();
        // The price doubles: equity is -10_000 and cash (30_000) cannot pay 40_000 to cover.
        set_price(&adapter, "AAPL", 200.0);
        assert!(market(&adapter, "AAPL", OrderSide::Sell, 1.0).await.is_err());
        market(&adapter, "AAPL", OrderSide::Buy, 100.0).await.unwrap();
        market(&adapter, "AAPL", OrderSide::Buy, 100.0).await.unwrap();
        assert!(adapter.get_positions().await.unwrap().is_empty());
        let balance = adapter.get_account_balance().await.unwrap();
        assert!(near(balance.realized_pnl, -20_000.0));
        assert!(near(balance.cash, -10_000.0));
    }

    #[tokio::test]
    async fn a_buy_that_opens_a_long_still_needs_cash() {
        let adapter = zero_cost(1_000.0);
        set_price(&adapter, "AAPL", 100.0);
        set_price(&adapter, "MSFT", 100.0);
        market(&adapter, "MSFT", OrderSide::Sell, 10.0).await.unwrap();
        // Cash is 2000; buying 21 shares needs 2100 of it.
        let error = market(&adapter, "AAPL", OrderSide::Buy, 21.0)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("Insufficient funds"), "{error}");
        market(&adapter, "AAPL", OrderSide::Buy, 20.0).await.unwrap();
    }

    #[tokio::test]
    async fn margin_available_reflects_cash_and_free_equity() {
        let adapter = zero_cost(10_000.0);
        set_price(&adapter, "AAPL", 100.0);
        let flat = adapter.get_account_balance().await.unwrap();
        assert!(near(flat.margin_available, 10_000.0));
        assert!(near(flat.margin_used, 0.0));

        market(&adapter, "AAPL", OrderSide::Sell, 100.0).await.unwrap();
        let short = adapter.get_account_balance().await.unwrap();
        // Equity 10_000, margin held 5_000, cash 20_000: free equity is the tighter bound.
        assert!(near(short.margin_used, 5_000.0));
        assert!(near(short.margin_available, 5_000.0));
    }

    #[tokio::test]
    async fn long_only_balance_is_unchanged_by_the_short_rework() {
        let adapter = zero_cost(100_000.0);
        set_price(&adapter, "AAPL", 100.0);
        market(&adapter, "AAPL", OrderSide::Buy, 10.0).await.unwrap();
        set_price(&adapter, "AAPL", 110.0);
        let balance = adapter.get_account_balance().await.unwrap();
        assert!(near(balance.cash, 99_000.0));
        assert!(near(balance.margin_used, 1_000.0));
        assert!(near(balance.margin_available, 99_000.0));
        assert!(near(balance.unrealized_pnl, 100.0));
        assert!(near(balance.total_value, 100_100.0));
    }

    #[tokio::test]
    async fn short_margin_pct_can_be_configured() {
        let adapter = zero_cost(10_000.0).with_short_margin_pct(1.0);
        assert_eq!(adapter.short_margin_pct(), 1.0);
        set_price(&adapter, "AAPL", 100.0);
        // 100% margin: at most 10_000 of shorts.
        assert!(market(&adapter, "AAPL", OrderSide::Sell, 101.0).await.is_err());
        market(&adapter, "AAPL", OrderSide::Sell, 100.0).await.unwrap();
        assert_eq!(
            zero_cost(1.0).with_short_margin_pct(f64::NAN).short_margin_pct(),
            DEFAULT_SHORT_MARGIN_PCT
        );
        assert_eq!(PaperTradingAdapter::new().short_margin_pct(), 0.5);
    }
}
