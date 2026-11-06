//! Shared Zerodha Kite Connect plumbing: one authenticated session (token,
//! rate limits, error parsing, health) used by the execution adapter, the REST
//! market-data adapter and the KiteTicker WebSocket.

pub mod instruments;
pub mod orders;
pub mod packets;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use apex_core::domain::models::Order;
use apex_core::ports::market_data::AdapterHealth;
use reqwest::{Client, Method};
use serde_json::Value;
use tokio::sync::{broadcast, Mutex};
use tokio::time::Instant;
use tracing::{debug, info, warn};

pub const KITE_BASE_URL: &str = "https://api.kite.trade";
pub const KITE_WS_URL: &str = "wss://ws.kite.trade";

/// Which Kite rate-limit bucket a request counts against.
#[derive(Debug, Clone, Copy)]
pub enum Bucket {
    Orders,
    Quote,
    Historical,
    General,
}

/// Spaces requests at a fixed minimum interval.
struct RateLimiter {
    interval: Duration,
    next: Mutex<Instant>,
}

impl RateLimiter {
    fn per_second(rate: u32) -> Self {
        Self {
            interval: Duration::from_millis(1000 / rate.max(1) as u64),
            next: Mutex::new(Instant::now()),
        }
    }

    async fn acquire(&self) {
        let at = {
            let mut next = self.next.lock().await;
            let at = (*next).max(Instant::now());
            *next = at + self.interval;
            at
        };
        tokio::time::sleep_until(at).await;
    }
}

/// A structured Kite API failure.
#[derive(Debug, Clone)]
pub struct KiteApiError {
    pub status: u16,
    /// Kite `error_type`, e.g. `TokenException`, `InputException`, `OrderException`.
    pub error_type: String,
    pub message: String,
}

impl std::fmt::Display for KiteApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Kite {} ({}): {}", self.error_type, self.status, self.message)
    }
}

impl std::error::Error for KiteApiError {}

/// A `POST` whose result is not known: the request may or may not have reached
/// Kite's order system (timeout, dropped connection, 5xx, unreadable reply). It
/// must NOT be reported as a plain rejection, or the caller may send it again.
#[derive(Debug, Clone)]
pub struct AmbiguousOutcome(pub String);

impl std::fmt::Display for AmbiguousOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "outcome unknown: {}", self.0)
    }
}

impl std::error::Error for AmbiguousOutcome {}

impl KiteApiError {
    pub fn is_token_error(&self) -> bool {
        self.error_type == "TokenException"
    }
}

pub struct KiteSession {
    api_key: String,
    base_url: String,
    token: RwLock<Option<String>>,
    http: Client,
    orders: RateLimiter,
    quote: RateLimiter,
    historical: RateLimiter,
    general: RateLimiter,
    health: RwLock<AdapterHealth>,
    token_expired: AtomicBool,
    order_events: broadcast::Sender<Order>,
}

impl KiteSession {
    pub fn new(api_key: String, token: Option<String>) -> Result<Arc<Self>> {
        Self::with_base_url(api_key, token, KITE_BASE_URL.to_string())
    }

