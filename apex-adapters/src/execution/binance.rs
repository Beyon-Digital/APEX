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

/// Binance REST API base URL
const BINANCE_REST_BASE: &str = "https://api.binance.com";

/// Binance execution adapter
pub struct BinanceExecutionAdapter {
    client: reqwest::Client,
    api_key: String,
    api_secret: String,
    status: Arc<RwLock<AdapterHealth>>,
    testnet: bool,
}

impl BinanceExecutionAdapter {
    /// Create a new Binance execution adapter
    pub fn new(api_key: String, api_secret: String, testnet: bool) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("APEX-Terminal/0.1")
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            api_key,
            api_secret,
            status: Arc::new(RwLock::new(AdapterHealth::Healthy)),
            testnet,
        }
    }

    /// Get the REST base URL based on testnet setting
    fn rest_base_url(&self) -> &str {
        if self.testnet {
            "https://testnet.binance.vision"
        } else {
            BINANCE_REST_BASE
        }
    }

    /// Convert APEX symbol to Binance format
    fn format_symbol(symbol: &Symbol) -> String {
        symbol.0.replace('/', "").to_uppercase()
    }

    /// Convert APEX order side to Binance format
    fn format_side(side: &OrderSide) -> &str {
        match side {
            OrderSide::Buy => "BUY",
            OrderSide::Sell => "SELL",
        }
    }

    /// Convert APEX order type to Binance format
    fn format_order_type(order_type: &OrderType) -> &str {
        match order_type {
            OrderType::Market => "MARKET",
            OrderType::Limit => "LIMIT",
            OrderType::Stop => "STOP_LOSS",
            OrderType::StopLimit => "STOP_LOSS_LIMIT",
            OrderType::TrailingStop => "TRAILING_STOP_MARKET",
        }
    }

    /// Convert APEX order status to Binance format
    fn parse_order_status(status: &str) -> OrderStatus {
        match status {
            "NEW" => OrderStatus::Pending,
            "PARTIALLY_FILLED" => OrderStatus::PartiallyFilled,
            "FILLED" => OrderStatus::Filled,
            "CANCELED" => OrderStatus::Cancelled,
            "REJECTED" => OrderStatus::Rejected,
            "EXPIRED" => OrderStatus::Cancelled,
            _ => OrderStatus::Pending,
        }
    }

    /// Generate Binance API signature
    fn sign(&self, query_string: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(self.api_secret.as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(query_string.as_bytes());
        let result = mac.finalize();
        hex::encode(result.into_bytes())
    }

    /// Place a new order on Binance
    async fn place_binance_order(&self, request: &NewOrderRequest) -> Result<BinanceOrderResponse> {
        let symbol = Self::format_symbol(&request.symbol);
        let side = Self::format_side(&request.side);
        let order_type = Self::format_order_type(&request.order_type);

        let mut params = vec![
            format!("symbol={}", symbol),
            format!("side={}", side),
            format!("type={}", order_type),
            format!("quantity={}", request.quantity),
        ];

        if let Some(price) = request.price {
            params.push(format!("price={}", price));
        }

        if let Some(stop_price) = request.stop_price {
            params.push(format!("stopPrice={}", stop_price));
        }

        params.push("timestamp=".to_string() + &Utc::now().timestamp_millis().to_string());

        let query_string = params.join("&");
        let signature = self.sign(&query_string);
        let signed_query = format!("{}&signature={}", query_string, signature);

        let url = format!("{}/api/v3/order?{}", self.rest_base_url(), signed_query);

        let response = self
            .client
            .post(&url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await
            .map_err(|e| anyhow!("Binance order request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Binance returned status {}: {}",
                status,
                error_text
            ));
        }

        let order_response: BinanceOrderResponse = response
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse Binance order response: {}", e))?;

        Ok(order_response)
    }

    /// Cancel an order on Binance
    async fn cancel_binance_order(&self, symbol: &str, order_id: &str) -> Result<()> {
        let params = vec![
            format!("symbol={}", symbol),
            format!("orderId={}", order_id),
            "timestamp=".to_string() + &Utc::now().timestamp_millis().to_string(),
        ];

        let query_string = params.join("&");
        let signature = self.sign(&query_string);
        let signed_query = format!("{}&signature={}", query_string, signature);

        let url = format!("{}/api/v3/order?{}", self.rest_base_url(), signed_query);

        let response = self
            .client
            .delete(&url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await
            .map_err(|e| anyhow!("Binance cancel request failed: {}", e))?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(anyhow!("Binance cancel failed: {}", error_text));
        }

        Ok(())
    }

    /// Get account information from Binance
    async fn get_account(&self) -> Result<BinanceAccount> {
        let query_string = format!("timestamp={}", Utc::now().timestamp_millis());
        let signature = self.sign(&query_string);
        let signed_query = format!("{}&signature={}", query_string, signature);

        let url = format!("{}/api/v3/account?{}", self.rest_base_url(), signed_query);

        let response = self
            .client
            .get(&url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await
            .map_err(|e| anyhow!("Binance account request failed: {}", e))?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(anyhow!("Binance account failed: {}", error_text));
        }

        let account: BinanceAccount = response
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse Binance account: {}", e))?;

        Ok(account)
    }

    /// Get open orders from Binance
    async fn get_open_orders(&self, symbol: Option<&str>) -> Result<Vec<BinanceOrder>> {
        let mut params =
            vec!["timestamp=".to_string() + &Utc::now().timestamp_millis().to_string()];

        if let Some(sym) = symbol {
            params.push(format!("symbol={}", sym));
        }

        let query_string = params.join("&");
        let signature = self.sign(&query_string);
        let signed_query = format!("{}&signature={}", query_string, signature);

        let url = format!(
            "{}/api/v3/openOrders?{}",
            self.rest_base_url(),
            signed_query
        );

        let response = self
            .client
            .get(&url)
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await
            .map_err(|e| anyhow!("Binance open orders request failed: {}", e))?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(anyhow!("Binance open orders failed: {}", error_text));
        }

        let orders: Vec<BinanceOrder> = response
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse Binance orders: {}", e))?;

        Ok(orders)
    }

    /// Convert Binance order to APEX order
    fn convert_order(bin_order: &BinanceOrder, broker_id: &str) -> Order {
        Order {
            id: OrderId(bin_order.order_id.to_string()),
            symbol: Symbol(bin_order.symbol.clone()),
            side: if bin_order.side == "BUY" {
                OrderSide::Buy
            } else {
                OrderSide::Sell
            },
            order_type: match bin_order.r#type.as_str() {
                "MARKET" => OrderType::Market,
                "LIMIT" => OrderType::Limit,
                "STOP_LOSS" => OrderType::Stop,
                "STOP_LOSS_LIMIT" => OrderType::StopLimit,
                _ => OrderType::Market,
            },
            quantity: bin_order.orig_qty.parse().unwrap_or(0.0),
            price: bin_order.price.parse().ok(),
            stop_price: bin_order.stop_price.parse().ok(),
            status: Self::parse_order_status(&bin_order.status),
            filled_qty: bin_order.executed_qty.parse().unwrap_or(0.0),
            avg_price: if bin_order
                .cummulative_quote_qty
                .parse::<f64>()
                .unwrap_or(0.0)
                > 0.0
                && bin_order.executed_qty.parse::<f64>().unwrap_or(0.0) > 0.0
            {
                bin_order
                    .cummulative_quote_qty
                    .parse::<f64>()
                    .unwrap_or(0.0)
                    / bin_order.executed_qty.parse::<f64>().unwrap_or(1.0)
            } else {
                0.0
            },
            created_at: DateTime::from_timestamp_millis(bin_order.time).unwrap_or_else(Utc::now),
            updated_at: DateTime::from_timestamp_millis(bin_order.update_time)
                .unwrap_or_else(Utc::now),
            broker_id: broker_id.to_string(),
            source: "binance".into(),
            rejection_reason: None,
        }
    }
}

