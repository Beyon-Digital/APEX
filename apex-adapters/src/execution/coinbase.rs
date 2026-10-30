use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use tokio::sync::RwLock;

use apex_core::domain::models::*;
use apex_core::ports::execution::*;
use apex_core::ports::market_data::AdapterHealth;

type HmacSha256 = Hmac<Sha256>;

/// Coinbase Pro REST API base URL
const COINBASE_REST_BASE: &str = "https://api.exchange.coinbase.com";

/// Coinbase Pro execution adapter
pub struct CoinbaseExecutionAdapter {
    client: reqwest::Client,
    api_key: String,
    api_secret: String,
    passphrase: String,
    status: Arc<RwLock<AdapterHealth>>,
}

impl CoinbaseExecutionAdapter {
    /// Create a new Coinbase Pro execution adapter
    pub fn new(api_key: String, api_secret: String, passphrase: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("APEX-Terminal/0.1")
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            api_key,
            api_secret,
            passphrase,
            status: Arc::new(RwLock::new(AdapterHealth::Healthy)),
        }
    }

    /// Convert APEX symbol to Coinbase Pro format
    fn format_symbol(symbol: &Symbol) -> String {
        symbol.0.replace('/', "-").to_uppercase()
    }

    /// Convert APEX order side to Coinbase Pro format
    fn format_side(side: &OrderSide) -> &str {
        match side {
            OrderSide::Buy => "buy",
            OrderSide::Sell => "sell",
        }
    }

    /// Convert APEX order type to Coinbase Pro format
    fn format_order_type(order_type: &OrderType) -> &str {
        match order_type {
            OrderType::Market => "market",
            OrderType::Limit => "limit",
            OrderType::Stop => "stop",
            OrderType::StopLimit => "limit", // Coinbase doesn't have stop-limit, use limit
            OrderType::TrailingStop => "market", // Coinbase doesn't have trailing stop
        }
    }

    /// Convert Coinbase Pro order status to APEX format
    fn parse_order_status(status: &str) -> OrderStatus {
        match status {
            "pending" => OrderStatus::Pending,
            "open" => OrderStatus::Open,
            "done" => OrderStatus::Filled,
            "rejected" => OrderStatus::Rejected,
            _ => OrderStatus::Pending,
        }
    }

    /// Generate Coinbase Pro API signature
    fn sign(&self, method: &str, request_path: &str, body: &str, timestamp: &str) -> String {
        let message = format!("{}{}{}{}", timestamp, method, request_path, body);
        let mut mac = HmacSha256::new_from_slice(self.api_secret.as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(message.as_bytes());
        let result = mac.finalize();
        hex::encode(result.into_bytes())
    }

    /// Place a new order on Coinbase Pro
    async fn place_coinbase_order(
        &self,
        request: &NewOrderRequest,
    ) -> Result<CoinbaseOrderResponse> {
        let symbol = Self::format_symbol(&request.symbol);
        let side = Self::format_side(&request.side);
        let order_type = Self::format_order_type(&request.order_type);

        let timestamp = Utc::now().timestamp().to_string();
        let request_path = "/orders";

        let mut order_body = serde_json::json!({
            "product_id": symbol,
            "side": side,
            "type": order_type,
            "size": request.quantity,
        });

        if let Some(price) = request.price {
            order_body["price"] = serde_json::json!(price);
        }

        if let Some(stop_price) = request.stop_price {
            order_body["stop_price"] = serde_json::json!(stop_price);
        }

        let body_str = order_body.to_string();
        let signature = self.sign("POST", request_path, &body_str, &timestamp);

        let url = format!("{}{}", COINBASE_REST_BASE, request_path);

        let response = self
            .client
            .post(&url)
            .header("CB-ACCESS-KEY", &self.api_key)
            .header("CB-ACCESS-SIGN", &signature)
            .header("CB-ACCESS-TIMESTAMP", &timestamp)
            .header("CB-ACCESS-PASSPHRASE", &self.passphrase)
            .header("Content-Type", "application/json")
            .body(body_str)
            .send()
            .await
            .map_err(|e| anyhow!("Coinbase order request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Coinbase returned status {}: {}",
                status,
                error_text
            ));
        }

        let order_response: CoinbaseOrderResponse = response
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse Coinbase order response: {}", e))?;

        Ok(order_response)
    }

    /// Cancel an order on Coinbase Pro
    async fn cancel_coinbase_order(&self, order_id: &str) -> Result<()> {
        let timestamp = Utc::now().timestamp().to_string();
        let request_path = format!("/orders/{}", order_id);
        let body = "";
        let signature = self.sign("DELETE", &request_path, body, &timestamp);

        let url = format!("{}{}", COINBASE_REST_BASE, request_path);

        let response = self
            .client
            .delete(&url)
            .header("CB-ACCESS-KEY", &self.api_key)
            .header("CB-ACCESS-SIGN", &signature)
            .header("CB-ACCESS-TIMESTAMP", &timestamp)
            .header("CB-ACCESS-PASSPHRASE", &self.passphrase)
            .send()
            .await
            .map_err(|e| anyhow!("Coinbase cancel request failed: {}", e))?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(anyhow!("Coinbase cancel failed: {}", error_text));
        }

        Ok(())
    }

    /// Get account information from Coinbase Pro
    async fn get_accounts(&self) -> Result<Vec<CoinbaseAccount>> {
        let timestamp = Utc::now().timestamp().to_string();
        let request_path = "/accounts";
        let body = "";
        let signature = self.sign("GET", request_path, body, &timestamp);

        let url = format!("{}{}", COINBASE_REST_BASE, request_path);

        let response = self
            .client
            .get(&url)
            .header("CB-ACCESS-KEY", &self.api_key)
            .header("CB-ACCESS-SIGN", &signature)
            .header("CB-ACCESS-TIMESTAMP", &timestamp)
            .header("CB-ACCESS-PASSPHRASE", &self.passphrase)
            .send()
            .await
            .map_err(|e| anyhow!("Coinbase accounts request failed: {}", e))?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(anyhow!("Coinbase accounts failed: {}", error_text));
        }

        let accounts: Vec<CoinbaseAccount> = response
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse Coinbase accounts: {}", e))?;

        Ok(accounts)
    }

    /// Get order from Coinbase Pro
    async fn get_order(&self, order_id: &str) -> Result<CoinbaseOrder> {
        let timestamp = Utc::now().timestamp().to_string();
        let request_path = format!("/orders/{}", order_id);
        let body = "";
        let signature = self.sign("GET", &request_path, body, &timestamp);

        let url = format!("{}{}", COINBASE_REST_BASE, request_path);

        let response = self
            .client
            .get(&url)
            .header("CB-ACCESS-KEY", &self.api_key)
            .header("CB-ACCESS-SIGN", &signature)
            .header("CB-ACCESS-TIMESTAMP", &timestamp)
            .header("CB-ACCESS-PASSPHRASE", &self.passphrase)
            .send()
            .await
            .map_err(|e| anyhow!("Coinbase order request failed: {}", e))?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(anyhow!("Coinbase order failed: {}", error_text));
        }

        let order: CoinbaseOrder = response
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse Coinbase order: {}", e))?;

        Ok(order)
    }

    /// Convert Coinbase order to APEX order
    fn convert_order(bin_order: &CoinbaseOrder, broker_id: &str) -> Order {
        Order {
            id: OrderId(bin_order.id.clone()),
            symbol: Symbol(bin_order.product_id.replace('-', "/")),
            side: if bin_order.side == "buy" {
                OrderSide::Buy
            } else {
                OrderSide::Sell
            },
            order_type: match bin_order.r#type.as_str() {
                "market" => OrderType::Market,
                "limit" => OrderType::Limit,
                "stop" => OrderType::Stop,
                _ => OrderType::Market,
            },
            quantity: bin_order.size.parse().unwrap_or(0.0),
            price: bin_order.price.parse().ok(),
            stop_price: bin_order.stop_price.parse().ok(),
            status: Self::parse_order_status(&bin_order.status),
            filled_qty: bin_order.filled_size.parse().unwrap_or(0.0),
            avg_price: bin_order.executed_value.parse::<f64>().unwrap_or(0.0)
                / bin_order.filled_size.parse::<f64>().unwrap_or(1.0),
            created_at: DateTime::from_timestamp(bin_order.created_at, 0).unwrap_or_else(Utc::now),
            updated_at: Utc::now(),
            broker_id: broker_id.to_string(),
            source: "coinbase".into(),
            rejection_reason: None,
        }
    }
}

