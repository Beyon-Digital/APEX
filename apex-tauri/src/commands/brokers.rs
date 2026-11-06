use crate::dto::BrokerConnectionDto;
use crate::state::AppState;
use crate::validation;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tauri::State;

/// List all broker connection surfaces and their current readiness.
#[tauri::command]
pub async fn list_broker_connections(
    state: State<'_, AppState>,
) -> Result<Vec<BrokerConnectionDto>, String> {
    Ok(state.broker_connections())
}

/// Set or update a live broker session token/JWT at runtime.
#[tauri::command]
pub async fn set_broker_session(
    broker_id: String,
    session_token: String,
    state: State<'_, AppState>,
) -> Result<BrokerConnectionDto, String> {
    validation::validate_broker_id(&broker_id)?;
    validation::validate_session_token(&session_token)?;

    state
        .set_broker_session(&broker_id, session_token.trim())
        .map_err(|err| err.to_string())?;
    // The feed just became usable; give it the symbols that were requested earlier.
    let _ = state.aggregator.sync_subscriptions().await;
    state
        .persist_session_token(&broker_id, session_token.trim(), None)
        .map_err(|err| err.to_string())?;

    state
        .broker_connection(&broker_id)
        .ok_or_else(|| format!("Unknown broker: {}", broker_id))
}

/// Exchange a Zerodha Kite `request_token` (from the Kite web login
/// redirect `https://kite.trade/connect/login?api_key=…`) for a daily
/// `access_token` and install it on the Zerodha adapters.
///
/// The API key and secret come from the vault (the secret never leaves this
/// call). The Kite checksum is `sha256(api_key + request_token + api_secret)`.
/// Accepts either the bare token or the full redirect URL.
#[tauri::command]
pub async fn zerodha_login(
    request_token: String,
    state: State<'_, AppState>,
) -> Result<BrokerConnectionDto, String> {
    let request_token = extract_request_token(&request_token);
    let request_token = request_token.as_str();
    if request_token.is_empty() || request_token.len() > 256 {
        return Err("Invalid request_token".to_string());
    }
    if !state.vault.is_unlocked() {
        return Err("Vault is locked. Unlock it to log in to Zerodha.".to_string());
    }
    let api_key = state
        .vault
        .get_secret("zerodha", "api_key")?
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| "Zerodha API key not set. Add it under Settings > Adapters.".to_string())?;
    let api_secret = state
        .vault
        .get_secret("zerodha", "api_secret")?
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| {
            "Zerodha API secret not set. It is required to exchange the request token.".to_string()
        })?;

    let checksum = hex::encode(
        Sha256::digest(format!("{}{}{}", api_key, request_token, api_secret).as_bytes()).to_vec(),
    );

    #[derive(Deserialize)]
    struct TokenResponse {
        data: Option<TokenData>,
        #[serde(default)]
        message: Option<String>,
    }
    #[derive(Deserialize)]
    struct TokenData {
        access_token: String,
    }

    let resp = state
        .http
        .post("https://api.kite.trade/session/token")
        .form(&[
            ("api_key", api_key.as_str()),
            ("request_token", request_token),
            ("checksum", checksum.as_str()),
        ])
        .send()
        .await
        .map_err(|e| format!("Kite session request failed: {e}"))?;

    let body: TokenResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse Kite session response: {e}"))?;
    let token = body
        .data
        .map(|d| d.access_token)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            format!(
                "Kite login failed: {}",
                body.message
                    .unwrap_or_else(|| "no access_token returned".into())
            )
        })?;

    state
        .set_broker_session("zerodha", &token)
        .map_err(|e| e.to_string())?;
    let _ = state.aggregator.sync_subscriptions().await;
    state
        .persist_session_token(
            "zerodha",
            &token,
            Some(crate::broker_runtime::kite_token_expiry(chrono::Utc::now())),
        )
        .map_err(|e| e.to_string())?;
    state
        .broker_connection("zerodha")
        .ok_or_else(|| "Unknown broker: zerodha".to_string())
}

/// Clear a live broker session token/JWT.
#[tauri::command]
pub async fn clear_broker_session(
    broker_id: String,
    state: State<'_, AppState>,
) -> Result<BrokerConnectionDto, String> {
    validation::validate_broker_id(&broker_id)?;

    state
        .clear_broker_session(&broker_id)
        .map_err(|err| err.to_string())?;
    state
        .forget_session_token(&broker_id)
        .map_err(|err| err.to_string())?;

    state
        .broker_connection(&broker_id)
        .ok_or_else(|| format!("Unknown broker: {}", broker_id))
}

/// Kite login URL for the stored API key, to open in the user's browser.
#[tauri::command]
pub async fn zerodha_login_url(state: State<'_, AppState>) -> Result<String, String> {
    if !state.vault.is_unlocked() {
        return Err("Vault is locked. Unlock it first.".to_string());
    }
    let api_key = state
        .vault
        .get_secret("zerodha", "api_key")?
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| "Zerodha API key not set. Add it under Settings > Adapters.".to_string())?;
    let encoded: String = api_key
        .trim()
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    Ok(format!("https://kite.zerodha.com/connect/login?v=3&api_key={encoded}"))
}

/// Accept either a bare request token or the full redirect URL containing
/// `request_token=…`.
fn extract_request_token(input: &str) -> String {
    let input = input.trim();
    if let Some(query) = input.split('?').nth(1) {
        for pair in query.split(['&', '#']) {
            if let Some(value) = pair.strip_prefix("request_token=") {
                return value.trim().to_string();
            }
        }
    }
    input.to_string()
}

#[cfg(test)]
mod tests {
    use super::extract_request_token;

    #[test]
    fn extracts_token_from_redirect_url() {
        assert_eq!(
            extract_request_token("http://127.0.0.1/?action=login&type=login&status=success&request_token=abc123&x=1"),
            "abc123"
        );
        assert_eq!(extract_request_token("  abc123 "), "abc123");
    }
}