    /// Point the session at a different host (used by tests).
    pub fn with_base_url(
        api_key: String,
        token: Option<String>,
        base_url: String,
    ) -> Result<Arc<Self>> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client")?;
        let health = if token.is_some() {
            AdapterHealth::Healthy
        } else {
            AdapterHealth::Unhealthy("Not authenticated".to_string())
        };
        Ok(Arc::new(Self {
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
            token: RwLock::new(token),
            http,
            orders: RateLimiter::per_second(8),
            quote: RateLimiter::per_second(1),
            historical: RateLimiter::per_second(3),
            general: RateLimiter::per_second(8),
            health: RwLock::new(health),
            token_expired: AtomicBool::new(false),
            order_events: broadcast::channel(256).0,
        }))
    }

    /// Order updates pushed over the KiteTicker WebSocket.
    pub fn subscribe_orders(&self) -> broadcast::Receiver<Order> {
        self.order_events.subscribe()
    }

    pub fn publish_order(&self, order: Order) {
        let _ = self.order_events.send(order);
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn token(&self) -> Option<String> {
        self.token.read().unwrap().clone()
    }

    pub fn is_authenticated(&self) -> bool {
        self.token.read().unwrap().is_some()
    }

    /// True once Kite rejected the token; cleared by a successful login.
    pub fn token_expired(&self) -> bool {
        self.token_expired.load(Ordering::Relaxed)
    }

    pub fn set_token(&self, token: String) {
        *self.token.write().unwrap() = Some(token);
        self.token_expired.store(false, Ordering::Relaxed);
        self.set_health(AdapterHealth::Healthy);
        info!("Zerodha access token updated");
    }

    pub fn clear_token(&self) {
        *self.token.write().unwrap() = None;
        self.set_health(AdapterHealth::Unhealthy("Not authenticated".to_string()));
        info!("Zerodha access token cleared");
    }

    pub fn health(&self) -> AdapterHealth {
        self.health.read().unwrap().clone()
    }

    fn set_health(&self, health: AdapterHealth) {
        *self.health.write().unwrap() = health;
    }

    pub fn ws_url(&self) -> Result<String> {
        let token = self
            .token()
            .ok_or_else(|| anyhow!("Not authenticated with Zerodha"))?;
        let base = if self.base_url == KITE_BASE_URL {
            KITE_WS_URL.to_string()
        } else {
            self.base_url.replacen("http", "ws", 1)
        };
        Ok(format!("{base}?api_key={}&access_token={token}", self.api_key))
    }

    fn limiter(&self, bucket: Bucket) -> &RateLimiter {
        match bucket {
            Bucket::Orders => &self.orders,
            Bucket::Quote => &self.quote,
            Bucket::Historical => &self.historical,
            Bucket::General => &self.general,
        }
    }

    fn auth_header(&self) -> Result<String> {
        let token = self.token();
        match token {
            Some(t) => Ok(format!("token {}:{}", self.api_key, t)),
            None => Err(anyhow!("Not authenticated with Zerodha")),
        }
    }

    /// GET returning the `data` payload.
    pub async fn get(&self, path: &str, query: &[(&str, String)], bucket: Bucket) -> Result<Value> {
        self.request(Method::GET, path, query, &[], bucket, true).await
    }

    /// GET returning the raw body (instruments CSV).
    pub async fn get_text(&self, path: &str, bucket: Bucket) -> Result<String> {
        let used = self.token();
        let response = self
            .send_once(Method::GET, path, &[], &[], bucket)
            .await?;
        let status = response.status();
        let body = response.text().await.context("Failed to read Kite response")?;
        if !status.is_success() {
            let error = self.parse_error(status.as_u16(), &body);
            if error.is_token_error() {
                self.expire_token(used);
            }
            return Err(error.into());
        }
        Ok(body)
    }

    /// Form-encoded POST/PUT/DELETE returning the `data` payload. Not retried:
    /// repeating an order request could duplicate it.
    pub async fn send_form(
        &self,
        method: Method,
        path: &str,
        form: &[(&str, String)],
        bucket: Bucket,
    ) -> Result<Value> {
        let retry = method != Method::POST;
        self.request(method, path, &[], form, bucket, retry).await
    }

    async fn send_once(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        form: &[(&str, String)],
        bucket: Bucket,
    ) -> Result<reqwest::Response> {
        let auth = self.auth_header()?;
        self.limiter(bucket).acquire().await;
        let mut builder = self
            .http
            .request(method, format!("{}{}", self.base_url, path))
            .header("Authorization", auth)
            .header("X-Kite-Version", "3");
        if !query.is_empty() {
            builder = builder.query(query);
        }
        if !form.is_empty() {
            builder = builder.form(form);
        }
        builder.send().await.context("Kite request failed")
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        form: &[(&str, String)],
        bucket: Bucket,
        retry: bool,
    ) -> Result<Value> {
        let attempts = if retry { 3 } else { 1 };
        let mut last_err = anyhow!("Kite request not attempted");
        let used = self.token();
        for attempt in 0..attempts {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(300 * (1 << attempt))).await;
            }
            let response = match self
                .send_once(method.clone(), path, query, form, bucket)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    if !self.is_authenticated() {
                        return Err(e);
                    }
                    self.set_health(AdapterHealth::Degraded(e.to_string()));
                    if !retry {
                        // The request may have been written before the connection failed.
                        return Err(AmbiguousOutcome(e.to_string()).into());
                    }
                    last_err = e;
                    continue;
                }
            };
            let status = response.status();
            let body = match response.text().await {
                Ok(body) => body,
                Err(e) if !retry => return Err(AmbiguousOutcome(format!("reply unreadable: {e}")).into()),
                Err(_) => String::new(),
            };
            if status.is_success() {
                self.set_health(AdapterHealth::Healthy);
                let parsed: Value = match serde_json::from_str(&body) {
                    Ok(v) => v,
                    Err(e) if !retry => {
                        return Err(AmbiguousOutcome(format!("unparseable reply from {path}: {e}")).into())
                    }
                    Err(e) => return Err(anyhow!("Invalid Kite response from {path}: {e}")),
                };
                return Ok(parsed.get("data").cloned().unwrap_or(Value::Null));
            }
            let error = self.parse_error(status.as_u16(), &body);
            if error.is_token_error() {
                self.expire_token(used);
                return Err(error.into());
            }
            let transient = status.as_u16() == 429 || status.is_server_error();
            self.set_health(AdapterHealth::Degraded(error.message.clone()));
            if !transient {
                return Err(error.into());
            }
            if !retry && status.is_server_error() {
                // A 5xx on an order write does not say whether the order was accepted.
                return Err(AmbiguousOutcome(format!("Kite returned {status}")).into());
            }
            warn!(path, status = status.as_u16(), "Transient Kite error, will retry");
            last_err = error.into();
        }
        debug!(path, "Kite request failed after retries");
        Err(last_err)
    }

    /// Mark the session expired, but only if the token that failed is still the
    /// current one. A late rejection of an old token must not wipe a fresh login.
    fn expire_token(&self, used: Option<String>) {
        let mut guard = self.token.write().unwrap();
        if *guard == used {
            *guard = None;
            drop(guard);
            self.token_expired.store(true, Ordering::Relaxed);
            self.set_health(AdapterHealth::Unhealthy(
                "Session expired. Log in to Zerodha again.".to_string(),
            ));
        }
    }

    fn parse_error(&self, status: u16, body: &str) -> KiteApiError {
        let parsed: Option<Value> = serde_json::from_str(body).ok();
        let field = |name: &str| {
            parsed
                .as_ref()
                .and_then(|v| v.get(name))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        KiteApiError {
            status,
            error_type: field("error_type").unwrap_or_else(|| "GeneralException".to_string()),
            message: field("message").unwrap_or_else(|| {
                let snippet: String = body.chars().take(200).collect();
                if snippet.is_empty() {
                    format!("HTTP {status}")
                } else {
                    snippet
                }
            }),
        }
    }
}

