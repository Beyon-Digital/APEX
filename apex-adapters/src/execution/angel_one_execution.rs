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

const BASE_URL: &str = "https://apiconnect.angelone.in";

/// Angel One SmartAPI execution adapter
///
/// Provides order placement, modification, cancellation, and position management
/// via Angel One SmartAPI
pub struct AngelOneExecutionAdapter {
    api_key: String,
    jwt_token: Arc<RwLock<Option<String>>>,
    client_code: String,
    client: Client,
    health: Arc<RwLock<AdapterHealth>>,
}

/// Angel One order placement request
#[derive(Debug, Serialize)]
struct AngelOneOrderRequest {
    variety: String,
    tradingsymbol: String,
    symboltoken: String,
    exchange: String,
    transactiontype: String,
    ordertype: String,
    quantity: u64,
    producttype: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    triggerprice: Option<f64>,
    duration: String,
}

/// Angel One order modify request
#[derive(Debug, Serialize)]
struct AngelOneModifyRequest {
    variety: String,
    orderid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    quantity: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    triggerprice: Option<f64>,
    ordertype: String,
    producttype: String,
    exchange: String,
    tradingsymbol: String,
    symboltoken: String,
    duration: String,
}

/// Angel One cancel request
#[derive(Debug, Serialize)]
struct AngelOneCancelRequest {
    variety: String,
    orderid: String,
}

/// Angel One API response wrapper
#[derive(Debug, Deserialize)]
struct AngelOneResponse<T> {
    #[serde(default)]
    status: bool,
    data: Option<T>,
    #[serde(default)]
    message: String,
}

/// Angel One order placement response data
#[derive(Debug, Deserialize)]
struct AngelOneOrderData {
    orderid: String,
}

/// Angel One order from order book
#[derive(Debug, Deserialize)]
struct AngelOneOrder {
    orderid: String,
    tradingsymbol: String,
    transactiontype: String,
    ordertype: String,
    quantity: u64,
    #[serde(default)]
    price: f64,
    #[serde(default)]
    triggerprice: f64,
    status: String,
    #[serde(default)]
    filledshares: u64,
    #[serde(default)]
    averageprice: f64,
    #[serde(default)]
    updatetime: String,
}

/// Angel One position
#[derive(Debug, Deserialize)]
struct AngelOnePosition {
    symbolname: String,
    #[serde(default)]
    netqty: String,
    #[serde(default)]
    averageprice: String,
    #[serde(default)]
    pnl: String,
}

