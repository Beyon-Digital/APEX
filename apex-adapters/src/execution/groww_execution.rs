use anyhow::{Context, Result};
use apex_core::{
    domain::models::*,
    ports::{execution::ExecutionPort, market_data::AdapterHealth},
};
use async_trait::async_trait;
use chrono::Utc;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};
use tracing::{debug, info};

/// Groww execution adapter
///
/// Provides order placement, modification, cancellation, and position management
/// via Groww API. Groww is a popular Indian brokerage for equities and mutual funds.
pub struct GrowwExecutionAdapter {
    api_key: String,
    access_token: Arc<RwLock<Option<String>>>,
    client: Client,
    health: Arc<RwLock<AdapterHealth>>,
}

/// Groww order placement request
#[derive(Debug, Serialize)]
struct GrowwOrderRequest {
    #[serde(rename = "tradingSymbol")]
    trading_symbol: String,
    exchange: String,
    #[serde(rename = "transactionType")]
    transaction_type: String,
    #[serde(rename = "orderType")]
    order_type: String,
    quantity: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "triggerPrice")]
    trigger_price: Option<f64>,
    product: String,
    validity: String,
}

/// Groww order response
#[derive(Debug, Deserialize)]
struct GrowwOrderResponse {
    #[serde(rename = "orderId")]
    order_id: String,
}

/// Groww order status
#[derive(Debug, Deserialize)]
struct GrowwOrder {
    #[serde(rename = "orderId")]
    order_id: String,
    #[serde(rename = "tradingSymbol")]
    trading_symbol: String,
    #[serde(rename = "transactionType")]
    transaction_type: String,
    #[serde(rename = "orderType")]
    order_type: String,
    quantity: u64,
    #[serde(default)]
    price: f64,
    #[serde(default, rename = "triggerPrice")]
    trigger_price: f64,
    status: String,
    #[serde(default, rename = "filledQuantity")]
    filled_quantity: u64,
    #[serde(default, rename = "averagePrice")]
    average_price: f64,
    #[serde(default, rename = "orderTimestamp")]
    order_timestamp: String,
}

/// Groww position
#[derive(Debug, Deserialize)]
struct GrowwPosition {
    #[serde(rename = "tradingSymbol")]
    trading_symbol: String,
    #[serde(rename = "netQuantity")]
    net_quantity: i64,
    #[serde(rename = "averagePrice")]
    average_price: f64,
    #[serde(default)]
    pnl: f64,
}

/// Groww margin data
#[derive(Debug, Deserialize)]
struct GrowwMarginData {
    #[serde(default, rename = "availableCash")]
    available_cash: f64,
    #[serde(default, rename = "usedMargin")]
    used_margin: f64,
}

impl GrowwExecutionAdapter {
    /// Create a new Groww execution adapter
    pub fn new(api_key: String, access_token: Option<String>) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            api_key,
            access_token: Arc::new(RwLock::new(access_token)),
            client,
            health: Arc::new(RwLock::new(AdapterHealth::Healthy)),
        })
    }

    /// Set access token
    pub fn set_access_token(&self, token: String) {
        let mut access_token = self.access_token.write().unwrap();
        *access_token = Some(token);
        info!("Groww access token updated");
    }

    /// Clear access token.
    pub fn clear_access_token(&self) {
        let mut access_token = self.access_token.write().unwrap();
        *access_token = None;
        self.set_health(AdapterHealth::Unhealthy("Not authenticated".to_string()));
        info!("Groww access token cleared");
    }

    /// Get authorization header
    fn get_auth_header(&self) -> Result<String> {
        let token = self.access_token.read().unwrap();
        match token.as_ref() {
            Some(t) => Ok(format!("Bearer {}", t)),
            None => Err(anyhow::anyhow!("No access token available")),
        }
    }

    /// Check if authenticated
    pub fn is_authenticated(&self) -> bool {
        self.access_token.read().unwrap().is_some()
    }

    /// Map APEX symbol to Groww trading symbol and exchange
    fn symbol_to_groww(&self, symbol: &Symbol) -> (String, String) {
        (symbol.0.clone(), "NSE".to_string())
    }

    /// Map APEX OrderSide to Groww transaction type
    fn side_to_transaction_type(&self, side: &OrderSide) -> &'static str {
        match side {
            OrderSide::Buy => "BUY",
            OrderSide::Sell => "SELL",
        }
    }

    /// Map APEX OrderType to Groww order type
    fn order_type_to_groww(&self, order_type: &OrderType) -> &'static str {
        match order_type {
            OrderType::Market => "MARKET",
            OrderType::Limit => "LIMIT",
            OrderType::Stop => "SL",
            OrderType::StopLimit => "SL-M",
            OrderType::TrailingStop => "SL",
        }
    }

    /// Map Groww status to APEX OrderStatus
    fn groww_status_to_order_status(&self, status: &str) -> OrderStatus {
        match status.to_uppercase().as_str() {
            "PENDING" | "OPEN" | "TRIGGER_PENDING" => OrderStatus::Open,
            "EXECUTED" | "COMPLETE" | "FILLED" => OrderStatus::Filled,
            "CANCELLED" => OrderStatus::Cancelled,
            "REJECTED" => OrderStatus::Rejected,
            "PARTIALLY_FILLED" => OrderStatus::PartiallyFilled,
            _ => OrderStatus::Pending,
        }
    }

    /// Update health status
    fn set_health(&self, health: AdapterHealth) {
        let mut h = self.health.write().unwrap();
        *h = health;
    }
}

