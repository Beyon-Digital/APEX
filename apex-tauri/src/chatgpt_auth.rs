//! "Sign in with ChatGPT" for the AI copilot, so it can run on the user's
//! ChatGPT plan allowance instead of an API key.
//!
//! This follows OpenAI's documented open-source / local-app flow
//! (developers.openai.com/siwc/token-sharing-open-source):
//!
//! * OAuth 2.0 authorization code + PKCE (S256) in the system browser, with a
//!   loopback callback on `127.0.0.1` (never `localhost`).
//! * Dynamic client registration happens in the first authorize request, using
//!   `client_id=dynamic_agent_client` plus an opaque per-host `ext_agent_host_id`.
//!   The issued `oaiapp_...` client id comes back on the callback and is reused.
//! * The ID token is verified (signature via JWKS, issuer, audience = issued
//!   client id, expiry, nonce) and the `chatgpt.tokens.use.direct` scope must be
//!   granted before any plan-backed request is made.
//! * Access tokens last an hour; the refresh token rotates on every refresh, so
//!   refreshes are serialized.
//! * Inference goes to `POST https://api.openai.com/v1/responses` with
//!   `store: false` and `stream: true`.
//!
//! Tokens live only in the encrypted vault (adapter id `chatgpt`) and are never
//! logged.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;

use crate::state::AppState;
use crate::vault::Vault;

pub const BOOTSTRAP_CLIENT_ID: &str = "dynamic_agent_client";
pub const SCOPES: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
pub const REQUIRED_SCOPE: &str = "chatgpt.tokens.use.direct";
pub const RESOURCE: &str = "https://api.openai.com/v1";
pub const APP_NAME: &str = "APEX Terminal";
pub const CALLBACK_PORT: u16 = 1455;
pub const CALLBACK_PATH: &str = "/auth/callback";
/// Refresh this long before the access token expires.
const REFRESH_MARGIN: chrono::Duration = chrono::Duration::minutes(5);
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);
const VAULT_ID: &str = "chatgpt";

#[derive(Debug, Clone)]
pub struct AuthEndpoints {
    pub issuer: String,
    pub authorize: String,
    pub token: String,
    pub discovery: String,
}

impl Default for AuthEndpoints {
    fn default() -> Self {
        Self {
            issuer: "https://auth.openai.com".into(),
            authorize: "https://auth.openai.com/api/accounts/authorize".into(),
            token: "https://auth.openai.com/api/accounts/oauth/token".into(),
            discovery: "https://auth.openai.com/.well-known/openid-configuration".into(),
        }
    }
}

// ── PKCE and the authorize URL ───────────────────────────────────────

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn new_pkce() -> Pkce {
    let verifier = random_token(48);
    let challenge = pkce_challenge(&verifier);
    Pkce { verifier, challenge }
}

pub struct AuthorizeRequest<'a> {
    /// The issued client id on a returning sign-in; `None` registers a new client.
    pub saved_client_id: Option<&'a str>,
    pub host_id: &'a str,
    pub redirect_uri: &'a str,
    pub state: &'a str,
    pub nonce: &'a str,
    pub code_challenge: &'a str,
    pub id_token_hint: Option<&'a str>,
    pub login_hint: Option<&'a str>,
}

fn enc(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub fn authorize_url(endpoints: &AuthEndpoints, req: &AuthorizeRequest) -> String {
    let mut params: Vec<(&str, String)> = vec![
        ("client_id", req.saved_client_id.unwrap_or(BOOTSTRAP_CLIENT_ID).to_string()),
    ];
    if req.saved_client_id.is_none() {
        params.push(("agent_name_hint", APP_NAME.to_string()));
    }
    params.extend([
        ("ext_agent_host_id", req.host_id.to_string()),
        ("response_type", "code".to_string()),
        ("redirect_uri", req.redirect_uri.to_string()),
        ("scope", SCOPES.to_string()),
        ("resource", RESOURCE.to_string()),
        ("state", req.state.to_string()),
        ("nonce", req.nonce.to_string()),
        ("code_challenge_method", "S256".to_string()),
        ("code_challenge", req.code_challenge.to_string()),
    ]);
    if let Some(hint) = req.id_token_hint {
        params.push(("id_token_hint", hint.to_string()));
    }
    if let Some(hint) = req.login_hint {
        params.push(("login_hint", hint.to_string()));
    }
    let query = params
        .iter()
        .map(|(k, v)| format!("{k}={}", enc(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{}?{query}", endpoints.authorize)
}

// ── Callback ─────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
pub struct Callback {
    pub code: String,
    /// The issued client id (always present on a new registration).
    pub client_id: Option<String>,
}

fn decode_component(raw: &str) -> String {
    let bytes = raw.replace('+', " ").into_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            if let Ok(hex) = u8::from_str_radix(&String::from_utf8_lossy(&bytes[i + 1..i + 3]), 16) {
                out.push(hex);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse_callback(
    query: &str,
    expected_state: &str,
    saved_client_id: Option<&str>,
) -> Result<Callback, String> {
    let params: std::collections::HashMap<String, String> = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (decode_component(k), decode_component(v)))
        .collect();
    // State first: nothing in an unauthenticated callback is trusted before this.
    if params.get("state").map(String::as_str) != Some(expected_state) {
        return Err("The sign-in response did not match this request (state mismatch). Start again.".into());
    }
    if let Some(error) = params.get("error") {
        return Err(if error == "access_denied" {
            "Sign-in was cancelled in the browser.".to_string()
        } else {
            format!("ChatGPT sign-in failed: {error}")
        });
    }
    let code = params
        .get("code")
        .filter(|c| !c.is_empty())
        .ok_or("The sign-in response had no authorization code.")?
        .clone();
    let returned = params.get("client_id").cloned();
    match (saved_client_id, &returned) {
        (Some(saved), Some(got)) if saved != got => {
            return Err("The sign-in response was for a different ChatGPT registration. Start again.".into());
        }
        (None, None) => {
            return Err("ChatGPT did not return a client id, so registration is incomplete.".into());
        }
        _ => {}
    }
    Ok(Callback { code, client_id: returned })
}

/// True when `query` carries `state=<expected>` exactly.
pub fn query_has_state(query: &str, expected: &str) -> bool {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .any(|(k, v)| k == "state" && decode_component(v) == expected)
}

/// Remove parameters that identify the user from an authorize URL before it is
/// shown anywhere (OpenAI asks that `id_token_hint` URLs stay out of logs and UIs).
pub fn redact_authorize_url(url: &str) -> String {
    let Some((base, query)) = url.split_once('?') else { return url.to_string() };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| !pair.starts_with("id_token_hint=") && !pair.starts_with("login_hint="))
        .collect();
    format!("{base}?{}", kept.join("&"))
}

/// Accept the browser redirect on `listener` and return its query string.
///
/// Every connection is served in its own task and any failure on one (an idle
/// preconnect, a reset, a bad request) only drops that connection. Only a request
/// that carries our one-time state can complete the sign-in, and `cancel` ends it.
async fn wait_for_callback(
    listener: TcpListener,
    expected_state: &str,
    cancel: &Notify,
) -> Result<String, String> {
    let (found_tx, mut found_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let expected = expected_state.to_string();
    let accept = async {
        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => continue, // e.g. ECONNABORTED: keep listening
            };
            let found_tx = found_tx.clone();
            let expected = expected.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = match tokio::time::timeout(Duration::from_secs(5), socket.read(&mut buf)).await {
                    Ok(Ok(n)) if n > 0 => n,
                    _ => return, // idle, reset or empty: just drop it
                };
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let target = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("")
                    .to_string();
                let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
                // Only the real redirect carries our one-time state. Anything else
                // (favicon, another local process poking the port) is ignored.
                if path != CALLBACK_PATH || !query_has_state(query, &expected) {
                    let _ = socket
                        .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await;
                    return;
                }
                let body = "<html><body style=\"font-family:sans-serif;background:#000;color:#ffa028;padding:3em\"><h2>APEX Terminal</h2><p>Signed in. You can close this tab and return to the app.</p></body></html>";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = found_tx.send(query.to_string());
            });
        }
    };
    tokio::select! {
        _ = accept => Err("The sign-in listener stopped unexpectedly.".to_string()),
        found = found_rx.recv() => found.ok_or_else(|| "The sign-in listener closed.".to_string()),
        _ = cancel.notified() => Err("Sign-in was cancelled.".to_string()),
        _ = tokio::time::sleep(SIGN_IN_TIMEOUT) => {
            Err("Timed out waiting for the browser sign-in (5 minutes).".to_string())
        }
    }
}

// ── Tokens ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub scope: Option<String>,
}

