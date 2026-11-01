use anyhow::{anyhow, Result};
use apex_core::{
    domain::models::*,
    ports::{execution::ExecutionPort, market_data::AdapterHealth},
};
use async_trait::async_trait;
use reqwest::Method;
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, info};

use crate::kite::instruments::InstrumentMaster;
use crate::kite::orders::{kite_order_type, order_from_json};
use crate::kite::{parse_symbol, to_apex_symbol, Bucket, KiteSession};

/// Zerodha Kite execution adapter
///
/// Order placement, modification, cancellation, positions, holdings and funds
/// via Kite Connect v3. Shares its [`KiteSession`] (token, rate limits,
/// health) with the market-data adapter and the KiteTicker feed.
pub struct ZerodhaExecutionAdapter {
    session: Arc<KiteSession>,
    instruments: Arc<InstrumentMaster>,
}

impl ZerodhaExecutionAdapter {
    /// Create an adapter with its own session.
    ///
    /// # Arguments
    /// * `api_key` - Zerodha API key
    /// * `access_token` - Access token after login
    pub fn new(api_key: String, access_token: Option<String>) -> Result<Self> {
        Ok(Self::from_shared(
            KiteSession::new(api_key, access_token)?,
            Arc::new(InstrumentMaster::new()),
        ))
    }

    /// Create an adapter on a session shared with the market-data adapter.
    pub fn from_shared(session: Arc<KiteSession>, instruments: Arc<InstrumentMaster>) -> Self {
        Self { session, instruments }
    }

    pub fn session(&self) -> &Arc<KiteSession> {
        &self.session
    }

    pub fn set_access_token(&self, token: String) {
        self.session.set_token(token);
    }

    pub fn clear_access_token(&self) {
        self.session.clear_token();
    }

    pub fn is_authenticated(&self) -> bool {
        self.session.is_authenticated()
    }

    fn require_auth(&self) -> Result<()> {
        if self.session.is_authenticated() {
            Ok(())
        } else {
            Err(anyhow!("Not authenticated with Zerodha. Log in again."))
        }
    }

    /// Default margin product: delivery for cash equities, normal for derivatives.
    fn default_product(exchange: &str) -> &'static str {
        match exchange {
            "NSE" | "BSE" => "CNC",
            _ => "NRML",
        }
    }

    /// Kite tags are alphanumeric and at most 20 characters.
    fn sanitize_tag(tag: &str) -> Option<String> {
        let clean: String = tag
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(20)
            .collect();
        (!clean.is_empty()).then_some(clean)
    }

    /// Build the form body for `POST /orders/{variety}`.
    fn order_form(order: &NewOrderRequest) -> Result<(String, Vec<(&'static str, String)>)> {
        let order_type = kite_order_type(&order.order_type)
            .ok_or_else(|| anyhow!("Zerodha does not support trailing stop orders"))?;
        if !(order.quantity.is_finite() && order.quantity > 0.0) || order.quantity.fract() != 0.0 {
            return Err(anyhow!("Zerodha quantity must be a positive whole number"));
        }
        let parsed = parse_symbol(&order.symbol.0);
        let params = order.broker_params.clone().unwrap_or_default();
        let exchange = params.exchange.clone().unwrap_or(parsed.exchange);
        let product = params
            .product
            .clone()
            .unwrap_or_else(|| Self::default_product(&exchange).to_string());
        let variety = params.variety.clone().unwrap_or_else(|| "regular".to_string());

        let mut form = vec![
            ("tradingsymbol", parsed.tradingsymbol),
            ("exchange", exchange),
            (
                "transaction_type",
                match order.side {
                    OrderSide::Buy => "BUY".to_string(),
                    OrderSide::Sell => "SELL".to_string(),
                },
            ),
            ("order_type", order_type.to_string()),
            ("quantity", (order.quantity as u64).to_string()),
            ("product", product),
            ("validity", params.validity.unwrap_or_else(|| "DAY".to_string())),
        ];
        match order.order_type {
            OrderType::Limit | OrderType::StopLimit => {
                let price = order
                    .price
                    .filter(|p| *p > 0.0)
                    .ok_or_else(|| anyhow!("A limit price is required"))?;
                form.push(("price", price.to_string()));
            }
            _ => {}
        }
        if matches!(order.order_type, OrderType::Stop | OrderType::StopLimit) {
            let trigger = order
                .stop_price
                .filter(|p| *p > 0.0)
                .ok_or_else(|| anyhow!("A stop trigger price is required"))?;
            form.push(("trigger_price", trigger.to_string()));
        }
        if let Some(tag) = order.tag.as_deref().and_then(Self::sanitize_tag) {
            form.push(("tag", tag));
        }
        Ok((variety, form))
    }

    fn position_from_json(v: &Value, broker: &str) -> Option<Position> {
        let quantity = v.get("quantity").and_then(|q| q.as_f64())?;
        if quantity == 0.0 {
            return None;
        }
        let average = v.get("average_price").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let pnl = v.get("pnl").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let abs_qty = quantity.abs();
        let pnl_pct = if average > 0.0 { pnl / (average * abs_qty) * 100.0 } else { 0.0 };
        let exchange = v.get("exchange").and_then(|x| x.as_str()).unwrap_or("");
        let symbol = v.get("tradingsymbol").and_then(|x| x.as_str())?;
        Some(Position {
            symbol: Symbol(to_apex_symbol(exchange, symbol)),
            quantity: abs_qty,
            avg_price: average,
            side: if quantity > 0.0 { OrderSide::Buy } else { OrderSide::Sell },
            pnl,
            pnl_pct,
            broker_id: broker.to_string(),
        })
    }
}

