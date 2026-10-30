//! Runtime broker/adapter construction.
//!
//! Each broker's adapters are built from [`BrokerCredentials`] (loaded from the
//! encrypted vault) so they can be added, replaced, or removed while the app is
//! running, instead of being read once from environment variables at startup.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use apex_adapters::execution::angel_one_execution::AngelOneExecutionAdapter;
use apex_adapters::execution::binance::BinanceExecutionAdapter;
use apex_adapters::execution::coinbase::CoinbaseExecutionAdapter;
use apex_adapters::execution::groww_execution::GrowwExecutionAdapter;
use apex_adapters::execution::robinhood_execution::RobinhoodExecutionAdapter;
use apex_adapters::execution::zerodha_execution::ZerodhaExecutionAdapter;
use apex_adapters::market_data::angel_one_market_data::AngelOneMarketDataAdapter;
use apex_adapters::market_data::binance::BinanceAdapter;
use apex_adapters::market_data::coinbase::CoinbaseAdapter;
use apex_adapters::market_data::groww_market_data::GrowwMarketDataAdapter;
use apex_adapters::market_data::polymarket::PolymarketAdapter;
use apex_adapters::market_data::robinhood_market_data::RobinhoodMarketDataAdapter;
use apex_adapters::market_data::zerodha_kite::ZerodhaKiteAdapter;
use apex_adapters::kite::instruments::InstrumentMaster;
use apex_adapters::kite::KiteSession;
use apex_core::ports::execution::ExecutionPort;
use apex_core::ports::market_data::MarketDataPort;

/// Broker ids that can be configured through the vault (paper is built in).
pub(crate) const CONFIGURABLE_BROKERS: &[&str] = &[
    "zerodha",
    "angel_one",
    "groww",
    "robinhood",
    "binance",
    "coinbase",
    "polymarket",
];

/// Vault field holding a broker's session token, for brokers that use one.
pub(crate) fn session_token_field(broker_id: &str) -> Option<&'static str> {
    match broker_id {
        "zerodha" | "groww" | "robinhood" => Some("access_token"),
        "angel_one" => Some("jwt_token"),
        _ => None,
    }
}

/// Kite access tokens expire at 06:00 IST (00:30 UTC) the next morning.
pub(crate) fn kite_token_expiry(now: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
    let today = now
        .date_naive()
        .and_hms_opt(0, 30, 0)
        .expect("valid time")
        .and_utc();
    if now < today {
        today
    } else {
        today + chrono::Duration::days(1)
    }
}

/// Decrypted credentials and plain settings for one adapter.
#[derive(Debug, Default, Clone)]
pub(crate) struct BrokerCredentials {
    pub secrets: HashMap<String, String>,
    pub settings: HashMap<String, String>,
}

impl BrokerCredentials {
    fn secret(&self, key: &str) -> Option<String> {
        non_empty(self.secrets.get(key))
    }

    fn setting(&self, key: &str) -> Option<String> {
        non_empty(self.settings.get(key))
    }

    fn flag(&self, key: &str) -> bool {
        self.setting(key)
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on"))
            .unwrap_or(false)
    }

    /// A stored session token, unless its recorded expiry has passed.
    fn live_token(&self, token_key: &str) -> Option<String> {
        let token = self.secret(token_key)?;
        if let Some(expiry) = self.setting(&format!("{token_key}_expires_at")) {
            if let Ok(expiry) = chrono::DateTime::parse_from_rfc3339(&expiry) {
                if expiry < chrono::Utc::now() {
                    return None;
                }
            }
        }
        Some(token)
    }
}