impl TokenSet {
    pub fn expires_at(&self) -> DateTime<Utc> {
        Utc::now() + chrono::Duration::seconds(self.expires_in.unwrap_or(3600))
    }

    pub fn has_plan_scope(&self) -> bool {
        self.scope
            .as_deref()
            .map(|s| s.split_whitespace().any(|x| x == REQUIRED_SCOPE))
            .unwrap_or(false)
    }
}

#[derive(Debug)]
pub enum AuthError {
    /// The credentials are unusable; the user must sign in again.
    Terminal(String),
    /// Network or server trouble; keep the credentials and retry later.
    Transient(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Terminal(m) | AuthError::Transient(m) => f.write_str(m),
        }
    }
}

const TERMINAL_REFRESH_ERRORS: &[&str] = &[
    "invalid_grant",
    "invalid_refresh_token",
    "token_expired",
    "refresh_token_expired",
    "refresh_token_invalidated",
    "refresh_token_reused",
    "invalid_client",
];

async fn token_request(
    http: &reqwest::Client,
    endpoints: &AuthEndpoints,
    form: &[(&str, &str)],
) -> Result<TokenSet, AuthError> {
    let response = http
        .post(&endpoints.token)
        .form(form)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| AuthError::Transient(format!("Could not reach ChatGPT sign-in: {e}")))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if status.is_success() {
        return serde_json::from_str::<TokenSet>(&body)
            .map_err(|e| AuthError::Transient(format!("Unexpected sign-in response: {e}")));
    }
    let code = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str().map(String::from)))
        .unwrap_or_default();
    if status.is_server_error() || status.as_u16() == 429 {
        return Err(AuthError::Transient(format!("ChatGPT sign-in is unavailable ({status}). Try again shortly.")));
    }
    // Only a genuine OAuth rejection means the credentials are dead. Any other 4xx
    // (a WAF or Cloudflare challenge, a 408, an error body without an OAuth code)
    // says nothing about the grant, so keep the tokens and let the user retry.
    let oauth_status = matches!(status.as_u16(), 400 | 401);
    if oauth_status && TERMINAL_REFRESH_ERRORS.contains(&code.as_str()) {
        return Err(AuthError::Terminal(format!("ChatGPT rejected the sign-in ({code})")));
    }
    Err(AuthError::Transient(format!("ChatGPT sign-in failed ({status})")))
}

pub async fn exchange_code(
    http: &reqwest::Client,
    endpoints: &AuthEndpoints,
    client_id: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<TokenSet, AuthError> {
    token_request(
        http,
        endpoints,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
            ("resource", RESOURCE),
        ],
    )
    .await
}

pub async fn refresh_tokens(
    http: &reqwest::Client,
    endpoints: &AuthEndpoints,
    client_id: &str,
    refresh_token: &str,
) -> Result<TokenSet, AuthError> {
    // `scope` is deliberately omitted so the existing grant is kept.
    token_request(
        http,
        endpoints,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
            ("resource", RESOURCE),
        ],
    )
    .await
}

// ── ID token verification ────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct IdentityClaims {
    pub sub: String,
    pub email: Option<String>,
}

pub async fn verify_id_token(
    http: &reqwest::Client,
    endpoints: &AuthEndpoints,
    id_token: &str,
    client_id: &str,
    nonce: &str,
) -> Result<IdentityClaims, String> {
    use jsonwebtoken::{decode, decode_header, jwk::JwkSet, Algorithm, DecodingKey, Validation};

    let header = decode_header(id_token).map_err(|e| format!("Malformed ID token: {e}"))?;
    // Never accept a symmetric algorithm for a token a public key should have signed.
    if matches!(header.alg, Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512) {
        return Err("ID token uses an unexpected signing algorithm".into());
    }
    let kid = header.kid.clone().ok_or("ID token has no key id")?;

    let discovery: Value = http
        .get(&endpoints.discovery)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("Could not fetch OpenAI discovery document: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Bad discovery document: {e}"))?;
    let jwks_uri = discovery
        .get("jwks_uri")
        .and_then(|v| v.as_str())
        .ok_or("Discovery document has no jwks_uri")?;
    let jwks: JwkSet = http
        .get(jwks_uri)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("Could not fetch signing keys: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Bad signing key set: {e}"))?;
    let jwk = jwks.find(&kid).ok_or("ID token was signed with an unknown key")?;
    let key = DecodingKey::from_jwk(jwk).map_err(|e| format!("Unusable signing key: {e}"))?;

    let mut validation = Validation::new(header.alg);
    validation.set_audience(&[client_id]);
    validation.set_issuer(&[endpoints.issuer.as_str()]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    let claims = decode::<Value>(id_token, &key, &validation)
        .map_err(|e| format!("ID token failed verification: {e}"))?
        .claims;

    if claims.get("nonce").and_then(|v| v.as_str()) != Some(nonce) {
        return Err("ID token nonce did not match this sign-in".into());
    }
    Ok(IdentityClaims {
        sub: claims.get("sub").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        email: claims.get("email").and_then(|v| v.as_str()).map(String::from),
    })
}