/// An exchange-qualified Kite symbol, e.g. `NSE:RELIANCE`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KiteSymbol {
    pub exchange: String,
    pub tradingsymbol: String,
}

impl KiteSymbol {
    /// `NSE:RELIANCE`, the key used by Kite quote endpoints.
    pub fn key(&self) -> String {
        format!("{}:{}", self.exchange, self.tradingsymbol)
    }
}

const EXCHANGES: &[&str] = &["NSE", "BSE", "NFO", "BFO", "MCX", "CDS", "BCD"];

/// Parse an APEX symbol into a Kite exchange and trading symbol.
/// Accepts `RELIANCE` (NSE), `RELIANCE.NS`, `RELIANCE.BO` and `NFO:NIFTY24DECFUT`.
pub fn parse_symbol(symbol: &str) -> KiteSymbol {
    let symbol = symbol.trim().to_uppercase();
    if let Some((exchange, rest)) = symbol.split_once(':') {
        if EXCHANGES.contains(&exchange) {
            return KiteSymbol {
                exchange: exchange.to_string(),
                tradingsymbol: rest.to_string(),
            };
        }
    }
    if let Some(base) = symbol.strip_suffix(".NS") {
        return KiteSymbol { exchange: "NSE".into(), tradingsymbol: base.to_string() };
    }
    if let Some(base) = symbol.strip_suffix(".BO") {
        return KiteSymbol { exchange: "BSE".into(), tradingsymbol: base.to_string() };
    }
    KiteSymbol { exchange: "NSE".into(), tradingsymbol: symbol }
}

