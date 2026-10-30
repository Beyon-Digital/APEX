use crate::domain::models::*;
use anyhow::Result;
use async_trait::async_trait;

use super::market_data::AdapterHealth;

/// Port trait for order execution (broker adapters)
#[async_trait]
pub trait ExecutionPort: Send + Sync {
    /// Place a new order
    async fn place_order(&self, order: &NewOrderRequest) -> Result<OrderId>;
    /// Cancel an existing order
    async fn cancel_order(&self, order_id: &OrderId) -> Result<()>;
    /// Modify an existing order
    async fn modify_order(&self, order_id: &OrderId, params: &ModifyParams) -> Result<()>;
    /// Get the current status of an order
    async fn get_order_status(&self, order_id: &OrderId) -> Result<Order>;
    /// Get all current positions
    async fn get_positions(&self) -> Result<Vec<Position>>;
    /// Get account balance
    async fn get_account_balance(&self) -> Result<AccountBalance>;
    /// For simulated brokers, update quote state and identify eligible resting
    /// orders with an estimated execution price. Real brokers fill independently.
    async fn eligible_orders(&self, _quote: &Quote) -> Result<Vec<(Order, f64)>> {
        Ok(Vec::new())
    }
    /// Atomically fill or reject a previously identified resting order.
    async fn resolve_order(
        &self,
        _id: &OrderId,
        _quote: &Quote,
        _rejection_reason: Option<&str>,
    ) -> Result<Option<Order>> {
        Ok(None)
    }
    /// Latest execution quote, if the adapter maintains one.
    fn latest_quote(&self, _symbol: &Symbol) -> Option<Quote> {
        None
    }
    /// Long-term holdings (delivery portfolio), for brokers that distinguish them
    /// from intraday positions.
    async fn get_holdings(&self) -> Result<Vec<Position>> {
        Ok(Vec::new())
    }
    /// Stream of order updates pushed by the broker (fills, rejections, cancels
    /// made elsewhere). `None` means the manager must poll.
    fn subscribe_order_updates(&self) -> Option<tokio::sync::broadcast::Receiver<Order>> {
        None
    }
    /// Unique broker identifier
    fn broker_id(&self) -> &'static str;
    /// Supported order types for this broker
    fn supported_order_types(&self) -> &[OrderType];
    /// Current health status
    fn health(&self) -> AdapterHealth;
    /// Whether the adapter is authenticated and ready for broker-side API calls.
    fn is_authenticated(&self) -> bool {
        true
    }
}