// ── Vault persistence ────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct StoredAuth {
    pub client_id: Option<String>,
    pub host_id: Option<String>,
    pub email: Option<String>,
    pub sub: Option<String>,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    /// Whether the plan-usage scope was granted.
    pub plan_usage: bool,
}

fn load_settings(vault: &Vault) -> serde_json::Map<String, Value> {
    vault
        .get_config(VAULT_ID)
        .ok()
        .flatten()
        .and_then(|c| serde_json::from_str(&c.settings_json).ok())
        .unwrap_or_default()
}

pub fn load(vault: &Vault) -> Result<StoredAuth, String> {
    if !vault.is_unlocked() {
        return Err("Unlock the vault to use ChatGPT sign-in.".into());
    }
    let settings = load_settings(vault);
    let text = |key: &str| settings.get(key).and_then(|v| v.as_str()).map(String::from);
    Ok(StoredAuth {
        client_id: text("client_id"),
        host_id: text("host_id"),
        email: text("email"),
        sub: text("sub"),
        access_token: vault.get_secret(VAULT_ID, "access_token")?,
        refresh_token: vault.get_secret(VAULT_ID, "refresh_token")?,
        id_token: vault.get_secret(VAULT_ID, "id_token")?,
        expires_at: text("expires_at").and_then(|s| DateTime::parse_from_rfc3339(&s).ok()).map(|d| d.with_timezone(&Utc)),
        plan_usage: settings.get("plan_usage").and_then(|v| v.as_bool()).unwrap_or(false),
    })
}

fn save_settings(vault: &Vault, update: impl FnOnce(&mut serde_json::Map<String, Value>)) -> Result<(), String> {
    let mut settings = load_settings(vault);
    update(&mut settings);
    vault.set_config(VAULT_ID, true, &Value::Object(settings).to_string())
}

pub fn save_host_id(vault: &Vault, host_id: &str) -> Result<(), String> {
    save_settings(vault, |s| {
        s.insert("host_id".into(), json!(host_id));
    })
}

pub fn save_signed_in(
    vault: &Vault,
    client_id: &str,
    claims: &IdentityClaims,
    tokens: &TokenSet,
) -> Result<(), String> {
    vault.set_secret(VAULT_ID, "access_token", &tokens.access_token)?;
    if let Some(refresh) = &tokens.refresh_token {
        vault.set_secret(VAULT_ID, "refresh_token", refresh)?;
    }
    if let Some(id) = &tokens.id_token {
        vault.set_secret(VAULT_ID, "id_token", id)?;
    }
    save_settings(vault, |s| {
        s.insert("client_id".into(), json!(client_id));
        s.insert("sub".into(), json!(claims.sub));
        s.insert("email".into(), json!(claims.email));
        s.insert("expires_at".into(), json!(tokens.expires_at().to_rfc3339()));
        s.insert("plan_usage".into(), json!(tokens.has_plan_scope()));
    })
}

fn save_refreshed(vault: &Vault, tokens: &TokenSet) -> Result<(), String> {
    // The refresh token rotates and the old one is already dead server-side, so it is
    // written first: losing the access token costs one refresh, losing this costs the login.
    if let Some(refresh) = &tokens.refresh_token {
        vault.set_secret(VAULT_ID, "refresh_token", refresh)?;
    }
    vault.set_secret(VAULT_ID, "access_token", &tokens.access_token)?;
    if let Some(id) = &tokens.id_token {
        vault.set_secret(VAULT_ID, "id_token", id)?;
    }
    save_settings(vault, |s| {
        s.insert("expires_at".into(), json!(tokens.expires_at().to_rfc3339()));
        if tokens.scope.is_some() {
            s.insert("plan_usage".into(), json!(tokens.has_plan_scope()));
        }
    })
}

/// User choices stored next to the registration.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Preferences {
    pub use_for_copilot: bool,
    pub model: Option<String>,
}

pub fn preferences(vault: &Vault) -> Preferences {
    if !vault.is_unlocked() {
        return Preferences::default();
    }
    let settings = load_settings(vault);
    Preferences {
        use_for_copilot: settings.get("use_for_copilot").and_then(|v| v.as_bool()).unwrap_or(false),
        model: settings.get("model").and_then(|v| v.as_str()).map(String::from),
    }
}

pub fn set_preferences(vault: &Vault, use_for_copilot: Option<bool>, model: Option<String>) -> Result<(), String> {
    if !vault.is_unlocked() {
        return Err("Unlock the vault first.".into());
    }
    save_settings(vault, |s| {
        if let Some(flag) = use_for_copilot {
            s.insert("use_for_copilot".into(), json!(flag));
        }
        if let Some(model) = model {
            s.insert("model".into(), json!(model));
        }
    })
}

/// Drop the tokens but keep the client registration and host id for the next sign-in.
pub fn clear_tokens(vault: &Vault) -> Result<(), String> {
    for field in ["access_token", "refresh_token", "id_token"] {
        vault.delete_secret(VAULT_ID, field)?;
    }
    save_settings(vault, |s| {
        s.remove("expires_at");
        s.remove("plan_usage");
    })
}

// ── Runtime ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ChatGptStatus {
    /// "locked" | "signed_out" | "waiting" | "connected" | "error"
    pub state: String,
    pub email: Option<String>,
    /// False when signed in but the plan-usage scope was not granted.
    pub plan_usage: bool,
    pub message: Option<String>,
}

#[derive(Default)]
pub struct Runtime {
    transient: Mutex<Option<(String, String)>>, // (state, message): "waiting" | "error"
    pub refresh_lock: tokio::sync::Mutex<()>,
    endpoints_override: Mutex<Option<AuthEndpoints>>,
    /// Rotated tokens the server issued but that could not be written to the vault
    /// yet. They are retried before the next refresh; the old refresh token is dead.
    rotated: Mutex<Option<TokenSet>>,
    /// Ends a pending browser sign-in.
    pub cancel: Notify,
}

impl Runtime {
    fn set(&self, state: &str, message: &str) {
        *self.transient.lock().unwrap() = Some((state.into(), message.into()));
    }

    fn clear(&self) {
        *self.transient.lock().unwrap() = None;
    }

    pub fn endpoints(&self) -> AuthEndpoints {
        self.endpoints_override.lock().unwrap().clone().unwrap_or_default()
    }

    #[cfg(test)]
    pub fn set_endpoints(&self, endpoints: AuthEndpoints) {
        *self.endpoints_override.lock().unwrap() = Some(endpoints);
    }
}

/// The message of a browser sign-in that failed, if the last one did. `status` hides it while a
/// refresh token from an earlier sign-in is still stored (the account stays usable), so the audit
/// of a sign-in outcome reads it here.
pub fn sign_in_error(state: &AppState) -> Option<String> {
    match state.chatgpt.transient.lock().unwrap().as_ref() {
        Some((s, message)) if s == "error" => Some(message.clone()),
        _ => None,
    }
}