#[async_trait]
impl ExecutionPort for GrowwExecutionAdapter {
    async fn place_order(&self, order: &NewOrderRequest) -> Result<OrderId> {
        if !self.is_authenticated() {
            self.set_health(AdapterHealth::Unhealthy("Not authenticated".to_string()));
            return Err(anyhow::anyhow!("Not authenticated with Groww"));
        }

        let (trading_symbol, exchange) = self.symbol_to_groww(&order.symbol);

        let groww_order = GrowwOrderRequest {
            trading_symbol,
            exchange,
            transaction_type: self.side_to_transaction_type(&order.side).to_string(),
            order_type: self.order_type_to_groww(&order.order_type).to_string(),
            quantity: order.quantity as u64,
            price: order.price,
            trigger_price: order.stop_price,
            product: "CNC".to_string(),
            validity: "DAY".to_string(),
        };

        let auth_header = self.get_auth_header()?;

        let response = self
            .client
            .post("https://api.groww.in/v1/orders/place")
            .header("Authorization", &auth_header)
            .header("X-Api-Key", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&groww_order)
            .send()
            .await
            .context("Failed to place order with Groww")?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            self.set_health(AdapterHealth::Degraded(format!(
                "Order placement failed: {}",
                status
            )));
            return Err(anyhow::anyhow!(
                "Groww order placement error {}: {}",
                status,
                error_text
            ));
        }

        let order_response: GrowwOrderResponse = response
            .json()
            .await
            .context("Failed to parse order response")?;