#[async_trait]
impl ExecutionPort for CoinbaseExecutionAdapter {
    async fn place_order(&self, request: &NewOrderRequest) -> Result<OrderId> {
        let coinbase_response = self.place_coinbase_order(request).await?;
        Ok(OrderId(coinbase_response.id))
    }

    async fn cancel_order(&self, order_id: &OrderId) -> Result<()> {
        self.cancel_coinbase_order(&order_id.0).await
    }

    async fn modify_order(&self, order_id: &OrderId, _params: &ModifyParams) -> Result<()> {
        // Coinbase doesn't support order modification, need to cancel and replace
        self.cancel_order(order_id).await?;

        // For production, need to fetch original order and place new one with modified params
        Err(anyhow!(
            "Order modification not implemented - requires cancel and replace"
        ))
    }

    async fn get_order_status(&self, order_id: &OrderId) -> Result<Order> {
        let bin_order = self.get_order(&order_id.0).await?;
        Ok(Self::convert_order(&bin_order, "coinbase"))
    }

    async fn get_positions(&self) -> Result<Vec<Position>> {
        let accounts = self.get_accounts().await?;
        let mut positions = Vec::new();

        for account in accounts {
            let balance: f64 = account.balance.parse().unwrap_or(0.0);
            if balance > 0.0 {
                positions.push(Position {
                    symbol: Symbol(account.currency.clone()),
                    quantity: balance,
                    avg_price: 0.0,       // Coinbase doesn't provide avg price
                    side: OrderSide::Buy, // Simplified - all positions are long
                    pnl: 0.0,
                    pnl_pct: 0.0,
                    broker_id: "coinbase".into(),
                });
            }
        }

        Ok(positions)
    }

