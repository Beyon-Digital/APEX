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
use tracing::{debug, info, warn};

/// Robinhood execution adapter
///
/// Provides order placement, modification, cancellation, and position management
/// via Robinhood API. Robinhood is a US-based commission-free brokerage for
/// equities, options, and crypto.
pub struct RobinhoodExecutionAdapter {
    _client_id: String,
    access_token: Arc<RwLock<Option<String>>>,
    client: Client,
    health: Arc<RwLock<AdapterHealth>>,
}

const ROBINHOOD_API_BASE: &str = "https://api.robinhood.com";

/// Robinhood order placement request
#[derive(Debug, Serialize)]
struct RobinhoodOrderRequest {
    account: String,
    instrument: String,
    symbol: String,
    #[serde(rename = "type")]
    order_type: String,
    time_in_force: String,
    trigger: String,
    quantity: String,
    side: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    price: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_price: Option<String>,
}

/// Robinhood order response
#[derive(Debug, Deserialize)]
struct RobinhoodOrderResponse {
    id: String,
}

/// Robinhood order status
#[derive(Debug, Deserialize)]
struct RobinhoodOrder {
    symbol: Option<String>,
    #[serde(rename = "type")]
    order_type: String,
    side: String,
    quantity: String,
    #[serde(default)]
    price: Option<String>,
    #[serde(default)]
    stop_price: Option<String>,
    state: String,
    #[serde(default)]
    cumulative_quantity: String,
    #[serde(default)]
    average_price: Option<String>,
    #[serde(default)]
    created_at: String,
}

/// Robinhood position
#[derive(Debug, Deserialize)]
struct RobinhoodPosition {
    quantity: String,
    average_buy_price: String,
    symbol: Option<String>,
}

