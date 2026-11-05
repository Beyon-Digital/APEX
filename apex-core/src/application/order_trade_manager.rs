use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, Result};
use dashmap::DashMap;
use tracing::{info, warn};

use crate::bus::message_bus::{BusMessage, MessageBus, Topic};
use crate::domain::models::*;
use crate::ports::execution::ExecutionPort;
use crate::ports::market_data::AdapterHealth;
use crate::ports::storage::StoragePort;

use super::risk_engine::{RiskEngine, RiskVerdict};

/// Paper orders exist only in memory for the current session. Mark any
/// previously journaled open orders cancelled before accepting new orders.
pub async fn cancel_orphaned_paper_orders(storage: &dyn StoragePort) -> Result<usize> {
    let mut count = 0;
    for status in [
        OrderStatus::Pending,
        OrderStatus::Open,
        OrderStatus::PartiallyFilled,
    ] {
        let orders = storage
            .query_orders(OrderQuery {
                symbol: None,
                status: Some(status),
                broker_id: Some("paper".into()),
                from: None,
                to: None,
                limit: None,
            })
            .await?;
        for mut order in orders {
            order.status = OrderStatus::Cancelled;
            order.updated_at = chrono::Utc::now();
            storage.update_order(&order).await?;
            count += 1;
        }
    }
    Ok(count)
}

/// Position map key: positions are tracked per broker so a paper position and a
/// live position in the same symbol never overwrite each other.
fn position_key(broker_id: &str, symbol: &str) -> String {
    format!("{broker_id}|{symbol}")
}

/// Order & Trade Manager — manages all order lifecycle
pub struct OrderTradeManager {
    risk_engine: Arc<RiskEngine>,
    execution: RwLock<HashMap<String, Arc<dyn ExecutionPort>>>,
    bus: Arc<MessageBus>,
    open_orders: Arc<DashMap<String, Order>>,
    positions: Arc<DashMap<String, Position>>,
    paper_realized: tokio::sync::Mutex<f64>,
    paper_operations: tokio::sync::Mutex<()>,
    /// Realized P&L already reported to the risk engine, per live broker.
    live_realized: tokio::sync::Mutex<HashMap<String, f64>>,
    /// Latest quotes, used to price live market orders for risk checks.
    quote_cache: RwLock<Option<Arc<DashMap<String, Quote>>>>,
}

impl OrderTradeManager {
    /// Create a new Order & Trade Manager
    pub fn new(risk_engine: Arc<RiskEngine>, bus: Arc<MessageBus>) -> Self {
        Self {
            risk_engine,
            execution: RwLock::new(HashMap::new()),
            bus,
            open_orders: Arc::new(DashMap::new()),
            positions: Arc::new(DashMap::new()),
            paper_realized: tokio::sync::Mutex::new(0.0),
            paper_operations: tokio::sync::Mutex::new(()),
            live_realized: tokio::sync::Mutex::new(HashMap::new()),
            quote_cache: RwLock::new(None),
        }
    }

    /// Provide the market-data quote cache so live market orders can be valued
    /// against the risk limits.
    pub fn set_quote_cache(&self, cache: Arc<DashMap<String, Quote>>) {
        *self.quote_cache.write().expect("quote cache lock poisoned") = Some(cache);
    }

    fn reference_price(&self, symbol: &Symbol) -> Option<f64> {
        self.quote_cache
            .read()
            .expect("quote cache lock poisoned")
            .as_ref()
            .and_then(|cache| cache.get(&symbol.0).map(|q| q.last))
            .filter(|p| *p > 0.0)
    }

    /// Register (or replace) an execution adapter. Safe to call at runtime.
    pub fn register_execution(&self, broker_id: String, adapter: Arc<dyn ExecutionPort>) {
        info!("Registering execution adapter: {}", broker_id);
        self.execution
            .write()
            .expect("execution registry lock poisoned")
            .insert(broker_id, adapter);
    }