impl AngelOneExecutionAdapter {
    /// Create a new Angel One execution adapter
    ///
    /// # Arguments
    /// * `api_key` - Angel One API key (private key)
    /// * `client_code` - Angel One client code
    /// * `jwt_token` - JWT token after login
    pub fn new(api_key: String, client_code: String, jwt_token: Option<String>) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            api_key,
            jwt_token: Arc::new(RwLock::new(jwt_token)),
            client_code,
            client,
            health: Arc::new(RwLock::new(AdapterHealth::Healthy)),
        })
    }

    /// Set JWT token
    pub fn set_jwt_token(&self, token: String) {
        let mut jwt_token = self.jwt_token.write().unwrap();
        *jwt_token = Some(token);
        info!("Angel One execution JWT token updated");
    }

    /// Clear JWT token.
    pub fn clear_jwt_token(&self) {
        let mut jwt_token = self.jwt_token.write().unwrap();
        *jwt_token = None;
        self.set_health(AdapterHealth::Unhealthy("Not authenticated".to_string()));
        info!("Angel One execution JWT token cleared");
    }

    /// Get authorization headers for Angel One SmartAPI
    fn get_auth_headers(&self) -> Result<Vec<(&'static str, String)>> {
        let token = self.jwt_token.read().unwrap();
        match token.as_ref() {
            Some(t) => Ok(vec![
                ("Authorization", format!("Bearer {}", t)),
                ("X-PrivateKey", self.api_key.clone()),
                ("X-ClientCode", self.client_code.clone()),
                ("X-ClientLocalIP", "127.0.0.1".to_string()),
                ("X-ClientPublicIP", "127.0.0.1".to_string()),
                ("X-MACAddress", "00:00:00:00:00:00".to_string()),
                ("X-UserType", "USER".to_string()),
                ("Content-Type", "application/json".to_string()),
            ]),
            None => Err(anyhow::anyhow!("No JWT token available")),
        }
    }

    /// Check if authenticated
    pub fn is_authenticated(&self) -> bool {
        self.jwt_token.read().unwrap().is_some()
    }

    /// Map APEX symbol to Angel One trading symbol and token
    /// Simplified mapping: symbol.0 => tradingsymbol, with hard-coded NSE exchange
    fn symbol_to_angel(&self, symbol: &Symbol) -> (String, String) {
        // In production, maintain a symbol-to-token mapping table
        (symbol.0.clone(), symbol.0.clone())
    }

    /// Map APEX OrderSide to Angel One transaction type
    fn side_mapping(&self, side: &OrderSide) -> &'static str {
        match side {
            OrderSide::Buy => "BUY",
            OrderSide::Sell => "SELL",
        }
    }

    /// Map APEX OrderType to Angel One order type
    fn order_type_mapping(&self, order_type: &OrderType) -> &'static str {
        match order_type {
            OrderType::Market => "MARKET",
            OrderType::Limit => "LIMIT",
            OrderType::Stop => "STOPLOSS_MARKET",
            OrderType::StopLimit => "STOPLOSS",
            OrderType::TrailingStop => "STOPLOSS_MARKET",
        }
    }

    /// Map Angel One status to APEX OrderStatus
    fn angel_status_to_order_status(&self, status: &str) -> OrderStatus {
        match status.to_lowercase().as_str() {
            "pending" | "open" | "trigger pending" => OrderStatus::Open,
            "complete" => OrderStatus::Filled,
            "cancelled" => OrderStatus::Cancelled,
            "rejected" => OrderStatus::Rejected,
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
impl ExecutionPort for AngelOneExecutionAdapter {
    async fn place_order(&self, order: &NewOrderRequest) -> Result<OrderId> {
        if !self.is_authenticated() {
            self.set_health(AdapterHealth::Unhealthy("Not authenticated".to_string()));
            return Err(anyhow::anyhow!("Not authenticated with Angel One"));
        }

        let (tradingsymbol, symboltoken) = self.symbol_to_angel(&order.symbol);

        let angel_order = AngelOneOrderRequest {
            variety: "NORMAL".to_string(),
            tradingsymbol,
            symboltoken,
            exchange: "NSE".to_string(),
            transactiontype: self.side_mapping(&order.side).to_string(),
            ordertype: self.order_type_mapping(&order.order_type).to_string(),
            quantity: order.quantity as u64,
            producttype: "DELIVERY".to_string(),
            price: order.price,
            triggerprice: order.stop_price,
            duration: "DAY".to_string(),
        };

        let headers = self.get_auth_headers()?;

        let mut request = self.client.post(format!(
            "{}/rest/secure/angelbroking/order/v1/placeOrder",
            BASE_URL
        ));

        for (key, value) in &headers {
            request = request.header(*key, value);
        }

        let response = request
            .json(&angel_order)
            .send()
            .await
            .context("Failed to place order with Angel One")?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            self.set_health(AdapterHealth::Degraded(format!(
                "Order placement failed: {}",
                status
            )));
            return Err(anyhow::anyhow!(
                "Angel One order placement error {}: {}",
                status,
                error_text
            ));
        }

        let order_response: AngelOneResponse<AngelOneOrderData> = response
            .json()
            .await
            .context("Failed to parse order response")?;

        if !order_response.status {
            self.set_health(AdapterHealth::Degraded(order_response.message.clone()));
            return Err(anyhow::anyhow!(
                "Angel One order placement rejected: {}",
                order_response.message
            ));
        }

        let order_data = order_response.data.ok_or_else(|| {
            anyhow::anyhow!("No order data in response: {}", order_response.message)
        })?;

        self.set_health(AdapterHealth::Healthy);

        info!("Placed order {} with Angel One", order_data.orderid);
        Ok(OrderId(order_data.orderid))
    }

    async fn cancel_order(&self, order_id: &OrderId) -> Result<()> {
        if !self.is_authenticated() {
            return Err(anyhow::anyhow!("Not authenticated with Angel One"));
        }

        let cancel_request = AngelOneCancelRequest {
            variety: "NORMAL".to_string(),
            orderid: order_id.0.clone(),
        };

        let headers = self.get_auth_headers()?;

        let mut request = self.client.post(format!(
            "{}/rest/secure/angelbroking/order/v1/cancelOrder",
            BASE_URL
        ));

        for (key, value) in &headers {
            request = request.header(*key, value);
        }

        let response = request
            .json(&cancel_request)
            .send()
            .await
            .context("Failed to cancel order with Angel One")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to cancel order: {}",
                response.status()
            ));
        }

        info!("Cancelled order {} with Angel One", order_id.0);
        Ok(())
    }

    async fn modify_order(&self, order_id: &OrderId, params: &ModifyParams) -> Result<()> {
        if !self.is_authenticated() {
            return Err(anyhow::anyhow!("Not authenticated with Angel One"));
        }

        let modify_request = AngelOneModifyRequest {
            variety: "NORMAL".to_string(),
            orderid: order_id.0.clone(),
            quantity: params.quantity.map(|q| q as u64),
            price: params.price,
            triggerprice: params.stop_price,
            ordertype: "LIMIT".to_string(),
            producttype: "DELIVERY".to_string(),
            exchange: "NSE".to_string(),
            tradingsymbol: String::new(),
            symboltoken: String::new(),
            duration: "DAY".to_string(),
        };

        let headers = self.get_auth_headers()?;

        let mut request = self.client.post(format!(
            "{}/rest/secure/angelbroking/order/v1/modifyOrder",
            BASE_URL
        ));

        for (key, value) in &headers {
            request = request.header(*key, value);
        }

        let response = request
            .json(&modify_request)
            .send()
            .await
            .context("Failed to modify order with Angel One")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to modify order: {}",
                response.status()
            ));
        }

        info!("Modified order {} with Angel One", order_id.0);
        Ok(())
    }

    async fn get_order_status(&self, order_id: &OrderId) -> Result<Order> {
        if !self.is_authenticated() {
            return Err(anyhow::anyhow!("Not authenticated with Angel One"));
        }

        let headers = self.get_auth_headers()?;

        let mut request = self.client.get(format!(
            "{}/rest/secure/angelbroking/order/v1/getOrderBook",
            BASE_URL
        ));

        for (key, value) in &headers {
            request = request.header(*key, value);
        }

        let response = request
            .send()
            .await
            .context("Failed to fetch orders from Angel One")?;

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
            let angel_order: AngelOneOrder = serde_json::from_value(order_value.clone())?;

            if angel_order.orderid == order_id.0 {
                let side = match angel_order.transactiontype.as_str() {
                    "BUY" => OrderSide::Buy,
                    "SELL" => OrderSide::Sell,
                    _ => OrderSide::Buy,
                };

                let order_type = match angel_order.ordertype.as_str() {
                    "MARKET" => OrderType::Market,
                    "LIMIT" => OrderType::Limit,
                    "STOPLOSS_MARKET" => OrderType::Stop,
                    "STOPLOSS" => OrderType::StopLimit,
                    _ => OrderType::Market,
                };

                let status = self.angel_status_to_order_status(&angel_order.status);

                let created_at = chrono::DateTime::parse_from_rfc3339(&angel_order.updatetime)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now);

                return Ok(Order {
                    id: order_id.clone(),
                    symbol: Symbol(angel_order.tradingsymbol),
                    side,
                    order_type,
                    quantity: angel_order.quantity as f64,
                    price: if angel_order.price > 0.0 {
                        Some(angel_order.price)
                    } else {
                        None
                    },
                    stop_price: if angel_order.triggerprice > 0.0 {
                        Some(angel_order.triggerprice)
                    } else {
                        None
                    },
                    status,
                    filled_qty: angel_order.filledshares as f64,
                    avg_price: angel_order.averageprice,
                    created_at,
                    updated_at: Utc::now(),
                    broker_id: "angel_one".to_string(),
                    source: "api".to_string(),
                    rejection_reason: None,
                });
            }
        }

        Err(anyhow::anyhow!("Order {} not found", order_id.0))
    }

    async fn get_positions(&self) -> Result<Vec<Position>> {
        if !self.is_authenticated() {
            return Err(anyhow::anyhow!("Not authenticated with Angel One"));
        }

        let headers = self.get_auth_headers()?;

        let mut request = self.client.get(format!(
            "{}/rest/secure/angelbroking/portfolio/v1/getPosition",
            BASE_URL
        ));

        for (key, value) in &headers {
            request = request.header(*key, value);
        }

        let response = request
            .send()
            .await
            .context("Failed to fetch positions from Angel One")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to fetch positions: {}",
                response.status()
            ));
        }

        let data: serde_json::Value = response.json().await?;
        let position_list = data
            .get("data")
            .and_then(|d| d.as_array())
            .ok_or_else(|| anyhow::anyhow!("Invalid positions response"))?;

        let mut positions = Vec::new();

        for pos_value in position_list {
            let angel_pos: AngelOnePosition = serde_json::from_value(pos_value.clone())?;

            let net_qty: f64 = angel_pos.netqty.parse().unwrap_or(0.0);
            if net_qty == 0.0 {
                continue; // Skip closed positions
            }

            let side = if net_qty > 0.0 {
                OrderSide::Buy
            } else {
                OrderSide::Sell
            };

            let quantity = net_qty.abs();
            let avg_price: f64 = angel_pos.averageprice.parse().unwrap_or(0.0);
            let pnl: f64 = angel_pos.pnl.parse().unwrap_or(0.0);

            let pnl_pct = if avg_price > 0.0 {
                (pnl / (avg_price * quantity)) * 100.0
            } else {
                0.0
            };

            positions.push(Position {
                symbol: Symbol(angel_pos.symbolname),
                quantity,
                avg_price,
                side,
                pnl,
                pnl_pct,
                broker_id: "angel_one".to_string(),
            });
        }

        debug!("Fetched {} positions from Angel One", positions.len());
        Ok(positions)
    }

    async fn get_account_balance(&self) -> Result<AccountBalance> {
        if !self.is_authenticated() {
            return Err(anyhow::anyhow!("Not authenticated with Angel One"));
        }

        let headers = self.get_auth_headers()?;

        let mut request = self.client.get(format!(
            "{}/rest/secure/angelbroking/user/v1/getRMS",
            BASE_URL
        ));

        for (key, value) in &headers {
            request = request.header(*key, value);
        }

        let response = request
            .send()
            .await
            .context("Failed to fetch account balance from Angel One")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to fetch balance: {}",
                response.status()
            ));
        }

        let data: serde_json::Value = response.json().await?;

        let rms = data
            .get("data")
            .ok_or_else(|| anyhow::anyhow!("No RMS data in response"))?;

        let available = rms
            .get("availablecash")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        let used = rms
            .get("utiliseddebits")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        let net = rms
            .get("net")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(available + used);

        Ok(AccountBalance {
            total_value: net,
            cash: available,
            margin_used: used,
            margin_available: available,
            unrealized_pnl: 0.0,
            realized_pnl: 0.0,
            currency: "INR".to_string(),
        })
    }

    fn broker_id(&self) -> &'static str {
        "angel_one"
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
        AngelOneExecutionAdapter::is_authenticated(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbol_to_angel() {
        let adapter =
            AngelOneExecutionAdapter::new("test_key".to_string(), "test_client".to_string(), None)
                .unwrap();
        let symbol = Symbol("RELIANCE".to_string());
        let (tradingsymbol, symboltoken) = adapter.symbol_to_angel(&symbol);
        assert_eq!(tradingsymbol, "RELIANCE");
        assert_eq!(symboltoken, "RELIANCE");
    }

    #[test]
    fn test_side_mapping() {
        let adapter =
            AngelOneExecutionAdapter::new("test_key".to_string(), "test_client".to_string(), None)
                .unwrap();
        assert_eq!(adapter.side_mapping(&OrderSide::Buy), "BUY");
        assert_eq!(adapter.side_mapping(&OrderSide::Sell), "SELL");
    }

    #[test]
    fn test_order_type_mapping() {
        let adapter =
            AngelOneExecutionAdapter::new("test_key".to_string(), "test_client".to_string(), None)
                .unwrap();
        assert_eq!(adapter.order_type_mapping(&OrderType::Market), "MARKET");
        assert_eq!(adapter.order_type_mapping(&OrderType::Limit), "LIMIT");
        assert_eq!(
            adapter.order_type_mapping(&OrderType::Stop),
            "STOPLOSS_MARKET"
        );
        assert_eq!(
            adapter.order_type_mapping(&OrderType::StopLimit),
            "STOPLOSS"
        );
    }
}