#[async_trait]
impl ExecutionPort for BinanceExecutionAdapter {
    async fn place_order(&self, request: &NewOrderRequest) -> Result<OrderId> {
        let binance_response = self.place_binance_order(request).await?;
        Ok(OrderId(binance_response.order_id.to_string()))
    }

    async fn cancel_order(&self, order_id: &OrderId) -> Result<()> {
        // Need to fetch order first to get symbol
        let orders = self.get_open_orders(None).await?;
        let order = orders
            .iter()
            .find(|o| o.order_id.to_string() == order_id.0)
            .ok_or_else(|| anyhow!("Order not found: {}", order_id.0))?;

        self.cancel_binance_order(&order.symbol, &order.order_id.to_string())
            .await
    }

    async fn modify_order(&self, order_id: &OrderId, _params: &ModifyParams) -> Result<()> {
        // Binance doesn't support order modification, need to cancel and replace
        self.cancel_order(order_id).await?;

        // For production, need to fetch original order and place new one with modified params
        // This is a simplified version
        Err(anyhow!(
            "Order modification not implemented - requires cancel and replace"
        ))
    }

    async fn get_order_status(&self, order_id: &OrderId) -> Result<Order> {
        let orders = self.get_open_orders(None).await?;
        let bin_order = orders
            .iter()
            .find(|o| o.order_id.to_string() == order_id.0)
            .ok_or_else(|| anyhow!("Order not found: {}", order_id.0))?;

        Ok(Self::convert_order(bin_order, "binance"))
    }

