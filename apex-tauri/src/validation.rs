//! IPC input validation helpers.
//!
//! Every Tauri command should validate its inputs before passing them to the
//! core domain.  These helpers enforce:
//!
//! * Symbol format (non-empty, alphanumeric + `. - _ ^ = : /`, max 20 chars;
//!   covers Yahoo-style `^GSPC`, `GC=F`, `EURUSD=X` and `NFO:SYMBOL`)
//! * Quantity bounds (positive, finite, capped at 1 000 000)
//! * Price bounds (positive, finite when present)
//! * String length limits (prevent oversized payloads)
//! * Broker ID whitelist (paper, zerodha, angel_one, groww, robinhood)
//! * Algorithm whitelist (random_forest, gradient_boosting, etc.)

/// Maximum allowed length for a symbol string.
const MAX_SYMBOL_LEN: usize = 20;
/// Maximum allowed quantity for a single order.
const MAX_QUANTITY: f64 = 1_000_000.0;
/// Maximum length for free-text fields (tags, model IDs, etc.).
const MAX_STRING_LEN: usize = 256;
/// Maximum allowed alert rule JSON size (bytes).
const MAX_ALERT_JSON_LEN: usize = 4096;
/// Maximum allowed broker session token size.
const MAX_SESSION_TOKEN_LEN: usize = 8192;

/// Known broker IDs.
const VALID_BROKERS: &[&str] = &[
    "paper",
    "zerodha",
    "angel_one",
    "groww",
    "robinhood",
    "binance",
    "coinbase",
];

/// Known ML algorithm identifiers.
const VALID_ALGORITHMS: &[&str] = &[
    "random_forest",
    "gradient_boosting",
    "logistic_regression",
    "xgboost",
];

/// Validate a ticker symbol.
pub fn validate_symbol(symbol: &str) -> Result<(), String> {
    if symbol.is_empty() {
        return Err("Symbol must not be empty".into());
    }
    if symbol.len() > MAX_SYMBOL_LEN {
        return Err(format!(
            "Symbol too long ({} chars, max {})",
            symbol.len(),
            MAX_SYMBOL_LEN
        ));
    }
    if !symbol
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '^' | '=' | ':' | '/'))
    {
        return Err(format!("Symbol contains invalid characters: {}", symbol));
    }
    Ok(())
}

/// Validate a quantity value.
pub fn validate_quantity(qty: f64) -> Result<(), String> {
    if !qty.is_finite() {
        return Err("Quantity must be a finite number".into());
    }
    if qty <= 0.0 {
        return Err("Quantity must be positive".into());
    }
    if qty > MAX_QUANTITY {
        return Err(format!(
            "Quantity exceeds maximum ({} > {})",
            qty, MAX_QUANTITY
        ));
    }
    Ok(())
}

/// Validate an optional price value.
pub fn validate_price(price: Option<f64>) -> Result<(), String> {
    if let Some(p) = price {
        if !p.is_finite() {
            return Err("Price must be a finite number".into());
        }
        if p < 0.0 {
            return Err("Price must not be negative".into());
        }
    }
    Ok(())
}

/// Validate a broker ID against the known whitelist.
pub fn validate_broker_id(broker_id: &str) -> Result<(), String> {
    if !VALID_BROKERS.contains(&broker_id) {
        return Err(format!(
            "Unknown broker: {}. Valid brokers: {}",
            broker_id,
            VALID_BROKERS.join(", ")
        ));
    }
    Ok(())
}

/// Validate an adapter id for vault storage: a configurable broker or an
/// `llm:<provider>` credential slot.
pub fn validate_adapter_id(adapter_id: &str) -> Result<(), String> {
    if let Some(provider) = adapter_id.strip_prefix("llm:") {
        let ok = !provider.is_empty()
            && provider.len() <= 64
            && provider
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'));
        return if ok {
            Ok(())
        } else {
            Err(format!("Invalid LLM provider id: {adapter_id}"))
        };
    }
    if crate::broker_runtime::CONFIGURABLE_BROKERS.contains(&adapter_id) {
        Ok(())
    } else {
        Err(format!("Unknown adapter: {adapter_id}"))
    }
}

/// Validate a generic string field against a maximum length.
pub fn validate_string_length(field: &str, name: &str) -> Result<(), String> {
    if field.len() > MAX_STRING_LEN {
        return Err(format!(
            "{} too long ({} chars, max {})",
            name,
            field.len(),
            MAX_STRING_LEN
        ));
    }
    Ok(())
}

/// Validate an alert rule JSON payload.
pub fn validate_alert_json(json: &str) -> Result<(), String> {
    if json.len() > MAX_ALERT_JSON_LEN {
        return Err(format!(
            "Alert rule JSON too large ({} bytes, max {})",
            json.len(),
            MAX_ALERT_JSON_LEN
        ));
    }
    // Ensure it parses as valid JSON
    serde_json::from_str::<serde_json::Value>(json).map_err(|e| format!("Invalid JSON: {}", e))?;
    Ok(())
}

/// Validate a broker session token payload.
pub fn validate_session_token(token: &str) -> Result<(), String> {
    if token.trim().is_empty() {
        return Err("Session token must not be empty".into());
    }

    if token.len() > MAX_SESSION_TOKEN_LEN {
        return Err(format!(
            "Session token too large ({} chars, max {})",
            token.len(),
            MAX_SESSION_TOKEN_LEN
        ));
    }

    Ok(())
}

/// Validate an ML algorithm name.
pub fn validate_algorithm(algo: &str) -> Result<(), String> {
    if !VALID_ALGORITHMS.contains(&algo) {
        return Err(format!(
            "Unknown algorithm: {}. Valid: {}",
            algo,
            VALID_ALGORITHMS.join(", ")
        ));
    }
    Ok(())
}