    /// Remove an execution adapter, returning it so the caller can shut it down.
    pub fn unregister_execution(&self, broker_id: &str) -> Option<Arc<dyn ExecutionPort>> {
        info!("Unregistering execution adapter: {}", broker_id);
        self.execution
            .write()
            .expect("execution registry lock poisoned")
            .remove(broker_id)
    }

    /// Look up an execution adapter. The registry lock is released before the
    /// returned `Arc` is awaited on.
    fn adapter(&self, broker_id: &str) -> Result<Arc<dyn ExecutionPort>> {
        self.execution
            .read()
            .expect("execution registry lock poisoned")
            .get(broker_id)
            .cloned()
            .ok_or_else(|| anyhow!("No execution adapter found for broker: {}", broker_id))
    }

    /// Submit a new order — risk check → journal → dispatch → update
    #[tracing::instrument(skip(self, request), fields(symbol = %request.symbol.0, side = ?request.side))]
    pub async fn submit_order(&self, request: NewOrderRequest, broker_id: &str) -> Result<OrderId> {
        let _paper_guard = if broker_id == "paper" {
            Some(self.paper_operations.lock().await)
        } else {
            None
        };
        let adapter = self.adapter(broker_id)?;
        // 1. Risk check (sync, no I/O) — halt check is FIRST inside check()
        let account = self.get_account_balance(broker_id).await?;
        let mut risk_request = request.clone();
        if broker_id == "paper"
            && (risk_request.order_type == OrderType::Market || risk_request.price.is_none())
        {
            let current = adapter
                .latest_quote(&request.symbol)
                .map(|q| q.last * 1.0002);
            risk_request.price = match (
                current,
                if request.order_type == OrderType::Market {
                    None
                } else {
                    request.stop_price
                },
            ) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
        }
        if broker_id != "paper" && risk_request.price.is_none() {
            // Live market/stop orders carry no price; value them from the latest
            // quote so max_order_value still applies, and fail closed without one.
            risk_request.price = self
                .reference_price(&request.symbol)
                .or(request.stop_price);
            if risk_request.price.is_none() {
                return Err(anyhow!(
                    "No live quote for {}; cannot verify the order against risk limits",
                    request.symbol.0
                ));
            }
        }
        let verdict = self.risk_engine.check(&risk_request, &account);
        match verdict {
            RiskVerdict::Pass => {}
            RiskVerdict::Reject(reason) => {
                warn!("Order rejected by risk engine: {}", reason);
                return Err(anyhow!("Risk check failed: {}", reason));
            }
        }

        // 2. Get the execution adapter
        // 3. Dispatch to broker adapter
        let order_id = adapter.place_order(&request).await?;

        // 4. Get order status and track it
        if let Ok(order) = adapter.get_order_status(&order_id).await {
            let has_fill = order.filled_qty > 0.0;
            self.open_orders.insert(order_id.0.clone(), order.clone());

            // 5. Publish OrderUpdate to message bus
            self.bus.publish(
                Topic::OrderUpdate(order_id.0.clone()),
                BusMessage::OrderData(order),
            );
            if has_fill {
                if broker_id == "paper" {
                    // Serialize account snapshots so concurrent strategies cannot
                    // count the same realized P&L twice.
                    let mut realized = self.paper_realized.lock().await;
                    if let Ok(balance) = self.get_account_balance(broker_id).await {
                        self.risk_engine
                            .update_pnl(balance.realized_pnl - *realized);
                        *realized = balance.realized_pnl;
                    }
                }
                if broker_id != "paper" {
                    self.sync_live_pnl(broker_id).await;
                }
                if let Err(error) = self.reconcile_positions(broker_id).await {
                    warn!(%error, "Order filled; position reconciliation will retry");
                }
            }
        }

        // Immediately evaluate a fresh cached quote as well as subsequent ones.
        if broker_id == "paper" && request.order_type != OrderType::Market {
            if let Some(quote) = adapter.latest_quote(&request.symbol) {
                self.process_paper_quote_locked(&quote).await?;
            }
        }

        Ok(order_id)
    }