pub fn status(state: &AppState) -> ChatGptStatus {
    if !state.vault.is_unlocked() {
        return ChatGptStatus { state: "locked".into(), email: None, plan_usage: false, message: Some("Unlock the vault to use ChatGPT sign-in.".into()) };
    }
    let pending = state.chatgpt.transient.lock().unwrap().clone();
    let stored = load(&state.vault).unwrap_or_default();
    if let Some((s, message)) = pending {
        if s == "waiting" || stored.refresh_token.is_none() {
            return ChatGptStatus { state: s, email: stored.email, plan_usage: false, message: Some(message) };
        }
    }
    if stored.refresh_token.is_some() {
        let message = (!stored.plan_usage).then(|| "Signed in, but ChatGPT plan usage was not granted. Sign in again and allow plan usage.".to_string());
        return ChatGptStatus { state: "connected".into(), email: stored.email, plan_usage: stored.plan_usage, message };
    }
    ChatGptStatus { state: "signed_out".into(), email: None, plan_usage: false, message: None }
}

/// Begin a sign-in: bind the loopback listener, return the browser URL, and finish
/// the exchange in the background.
pub async fn begin_sign_in(state: &AppState) -> Result<String, String> {
    if !state.vault.is_unlocked() {
        return Err("Unlock the vault before signing in with ChatGPT.".into());
    }
    if matches!(state.chatgpt.transient.lock().unwrap().as_ref(), Some((s, _)) if s == "waiting") {
        return Err("A ChatGPT sign-in is already in progress. Finish it in your browser or wait for it to time out.".into());
    }
    let app = state.app.get().ok_or("The app window is not ready.")?.clone();
    let endpoints = state.chatgpt.endpoints();
    let stored = load(&state.vault)?;

    // A stable, opaque id for this machine. Not a credential and carries no personal data.
    let host_id = match stored.host_id.clone() {
        Some(id) => id,
        None => {
            let id = format!("urn:uuid:{}", uuid::Uuid::new_v4());
            save_host_id(&state.vault, &id)?;
            id
        }
    };

    // First registration must use the documented callback port; a returning
    // sign-in may fall back to any free port.
    let listener = match TcpListener::bind(("127.0.0.1", CALLBACK_PORT)).await {
        Ok(l) => l,
        Err(_) if stored.client_id.is_some() => TcpListener::bind(("127.0.0.1", 0)).await.map_err(|e| e.to_string())?,
        Err(_) => {
            return Err(format!(
                "Port {CALLBACK_PORT} is busy (another tool, such as the Codex CLI, may be using it). Close it and try again."
            ))
        }
    };
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");

    let pkce = new_pkce();
    let oauth_state = random_token(24);
    let nonce = random_token(24);
    let url = authorize_url(
        &endpoints,
        &AuthorizeRequest {
            saved_client_id: stored.client_id.as_deref(),
            host_id: &host_id,
            redirect_uri: &redirect_uri,
            state: &oauth_state,
            nonce: &nonce,
            code_challenge: &pkce.challenge,
            // The account chooser is shown instead of hinting: a hint would put the
            // user's email and ID token on a command line other local users can read.
            id_token_hint: None,
            login_hint: None,
        },
    );

    state.chatgpt.set("waiting", "Waiting for you to finish signing in in the browser…");
    let saved_client = stored.client_id.clone();
    tauri::async_runtime::spawn(async move {
        use tauri::Manager;
        let state = app.state::<AppState>();
        let outcome = async {
            let query = wait_for_callback(listener, &oauth_state, &state.chatgpt.cancel).await?;
            let callback = parse_callback(&query, &oauth_state, saved_client.as_deref())?;
            let client_id = callback
                .client_id
                .clone()
                .or(saved_client.clone())
                .ok_or("No client id")?;
            let tokens = exchange_code(&state.http, &endpoints, &client_id, &callback.code, &pkce.verifier, &redirect_uri)
                .await
                .map_err(|e| e.to_string())?;
            let id_token = tokens.id_token.as_deref().ok_or("ChatGPT did not return an ID token")?;
            let claims = verify_id_token(&state.http, &endpoints, id_token, &client_id, &nonce).await?;
            let _guard = state.chatgpt.refresh_lock.lock().await;
            *state.chatgpt.rotated.lock().unwrap() = None;
            save_signed_in(&state.vault, &client_id, &claims, &tokens)?;
            Ok::<_, String>(tokens.has_plan_scope())
        }
        .await;
        match outcome {
            Ok(_) => state.chatgpt.clear(),
            Err(error) => state.chatgpt.set("error", &error),
        }
    });
    Ok(url)
}

/// End a pending browser sign-in and free the callback port.
pub fn cancel_sign_in(state: &AppState) {
    state.chatgpt.cancel.notify_waiters();
    state.chatgpt.clear();
}

/// A valid access token, refreshing (and rotating the refresh token) when needed.
pub async fn access_token(state: &AppState) -> Result<String, String> {
    // Serialize refreshes: the refresh token rotates, so two racing refreshes would
    // invalidate each other.
    let _guard = state.chatgpt.refresh_lock.lock().await;
    // A previous refresh rotated the token but could not save it: save it first.
    let pending = state.chatgpt.rotated.lock().unwrap().clone();
    if let Some(tokens) = pending {
        match save_refreshed(&state.vault, &tokens) {
            Ok(()) => {
                *state.chatgpt.rotated.lock().unwrap() = None;
                return Ok(tokens.access_token);
            }
            Err(_) => return Ok(tokens.access_token), // still usable; keep it in memory
        }
    }
    let stored = load(&state.vault)?;
    let (Some(client_id), Some(refresh)) = (stored.client_id.clone(), stored.refresh_token.clone()) else {
        return Err("Sign in with ChatGPT first (Copilot > Provider).".into());
    };
    if !stored.plan_usage {
        return Err("ChatGPT plan usage was not granted. Sign in again and allow plan usage.".into());
    }
    if let (Some(token), Some(expires)) = (&stored.access_token, stored.expires_at) {
        if expires - REFRESH_MARGIN > Utc::now() {
            return Ok(token.clone());
        }
    }
    let endpoints = state.chatgpt.endpoints();
    match refresh_tokens(&state.http, &endpoints, &client_id, &refresh).await {
        Ok(tokens) => {
            if let Err(error) = save_refreshed(&state.vault, &tokens) {
                // The server already rotated the refresh token. Keep the new one in
                // memory and still return the working access token.
                tracing::warn!(%error, "Could not store the rotated ChatGPT token; keeping it in memory");
                let access = tokens.access_token.clone();
                *state.chatgpt.rotated.lock().unwrap() = Some(tokens);
                return Ok(access);
            }
            Ok(tokens.access_token)
        }
        Err(AuthError::Terminal(_)) => {
            clear_tokens(&state.vault)?;
            Err("Your ChatGPT session expired or was revoked. Sign in again.".into())
        }
        Err(AuthError::Transient(message)) => Err(message),
    }
}