        self.set_health(AdapterHealth::Healthy);
        info!("Placed order {} with Groww", order_response.order_id);
        Ok(OrderId(order_response.order_id))
    }

    async fn cancel_order(&self, order_id: &OrderId) -> Result<()> {
        if !self.is_authenticated() {
            return Err(anyhow::anyhow!("Not authenticated with Groww"));
        }

        let url = format!("https://api.groww.in/v1/orders/cancel/{}", order_id.0);
        let auth_header = self.get_auth_header()?;

        let response = self
            .client
            .delete(&url)
            .header("Authorization", &auth_header)
            .header("X-Api-Key", &self.api_key)
            .send()
            .await
            .context("Failed to cancel order with Groww")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to cancel order: {}",
                response.status()
            ));
        }

        info!("Cancelled order {} with Groww", order_id.0);
        Ok(())
    }

    async fn modify_order(&self, order_id: &OrderId, params: &ModifyParams) -> Result<()> {
        if !self.is_authenticated() {
            return Err(anyhow::anyhow!("Not authenticated with Groww"));
        }

        let url = format!("https://api.groww.in/v1/orders/modify/{}", order_id.0);
        let auth_header = self.get_auth_header()?;

        let mut body = serde_json::Map::new();
        if let Some(qty) = params.quantity {
            body.insert("quantity".to_string(), serde_json::Value::from(qty as u64));
        }
        if let Some(price) = params.price {
            body.insert("price".to_string(), serde_json::Value::from(price));
        }
        if let Some(trigger_price) = params.stop_price {
            body.insert(
                "triggerPrice".to_string(),
                serde_json::Value::from(trigger_price),
            );
        }

        let response = self
            .client
            .put(&url)
            .header("Authorization", &auth_header)
            .header("X-Api-Key", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to modify order with Groww")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to modify order: {}",
                response.status()
            ));
        }

        info!("Modified order {} with Groww", order_id.0);
        Ok(())
    }

    async fn get_order_status(&self, order_id: &OrderId) -> Result<Order> {
        if !self.is_authenticated() {
            return Err(anyhow::anyhow!("Not authenticated with Groww"));
        }

        let auth_header = self.get_auth_header()?;

        let response = self
            .client
            .get("https://api.groww.in/v1/orders")
            .header("Authorization", &auth_header)
            .header("X-Api-Key", &self.api_key)
            .send()
            .await
            .context("Failed to fetch orders from Groww")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to fetch order status: {}",
                response.status()
            ));
        }

        let data: serde_json::Value = response.json().await?;
        let orders = data
            .get("data")
            .and_then(|d| d.as_array())
            .ok_or_else(|| anyhow::anyhow!("Invalid orders response"))?;

        for order_value in orders {
            let groww_order: GrowwOrder = serde_json::from_value(order_value.clone())?;

            if groww_order.order_id == order_id.0 {
                let side = match groww_order.transaction_type.as_str() {
                    "BUY" => OrderSide::Buy,
                    _ => OrderSide::Sell,
                };
                let order_type = match groww_order.order_type.as_str() {
                    "MARKET" => OrderType::Market,
                    "LIMIT" => OrderType::Limit,
                    "SL" => OrderType::Stop,
                    "SL-M" => OrderType::StopLimit,
                    _ => OrderType::Market,
                };
                let status = self.groww_status_to_order_status(&groww_order.status);
                let created_at = chrono::DateTime::parse_from_rfc3339(&groww_order.order_timestamp)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now);

                return Ok(Order {
                    id: order_id.clone(),
                    symbol: Symbol(groww_order.trading_symbol),
                    side,
                    order_type,
                    quantity: groww_order.quantity as f64,
                    price: if groww_order.price > 0.0 {
                        Some(groww_order.price)
                    } else {
                        None
                    },
                    stop_price: if groww_order.trigger_price > 0.0 {
                        Some(groww_order.trigger_price)
                    } else {
                        None
                    },
                    status,
                    filled_qty: groww_order.filled_quantity as f64,
                    avg_price: groww_order.average_price,
                    created_at,
                    updated_at: Utc::now(),
                    broker_id: "groww".to_string(),
                    source: "api".to_string(),
                    rejection_reason: None,
                });
            }
        }

        Err(anyhow::anyhow!("Order {} not found", order_id.0))
    }

    async fn get_positions(&self) -> Result<Vec<Position>> {
        if !self.is_authenticated() {
            return Err(anyhow::anyhow!("Not authenticated with Groww"));
        }

        let auth_header = self.get_auth_header()?;

        let response = self
            .client
            .get("https://api.groww.in/v1/portfolio/positions")
            .header("Authorization", &auth_header)
            .header("X-Api-Key", &self.api_key)
            .send()
            .await
            .context("Failed to fetch positions from Groww")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to fetch positions: {}",
                response.status()
            ));
        }

        let data: serde_json::Value = response.json().await?;
        let positions_data = data
            .get("data")
            .and_then(|d| d.as_array())
            .ok_or_else(|| anyhow::anyhow!("Invalid positions response"))?;

        let mut positions = Vec::new();
        for pos_value in positions_data {
            let groww_pos: GrowwPosition = serde_json::from_value(pos_value.clone())?;
            if groww_pos.net_quantity == 0 {
                continue;
            }
            let side = if groww_pos.net_quantity > 0 {
                OrderSide::Buy
            } else {
                OrderSide::Sell
            };
            let quantity = groww_pos.net_quantity.unsigned_abs() as f64;
            let pnl_pct = if groww_pos.average_price > 0.0 {
                (groww_pos.pnl / (groww_pos.average_price * quantity)) * 100.0
            } else {
                0.0
            };
            positions.push(Position {
                symbol: Symbol(groww_pos.trading_symbol),
                quantity,
                avg_price: groww_pos.average_price,
                side,
                pnl: groww_pos.pnl,
                pnl_pct,
                broker_id: "groww".to_string(),
            });
        }

        debug!("Fetched {} positions from Groww", positions.len());
        Ok(positions)
    }

    async fn get_account_balance(&self) -> Result<AccountBalance> {
        if !self.is_authenticated() {
            return Err(anyhow::anyhow!("Not authenticated with Groww"));
        }

        let auth_header = self.get_auth_header()?;

        let response = self
            .client
            .get("https://api.groww.in/v1/user/margins")
            .header("Authorization", &auth_header)
            .header("X-Api-Key", &self.api_key)
            .send()
            .await
            .context("Failed to fetch account balance from Groww")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to fetch balance: {}",
                response.status()
            ));
        }

        let data: serde_json::Value = response.json().await?;
        let margin: GrowwMarginData = serde_json::from_value(
            data.get("data")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("No margin data"))?,
        )
        .context("Failed to parse Groww margin data")?;

        let available = margin.available_cash;
        let used = margin.used_margin;

        Ok(AccountBalance {
            total_value: available + used,
            cash: available,
            margin_used: used,
            margin_available: available,
            unrealized_pnl: 0.0,
            realized_pnl: 0.0,
            currency: "INR".to_string(),
        })
    }

    fn broker_id(&self) -> &'static str {
        "groww"
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
        self.health.read().unwrap().clone()
    }

    fn is_authenticated(&self) -> bool {
        GrowwExecutionAdapter::is_authenticated(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbol_to_groww() {
        let adapter = GrowwExecutionAdapter::new("test_key".to_string(), None).unwrap();
        let symbol = Symbol("RELIANCE".to_string());
        let (trading_symbol, exchange) = adapter.symbol_to_groww(&symbol);
        assert_eq!(trading_symbol, "RELIANCE");
        assert_eq!(exchange, "NSE");
    }

    #[test]
    fn test_side_mapping() {
        let adapter = GrowwExecutionAdapter::new("test_key".to_string(), None).unwrap();
        assert_eq!(adapter.side_to_transaction_type(&OrderSide::Buy), "BUY");
        assert_eq!(adapter.side_to_transaction_type(&OrderSide::Sell), "SELL");
    }

    #[test]
    fn test_order_type_mapping() {
        let adapter = GrowwExecutionAdapter::new("test_key".to_string(), None).unwrap();
        assert_eq!(adapter.order_type_to_groww(&OrderType::Market), "MARKET");
        assert_eq!(adapter.order_type_to_groww(&OrderType::Limit), "LIMIT");
        assert_eq!(adapter.order_type_to_groww(&OrderType::Stop), "SL");
        assert_eq!(adapter.order_type_to_groww(&OrderType::StopLimit), "SL-M");
    }

    #[test]
    fn test_status_mapping() {
        let adapter = GrowwExecutionAdapter::new("test_key".to_string(), None).unwrap();
        assert_eq!(
            adapter.groww_status_to_order_status("PENDING"),
            OrderStatus::Open
        );
        assert_eq!(
            adapter.groww_status_to_order_status("EXECUTED"),
            OrderStatus::Filled
        );
        assert_eq!(
            adapter.groww_status_to_order_status("CANCELLED"),
            OrderStatus::Cancelled
        );
        assert_eq!(
            adapter.groww_status_to_order_status("REJECTED"),
            OrderStatus::Rejected
        );
        assert_eq!(
            adapter.groww_status_to_order_status("PARTIALLY_FILLED"),
            OrderStatus::PartiallyFilled
        );
    }

    #[test]
    fn test_health_default() {
        let adapter = GrowwExecutionAdapter::new("test_key".to_string(), None).unwrap();
        assert_eq!(adapter.health(), AdapterHealth::Healthy);
    }

    #[test]
    fn test_not_authenticated() {
        let adapter = GrowwExecutionAdapter::new("test_key".to_string(), None).unwrap();
        assert!(!adapter.is_authenticated());
    }
}