#[async_trait]
impl ExecutionPort for ZerodhaExecutionAdapter {
    async fn place_order(&self, order: &NewOrderRequest) -> Result<OrderId> {
        self.require_auth()?;
        let (variety, form) = Self::order_form(order)?;
        let data = self
            .session
            .send_form(Method::POST, &format!("/orders/{variety}"), &form, Bucket::Orders)
            .await?;
        let id = data
            .get("order_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Kite did not return an order_id"))?;
        info!("Placed order {id} with Zerodha");
        Ok(OrderId(id.to_string()))
    }

    async fn cancel_order(&self, order_id: &OrderId) -> Result<()> {
        self.require_auth()?;
        // Cancelling needs the order's variety; look it up (defaults to regular).
        let variety = self.order_variety(order_id).await.unwrap_or_else(|_| "regular".into());
        self.session
            .send_form(
                Method::DELETE,
                &format!("/orders/{variety}/{}", order_id.0),
                &[],
                Bucket::Orders,
            )
            .await?;
        info!("Cancelled order {} with Zerodha", order_id.0);
        Ok(())
    }

    async fn modify_order(&self, order_id: &OrderId, params: &ModifyParams) -> Result<()> {
        self.require_auth()?;
        let mut form: Vec<(&str, String)> = Vec::new();
        if let Some(qty) = params.quantity {
            if qty.fract() != 0.0 || qty <= 0.0 {
                return Err(anyhow!("Zerodha quantity must be a positive whole number"));
            }
            form.push(("quantity", (qty as u64).to_string()));
        }
        if let Some(price) = params.price {
            form.push(("price", price.to_string()));
        }
        if let Some(trigger) = params.stop_price {
            form.push(("trigger_price", trigger.to_string()));
        }
        if form.is_empty() {
            return Err(anyhow!("Nothing to modify"));
        }
        let variety = self.order_variety(order_id).await.unwrap_or_else(|_| "regular".into());
        self.session
            .send_form(
                Method::PUT,
                &format!("/orders/{variety}/{}", order_id.0),
                &form,
                Bucket::Orders,
            )
            .await?;
        info!("Modified order {} with Zerodha", order_id.0);
        Ok(())
    }

    async fn get_order_status(&self, order_id: &OrderId) -> Result<Order> {
        self.require_auth()?;
        // /orders/{id} returns the order's state history; the last entry is current.
        let data = self
            .session
            .get(&format!("/orders/{}", order_id.0), &[], Bucket::General)
            .await?;
        data.as_array()
            .and_then(|history| history.last())
            .and_then(order_from_json)
            .ok_or_else(|| anyhow!("Order {} not found", order_id.0))
    }

    async fn get_positions(&self) -> Result<Vec<Position>> {
        self.require_auth()?;
        let data = self.session.get("/portfolio/positions", &[], Bucket::General).await?;
        let positions: Vec<Position> = data
            .get("net")
            .and_then(|n| n.as_array())
            .map(|net| net.iter().filter_map(|p| Self::position_from_json(p, "zerodha")).collect())
            .unwrap_or_default();
        debug!("Fetched {} positions from Zerodha", positions.len());
        Ok(positions)
    }

    async fn get_holdings(&self) -> Result<Vec<Position>> {
        self.require_auth()?;
        let data = self.session.get("/portfolio/holdings", &[], Bucket::General).await?;
        Ok(data
            .as_array()
            .map(|holdings| {
                holdings
                    .iter()
                    .filter_map(|h| {
                        let qty = h.get("quantity").and_then(|q| q.as_f64()).unwrap_or(0.0)
                            + h.get("t1_quantity").and_then(|q| q.as_f64()).unwrap_or(0.0);
                        let mut h = h.clone();
                        h.as_object_mut()?.insert("quantity".into(), qty.into());
                        Self::position_from_json(&h, "zerodha")
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn get_account_balance(&self) -> Result<AccountBalance> {
        self.require_auth()?;
        let data = self.session.get("/user/margins", &[], Bucket::General).await?;
        let equity = data
            .get("equity")
            .ok_or_else(|| anyhow!("No equity margin data"))?;
        let amount = |group: &str, key: &str| {
            equity
                .get(group)
                .and_then(|g| g.get(key))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0)
        };
        let cash = amount("available", "cash");
        let live_balance = equity
            .get("available")
            .and_then(|g| g.get("live_balance"))
            .and_then(|v| v.as_f64())
            .unwrap_or(cash);
        let used = amount("utilised", "debits");

        // P&L comes from the day's net positions; a failure here must not hide balances.
        let (realized, unrealized) = match self.session.get("/portfolio/positions", &[], Bucket::General).await {
            Ok(positions) => positions
                .get("net")
                .and_then(|n| n.as_array())
                .map(|net| {
                    net.iter().fold((0.0, 0.0), |(r, u), p| {
                        (
                            r + p.get("realised").and_then(|x| x.as_f64()).unwrap_or(0.0),
                            u + p.get("unrealised").and_then(|x| x.as_f64()).unwrap_or(0.0),
                        )
                    })
                })
                .unwrap_or((0.0, 0.0)),
            Err(_) => (0.0, 0.0),
        };

        Ok(AccountBalance {
            total_value: cash + used,
            cash,
            margin_used: used,
            margin_available: live_balance,
            unrealized_pnl: unrealized,
            realized_pnl: realized,
            currency: "INR".to_string(),
        })
    }

    fn subscribe_order_updates(&self) -> Option<tokio::sync::broadcast::Receiver<Order>> {
        Some(self.session.subscribe_orders())
    }

    fn broker_id(&self) -> &'static str {
        "zerodha"
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
        self.session.health()
    }

    fn is_authenticated(&self) -> bool {
        ZerodhaExecutionAdapter::is_authenticated(self)
    }
}

impl ZerodhaExecutionAdapter {
    /// Variety of an existing order (regular, amo, co, iceberg), needed in the
    /// cancel/modify URL. Reads it from the order's history.
    async fn order_variety(&self, order_id: &OrderId) -> Result<String> {
        let data = self
            .session
            .get(&format!("/orders/{}", order_id.0), &[], Bucket::General)
            .await?;
        data.as_array()
            .and_then(|h| h.last())
            .and_then(|o| o.get("variety"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("Unknown order variety"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn adapter(server: &MockServer) -> ZerodhaExecutionAdapter {
        let session = KiteSession::with_base_url(
            "key".into(),
            Some("tok".into()),
            server.uri(),
        )
        .unwrap();
        ZerodhaExecutionAdapter::from_shared(session, Arc::new(InstrumentMaster::new()))
    }

    fn limit_buy() -> NewOrderRequest {
        NewOrderRequest {
            symbol: Symbol("RELIANCE.NS".into()),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: 1.0,
            price: Some(1200.5),
            stop_price: None,
            tag: Some("apex-manual!".into()),
            broker_params: None,
        }
    }

    #[test]
    fn order_form_maps_fields() {
        let (variety, form) = ZerodhaExecutionAdapter::order_form(&limit_buy()).unwrap();
        assert_eq!(variety, "regular");
        let get = |k: &str| form.iter().find(|(key, _)| *key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("tradingsymbol"), Some("RELIANCE"));
        assert_eq!(get("exchange"), Some("NSE"));
        assert_eq!(get("product"), Some("CNC"));
        assert_eq!(get("order_type"), Some("LIMIT"));
        assert_eq!(get("price"), Some("1200.5"));
        assert_eq!(get("tag"), Some("apexmanual"));
        assert_eq!(get("trigger_price"), None);
    }

    #[test]
    fn order_form_validates_inputs() {
        let mut order = limit_buy();
        order.quantity = 1.5;
        assert!(ZerodhaExecutionAdapter::order_form(&order).is_err());
        let mut order = limit_buy();
        order.price = None;
        assert!(ZerodhaExecutionAdapter::order_form(&order).is_err());
        let mut order = limit_buy();
        order.order_type = OrderType::Stop;
        order.stop_price = None;
        assert!(ZerodhaExecutionAdapter::order_form(&order).is_err());
        let mut order = limit_buy();
        order.order_type = OrderType::TrailingStop;
        assert!(ZerodhaExecutionAdapter::order_form(&order).is_err());
    }

    #[test]
    fn broker_params_override_defaults() {
        let mut order = limit_buy();
        order.broker_params = Some(BrokerOrderParams {
            product: Some("MIS".into()),
            ..Default::default()
        });
        let (_, form) = ZerodhaExecutionAdapter::order_form(&order).unwrap();
        assert!(form.iter().any(|(k, v)| *k == "product" && v == "MIS"));
    }

    #[tokio::test]
    async fn place_order_posts_form_encoded_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/orders/regular"))
            .and(header("Authorization", "token key:tok"))
            .and(header("X-Kite-Version", "3"))
            .and(header("content-type", "application/x-www-form-urlencoded"))
            .and(body_string_contains("tradingsymbol=RELIANCE"))
            .and(body_string_contains("order_type=LIMIT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"status": "success", "data": {"order_id": "151220000000000"}}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let id = adapter(&server).place_order(&limit_buy()).await.unwrap();
        assert_eq!(id.0, "151220000000000");
    }

    #[tokio::test]
    async fn token_exception_clears_session() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/orders/regular"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "status": "error",
                "message": "Incorrect `api_key` or `access_token`.",
                "error_type": "TokenException"
            })))
            .mount(&server)
            .await;
        let adapter = adapter(&server);
        let err = adapter.place_order(&limit_buy()).await.unwrap_err();
        assert!(err.to_string().contains("TokenException"));
        assert!(!adapter.is_authenticated());
        assert!(adapter.session().token_expired());
        assert!(matches!(adapter.health(), AdapterHealth::Unhealthy(_)));
    }

    #[tokio::test]
    async fn order_exception_is_not_retried_and_keeps_session() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/orders/regular"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "status": "error",
                "message": "Insufficient funds.",
                "error_type": "InputException"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let adapter = adapter(&server);
        let err = adapter.place_order(&limit_buy()).await.unwrap_err();
        assert!(err.to_string().contains("Insufficient funds"));
        assert!(adapter.is_authenticated());
    }

    #[tokio::test]
    async fn order_status_uses_latest_history_entry() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/orders/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": [
                    {"order_id": "42", "status": "OPEN", "exchange": "NSE", "tradingsymbol": "TCS",
                     "transaction_type": "BUY", "order_type": "LIMIT", "quantity": 10,
                     "filled_quantity": 0, "price": 3000.0, "average_price": 0},
                    {"order_id": "42", "status": "COMPLETE", "exchange": "NSE", "tradingsymbol": "TCS",
                     "transaction_type": "BUY", "order_type": "LIMIT", "quantity": 10,
                     "filled_quantity": 10, "price": 3000.0, "average_price": 2999.5}
                ]
            })))
            .mount(&server)
            .await;
        let order = adapter(&server).get_order_status(&OrderId("42".into())).await.unwrap();
        assert_eq!(order.status, OrderStatus::Filled);
        assert_eq!(order.filled_qty, 10.0);
        assert_eq!(order.avg_price, 2999.5);
        assert_eq!(order.symbol.0, "TCS.NS");
    }

    #[tokio::test]
    async fn cancel_uses_order_variety() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/orders/7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": [{"order_id": "7", "variety": "amo", "status": "AMO REQ RECEIVED"}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/orders/amo/7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"status": "success", "data": {"order_id": "7"}}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        adapter(&server).cancel_order(&OrderId("7".into())).await.unwrap();
    }

    #[tokio::test]
    async fn positions_and_balance() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/portfolio/positions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": {"net": [
                    {"exchange": "NSE", "tradingsymbol": "INFY", "quantity": -5, "average_price": 1500.0,
                     "pnl": 25.0, "realised": 10.0, "unrealised": 15.0},
                    {"exchange": "NSE", "tradingsymbol": "TCS", "quantity": 0, "average_price": 0.0,
                     "pnl": 0.0, "realised": 5.0, "unrealised": 0.0}
                ]}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/user/margins"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": {"equity": {
                    "available": {"cash": 100000.0, "live_balance": 99000.0},
                    "utilised": {"debits": 1000.0}
                }}
            })))
            .mount(&server)
            .await;
        let adapter = adapter(&server);
        let positions = adapter.get_positions().await.unwrap();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].symbol.0, "INFY.NS");
        assert_eq!(positions[0].side, OrderSide::Sell);
        let balance = adapter.get_account_balance().await.unwrap();
        assert_eq!(balance.cash, 100000.0);
        assert_eq!(balance.margin_available, 99000.0);
        assert_eq!(balance.realized_pnl, 15.0);
        assert_eq!(balance.unrealized_pnl, 15.0);
    }

    #[tokio::test]
    async fn unauthenticated_calls_fail_fast() {
        let adapter = ZerodhaExecutionAdapter::new("key".into(), None).unwrap();
        assert!(adapter.place_order(&limit_buy()).await.is_err());
        assert!(adapter.get_positions().await.is_err());
    }
}