    async fn get_positions(&self) -> Result<Vec<Position>> {
        let account = self.get_account().await?;
        let mut positions = Vec::new();

        for balance in &account.balances {
            let free: f64 = balance.free.parse().unwrap_or(0.0);
            let locked: f64 = balance.locked.parse().unwrap_or(0.0);
            let total = free + locked;

            if total > 0.0 {
                positions.push(Position {
                    symbol: Symbol(balance.asset.clone()),
                    quantity: total,
                    avg_price: 0.0, // Binance doesn't provide avg price in account endpoint
                    side: OrderSide::Buy, // Simplified - all positions are long
                    pnl: 0.0,
                    pnl_pct: 0.0,
                    broker_id: "binance".into(),
                });
            }
        }

        Ok(positions)
    }

    async fn get_account_balance(&self) -> Result<AccountBalance> {
        let account = self.get_account().await?;

        let total_value: f64 = account
            .balances
            .iter()
            .map(|b| {
                let free: f64 = b.free.parse().unwrap_or(0.0);
                let locked: f64 = b.locked.parse().unwrap_or(0.0);
                free + locked
            })
            .sum();

        Ok(AccountBalance {
            total_value,
            cash: total_value, // Simplified - need to convert non-USDT assets
            margin_used: 0.0,
            margin_available: total_value,
            unrealized_pnl: 0.0,
            realized_pnl: 0.0,
            currency: "USDT".into(),
        })
    }

    fn broker_id(&self) -> &'static str {
        "binance"
    }

    fn supported_order_types(&self) -> &[OrderType] {
        &[
            OrderType::Market,
            OrderType::Limit,
            OrderType::Stop,
            OrderType::StopLimit,
            OrderType::TrailingStop,
        ]
    }

    fn health(&self) -> AdapterHealth {
        self.status
            .try_read()
            .map(|s| s.clone())
            .unwrap_or(AdapterHealth::Healthy)
    }

    fn is_authenticated(&self) -> bool {
        !self.api_key.is_empty() && !self.api_secret.is_empty()
    }
}

// --- Binance API Response Types ---

#[derive(Debug, Deserialize)]
struct BinanceOrderResponse {
    #[serde(rename = "symbol")]
    symbol: String,
    #[serde(rename = "orderId")]
    order_id: u64,
    #[serde(rename = "clientOrderId")]
    client_order_id: String,
    #[serde(rename = "transactTime")]
    transact_time: u64,
}

#[derive(Debug, Deserialize)]
struct BinanceOrder {
    #[serde(rename = "symbol")]
    symbol: String,
    #[serde(rename = "orderId")]
    order_id: u64,
    #[serde(rename = "clientOrderId")]
    client_order_id: String,
    #[serde(rename = "price")]
    price: String,
    #[serde(rename = "origQty")]
    orig_qty: String,
    #[serde(rename = "executedQty")]
    executed_qty: String,
    #[serde(rename = "cummulativeQuoteQty")]
    cummulative_quote_qty: String,
    #[serde(rename = "status")]
    status: String,
    #[serde(rename = "timeInForce")]
    time_in_force: String,
    #[serde(rename = "type")]
    r#type: String,
    #[serde(rename = "side")]
    side: String,
    #[serde(rename = "stopPrice")]
    stop_price: String,
    #[serde(rename = "time")]
    time: i64,
    #[serde(rename = "updateTime")]
    update_time: i64,
}

#[derive(Debug, Deserialize)]
struct BinanceAccount {
    #[serde(rename = "balances")]
    balances: Vec<BinanceBalance>,
}

#[derive(Debug, Deserialize)]
struct BinanceBalance {
    #[serde(rename = "asset")]
    asset: String,
    #[serde(rename = "free")]
    free: String,
    #[serde(rename = "locked")]
    locked: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_symbol() {
        assert_eq!(
            BinanceExecutionAdapter::format_symbol(&Symbol("BTC/USDT".into())),
            "BTCUSDT"
        );
        assert_eq!(
            BinanceExecutionAdapter::format_symbol(&Symbol("ETH/BTC".into())),
            "ETHBTC"
        );
    }

    #[test]
    fn test_format_side() {
        assert_eq!(BinanceExecutionAdapter::format_side(&OrderSide::Buy), "BUY");
        assert_eq!(
            BinanceExecutionAdapter::format_side(&OrderSide::Sell),
            "SELL"
        );
    }

    #[test]
    fn test_format_order_type() {
        assert_eq!(
            BinanceExecutionAdapter::format_order_type(&OrderType::Market),
            "MARKET"
        );
        assert_eq!(
            BinanceExecutionAdapter::format_order_type(&OrderType::Limit),
            "LIMIT"
        );
    }

    #[test]
    fn test_parse_order_status() {
        assert_eq!(
            BinanceExecutionAdapter::parse_order_status("NEW"),
            OrderStatus::Pending
        );
        assert_eq!(
            BinanceExecutionAdapter::parse_order_status("FILLED"),
            OrderStatus::Filled
        );
        assert_eq!(
            BinanceExecutionAdapter::parse_order_status("CANCELED"),
            OrderStatus::Cancelled
        );
    }
}