impl RobinhoodExecutionAdapter {
    /// Create a new Robinhood execution adapter
    pub fn new(client_id: String, access_token: Option<String>) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            _client_id: client_id,
            access_token: Arc::new(RwLock::new(access_token)),
            client,
            health: Arc::new(RwLock::new(AdapterHealth::Healthy)),
        })
    }

    /// Set access token (after OAuth flow)
    pub fn set_access_token(&self, token: String) {
        let mut access_token = self.access_token.write().unwrap();
        *access_token = Some(token);
        info!("Robinhood access token updated");
    }

    /// Clear access token.
    pub fn clear_access_token(&self) {
        let mut access_token = self.access_token.write().unwrap();
        *access_token = None;
        self.set_health(AdapterHealth::Unhealthy("Not authenticated".to_string()));
        info!("Robinhood access token cleared");
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

    /// Map APEX symbol to Robinhood instrument URL
    /// In production, maintain a symbol-to-instrument URL mapping
    fn symbol_to_instrument_url(&self, symbol: &Symbol) -> String {
        format!("https://api.robinhood.com/instruments/?symbol={}", symbol.0)
    }

    /// Map APEX OrderSide to Robinhood side
    fn side_to_robinhood(&self, side: &OrderSide) -> &'static str {
        match side {
            OrderSide::Buy => "buy",
            OrderSide::Sell => "sell",
        }
    }

    /// Map APEX OrderType to Robinhood order type and trigger
    fn order_type_to_robinhood(&self, order_type: &OrderType) -> (&'static str, &'static str) {
        match order_type {
            OrderType::Market => ("market", "immediate"),
            OrderType::Limit => ("limit", "immediate"),
            OrderType::Stop => ("market", "stop"),
            OrderType::StopLimit => ("limit", "stop"),
            OrderType::TrailingStop => ("market", "stop"), // simplified
        }
    }

    /// Map Robinhood state to APEX OrderStatus
    fn robinhood_state_to_status(&self, state: &str) -> OrderStatus {
        match state {
            "queued" | "unconfirmed" | "confirmed" => OrderStatus::Open,
            "partially_filled" => OrderStatus::PartiallyFilled,
            "filled" => OrderStatus::Filled,
            "cancelled" => OrderStatus::Cancelled,
            "rejected" | "failed" => OrderStatus::Rejected,
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
impl ExecutionPort for RobinhoodExecutionAdapter {
    async fn place_order(&self, order: &NewOrderRequest) -> Result<OrderId> {
        if !self.is_authenticated() {
            self.set_health(AdapterHealth::Unhealthy("Not authenticated".to_string()));
            return Err(anyhow::anyhow!("Not authenticated with Robinhood"));
        }

        let (order_type, trigger) = self.order_type_to_robinhood(&order.order_type);
        let instrument_url = self.symbol_to_instrument_url(&order.symbol);

        let rh_order = RobinhoodOrderRequest {
            account: String::new(), // Populated by pre-flight account lookup in production
            instrument: instrument_url,
            symbol: order.symbol.0.clone(),
            order_type: order_type.to_string(),
            time_in_force: "gfd".to_string(), // Good for day
            trigger: trigger.to_string(),
            quantity: format!("{:.4}", order.quantity), // Supports fractional shares
            side: self.side_to_robinhood(&order.side).to_string(),
            price: order.price.map(|p| format!("{:.2}", p)),
            stop_price: order.stop_price.map(|p| format!("{:.2}", p)),
        };

        let auth_header = self.get_auth_header()?;

        let response = self
            .client
            .post(format!("{}/orders/", ROBINHOOD_API_BASE))
            .header("Authorization", &auth_header)
            .json(&rh_order)
            .send()
            .await
            .context("Failed to place order with Robinhood")?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            self.set_health(AdapterHealth::Degraded(format!(
                "Order placement failed: {}",
                status
            )));
            return Err(anyhow::anyhow!(
                "Robinhood order error {}: {}",
                status,
                error_text
            ));
        }

        let order_response: RobinhoodOrderResponse = response
            .json()
            .await
            .context("Failed to parse order response")?;

        self.set_health(AdapterHealth::Healthy);
        info!("Placed order {} with Robinhood", order_response.id);
        Ok(OrderId(order_response.id))
    }

    async fn cancel_order(&self, order_id: &OrderId) -> Result<()> {
        if !self.is_authenticated() {
            return Err(anyhow::anyhow!("Not authenticated with Robinhood"));
        }

        let url = format!("{}/orders/{}/cancel/", ROBINHOOD_API_BASE, order_id.0);
        let auth_header = self.get_auth_header()?;

        let response = self
            .client
            .post(&url)
            .header("Authorization", &auth_header)
            .send()
            .await
            .context("Failed to cancel order with Robinhood")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to cancel order: {}",
                response.status()
            ));
        }

        info!("Cancelled order {} with Robinhood", order_id.0);
        Ok(())
    }

    async fn modify_order(&self, order_id: &OrderId, _params: &ModifyParams) -> Result<()> {
        // Robinhood doesn't support direct order modification
        // The pattern is cancel + replace
        warn!("Robinhood does not support direct order modification; cancel and resubmit instead");
        Err(anyhow::anyhow!(
            "Robinhood does not support order modification. Cancel order {} and place a new one.",
            order_id.0
        ))
    }

    async fn get_order_status(&self, order_id: &OrderId) -> Result<Order> {
        if !self.is_authenticated() {
            return Err(anyhow::anyhow!("Not authenticated with Robinhood"));
        }

        let url = format!("{}/orders/{}/", ROBINHOOD_API_BASE, order_id.0);
        let auth_header = self.get_auth_header()?;

        let response = self
            .client
            .get(&url)
            .header("Authorization", &auth_header)
            .send()
            .await
            .context("Failed to fetch order from Robinhood")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to fetch order status: {}",
                response.status()
            ));
        }

        let rh_order: RobinhoodOrder = response
            .json()
            .await
            .context("Failed to parse order response")?;

        let side = match rh_order.side.as_str() {
            "buy" => OrderSide::Buy,
            _ => OrderSide::Sell,
        };

        let order_type = match rh_order.order_type.as_str() {
            "market" => OrderType::Market,
            "limit" => OrderType::Limit,
            _ => OrderType::Market,
        };

        let status = self.robinhood_state_to_status(&rh_order.state);

        let created_at = chrono::DateTime::parse_from_rfc3339(&rh_order.created_at)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let quantity: f64 = rh_order.quantity.parse().unwrap_or(0.0);
        let filled_qty: f64 = rh_order.cumulative_quantity.parse().unwrap_or(0.0);
        let avg_price: f64 = rh_order
            .average_price
            .as_deref()
            .unwrap_or("0")
            .parse()
            .unwrap_or(0.0);

        Ok(Order {
            id: order_id.clone(),
            symbol: Symbol(rh_order.symbol.unwrap_or_default()),
            side,
            order_type,
            quantity,
            price: rh_order.price.as_deref().and_then(|p| p.parse().ok()),
            stop_price: rh_order.stop_price.as_deref().and_then(|p| p.parse().ok()),
            status,
            filled_qty,
            avg_price,
            created_at,
            updated_at: Utc::now(),
            broker_id: "robinhood".to_string(),
            source: "api".to_string(),
            rejection_reason: None,
        })
    }

    async fn get_positions(&self) -> Result<Vec<Position>> {
        if !self.is_authenticated() {
            return Err(anyhow::anyhow!("Not authenticated with Robinhood"));
        }

        let auth_header = self.get_auth_header()?;

        let response = self
            .client
            .get(format!("{}/positions/?nonzero=true", ROBINHOOD_API_BASE))
            .header("Authorization", &auth_header)
            .send()
            .await
            .context("Failed to fetch positions from Robinhood")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to fetch positions: {}",
                response.status()
            ));
        }

        let data: serde_json::Value = response.json().await?;
        let results = data
            .get("results")
            .and_then(|r| r.as_array())
            .ok_or_else(|| anyhow::anyhow!("Invalid positions response"))?;

        let mut positions = Vec::new();
        for pos_value in results {
            let rh_pos: RobinhoodPosition = serde_json::from_value(pos_value.clone())?;
            let quantity: f64 = rh_pos.quantity.parse().unwrap_or(0.0);
            if quantity == 0.0 {
                continue;
            }
            let avg_price: f64 = rh_pos.average_buy_price.parse().unwrap_or(0.0);
            let symbol_name = rh_pos.symbol.unwrap_or_else(|| "UNKNOWN".to_string());

            positions.push(Position {
                symbol: Symbol(symbol_name),
                quantity,
                avg_price,
                side: OrderSide::Buy, // Robinhood positions are long-only for equities
                pnl: 0.0,             // Would need current price to calculate
                pnl_pct: 0.0,
                broker_id: "robinhood".to_string(),
            });
        }

        debug!("Fetched {} positions from Robinhood", positions.len());
        Ok(positions)
    }

    async fn get_account_balance(&self) -> Result<AccountBalance> {
        if !self.is_authenticated() {
            return Err(anyhow::anyhow!("Not authenticated with Robinhood"));
        }

        let auth_header = self.get_auth_header()?;

        let response = self
            .client
            .get(format!("{}/accounts/", ROBINHOOD_API_BASE))
            .header("Authorization", &auth_header)
            .send()
            .await
            .context("Failed to fetch account from Robinhood")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to fetch account: {}",
                response.status()
            ));
        }

        let data: serde_json::Value = response.json().await?;
        let accounts = data
            .get("results")
            .and_then(|r| r.as_array())
            .ok_or_else(|| anyhow::anyhow!("No account data"))?;

        if accounts.is_empty() {
            return Err(anyhow::anyhow!("No Robinhood accounts found"));
        }

        let account = &accounts[0];
        let cash: f64 = account
            .get("portfolio_cash")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);
        let buying_power: f64 = account
            .get("buying_power")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);

        Ok(AccountBalance {
            total_value: cash, // Would need portfolio value API call
            cash,
            margin_used: 0.0,
            margin_available: buying_power,
            unrealized_pnl: 0.0,
            realized_pnl: 0.0,
            currency: "USD".to_string(),
        })
    }

    fn broker_id(&self) -> &'static str {
        "robinhood"
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
        RobinhoodExecutionAdapter::is_authenticated(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_side_mapping() {
        let adapter = RobinhoodExecutionAdapter::new("test_id".to_string(), None).unwrap();
        assert_eq!(adapter.side_to_robinhood(&OrderSide::Buy), "buy");
        assert_eq!(adapter.side_to_robinhood(&OrderSide::Sell), "sell");
    }

    #[test]
    fn test_order_type_mapping() {
        let adapter = RobinhoodExecutionAdapter::new("test_id".to_string(), None).unwrap();
        assert_eq!(
            adapter.order_type_to_robinhood(&OrderType::Market),
            ("market", "immediate")
        );
        assert_eq!(
            adapter.order_type_to_robinhood(&OrderType::Limit),
            ("limit", "immediate")
        );
        assert_eq!(
            adapter.order_type_to_robinhood(&OrderType::Stop),
            ("market", "stop")
        );
        assert_eq!(
            adapter.order_type_to_robinhood(&OrderType::StopLimit),
            ("limit", "stop")
        );
    }

    #[test]
    fn test_status_mapping() {
        let adapter = RobinhoodExecutionAdapter::new("test_id".to_string(), None).unwrap();
        assert_eq!(
            adapter.robinhood_state_to_status("queued"),
            OrderStatus::Open
        );
        assert_eq!(
            adapter.robinhood_state_to_status("confirmed"),
            OrderStatus::Open
        );
        assert_eq!(
            adapter.robinhood_state_to_status("partially_filled"),
            OrderStatus::PartiallyFilled
        );
        assert_eq!(
            adapter.robinhood_state_to_status("filled"),
            OrderStatus::Filled
        );
        assert_eq!(
            adapter.robinhood_state_to_status("cancelled"),
            OrderStatus::Cancelled
        );
        assert_eq!(
            adapter.robinhood_state_to_status("rejected"),
            OrderStatus::Rejected
        );
    }

    #[test]
    fn test_health_default() {
        let adapter = RobinhoodExecutionAdapter::new("test_id".to_string(), None).unwrap();
        assert_eq!(adapter.health(), AdapterHealth::Healthy);
    }

    #[test]
    fn test_not_authenticated() {
        let adapter = RobinhoodExecutionAdapter::new("test_id".to_string(), None).unwrap();
        assert!(!adapter.is_authenticated());
    }

    #[test]
    fn test_set_access_token() {
        let adapter = RobinhoodExecutionAdapter::new("test_id".to_string(), None).unwrap();
        assert!(!adapter.is_authenticated());
        adapter.set_access_token("test_token".to_string());
        assert!(adapter.is_authenticated());
    }
}