/// APEX-facing symbol for a Kite exchange and trading symbol: `RELIANCE.NS`,
/// `TCS.BO`, or `NFO:NIFTY24DECFUT`. Inverse of [`parse_symbol`].
pub fn to_apex_symbol(exchange: &str, tradingsymbol: &str) -> String {
    match exchange {
        "NSE" => format!("{tradingsymbol}.NS"),
        "BSE" => format!("{tradingsymbol}.BO"),
        "" => tradingsymbol.to_string(),
        other => format!("{other}:{tradingsymbol}"),
    }
}

/// True when the symbol is explicitly Indian-exchange qualified.
pub fn is_explicit_indian_symbol(symbol: &str) -> bool {
    let upper = symbol.trim().to_uppercase();
    upper.ends_with(".NS")
        || upper.ends_with(".BO")
        || upper
            .split_once(':')
            .map(|(ex, _)| EXCHANGES.contains(&ex))
            .unwrap_or(false)
}

/// Parse Kite timestamps: `2024-01-15 09:15:00` (IST) or `2024-01-15T09:15:00+0530`.
pub fn parse_kite_time(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::{FixedOffset, NaiveDateTime, TimeZone, Utc};
    let ist = FixedOffset::east_opt(5 * 3600 + 1800)?;
    if let Ok(dt) = chrono::DateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%z") {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(dt.with_timezone(&Utc));
    }
    NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .ok()
        .and_then(|naive| ist.from_local_datetime(&naive).single())
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_parsing() {
        assert_eq!(parse_symbol("RELIANCE").key(), "NSE:RELIANCE");
        assert_eq!(parse_symbol("reliance.ns").key(), "NSE:RELIANCE");
        assert_eq!(parse_symbol("TCS.BO").key(), "BSE:TCS");
        assert_eq!(parse_symbol("NFO:NIFTY24DECFUT").key(), "NFO:NIFTY24DECFUT");
        assert!(is_explicit_indian_symbol("INFY.NS"));
        assert!(!is_explicit_indian_symbol("AAPL"));
    }

    #[test]
    fn kite_time_formats() {
        let a = parse_kite_time("2024-01-15 09:15:00").unwrap();
        assert_eq!(a.to_rfc3339(), "2024-01-15T03:45:00+00:00");
        let b = parse_kite_time("2024-01-15T09:15:00+0530").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn a_late_rejection_of_an_old_token_cannot_wipe_a_fresh_login() {
        let session = KiteSession::new("key".into(), Some("old".into())).unwrap();
        let used = session.token();
        session.set_token("fresh".into()); // the user logged in again while a request was in flight
        session.expire_token(used);
        assert_eq!(session.token().as_deref(), Some("fresh"));
        assert!(!session.token_expired());
        // The same rejection for the current token does expire it.
        session.expire_token(session.token());
        assert!(session.token().is_none() && session.token_expired());
    }

    #[test]
    fn an_ambiguous_outcome_is_distinguishable_from_a_rejection() {
        let ambiguous: anyhow::Error = AmbiguousOutcome("timed out".into()).into();
        assert!(ambiguous.downcast_ref::<AmbiguousOutcome>().is_some());
        assert!(ambiguous.to_string().contains("outcome unknown"));
        let rejected: anyhow::Error =
            KiteApiError { status: 400, error_type: "InputException".into(), message: "x".into() }.into();
        assert!(rejected.downcast_ref::<AmbiguousOutcome>().is_none());
    }
}