/// Revoke the refresh token (best effort) and forget the tokens.
pub async fn sign_out(state: &AppState) -> Result<String, String> {
    // Wait for any refresh in flight, so it cannot save fresh tokens after we clear them.
    let _guard = state.chatgpt.refresh_lock.lock().await;
    *state.chatgpt.rotated.lock().unwrap() = None;
    let stored = load(&state.vault)?;
    let endpoints = state.chatgpt.endpoints();
    let mut note = "Signed out.".to_string();
    if let (Some(refresh), Some(client_id)) = (&stored.refresh_token, &stored.client_id) {
        let revoked = async {
            let discovery: Value = state
                .http
                .get(&endpoints.discovery)
                .timeout(Duration::from_secs(10))
                .send()
                .await
                .ok()?
                .json()
                .await
                .ok()?;
            let url = discovery.get("revocation_endpoint")?.as_str()?.to_string();
            let response = state
                .http
                .post(url)
                .form(&[("token", refresh.as_str()), ("token_type_hint", "refresh_token"), ("client_id", client_id.as_str())])
                .timeout(Duration::from_secs(10))
                .send()
                .await
                .ok()?;
            response.status().is_success().then_some(())
        }
        .await;
        if revoked.is_none() {
            note = "Signed out locally. Remote revocation was not confirmed; you can disconnect APEX in ChatGPT Settings.".into();
        }
    }
    clear_tokens(&state.vault)?;
    state.chatgpt.clear();
    Ok(note)
}

// ── Inference helpers ────────────────────────────────────────────────

/// Friendly text for the documented plan-usage error codes.
pub fn describe_error_code(code: &str, param: Option<&str>) -> String {
    match code {
        "subscription_sharing_user_not_eligible" => "Your ChatGPT plan or workspace can't be used for this. Use an API key provider instead.".into(),
        "subscription_sharing_usage_limit_exceeded" => "ChatGPT plan usage limit reached for this app. Check ChatGPT Settings > Usage to raise or reset it.".into(),
        "subscription_sharing_usage_unavailable" | "subscription_sharing_user_unavailable" => "ChatGPT couldn't check your plan usage right now. Try again shortly.".into(),
        "subscription_sharing_unsupported_capability" => format!("ChatGPT plan requests don't support `{}`.", param.unwrap_or("that feature")),
        "subscription_sharing_route_not_supported" => "ChatGPT plan access doesn't support that API route.".into(),
        "subscription_sharing_invalid_user" => "ChatGPT rejected your sign-in. Sign in again.".into(),
        "chatpass_v2_scope_not_authorized" | "chatpass_v2_invalid_authorization_context" => "This sign-in doesn't allow plan usage. Sign in again and allow plan usage.".into(),
        other => format!("ChatGPT request failed ({other})."),
    }
}

/// Reduce a `/v1/responses` SSE body to the completed response object.
pub fn parse_responses_stream(body: &str) -> Result<Value, String> {
    for line in body.lines() {
        let Some(data) = line.strip_prefix("data:") else { continue };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(data) else { continue };
        match event.get("type").and_then(|t| t.as_str()) {
            Some("response.completed") => {
                return event.get("response").cloned().ok_or_else(|| "completed event had no response".to_string())
            }
            Some("response.failed") => {
                let error = event.pointer("/response/error");
                let code = error.and_then(|e| e.get("code")).and_then(|c| c.as_str()).unwrap_or("unknown");
                let param = error.and_then(|e| e.get("param")).and_then(|p| p.as_str());
                return Err(describe_error_code(code, param));
            }
            Some("response.incomplete") => {
                let reason = event.pointer("/response/incomplete_details/reason").and_then(|r| r.as_str()).unwrap_or("unknown reason");
                return Err(format!("ChatGPT stopped before finishing ({reason})."));
            }
            _ => {}
        }
    }
    Err("The ChatGPT response ended before it completed.".into())
}

/// Explain an HTTP failure that happened before any stream opened.
pub fn describe_http_failure(status: u16, body: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let code = parsed.as_ref().and_then(|v| v.pointer("/error/code")).and_then(|c| c.as_str());
    let param = parsed.as_ref().and_then(|v| v.pointer("/error/param")).and_then(|c| c.as_str());
    if let Some(code) = code {
        return describe_error_code(code, param);
    }
    match status {
        401 => "ChatGPT rejected your sign-in. Sign in again.".into(),
        403 => "ChatGPT plan access isn't allowed here (a region or policy restriction).".into(),
        503 => "ChatGPT plan access is temporarily unavailable. Try again shortly.".into(),
        other => format!("ChatGPT request failed (HTTP {other})."),
    }
}