    async fn get_account_balance(&self) -> Result<AccountBalance> {
        let accounts = self.get_accounts().await?;

        let total_value: f64 = accounts
            .iter()
            .map(|a| a.balance.parse().unwrap_or(0.0))
            .sum();

        Ok(AccountBalance {
            total_value,
            cash: total_value, // Simplified - need to convert non-USD assets
            margin_used: 0.0,
            margin_available: total_value,
            unrealized_pnl: 0.0,
            realized_pnl: 0.0,
            currency: "USD".into(),
        })
    }

    fn broker_id(&self) -> &'static str {
        "coinbase"
    }

    fn supported_order_types(&self) -> &[OrderType] {
        &[OrderType::Market, OrderType::Limit, OrderType::Stop]
    }

    fn health(&self) -> AdapterHealth {
        self.status
            .try_read()
            .map(|s| s.clone())
            .unwrap_or(AdapterHealth::Healthy)
    }

    fn is_authenticated(&self) -> bool {
        !self.api_key.is_empty() && !self.api_secret.is_empty() && !self.passphrase.is_empty()
    }
}

// --- Coinbase API Response Types ---

#[derive(Debug, Deserialize)]
struct CoinbaseOrderResponse {
    id: String,
}

#[derive(Debug, Deserialize)]
struct CoinbaseOrder {
    id: String,
    #[serde(rename = "product_id")]
    product_id: String,
    side: String,
    #[serde(rename = "type")]
    r#type: String,
    size: String,
    price: String,
    #[serde(rename = "stop_price")]
    stop_price: String,
    status: String,
    #[serde(rename = "filled_size")]
    filled_size: String,
    #[serde(rename = "executed_value")]
    executed_value: String,
    #[serde(rename = "created_at")]
    created_at: i64,
}

#[derive(Debug, Deserialize)]
struct CoinbaseAccount {
    id: String,
    currency: String,
    balance: String,
    available: String,
    hold: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_symbol() {
        assert_eq!(
            CoinbaseExecutionAdapter::format_symbol(&Symbol("BTC/USD".into())),
            "BTC-USD"
        );
        assert_eq!(
            CoinbaseExecutionAdapter::format_symbol(&Symbol("ETH/USDT".into())),
            "ETH-USDT"
        );
    }

    #[test]
    fn test_format_side() {
        assert_eq!(
            CoinbaseExecutionAdapter::format_side(&OrderSide::Buy),
            "buy"
        );
        assert_eq!(
            CoinbaseExecutionAdapter::format_side(&OrderSide::Sell),
            "sell"
        );
    }

    #[test]
    fn test_format_order_type() {
        assert_eq!(
            CoinbaseExecutionAdapter::format_order_type(&OrderType::Market),
            "market"
        );
        assert_eq!(
            CoinbaseExecutionAdapter::format_order_type(&OrderType::Limit),
            "limit"
        );
    }

    #[test]
    fn test_parse_order_status() {
        assert_eq!(
            CoinbaseExecutionAdapter::parse_order_status("pending"),
            OrderStatus::Pending
        );
        assert_eq!(
            CoinbaseExecutionAdapter::parse_order_status("done"),
            OrderStatus::Filled
        );
        assert_eq!(
            CoinbaseExecutionAdapter::parse_order_status("rejected"),
            OrderStatus::Rejected
        );
    }
}