/// Validate an LLM provider base URL: https everywhere; plain http only for
/// loopback/private hosts (local inference servers like Ollama/vLLM).
/// The configured endpoint receives the bearer key + prompts, so open
/// internet over cleartext is rejected.
pub fn validate_provider_url(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url.trim())
        .map_err(|e| format!("Invalid provider base_url `{url}`: {e}"))?;
    let scheme = parsed.scheme();
    if scheme == "https" {
        return Ok(());
    }
    if scheme != "http" {
        return Err(format!(
            "Provider base_url must be http(s) — got `{scheme}://`"
        ));
    }
    // IPv6 hosts arrive bracketed ("[::1]") — strip before parsing.
    let host = parsed
        .host_str()
        .unwrap_or("")
        .trim_start_matches('[')
        .trim_end_matches(']');
    if host == "localhost"
        || host == "localhost.localdomain"
        || host.ends_with(".local")
        || host.ends_with(".internal")
    {
        return Ok(());
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        let private = match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.octets()[0] == 169 && v4.octets()[1] == 254
            }
            std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local(),
        };
        if private {
            return Ok(());
        }
    }
    Err(format!(
        "Insecure http endpoint `{host}` — use https, or a loopback/private host"
    ))
}

/// Validate that a file path does not contain path traversal sequences.
pub fn validate_path(path: &str) -> Result<(), String> {
    if path.contains("..") {
        return Err("Path traversal detected: '..' not allowed".into());
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return Err("Absolute paths not allowed".into());
    }
    if path.len() > 512 {
        return Err(format!("Path too long ({} chars, max 512)", path.len()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_symbols() {
        assert!(validate_symbol("AAPL").is_ok());
        assert!(validate_symbol("RELIANCE.NS").is_ok());
        assert!(validate_symbol("BRK-B").is_ok());
    }

    #[test]
    fn invalid_symbols() {
        assert!(validate_symbol("").is_err());
        assert!(validate_symbol("AAAAABBBBBCCCCCDDDDDE").is_err()); // too long
        assert!(validate_symbol("SYM BOL").is_err()); // spaces
        assert!(validate_symbol("SYM$BOL").is_err()); // special chars
    }

    #[test]
    fn valid_quantities() {
        assert!(validate_quantity(1.0).is_ok());
        assert!(validate_quantity(100.0).is_ok());
        assert!(validate_quantity(999_999.0).is_ok());
    }

    #[test]
    fn provider_url_https_always_ok() {
        assert!(validate_provider_url("https://openrouter.ai/api/v1").is_ok());
        assert!(validate_provider_url("https://evil.example.com/x").is_ok());
    }

    #[test]
    fn provider_url_http_private_only() {
        assert!(validate_provider_url("http://localhost:11434/v1").is_ok());
        assert!(validate_provider_url("http://127.0.0.1:8000").is_ok());
        assert!(validate_provider_url("http://192.168.1.5:8000").is_ok());
        assert!(validate_provider_url("http://[::1]:8000").is_ok());
        assert!(validate_provider_url("http://nas.local/v1").is_ok());
        // Public hosts over cleartext are rejected — the bearer key + prompts
        // would be exposed.
        assert!(validate_provider_url("http://openrouter.ai/api/v1").is_err());
        assert!(validate_provider_url("http://8.8.8.8:443").is_err());
        assert!(validate_provider_url("ftp://localhost").is_err());
        assert!(validate_provider_url("not a url").is_err());
    }

    #[test]
    fn invalid_quantities() {
        assert!(validate_quantity(0.0).is_err());
        assert!(validate_quantity(-1.0).is_err());
        assert!(validate_quantity(f64::NAN).is_err());
        assert!(validate_quantity(f64::INFINITY).is_err());
        assert!(validate_quantity(2_000_000.0).is_err());
    }

    #[test]
    fn valid_prices() {
        assert!(validate_price(None).is_ok());
        assert!(validate_price(Some(150.0)).is_ok());
        assert!(validate_price(Some(0.0)).is_ok());
    }

    #[test]
    fn invalid_prices() {
        assert!(validate_price(Some(-1.0)).is_err());
        assert!(validate_price(Some(f64::NAN)).is_err());
    }

    #[test]
    fn valid_broker_ids() {
        assert!(validate_broker_id("paper").is_ok());
        assert!(validate_broker_id("zerodha").is_ok());
    }

    #[test]
    fn index_and_fx_symbols_allowed() {
        for symbol in ["^GSPC", "GC=F", "EURUSD=X", "NFO:NIFTY24DECFUT", "BTC/USDT"] {
            assert!(validate_symbol(symbol).is_ok(), "{symbol}");
        }
        assert!(validate_symbol("A B").is_err());
        assert!(validate_symbol("AAPL;rm").is_err());
    }

    #[test]
    fn invalid_broker_ids() {
        assert!(validate_broker_id("unknown_broker").is_err());
        assert!(validate_broker_id("").is_err());
    }

    #[test]
    fn path_traversal_blocked() {
        assert!(validate_path("data/file.csv").is_ok());
        assert!(validate_path("../etc/passwd").is_err());
        assert!(validate_path("/absolute/path").is_err());
    }

    #[test]
    fn valid_algorithms() {
        assert!(validate_algorithm("random_forest").is_ok());
        assert!(validate_algorithm("xgboost").is_ok());
    }

    #[test]
    fn invalid_algorithms() {
        assert!(validate_algorithm("neural_network").is_err());
    }
}