    /// Cancel an order
    #[tracing::instrument(skip(self, order_id), fields(order_id = %order_id.0))]
    pub async fn cancel_order(&self, order_id: &OrderId, broker_id: &str) -> Result<()> {
        let _paper_guard = if broker_id == "paper" {
            Some(self.paper_operations.lock().await)
        } else {
            None
        };
        let adapter = self.adapter(broker_id)?;

        adapter.cancel_order(order_id).await?;

        // Update local tracking
        if let Some(mut order) = self.open_orders.get_mut(&order_id.0) {
            order.status = OrderStatus::Cancelled;
        }

        self.bus.publish(
            Topic::OrderUpdate(order_id.0.clone()),
            BusMessage::OrderData(
                self.open_orders
                    .get(&order_id.0)
                    .map(|o| o.clone())
                    .unwrap_or_else(|| Order {
                        id: order_id.clone(),
                        symbol: Symbol("".into()),
                        side: OrderSide::Buy,
                        order_type: OrderType::Market,
                        quantity: 0.0,
                        price: None,
                        stop_price: None,
                        status: OrderStatus::Cancelled,
                        filled_qty: 0.0,
                        avg_price: 0.0,
                        created_at: chrono::Utc::now(),
                        updated_at: chrono::Utc::now(),
                        broker_id: broker_id.to_string(),
                        source: "manual".into(),
                        rejection_reason: None,
                    }),
            ),
        );

        Ok(())
    }

    /// Modify an existing order.
    #[tracing::instrument(skip(self, order_id, params), fields(order_id = %order_id.0))]
    pub async fn modify_order(
        &self,
        order_id: &OrderId,
        broker_id: &str,
        params: &ModifyParams,
    ) -> Result<()> {
        let _paper_guard = if broker_id == "paper" {
            Some(self.paper_operations.lock().await)
        } else {
            None
        };
        let adapter = self.adapter(broker_id)?;

        adapter.modify_order(order_id, params).await?;

        let order = adapter.get_order_status(order_id).await?;
        self.open_orders.insert(order_id.0.clone(), order.clone());

        self.bus.publish(
            Topic::OrderUpdate(order_id.0.clone()),
            BusMessage::OrderData(order),
        );

        Ok(())
    }

    /// Match resting paper orders against a fresh quote, serializing fills and
    /// cancellations so each order can transition only once.
    pub async fn process_paper_quote(&self, quote: &Quote) -> Result<()> {
        let _guard = self.paper_operations.lock().await;
        self.process_paper_quote_locked(quote).await
    }

    async fn process_paper_quote_locked(&self, quote: &Quote) -> Result<()> {
        let adapter = self.adapter("paper")?;
        for (order, execution_price) in adapter.eligible_orders(quote).await? {
            let account = adapter.get_account_balance().await?;
            let request = NewOrderRequest {
                symbol: order.symbol.clone(),
                side: order.side.clone(),
                order_type: order.order_type.clone(),
                quantity: order.quantity,
                price: Some(execution_price),
                stop_price: order.stop_price,
                tag: Some(order.source.clone()),
                broker_params: None,
            };
            let rejection_reason = match self.risk_engine.check_resting(&request, &account) {
                RiskVerdict::Pass => None,
                RiskVerdict::Reject(reason) => {
                    warn!(order = %order.id.0, %reason, "Resting paper order rejected by risk check");
                    Some(format!("Risk check failed: {reason}"))
                }
            };
            if let Some(updated) = adapter
                .resolve_order(&order.id, quote, rejection_reason.as_deref())
                .await?
            {
                let filled = updated.status == OrderStatus::Filled;
                self.open_orders
                    .insert(updated.id.0.clone(), updated.clone());
                self.bus.publish(
                    Topic::OrderUpdate(updated.id.0.clone()),
                    BusMessage::OrderData(updated),
                );
                if filled {
                    let mut realized = self.paper_realized.lock().await;
                    let balance = adapter.get_account_balance().await?;
                    self.risk_engine
                        .update_pnl(balance.realized_pnl - *realized);
                    *realized = balance.realized_pnl;
                    self.reconcile_positions("paper").await?;
                }
            }
        }
        Ok(())
    }