fn non_empty(value: Option<&String>) -> Option<String> {
    let trimmed = value?.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Result of building one broker: its UI entry plus the adapters to register.
pub(crate) struct BuiltBroker {
    pub entry: BrokerRuntimeEntry,
    pub execution: Option<Arc<dyn ExecutionPort>>,
    pub market_data: Option<Arc<dyn MarketDataPort>>,
}

impl BuiltBroker {
    fn new(entry: BrokerRuntimeEntry) -> Self {
        Self { entry, execution: None, market_data: None }
    }
}

pub(crate) fn paper_entry() -> BrokerRuntimeEntry {
    BrokerRuntimeEntry {
        display_name: "Paper Trading",
        mode: "paper",
        token_label: None,
        execution_available: true,
        market_data_available: false,
        execution: None,
        market_data: None,
    }
}

fn live_entry(
    display_name: &'static str,
    token_label: Option<&'static str>,
) -> BrokerRuntimeEntry {
    BrokerRuntimeEntry {
        display_name,
        mode: "live",
        token_label,
        execution_available: false,
        market_data_available: false,
        execution: None,
        market_data: None,
    }
}

/// Build the adapters for `broker_id` from `creds`. Missing credentials yield
/// an unconfigured entry with no adapters rather than an error.
/// `global_feed` is the `[market_data] adapter` config value, which keeps the
/// opt-in public feeds (Binance, Coinbase, Polymarket) working as before.
pub(crate) fn build_broker(
    broker_id: &str,
    creds: &BrokerCredentials,
    global_feed: &str,
) -> Result<BuiltBroker> {
    let feed_enabled = creds.flag("market_data_feed") || global_feed == broker_id;

    match broker_id {
        "zerodha" => {
            let mut built = BuiltBroker::new(live_entry("Zerodha Kite", Some("Access Token")));
            if let Some(api_key) = creds.secret("api_key") {
                let token = creds.live_token("access_token");
                // One session and instruments master shared by orders, quotes and the ticker.
                match KiteSession::new(api_key, token) {
                    Ok(session) => {
                        let instruments = Arc::new(InstrumentMaster::new());
                        let execution = Arc::new(ZerodhaExecutionAdapter::from_shared(
                            session.clone(),
                            instruments.clone(),
                        ));
                        built.execution = Some(execution.clone());
                        built.entry.execution = Some(ExecutionHandle::Zerodha(execution));
                        built.entry.execution_available = true;

                        let market_data = Arc::new(ZerodhaKiteAdapter::from_shared(session, instruments));
                        built.market_data = Some(market_data.clone());
                        built.entry.market_data = Some(MarketDataHandle::Zerodha(market_data));
                        built.entry.market_data_available = true;
                    }
                    Err(err) => tracing::warn!(error = %err, "Failed to initialize Zerodha session"),
                }
            }
            Ok(built)
        }
        "angel_one" => {
            let mut built = BuiltBroker::new(live_entry("Angel One", Some("JWT Token")));
            if let (Some(api_key), Some(client_code)) =
                (creds.secret("api_key"), creds.setting("client_code"))
            {
                let token = creds.live_token("jwt_token");
                match AngelOneExecutionAdapter::new(api_key.clone(), client_code, token.clone()) {
                    Ok(adapter) => {
                        let adapter = Arc::new(adapter);
                        built.execution = Some(adapter.clone());
                        built.entry.execution = Some(ExecutionHandle::AngelOne(adapter));
                        built.entry.execution_available = true;
                    }
                    Err(err) => tracing::warn!(error = %err, "Failed to initialize Angel One execution adapter"),
                }
                match AngelOneMarketDataAdapter::new(api_key, token) {
                    Ok(adapter) => {
                        let adapter = Arc::new(adapter);
                        built.market_data = Some(adapter.clone());
                        built.entry.market_data = Some(MarketDataHandle::AngelOne(adapter));
                        built.entry.market_data_available = true;
                    }
                    Err(err) => tracing::warn!(error = %err, "Failed to initialize Angel One market data adapter"),
                }
            }
            Ok(built)
        }
        "groww" => {
            let mut built = BuiltBroker::new(live_entry("Groww", Some("Access Token")));
            if let Some(api_key) = creds.secret("api_key") {
                let token = creds.live_token("access_token");
                match GrowwExecutionAdapter::new(api_key.clone(), token.clone()) {
                    Ok(adapter) => {
                        let adapter = Arc::new(adapter);
                        built.execution = Some(adapter.clone());
                        built.entry.execution = Some(ExecutionHandle::Groww(adapter));
                        built.entry.execution_available = true;
                    }
                    Err(err) => tracing::warn!(error = %err, "Failed to initialize Groww execution adapter"),
                }
                match GrowwMarketDataAdapter::new(api_key, token) {
                    Ok(adapter) => {
                        let adapter = Arc::new(adapter);
                        built.market_data = Some(adapter.clone());
                        built.entry.market_data = Some(MarketDataHandle::Groww(adapter));
                        built.entry.market_data_available = true;
                    }
                    Err(err) => tracing::warn!(error = %err, "Failed to initialize Groww market data adapter"),
                }
            }
            Ok(built)
        }
        "robinhood" => {
            let mut built = BuiltBroker::new(live_entry("Robinhood", Some("Access Token")));
            let token = creds.live_token("access_token");
            if let Some(client_id) = creds.secret("client_id") {
                match RobinhoodExecutionAdapter::new(client_id, token.clone()) {
                    Ok(adapter) => {
                        let adapter = Arc::new(adapter);
                        built.execution = Some(adapter.clone());
                        built.entry.execution = Some(ExecutionHandle::Robinhood(adapter));
                        built.entry.execution_available = true;
                    }
                    Err(err) => tracing::warn!(error = %err, "Failed to initialize Robinhood execution adapter"),
                }
            }
            // Market data only needs the (optional) access token.
            match RobinhoodMarketDataAdapter::new(token) {
                Ok(adapter) => {
                    let adapter = Arc::new(adapter);
                    built.market_data = Some(adapter.clone());
                    built.entry.market_data = Some(MarketDataHandle::Robinhood(adapter));
                    built.entry.market_data_available = true;
                }
                Err(err) => tracing::warn!(error = %err, "Failed to initialize Robinhood market data adapter"),
            }
            Ok(built)
        }
        "binance" => {
            let mut built = BuiltBroker::new(live_entry("Binance", Some("API Key")));
            if let (Some(api_key), Some(api_secret)) =
                (creds.secret("api_key"), creds.secret("api_secret"))
            {
                let testnet = creds.flag("testnet");
                let adapter = Arc::new(BinanceExecutionAdapter::new(api_key, api_secret, testnet));
                built.execution = Some(adapter.clone());
                built.entry.execution = Some(ExecutionHandle::Binance(adapter));
                built.entry.execution_available = true;
            }
            // Public feeds are opt-in: unrelated watchlist symbols must not
            // start exchange sockets merely because the app launched.
            if feed_enabled {
                let adapter = Arc::new(BinanceAdapter::new());
                built.market_data = Some(adapter.clone());
                built.entry.market_data = Some(MarketDataHandle::Binance(adapter));
                built.entry.market_data_available = true;
            }
            Ok(built)
        }
        "coinbase" => {
            let mut built = BuiltBroker::new(live_entry("Coinbase Pro", Some("API Key")));
            if let (Some(api_key), Some(api_secret), Some(passphrase)) = (
                creds.secret("api_key"),
                creds.secret("api_secret"),
                creds.secret("passphrase"),
            ) {
                let adapter = Arc::new(CoinbaseExecutionAdapter::new(api_key, api_secret, passphrase));
                built.execution = Some(adapter.clone());
                built.entry.execution = Some(ExecutionHandle::Coinbase(adapter));
                built.entry.execution_available = true;
            }
            if feed_enabled {
                let adapter = Arc::new(CoinbaseAdapter::new());
                built.market_data = Some(adapter.clone());
                built.entry.market_data = Some(MarketDataHandle::Coinbase(adapter));
                built.entry.market_data_available = true;
            }
            Ok(built)
        }
        "polymarket" => {
            let mut built = BuiltBroker::new(live_entry("Polymarket", None));
            if feed_enabled {
                let adapter = Arc::new(PolymarketAdapter::new());
                built.market_data = Some(adapter.clone());
                built.entry.market_data = Some(MarketDataHandle::Polymarket(adapter));
                built.entry.market_data_available = true;
            }
            Ok(built)
        }
        other => Err(anyhow!("Unknown broker: {other}")),
    }
}

pub(crate) enum ExecutionHandle {
    Zerodha(Arc<ZerodhaExecutionAdapter>),
    AngelOne(Arc<AngelOneExecutionAdapter>),
    Groww(Arc<GrowwExecutionAdapter>),
    Robinhood(Arc<RobinhoodExecutionAdapter>),
    Binance(Arc<BinanceExecutionAdapter>),
    Coinbase(Arc<CoinbaseExecutionAdapter>),
}

impl ExecutionHandle {
    fn is_authenticated(&self) -> bool {
        match self {
            Self::Zerodha(adapter) => adapter.is_authenticated(),
            Self::AngelOne(adapter) => adapter.is_authenticated(),
            Self::Groww(adapter) => adapter.is_authenticated(),
            Self::Robinhood(adapter) => adapter.is_authenticated(),
            Self::Binance(adapter) => adapter.is_authenticated(),
            Self::Coinbase(adapter) => adapter.is_authenticated(),
        }
    }

    fn set_session_token(&self, token: &str) {
        match self {
            Self::Zerodha(adapter) => adapter.set_access_token(token.to_string()),
            Self::AngelOne(adapter) => adapter.set_jwt_token(token.to_string()),
            Self::Groww(adapter) => adapter.set_access_token(token.to_string()),
            Self::Robinhood(adapter) => adapter.set_access_token(token.to_string()),
            Self::Binance(_) => {} // Binance uses API key/secret, not session token
            Self::Coinbase(_) => {} // Coinbase uses API key/secret/passphrase, not session token
        }
    }

    fn clear_session_token(&self) {
        match self {
            Self::Zerodha(adapter) => adapter.clear_access_token(),
            Self::AngelOne(adapter) => adapter.clear_jwt_token(),
            Self::Groww(adapter) => adapter.clear_access_token(),
            Self::Robinhood(adapter) => adapter.clear_access_token(),
            Self::Binance(_) => {}
            Self::Coinbase(_) => {}
        }
    }
}

pub(crate) enum MarketDataHandle {
    Zerodha(Arc<ZerodhaKiteAdapter>),
    AngelOne(Arc<AngelOneMarketDataAdapter>),
    Groww(Arc<GrowwMarketDataAdapter>),
    Robinhood(Arc<RobinhoodMarketDataAdapter>),
    Binance(Arc<BinanceAdapter>),
    Coinbase(Arc<CoinbaseAdapter>),
    Polymarket(Arc<PolymarketAdapter>),
}

impl MarketDataHandle {
    /// The Kite instruments master, when this is the Zerodha feed.
    pub(crate) fn instruments(&self) -> Option<Arc<InstrumentMaster>> {
        match self {
            Self::Zerodha(adapter) => Some(adapter.instruments().clone()),
            _ => None,
        }
    }

    /// The Kite session, when this is the Zerodha feed.
    pub(crate) fn kite_session(&self) -> Option<Arc<KiteSession>> {
        match self {
            Self::Zerodha(adapter) => Some(adapter.session().clone()),
            _ => None,
        }
    }

    /// Start any background feed (KiteTicker) once credentials are present.
    pub(crate) fn start_background(&self) {
        if let Self::Zerodha(adapter) = self {
            adapter.start_background();
        }
    }

    pub(crate) fn adapter_id(&self) -> String {
        match self {
            Self::Zerodha(a) => a.adapter_id().to_string(),
            Self::AngelOne(a) => a.adapter_id().to_string(),
            Self::Groww(a) => a.adapter_id().to_string(),
            Self::Robinhood(a) => a.adapter_id().to_string(),
            Self::Binance(a) => a.adapter_id().to_string(),
            Self::Coinbase(a) => a.adapter_id().to_string(),
            Self::Polymarket(a) => a.adapter_id().to_string(),
        }
    }

    fn is_authenticated(&self) -> bool {
        match self {
            Self::Zerodha(adapter) => adapter.is_authenticated(),
            Self::AngelOne(adapter) => adapter.is_authenticated(),
            Self::Groww(adapter) => adapter.is_authenticated(),
            Self::Robinhood(adapter) => adapter.is_authenticated(),
            Self::Binance(_) => true, // Binance market data doesn't require auth
            Self::Coinbase(_) => true, // Coinbase market data doesn't require auth
            Self::Polymarket(_) => true, // Polymarket market data doesn't require auth
        }
    }

    fn set_session_token(&self, token: &str) {
        match self {
            Self::Zerodha(adapter) => adapter.set_access_token(token.to_string()),
            Self::AngelOne(adapter) => adapter.set_jwt_token(token.to_string()),
            Self::Groww(adapter) => adapter.set_access_token(token.to_string()),
            Self::Robinhood(adapter) => adapter.set_access_token(token.to_string()),
            Self::Binance(_) => {}
            Self::Coinbase(_) => {}
            Self::Polymarket(_) => {}
        }
    }

    fn clear_session_token(&self) {
        match self {
            Self::Zerodha(adapter) => adapter.clear_access_token(),
            Self::AngelOne(adapter) => adapter.clear_jwt_token(),
            Self::Groww(adapter) => adapter.clear_access_token(),
            Self::Robinhood(adapter) => adapter.clear_access_token(),
            Self::Binance(_) => {}
            Self::Coinbase(_) => {}
            Self::Polymarket(_) => {}
        }
    }
}

pub(crate) struct BrokerRuntimeEntry {
    pub(crate) display_name: &'static str,
    pub(crate) mode: &'static str,
    pub(crate) token_label: Option<&'static str>,
    pub(crate) execution_available: bool,
    pub(crate) market_data_available: bool,
    pub(crate) execution: Option<ExecutionHandle>,
    pub(crate) market_data: Option<MarketDataHandle>,
}

impl BrokerRuntimeEntry {
    pub(crate) fn configured(&self) -> bool {
        self.mode == "paper" || self.execution_available || self.market_data_available
    }

    pub(crate) fn authenticated(&self) -> bool {
        if self.mode == "paper" {
            return true;
        }

        let execution_auth = self
            .execution
            .as_ref()
            .map(ExecutionHandle::is_authenticated)
            .unwrap_or(false);
        let market_auth = self
            .market_data
            .as_ref()
            .map(MarketDataHandle::is_authenticated)
            .unwrap_or(false);

        execution_auth || market_auth
    }

    fn capability_summary(&self) -> &'static str {
        match (self.execution_available, self.market_data_available) {
            (true, true) => "live orders, account access, and market data",
            (true, false) => "live orders and account access",
            (false, true) => "live market data",
            (false, false) => "live connectivity",
        }
    }

    pub(crate) fn status_and_message(&self) -> (String, String) {
        if self.mode == "paper" {
            return (
                "ready".to_string(),
                "Paper trading is active for safe simulated execution.".to_string(),
            );
        }

        if !self.configured() {
            return (
                "not_configured".to_string(),
                format!(
                    "To enable {}, add its credentials under Settings > Adapters.",
                    self.display_name
                ),
            );
        }

        if !self.authenticated() {
            let token_label = self.token_label.unwrap_or("session token");
            return (
                "auth_required".to_string(),
                format!(
                    "{} is configured for {}. Paste a {} to enable live connectivity.",
                    self.display_name,
                    self.capability_summary(),
                    token_label
                ),
            );
        }

        if !self.execution_available && self.market_data_available && self.token_label == Some("API Key") {
            return (
                "ready".to_string(),
                format!("{} public market data is enabled. Check Health for connection status.", self.display_name),
            );
        }

        (
            "connected".to_string(),
            format!(
                "{} is authenticated and ready for safe live data and account checks.",
                self.display_name
            ),
        )
    }

    pub(crate) fn set_session_token(&self, token: &str) -> Result<()> {
        if self.mode == "paper" {
            return Err(anyhow!("Paper trading does not use a live session token"));
        }

        if !self.configured() {
            return Err(anyhow!(
                "{} is not configured. Add its credentials under Settings > Adapters.",
                self.display_name
            ));
        }

        let mut applied = false;

        if let Some(execution) = &self.execution {
            execution.set_session_token(token);
            applied = true;
        }

        if let Some(market_data) = &self.market_data {
            market_data.set_session_token(token);
            applied = true;
        }

        if !applied {
            return Err(anyhow!(
                "{} does not expose a live session surface",
                self.display_name
            ));
        }

        Ok(())
    }

    pub(crate) fn clear_session_token(&self) -> Result<()> {
        if self.mode == "paper" {
            return Ok(());
        }

        let mut cleared = false;

        if let Some(execution) = &self.execution {
            execution.clear_session_token();
            cleared = true;
        }

        if let Some(market_data) = &self.market_data {
            market_data.clear_session_token();
            cleared = true;
        }

        if !cleared {
            return Err(anyhow!(
                "{} is not configured. Add its credentials under Settings > Adapters.",
                self.display_name
            ));
        }

        Ok(())
    }

    pub(crate) fn to_dto(&self, broker_id: &str) -> crate::dto::BrokerConnectionDto {
        let (status, message) = self.status_and_message();

        crate::dto::BrokerConnectionDto {
            broker_id: broker_id.to_string(),
            display_name: self.display_name.to_string(),
            mode: self.mode.to_string(),
            status,
            configured: self.configured(),
            authenticated: self.authenticated(),
            execution_available: self.execution_available,
            market_data_available: self.market_data_available,
            token_field_label: self.token_label.unwrap_or("").to_string(),
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn kite_expiry_is_next_0600_ist() {
        let before = chrono::Utc.with_ymd_and_hms(2026, 10, 30, 0, 10, 0).unwrap();
        assert_eq!(
            kite_token_expiry(before),
            chrono::Utc.with_ymd_and_hms(2026, 10, 30, 0, 30, 0).unwrap()
        );
        let after = chrono::Utc.with_ymd_and_hms(2026, 10, 30, 5, 0, 0).unwrap();
        assert_eq!(
            kite_token_expiry(after),
            chrono::Utc.with_ymd_and_hms(2026, 10, 31, 0, 30, 0).unwrap()
        );
    }

    #[test]
    fn expired_token_is_ignored() {
        let mut creds = BrokerCredentials::default();
        creds.secrets.insert("access_token".into(), "tok".into());
        creds.settings.insert(
            "access_token_expires_at".into(),
            (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339(),
        );
        assert!(creds.live_token("access_token").is_none());
        creds.settings.insert(
            "access_token_expires_at".into(),
            (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
        );
        assert_eq!(creds.live_token("access_token").as_deref(), Some("tok"));
    }

    #[test]
    fn unconfigured_brokers_build_without_adapters() {
        let creds = BrokerCredentials::default();
        for id in CONFIGURABLE_BROKERS {
            let built = build_broker(id, &creds, "yahoo_finance").unwrap();
            assert!(built.execution.is_none(), "{id}");
            assert_eq!(built.entry.execution_available, false, "{id}");
        }
    }

    #[test]
    fn zerodha_builds_both_adapters_from_api_key() {
        let mut creds = BrokerCredentials::default();
        creds.secrets.insert("api_key".into(), "key".into());
        let built = build_broker("zerodha", &creds, "yahoo_finance").unwrap();
        assert!(built.execution.is_some() && built.market_data.is_some());
    }
}