/// One plan-backed `POST /v1/responses` call. The endpoint requires
/// `store: false` and `stream: true` and rejects several parameters
/// (`max_output_tokens`, `temperature`, `previous_response_id`, a `system` role),
/// so the body carries only what is documented to work.
pub async fn responses_round(
    state: &AppState,
    model: &str,
    input: &[Value],
    tools: &Value,
) -> Result<Value, String> {
    let token = access_token(state).await?;
    let response = state
        .http
        .post(format!("{RESOURCE}/responses"))
        .bearer_auth(&token)
        .timeout(Duration::from_secs(180))
        .json(&json!({
            "model": model,
            "input": input,
            "tools": tools,
            "store": false,
            "stream": true,
        }))
        .send()
        .await
        .map_err(|e| format!("ChatGPT request failed: {e}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(describe_http_failure(status.as_u16(), &body));
    }
    parse_responses_stream(&body)
}

/// Models the signed-in account can use for plan-backed requests.
pub async fn list_models(state: &AppState) -> Result<Vec<(String, String)>, String> {
    let token = access_token(state).await?;
    let response = state
        .http
        .get(format!("{RESOURCE}/models"))
        .bearer_auth(&token)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("Could not list ChatGPT models: {e}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(describe_http_failure(status.as_u16(), &body));
    }
    parse_models(&body)
}

pub fn parse_models(body: &str) -> Result<Vec<(String, String)>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("Bad model list: {e}"))?;
    let mut out = Vec::new();
    // Documented shape: { models: [{ slug, display_name, visibility }] }.
    if let Some(models) = v.get("models").and_then(|m| m.as_array()) {
        for m in models {
            if m.get("visibility").and_then(|x| x.as_str()).map(|x| x != "list").unwrap_or(false) {
                continue;
            }
            if let Some(slug) = m.get("slug").and_then(|s| s.as_str()) {
                let name = m.get("display_name").and_then(|s| s.as_str()).unwrap_or(slug);
                out.push((slug.to_string(), name.to_string()));
            }
        }
    }
    // Tolerate the standard OpenAI list shape too.
    if out.is_empty() {
        if let Some(data) = v.get("data").and_then(|d| d.as_array()) {
            for m in data {
                if let Some(id) = m.get("id").and_then(|s| s.as_str()) {
                    out.push((id.to_string(), id.to_string()));
                }
            }
        }
    }
    if out.is_empty() {
        return Err("ChatGPT returned no models for this account.".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn pkce_matches_the_rfc7636_example() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let p = new_pkce();
        assert!(p.verifier.len() >= 43 && p.verifier.len() <= 128);
        assert_eq!(pkce_challenge(&p.verifier), p.challenge);
    }

    #[test]
    fn first_registration_url_follows_the_documented_shape() {
        let url = authorize_url(
            &AuthEndpoints::default(),
            &AuthorizeRequest {
                saved_client_id: None,
                host_id: "urn:uuid:11111111-2222-4333-8444-555555555555",
                redirect_uri: "http://127.0.0.1:1455/auth/callback",
                state: "S",
                nonce: "N",
                code_challenge: "C",
                id_token_hint: None,
                login_hint: None,
            },
        );
        assert!(url.starts_with("https://auth.openai.com/api/accounts/authorize?"));
        for expected in [
            "client_id=dynamic_agent_client",
            "agent_name_hint=APEX%20Terminal",
            "ext_agent_host_id=urn%3Auuid%3A11111111-2222-4333-8444-555555555555",
            "response_type=code",
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback",
            "resource=https%3A%2F%2Fapi.openai.com%2Fv1",
            "code_challenge_method=S256",
            "code_challenge=C",
            "state=S",
            "nonce=N",
        ] {
            assert!(url.contains(expected), "missing {expected} in {url}");
        }
        assert!(url.contains("scope=openid%20profile%20email%20offline_access%20resource.invoke%20chatgpt.tokens.use.direct"));
        assert!(!url.contains("localhost"));
    }

    #[test]
    fn returning_sign_in_reuses_the_client_and_may_hint_the_account() {
        let url = authorize_url(
            &AuthEndpoints::default(),
            &AuthorizeRequest {
                saved_client_id: Some("oaiapp_abc"),
                host_id: "urn:uuid:x",
                redirect_uri: "http://127.0.0.1:1456/auth/callback",
                state: "S",
                nonce: "N",
                code_challenge: "C",
                id_token_hint: Some("hint.jwt.value"),
                login_hint: Some("me@example.com"),
            },
        );
        assert!(url.contains("client_id=oaiapp_abc"));
        assert!(!url.contains("dynamic_agent_client"));
        assert!(!url.contains("agent_name_hint"));
        assert!(url.contains("id_token_hint=hint.jwt.value"));
        assert!(url.contains("login_hint=me%40example.com"));
    }

    #[test]
    fn callback_checks_state_first_and_requires_a_client_on_registration() {
        assert_eq!(
            parse_callback("code=abc&state=S&client_id=oaiapp_1", "S", None).unwrap(),
            Callback { code: "abc".into(), client_id: Some("oaiapp_1".into()) }
        );
        // State mismatch wins over everything else, even an error parameter.
        assert!(parse_callback("error=access_denied&state=EVIL", "S", None).unwrap_err().contains("state"));
        assert!(parse_callback("error=access_denied&state=S", "S", None).unwrap_err().contains("cancelled"));
        assert!(parse_callback("code=abc&state=S", "S", None).unwrap_err().contains("client id"));
        // Reauthorization may omit the client id but must never switch to another one.
        assert!(parse_callback("code=abc&state=S", "S", Some("oaiapp_1")).is_ok());
        assert!(parse_callback("code=abc&state=S&client_id=oaiapp_2", "S", Some("oaiapp_1")).is_err());
        assert!(parse_callback("state=S&client_id=oaiapp_1", "S", None).unwrap_err().contains("authorization code"));
    }

    #[test]
    fn stream_parser_returns_the_completed_response() {
        let body = "event: response.created\ndata: {\"type\":\"response.created\"}\n\n\
                    data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hel\"}\n\n\
                    data: {\"type\":\"response.completed\",\"response\":{\"model\":\"m\",\"output\":[{\"type\":\"message\"}]}}\n\n\
                    data: [DONE]\n";
        let response = parse_responses_stream(body).unwrap();
        assert_eq!(response["model"], "m");
        assert_eq!(response["output"][0]["type"], "message");
    }

    #[test]
    fn stream_parser_maps_failures_to_clear_messages() {
        let limit = "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"subscription_sharing_usage_limit_exceeded\"}}}\n";
        assert!(parse_responses_stream(limit).unwrap_err().contains("usage limit"));
        let unsupported = "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"subscription_sharing_unsupported_capability\",\"param\":\"temperature\"}}}\n";
        assert!(parse_responses_stream(unsupported).unwrap_err().contains("temperature"));
        let incomplete = "data: {\"type\":\"response.incomplete\",\"response\":{\"incomplete_details\":{\"reason\":\"max_tokens\"}}}\n";
        assert!(parse_responses_stream(incomplete).unwrap_err().contains("max_tokens"));
        // A stream that just stops is an error, never a success.
        assert!(parse_responses_stream("data: {\"type\":\"response.output_text.delta\",\"delta\":\"x\"}\n").is_err());
    }

    #[test]
    fn http_failures_before_the_stream_are_explained() {
        assert!(describe_http_failure(401, "{\"detail\":\"x\"}").contains("Sign in again"));
        assert!(describe_http_failure(503, "").contains("temporarily"));
        assert!(describe_http_failure(429, "{\"error\":{\"code\":\"subscription_sharing_usage_limit_exceeded\"}}").contains("usage limit"));
    }

    #[test]
    fn model_list_keeps_only_listed_models() {
        let body = r#"{"models":[
            {"slug":"gpt-a","display_name":"GPT A","visibility":"list"},
            {"slug":"hidden","display_name":"Hidden","visibility":"hide"}]}"#;
        assert_eq!(parse_models(body).unwrap(), vec![("gpt-a".to_string(), "GPT A".to_string())]);
        assert_eq!(parse_models(r#"{"data":[{"id":"m1"}]}"#).unwrap()[0].0, "m1");
        assert!(parse_models(r#"{"models":[]}"#).is_err());
    }

    #[test]
    fn plan_scope_is_detected_from_the_granted_scope_string() {
        let with = TokenSet { access_token: "a".into(), refresh_token: None, id_token: None, expires_in: Some(3600), scope: Some("openid chatgpt.tokens.use.direct".into()) };
        let without = TokenSet { scope: Some("openid email".into()), ..with.clone() };
        assert!(with.has_plan_scope());
        assert!(!without.has_plan_scope());
    }

    fn endpoints_for(server: &MockServer) -> AuthEndpoints {
        AuthEndpoints {
            issuer: server.uri(),
            authorize: format!("{}/authorize", server.uri()),
            token: format!("{}/token", server.uri()),
            discovery: format!("{}/.well-known/openid-configuration", server.uri()),
        }
    }

    #[tokio::test]
    async fn code_exchange_sends_pkce_and_resource() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("client_id=oaiapp_1"))
            .and(body_string_contains("code_verifier=VER"))
            .and(body_string_contains("resource=https%3A%2F%2Fapi.openai.com%2Fv1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "AT", "refresh_token": "RT", "id_token": "IDT",
                "token_type": "Bearer", "expires_in": 3600,
                "scope": "openid chatgpt.tokens.use.direct"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let tokens = exchange_code(&reqwest::Client::new(), &endpoints_for(&server), "oaiapp_1", "CODE", "VER", "http://127.0.0.1:1455/auth/callback")
            .await
            .unwrap();
        assert_eq!(tokens.access_token, "AT");
        assert!(tokens.has_plan_scope());
    }

    #[tokio::test]
    async fn refresh_omits_scope_and_classifies_failures() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("refresh_token=GOOD"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "AT2", "refresh_token": "RT2", "expires_in": 3600
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("refresh_token=REUSED"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "refresh_token_reused"})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("refresh_token=FLAKY"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        let endpoints = endpoints_for(&server);
        let ok = refresh_tokens(&http, &endpoints, "oaiapp_1", "GOOD").await.unwrap();
        assert_eq!(ok.refresh_token.as_deref(), Some("RT2"));
        assert!(matches!(refresh_tokens(&http, &endpoints, "oaiapp_1", "REUSED").await, Err(AuthError::Terminal(_))));
        assert!(matches!(refresh_tokens(&http, &endpoints, "oaiapp_1", "FLAKY").await, Err(AuthError::Transient(_))));
        let requests = server.received_requests().await.unwrap();
        assert!(requests.iter().all(|r| !String::from_utf8_lossy(&r.body).contains("scope=")));
    }

    /// An ephemeral P-256 key made fresh for each test run; nothing secret is stored in the repo.
    struct TestKey {
        pkcs8: Vec<u8>,
        x: String,
        y: String,
    }

    fn test_key() -> TestKey {
        use ring::rand::SystemRandom;
        use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap();
        let public = pair.public_key().as_ref(); // 0x04 || x || y
        TestKey {
            pkcs8: pkcs8.as_ref().to_vec(),
            x: URL_SAFE_NO_PAD.encode(&public[1..33]),
            y: URL_SAFE_NO_PAD.encode(&public[33..65]),
        }
    }

    fn signed_id_token(key: &TestKey, issuer: &str, aud: &str, nonce: &str, exp_offset: i64) -> String {
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
        header.kid = Some("test-key".into());
        let claims = json!({
            "iss": issuer, "aud": aud, "sub": "user-123", "email": "me@example.com",
            "nonce": nonce, "exp": (Utc::now().timestamp() + exp_offset), "iat": Utc::now().timestamp(),
        });
        jsonwebtoken::encode(&header, &claims, &jsonwebtoken::EncodingKey::from_ec_der(&key.pkcs8)).unwrap()
    }

    async fn serve_jwks(server: &MockServer, key: &TestKey) {
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "issuer": server.uri(), "jwks_uri": format!("{}/jwks", server.uri()),
                "revocation_endpoint": format!("{}/revoke", server.uri()),
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"keys": [{
                "kty": "EC", "crv": "P-256", "kid": "test-key", "use": "sig", "alg": "ES256",
                "x": key.x, "y": key.y
            }]})))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn id_token_is_verified_against_the_published_keys() {
        let server = MockServer::start().await;
        let key = test_key();
        serve_jwks(&server, &key).await;
        let endpoints = endpoints_for(&server);
        let http = reqwest::Client::new();

        let good = signed_id_token(&key, &server.uri(), "oaiapp_1", "NONCE", 600);
        let claims = verify_id_token(&http, &endpoints, &good, "oaiapp_1", "NONCE").await.unwrap();
        assert_eq!(claims, IdentityClaims { sub: "user-123".into(), email: Some("me@example.com".into()) });

        // Wrong audience, nonce, issuer or expiry must each be rejected.
        let wrong_aud = signed_id_token(&key, &server.uri(), "oaiapp_OTHER", "NONCE", 600);
        assert!(verify_id_token(&http, &endpoints, &wrong_aud, "oaiapp_1", "NONCE").await.is_err());
        assert!(verify_id_token(&http, &endpoints, &good, "oaiapp_1", "DIFFERENT").await.unwrap_err().contains("nonce"));
        let wrong_iss = signed_id_token(&key, "https://evil.example", "oaiapp_1", "NONCE", 600);
        assert!(verify_id_token(&http, &endpoints, &wrong_iss, "oaiapp_1", "NONCE").await.is_err());
        let expired = signed_id_token(&key, &server.uri(), "oaiapp_1", "NONCE", -3600);
        assert!(verify_id_token(&http, &endpoints, &expired, "oaiapp_1", "NONCE").await.is_err());
        // A tampered payload fails the signature check.
        let mut parts: Vec<String> = good.split('.').map(String::from).collect();
        parts[1] = URL_SAFE_NO_PAD.encode(br#"{"iss":"x","aud":"oaiapp_1","sub":"admin","nonce":"NONCE","exp":9999999999}"#);
        assert!(verify_id_token(&http, &endpoints, &parts.join("."), "oaiapp_1", "NONCE").await.is_err());
    }

    #[tokio::test]
    async fn symmetric_algorithms_are_refused() {
        let server = MockServer::start().await;
        serve_jwks(&server, &test_key()).await;
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);
        header.kid = Some("test-key".into());
        let forged = jsonwebtoken::encode(
            &header,
            &json!({"iss": server.uri(), "aud": "oaiapp_1", "sub": "x", "nonce": "N", "exp": Utc::now().timestamp() + 600}),
            &jsonwebtoken::EncodingKey::from_secret(b"public-key-as-secret"),
        )
        .unwrap();
        let err = verify_id_token(&reqwest::Client::new(), &endpoints_for(&server), &forged, "oaiapp_1", "N").await.unwrap_err();
        assert!(err.contains("algorithm"), "{err}");
    }

    #[test]
    fn vault_round_trip_keeps_registration_after_tokens_are_cleared() {
        let vault = Vault::open_in_memory().unwrap();
        vault.init("correct horse").unwrap();
        let tokens = TokenSet {
            access_token: "AT".into(),
            refresh_token: Some("RT".into()),
            id_token: Some("ID".into()),
            expires_in: Some(3600),
            scope: Some("openid chatgpt.tokens.use.direct".into()),
        };
        save_host_id(&vault, "urn:uuid:host").unwrap();
        save_signed_in(&vault, "oaiapp_1", &IdentityClaims { sub: "u".into(), email: Some("a@b.c".into()) }, &tokens).unwrap();
        let stored = load(&vault).unwrap();
        assert_eq!(stored.client_id.as_deref(), Some("oaiapp_1"));
        assert_eq!(stored.refresh_token.as_deref(), Some("RT"));
        assert!(stored.plan_usage);
        assert_eq!(stored.host_id.as_deref(), Some("urn:uuid:host"));

        // A refresh stores the rotated token and keeps everything else.
        save_refreshed(&vault, &TokenSet { access_token: "AT2".into(), refresh_token: Some("RT2".into()), id_token: None, expires_in: Some(3600), scope: None }).unwrap();
        let rotated = load(&vault).unwrap();
        assert_eq!(rotated.refresh_token.as_deref(), Some("RT2"));
        assert_eq!(rotated.access_token.as_deref(), Some("AT2"));
        assert!(rotated.plan_usage, "scope unchanged when the refresh response omits it");

        clear_tokens(&vault).unwrap();
        let cleared = load(&vault).unwrap();
        assert!(cleared.refresh_token.is_none() && cleared.access_token.is_none() && !cleared.plan_usage);
        assert_eq!(cleared.client_id.as_deref(), Some("oaiapp_1"), "registration is kept for the next sign-in");
        assert_eq!(cleared.host_id.as_deref(), Some("urn:uuid:host"));
    }

    #[test]
    fn locked_vault_refuses_to_load() {
        let vault = Vault::open_in_memory().unwrap();
        assert!(load(&vault).is_err());
    }

    #[test]
    fn only_the_real_redirect_state_is_accepted() {
        assert!(query_has_state("code=abc&state=S1", "S1"));
        assert!(!query_has_state("code=abc&state=OTHER", "S1"));
        assert!(!query_has_state("code=abc", "S1"));
        assert!(!query_has_state("", "S1"));
    }

    #[tokio::test]
    async fn a_stray_local_request_cannot_abort_the_sign_in() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let cancel = Arc::new(Notify::new());
        let cancel_for_task = cancel.clone();
        let waiting = tokio::spawn(async move { wait_for_callback(listener, "GOODSTATE", &cancel_for_task).await });
        let http = reqwest::Client::new();
        // Wrong path, then the right path with the wrong state: both ignored.
        let stray = http.get(format!("http://127.0.0.1:{port}/favicon.ico")).send().await.unwrap();
        assert_eq!(stray.status(), 404);
        let forged = http
            .get(format!("http://127.0.0.1:{port}{CALLBACK_PATH}?code=evil&state=BAD"))
            .send()
            .await
            .unwrap();
        assert_eq!(forged.status(), 404);
        assert!(!waiting.is_finished());
        // The genuine redirect completes it.
        let ok = http
            .get(format!("http://127.0.0.1:{port}{CALLBACK_PATH}?code=real&state=GOODSTATE&client_id=oaiapp_1"))
            .send()
            .await
            .unwrap();
        assert_eq!(ok.status(), 200);
        let query = waiting.await.unwrap().unwrap();
        assert!(query.contains("code=real"));
    }

    #[test]
    fn account_hints_are_removed_before_a_url_is_shown() {
        let url = "https://auth.openai.com/api/accounts/authorize?client_id=oaiapp_1&state=S&id_token_hint=a.b.c&login_hint=me%40example.com&nonce=N";
        let shown = redact_authorize_url(url);
        assert!(!shown.contains("id_token_hint") && !shown.contains("login_hint") && !shown.contains("example.com"));
        assert!(shown.contains("client_id=oaiapp_1") && shown.contains("state=S") && shown.contains("nonce=N"));
        assert_eq!(redact_authorize_url("no-query"), "no-query");
    }

    #[tokio::test]
    async fn an_idle_connection_does_not_block_or_abort_the_sign_in() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let cancel = Arc::new(Notify::new());
        let cancel_for_task = cancel.clone();
        let waiting = tokio::spawn(async move { wait_for_callback(listener, "S", &cancel_for_task).await });
        // A browser preconnect: connect and say nothing.
        let _idle = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        // The genuine redirect still gets through immediately.
        let ok = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}{CALLBACK_PATH}?code=c&state=S&client_id=oaiapp_1"))
            .send()
            .await
            .unwrap();
        assert_eq!(ok.status(), 200);
        assert!(waiting.await.unwrap().unwrap().contains("code=c"));
    }

    #[tokio::test]
    async fn a_pending_sign_in_can_be_cancelled() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let cancel = Arc::new(Notify::new());
        let cancel_for_task = cancel.clone();
        let waiting = tokio::spawn(async move { wait_for_callback(listener, "S", &cancel_for_task).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.notify_waiters();
        assert!(waiting.await.unwrap().unwrap_err().contains("cancelled"));
    }

    #[tokio::test]
    async fn only_real_oauth_rejections_clear_the_login() {
        let server = MockServer::start().await;
        for (token, status, body) in [
            ("WAF", 403, json!({"detail": "challenge"})),
            ("TIMEOUT", 408, json!({})),
            ("NOCODE", 400, json!({"message": "bad"})),
            ("DEAD", 400, json!({"error": "invalid_grant"})),
            ("DEAD401", 401, json!({"error": "refresh_token_expired"})),
        ] {
            Mock::given(method("POST"))
                .and(path("/token"))
                .and(body_string_contains(format!("refresh_token={token}")))
                .respond_with(ResponseTemplate::new(status).set_body_json(body))
                .mount(&server)
                .await;
        }
        let http = reqwest::Client::new();
        let endpoints = endpoints_for(&server);
        for token in ["WAF", "TIMEOUT", "NOCODE"] {
            assert!(
                matches!(refresh_tokens(&http, &endpoints, "oaiapp_1", token).await, Err(AuthError::Transient(_))),
                "{token} must keep the stored tokens"
            );
        }
        for token in ["DEAD", "DEAD401"] {
            assert!(matches!(refresh_tokens(&http, &endpoints, "oaiapp_1", token).await, Err(AuthError::Terminal(_))), "{token}");
        }
    }

    #[test]
    fn the_rotated_refresh_token_is_written_before_the_access_token() {
        let vault = Vault::open_in_memory().unwrap();
        vault.init("correct horse").unwrap();
        vault.set_secret(VAULT_ID, "refresh_token", "OLD").unwrap();
        save_refreshed(
            &vault,
            &TokenSet { access_token: "AT".into(), refresh_token: Some("NEW".into()), id_token: None, expires_in: Some(3600), scope: None },
        )
        .unwrap();
        assert_eq!(vault.get_secret(VAULT_ID, "refresh_token").unwrap().as_deref(), Some("NEW"));
        assert_eq!(vault.get_secret(VAULT_ID, "access_token").unwrap().as_deref(), Some("AT"));
    }
}