    /// Handle a fill event — update positions and P&L
    #[tracing::instrument(skip(self, fill), fields(order = %fill.order_id.0, symbol = %fill.symbol.0))]
    pub async fn handle_fill(&self, fill: FillEvent) {
        // 1. Update order status
        if let Some(mut order) = self.open_orders.get_mut(&fill.order_id.0) {
            if fill.quantity >= order.quantity {
                order.status = OrderStatus::Filled;
            } else {
                order.status = OrderStatus::PartiallyFilled;
            }
            order.filled_qty = fill.quantity;
            order.avg_price = fill.price;
            order.updated_at = chrono::Utc::now();

            self.bus.publish(
                Topic::OrderUpdate(fill.order_id.0.clone()),
                BusMessage::OrderData(order.clone()),
            );
        }

        // 2. Update position
        let symbol_key = position_key(&fill.broker_id, &fill.symbol.0);
        let pnl = if let Some(mut pos) = self.positions.get_mut(&symbol_key) {
            // Existing position — calculate P&L
            let pnl = match (&pos.side, &fill.side) {
                (OrderSide::Buy, OrderSide::Sell) => (fill.price - pos.avg_price) * fill.quantity,
                (OrderSide::Sell, OrderSide::Buy) => (pos.avg_price - fill.price) * fill.quantity,
                _ => 0.0, // Adding to position
            };
            pos.pnl += pnl;
            pnl
        } else {
            // New position
            self.positions.insert(
                symbol_key.clone(),
                Position {
                    symbol: fill.symbol.clone(),
                    quantity: fill.quantity,
                    avg_price: fill.price,
                    side: fill.side,
                    pnl: 0.0,
                    pnl_pct: 0.0,
                    broker_id: fill.broker_id,
                },
            );
            0.0
        };

        // 3. Update session P&L in risk engine
        if pnl != 0.0 {
            self.risk_engine.update_pnl(pnl - fill.commission);
        }

        // 4. Publish position update
        self.bus.publish(
            Topic::PositionUpdate,
            BusMessage::PositionData(
                self.positions
                    .get(&symbol_key)
                    .map(|p| p.clone())
                    .unwrap_or_else(|| Position {
                        symbol: fill.symbol,
                        quantity: 0.0,
                        avg_price: 0.0,
                        side: OrderSide::Buy,
                        pnl: 0.0,
                        pnl_pct: 0.0,
                        broker_id: String::new(),
                    }),
            ),
        );
    }

    /// Report a live broker's realized P&L change to the risk engine so the
    /// daily-loss halt covers live trading, not only paper.
    pub async fn sync_live_pnl(&self, broker_id: &str) {
        let Ok(balance) = self.get_account_balance(broker_id).await else {
            return;
        };
        let mut realized = self.live_realized.lock().await;
        let previous = realized.get(broker_id).copied().unwrap_or(0.0);
        let delta = balance.realized_pnl - previous;
        if delta != 0.0 {
            self.risk_engine.update_pnl(delta);
            realized.insert(broker_id.to_string(), balance.realized_pnl);
        }
    }

    /// Apply an order state pushed by (or polled from) a live broker: track it,
    /// publish it, and on a new fill refresh positions and P&L. Orders placed
    /// outside APEX are picked up the same way.
    pub async fn apply_broker_order_update(&self, order: Order) {
        let previous_fill = self
            .open_orders
            .get(&order.id.0)
            .map(|existing| (existing.filled_qty, existing.status.clone()));
        if let Some((filled, status)) = &previous_fill {
            if *filled == order.filled_qty && *status == order.status {
                return; // nothing changed
            }
        }
        let broker_id = order.broker_id.clone();
        let new_fill = order.filled_qty > previous_fill.map(|(f, _)| f).unwrap_or(0.0);
        self.open_orders.insert(order.id.0.clone(), order.clone());
        self.bus.publish(
            Topic::OrderUpdate(order.id.0.clone()),
            BusMessage::OrderData(order),
        );
        if new_fill && broker_id != "paper" {
            self.sync_live_pnl(&broker_id).await;
            if let Err(error) = self.reconcile_positions(&broker_id).await {
                warn!(%error, "Broker fill received; position reconciliation will retry");
            }
        }
    }

