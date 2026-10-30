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
            state: Arc::new(RwLock::new(PaperState::new(initial_capital))),
            quote_cache: Arc::new(DashMap::new()),
        }
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

    fn fill_locked(
        &self,
        state: &mut PaperState,
        order_id: &str,
        request: &NewOrderRequest,
        fill_price: f64,
    ) -> Result<()> {
        let trade_value = fill_price * request.quantity;
        let commission = self.calculate_commission(trade_value);

        // Check sufficient funds for buy
        if request.side == OrderSide::Buy && (trade_value + commission) > state.cash {
            let reason = format!(
                "Insufficient funds: need {:.2}, have {:.2}",
                trade_value + commission,
                state.cash
            );
            if let Some(order) = state.orders.get_mut(order_id) {
                order.status = OrderStatus::Rejected;
                order.updated_at = Utc::now();
                order.rejection_reason = Some(reason.clone());
            }
            return Err(anyhow!(reason));
        }

        // Execute the fill
        match request.side {
            OrderSide::Buy => {
                state.cash -= trade_value + commission;
            }
            OrderSide::Sell => {
                state.cash += trade_value - commission;
            }
        }

        // Fees are realized expenses on both entry and exit.
        state.realized_pnl -= commission;

        // Update order status
        if let Some(order) = state.orders.get_mut(order_id) {
            order.status = OrderStatus::Filled;
            order.filled_qty = request.quantity;
            order.avg_price = fill_price;
            order.updated_at = Utc::now();
        }

        // Update position
        self.update_position(state, request, fill_price);
        state.triggered_stops.remove(order_id);

        Ok(())
    }

    /// Update position after a fill
    fn update_position(&self, state: &mut PaperState, request: &NewOrderRequest, fill_price: f64) {
        let symbol_key = request.symbol.0.clone();

        if let Some(pos) = state.positions.get_mut(&symbol_key) {
            match (&pos.side, &request.side) {
                // Adding to position
                (OrderSide::Buy, OrderSide::Buy) | (OrderSide::Sell, OrderSide::Sell) => {
                    let total_qty = pos.quantity + request.quantity;
                    pos.avg_price =
                        (pos.avg_price * pos.quantity + fill_price * request.quantity) / total_qty;
                    pos.quantity = total_qty;
                }
                // Reducing or closing position
                _ => {
                    if request.quantity >= pos.quantity {
                        // Close the position
                        let pnl = match pos.side {
                            OrderSide::Buy => (fill_price - pos.avg_price) * pos.quantity,
                            OrderSide::Sell => (pos.avg_price - fill_price) * pos.quantity,
                        };
                        state.realized_pnl += pnl;

                        let remaining = request.quantity - pos.quantity;
                        if remaining > 0.0 {
                            // Open a new position in the opposite direction
                            state.positions.insert(
                                symbol_key,
                                Position {
                                    symbol: request.symbol.clone(),
                                    quantity: remaining,
                                    avg_price: fill_price,
                                    side: request.side.clone(),
                                    pnl: 0.0,
                                    pnl_pct: 0.0,
                                    broker_id: "paper".into(),
                                },
                            );
                        } else {
                            state.positions.remove(&request.symbol.0);
                        }
                        return;
                    } else {
                        // Partial close
                        let pnl = match pos.side {
                            OrderSide::Buy => (fill_price - pos.avg_price) * request.quantity,
                            OrderSide::Sell => (pos.avg_price - fill_price) * request.quantity,
                        };
                        state.realized_pnl += pnl;
                        pos.quantity -= request.quantity;
                    }
                }
            }

            // Update unrealized P&L based on last price
            if let Some(quote) = self.quote_cache.get(&request.symbol.0) {
                let pos = state.positions.get_mut(&symbol_key).unwrap();
                pos.pnl = match pos.side {
                    OrderSide::Buy => (quote.last - pos.avg_price) * pos.quantity,
                    OrderSide::Sell => (pos.avg_price - quote.last) * pos.quantity,
                };
                if pos.avg_price > 0.0 {
                    pos.pnl_pct = pos.pnl / (pos.avg_price * pos.quantity) * 100.0;
                }
            }
        } else {
            // New position
            state.positions.insert(
                symbol_key,
                Position {
                    symbol: request.symbol.clone(),
                    quantity: request.quantity,
                    avg_price: fill_price,
                    side: request.side.clone(),
                    pnl: 0.0,
                    pnl_pct: 0.0,
                    broker_id: "paper".into(),
                },
            );
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
        let positions_value: f64 = state
            .positions
            .values()
            .map(|p| p.quantity * p.avg_price)
            .sum();

        Ok(AccountBalance {
            total_value: state.cash + positions_value + unrealized_pnl,
            cash: state.cash,
            margin_used: positions_value,
            margin_available: state.cash,
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
}