    /// Follow a broker's push stream of order updates, if it offers one.
    /// Call after registering the adapter; the task ends when the adapter is dropped.
    pub fn attach_order_stream(self: &Arc<Self>, broker_id: &str) {
        let Ok(adapter) = self.adapter(broker_id) else { return };
        let Some(mut updates) = adapter.subscribe_order_updates() else { return };
        let otm = self.clone();
        tokio::spawn(async move {
            loop {
                match updates.recv().await {
                    Ok(order) => otm.apply_broker_order_update(order).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// Poll working live orders so fills and cancels are seen even when the
    /// broker offers no push stream (or the stream drops).
    pub fn start_live_order_sync(otm: Arc<Self>, interval: std::time::Duration) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                otm.poll_live_orders().await;
            }
        });
    }

    async fn poll_live_orders(&self) {
        let working: Vec<Order> = self
            .open_orders
            .iter()
            .filter(|entry| {
                let order = entry.value();
                order.broker_id != "paper"
                    && matches!(
                        order.status,
                        OrderStatus::Pending | OrderStatus::Open | OrderStatus::PartiallyFilled
                    )
            })
            .map(|entry| entry.value().clone())
            .collect();
        for order in working {
            let Ok(adapter) = self.adapter(&order.broker_id) else { continue };
            if !adapter.is_authenticated() {
                continue;
            }
            match adapter.get_order_status(&order.id).await {
                Ok(latest) => self.apply_broker_order_update(latest).await,
                Err(error) => {
                    warn!(order = %order.id.0, %error, "Live order status poll failed");
                }
            }
        }
    }

    /// Holdings reported by a broker.
    pub async fn get_holdings(&self, broker_id: &str) -> Result<Vec<Position>> {
        self.adapter(broker_id)?.get_holdings().await
    }

    /// Reconcile positions with broker
    #[tracing::instrument(skip(self), fields(broker = %broker_id))]
    pub async fn reconcile_positions(&self, broker_id: &str) -> Result<()> {
        let adapter = self.adapter(broker_id)?;

        let broker_positions = adapter.get_positions().await?;

        let live_keys: std::collections::HashSet<_> = broker_positions
            .iter()
            .filter(|p| p.quantity != 0.0)
            .map(|p| position_key(broker_id, &p.symbol.0))
            .collect();
        let closed: Vec<_> = self
            .positions
            .iter()
            .filter(|p| p.broker_id == broker_id && !live_keys.contains(p.key()))
            .map(|p| p.key().clone())
            .collect();
        for key in closed {
            if let Some((_, mut position)) = self.positions.remove(&key) {
                position.quantity = 0.0;
                self.bus
                    .publish(Topic::PositionUpdate, BusMessage::PositionData(position));
            }
        }
        for mut broker_pos in broker_positions.into_iter().filter(|p| p.quantity != 0.0) {
            broker_pos.broker_id = broker_id.to_string();
            self.positions.insert(
                position_key(broker_id, &broker_pos.symbol.0),
                broker_pos.clone(),
            );
            self.bus
                .publish(Topic::PositionUpdate, BusMessage::PositionData(broker_pos));
        }

        Ok(())
    }

    /// Get account balance from a broker
    pub async fn get_account_balance(&self, broker_id: &str) -> Result<AccountBalance> {
        let adapter = self.adapter(broker_id)?;
        adapter.get_account_balance().await
    }

    /// Snapshot execution adapter health by broker id.
    pub fn execution_health(&self) -> Vec<(String, AdapterHealth)> {
        self.execution
            .read()
            .expect("execution registry lock poisoned")
            .iter()
            .map(|(broker_id, adapter)| (broker_id.clone(), adapter.health()))
            .collect()
    }

    /// Get all open orders
    pub fn open_orders(&self) -> Vec<Order> {
        self.open_orders.iter().map(|r| r.value().clone()).collect()
    }

    /// The position a given broker holds in `symbol`, if any.
    pub fn position(&self, broker_id: &str, symbol: &str) -> Option<Position> {
        self.positions
            .get(&position_key(broker_id, symbol))
            .map(|p| p.clone())
    }

    /// Get all positions
    pub fn get_positions(&self) -> Vec<Position> {
        self.positions.iter().map(|r| r.value().clone()).collect()
    }

    /// Get the number of registered execution adapters
    pub fn execution_adapter_count(&self) -> usize {
        self.execution
            .read()
            .expect("execution registry lock poisoned")
            .len()
    }

    /// Get the list of registered broker IDs
    pub fn broker_ids(&self) -> Vec<String> {
        self.execution
            .read()
            .expect("execution registry lock poisoned")
            .keys()
            .cloned()
            .collect()
    }

    /// Get the list of authenticated broker IDs.
    pub fn authenticated_broker_ids(&self) -> Vec<String> {
        self.execution
            .read()
            .expect("execution registry lock poisoned")
            .iter()
            .filter_map(|(broker_id, adapter)| {
                adapter.is_authenticated().then(|| broker_id.clone())
            })
            .collect()
    }

    /// Start a periodic position reconciliation loop.
    ///
    /// Every `interval` seconds, reconcile positions with all registered
    /// brokers. This runs as a background Tokio task and logs any
    /// discrepancies.
    pub fn start_reconciliation_loop(otm: Arc<Self>, interval: std::time::Duration) {
        tokio::spawn(async move {
            info!(
                "Starting position reconciliation loop (interval: {:?})",
                interval
            );
            loop {
                tokio::time::sleep(interval).await;

                let broker_ids = otm.authenticated_broker_ids();
                for broker_id in &broker_ids {
                    if let Err(e) = otm.reconcile_positions(broker_id).await {
                        warn!(broker_id = %broker_id, error = %e, "Reconciliation failed");
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::risk_engine::RiskConfig;

    #[test]
    fn test_create_otm() {
        let bus = Arc::new(MessageBus::new());
        let risk = Arc::new(RiskEngine::new(RiskConfig::default()));
        let otm = OrderTradeManager::new(risk, bus);
        assert_eq!(otm.execution_adapter_count(), 0);
        assert_eq!(otm.open_orders().len(), 0);
        assert_eq!(otm.get_positions().len(), 0);
    }

    #[tokio::test]
    async fn test_submit_order_no_adapter() {
        let bus = Arc::new(MessageBus::new());
        let risk = Arc::new(RiskEngine::new(RiskConfig::default()));
        let otm = OrderTradeManager::new(risk, bus);

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

        let result = otm.submit_order(request, "nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_handle_fill_creates_position() {
        let bus = Arc::new(MessageBus::new());
        let risk = Arc::new(RiskEngine::new(RiskConfig::default()));
        let otm = OrderTradeManager::new(risk, bus);

        let fill = FillEvent {
            order_id: OrderId("test-1".into()),
            symbol: Symbol("AAPL".into()),
            side: OrderSide::Buy,
            quantity: 10.0,
            price: 150.0,
            commission: 0.5,
            filled_at: chrono::Utc::now(),
            broker_id: "paper".into(),
        };

        otm.handle_fill(fill).await;

        let positions = otm.get_positions();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].quantity, 10.0);
        assert!((positions[0].avg_price - 150.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_handle_fill_updates_risk_pnl() {
        let bus = Arc::new(MessageBus::new());
        let risk = Arc::new(RiskEngine::new(RiskConfig::default()));
        let otm = OrderTradeManager::new(risk.clone(), bus);

        // Create initial position
        let fill1 = FillEvent {
            order_id: OrderId("test-1".into()),
            symbol: Symbol("AAPL".into()),
            side: OrderSide::Buy,
            quantity: 10.0,
            price: 150.0,
            commission: 0.0,
            filled_at: chrono::Utc::now(),
            broker_id: "paper".into(),
        };
        otm.handle_fill(fill1).await;

        // Sell at a loss
        let fill2 = FillEvent {
            order_id: OrderId("test-2".into()),
            symbol: Symbol("AAPL".into()),
            side: OrderSide::Sell,
            quantity: 10.0,
            price: 140.0,
            commission: 1.0,
            filled_at: chrono::Utc::now(),
            broker_id: "paper".into(),
        };
        otm.handle_fill(fill2).await;

        // P&L should be (140-150) * 10 - 1.0 = -101.0
        assert!(risk.session_pnl() < 0.0);
    }

    /// Scripted live broker: a settable balance, positions and order status.
    struct MockLive {
        realized: std::sync::Mutex<f64>,
        status: std::sync::Mutex<Option<Order>>,
        placed: std::sync::Mutex<Vec<NewOrderRequest>>,
    }

    impl MockLive {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                realized: std::sync::Mutex::new(0.0),
                status: std::sync::Mutex::new(None),
                placed: std::sync::Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait::async_trait]
    impl ExecutionPort for MockLive {
        async fn place_order(&self, order: &NewOrderRequest) -> Result<OrderId> {
            self.placed.lock().unwrap().push(order.clone());
            Ok(OrderId("live-1".into()))
        }
        async fn cancel_order(&self, _: &OrderId) -> Result<()> {
            Ok(())
        }
        async fn modify_order(&self, _: &OrderId, _: &ModifyParams) -> Result<()> {
            Ok(())
        }
        async fn get_order_status(&self, _: &OrderId) -> Result<Order> {
            self.status.lock().unwrap().clone().ok_or_else(|| anyhow!("none"))
        }
        async fn get_positions(&self) -> Result<Vec<Position>> {
            Ok(Vec::new())
        }
        async fn get_account_balance(&self) -> Result<AccountBalance> {
            Ok(AccountBalance {
                total_value: 1_000_000.0,
                cash: 1_000_000.0,
                margin_used: 0.0,
                margin_available: 1_000_000.0,
                unrealized_pnl: 0.0,
                realized_pnl: *self.realized.lock().unwrap(),
                currency: "INR".into(),
            })
        }
        fn broker_id(&self) -> &'static str {
            "live"
        }
        fn supported_order_types(&self) -> &[OrderType] {
            &[OrderType::Market, OrderType::Limit]
        }
        fn health(&self) -> AdapterHealth {
            AdapterHealth::Healthy
        }
    }

    fn live_order(status: OrderStatus, filled: f64) -> Order {
        Order {
            id: OrderId("live-1".into()),
            symbol: Symbol("RELIANCE.NS".into()),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: 10.0,
            price: Some(100.0),
            stop_price: None,
            status,
            filled_qty: filled,
            avg_price: 100.0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            broker_id: "live".into(),
            source: "api".into(),
            rejection_reason: None,
        }
    }

    fn live_otm() -> (OrderTradeManager, Arc<MockLive>, Arc<RiskEngine>) {
        let bus = Arc::new(MessageBus::new());
        let risk = Arc::new(RiskEngine::new(RiskConfig::default()));
        let otm = OrderTradeManager::new(risk.clone(), bus);
        let broker = MockLive::new();
        otm.register_execution("live".into(), broker.clone());
        (otm, broker, risk)
    }

    fn market_buy(symbol: &str) -> NewOrderRequest {
        NewOrderRequest {
            symbol: Symbol(symbol.into()),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: 10.0,
            price: None,
            stop_price: None,
            tag: None,
            broker_params: None,
        }
    }

    #[tokio::test]
    async fn live_market_order_without_quote_fails_closed() {
        let (otm, broker, _) = live_otm();
        let err = otm.submit_order(market_buy("RELIANCE.NS"), "live").await.unwrap_err();
        assert!(err.to_string().contains("No live quote"));
        assert!(broker.placed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn live_market_order_is_valued_from_quote_cache() {
        let (otm, broker, _) = live_otm();
        let cache = Arc::new(DashMap::new());
        cache.insert(
            "RELIANCE.NS".to_string(),
            Quote {
                symbol: Symbol("RELIANCE.NS".into()),
                bid: 99.0,
                ask: 101.0,
                last: 100.0,
                open: 100.0,
                high: 100.0,
                low: 100.0,
                volume: 1,
                change_pct: 0.0,
                vwap: 100.0,
                updated_at: chrono::Utc::now(),
            },
        );
        otm.set_quote_cache(cache.clone());
        *broker.status.lock().unwrap() = Some(live_order(OrderStatus::Open, 0.0));
        otm.submit_order(market_buy("RELIANCE.NS"), "live").await.unwrap();
        assert_eq!(broker.placed.lock().unwrap().len(), 1);

        // 10 shares at a (huge) last price now exceeds max_order_value.
        cache.get_mut("RELIANCE.NS").unwrap().last = 1_000_000.0;
        let err = otm.submit_order(market_buy("RELIANCE.NS"), "live").await.unwrap_err();
        assert!(err.to_string().contains("Risk check failed"));
    }

    #[tokio::test]
    async fn broker_updates_are_tracked_once_and_fills_report_pnl() {
        let (otm, broker, risk) = live_otm();
        let mut rx = otm.bus.subscribe(Topic::OrderUpdate("*".into()));

        otm.apply_broker_order_update(live_order(OrderStatus::Open, 0.0)).await;
        otm.apply_broker_order_update(live_order(OrderStatus::Open, 0.0)).await; // duplicate
        assert_eq!(otm.open_orders().len(), 1);

        *broker.realized.lock().unwrap() = -250.0;
        otm.apply_broker_order_update(live_order(OrderStatus::Filled, 10.0)).await;
        assert_eq!(otm.open_orders()[0].status, OrderStatus::Filled);
        assert_eq!(risk.session_pnl(), -250.0);

        // Re-reporting the same realized P&L must not double count.
        otm.sync_live_pnl("live").await;
        assert_eq!(risk.session_pnl(), -250.0);

        let mut published = 0;
        while rx.try_recv().is_ok() {
            published += 1;
        }
        assert_eq!(published, 2, "duplicate update must not be republished");
    }

    #[tokio::test]
    async fn polling_picks_up_fills_for_working_orders() {
        let (otm, broker, _) = live_otm();
        otm.apply_broker_order_update(live_order(OrderStatus::Open, 0.0)).await;
        *broker.status.lock().unwrap() = Some(live_order(OrderStatus::Filled, 10.0));
        otm.poll_live_orders().await;
        assert_eq!(otm.open_orders()[0].status, OrderStatus::Filled);
    }

    #[tokio::test]
    async fn positions_are_tracked_per_broker() {
        let bus = Arc::new(MessageBus::new());
        let risk = Arc::new(RiskEngine::new(RiskConfig::default()));
        let otm = OrderTradeManager::new(risk, bus);
        let fill = |broker: &str, qty: f64| FillEvent {
            order_id: OrderId(format!("{broker}-1")),
            symbol: Symbol("RELIANCE.NS".into()),
            side: OrderSide::Buy,
            quantity: qty,
            price: 100.0,
            commission: 0.0,
            filled_at: chrono::Utc::now(),
            broker_id: broker.into(),
        };
        otm.handle_fill(fill("paper", 5.0)).await;
        otm.handle_fill(fill("zerodha", 7.0)).await;
        assert_eq!(otm.get_positions().len(), 2);
        assert_eq!(otm.position("paper", "RELIANCE.NS").unwrap().quantity, 5.0);
        assert_eq!(otm.position("zerodha", "RELIANCE.NS").unwrap().quantity, 7.0);
        assert!(otm.position("binance", "RELIANCE.NS").is_none());
    }
}
