use crate::chatgpt_auth;
use crate::state::AppState;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::State;

#[derive(Debug, Clone, Serialize)]
pub struct ChatGptStatusDto {
    /// "locked" | "signed_out" | "waiting" | "connected" | "error"
    pub state: String,
    pub email: Option<String>,
    pub plan_usage: bool,
    pub message: Option<String>,
    /// The copilot is set to use the ChatGPT plan.
    pub use_for_copilot: bool,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatGptModelDto {
    pub slug: String,
    pub name: String,
}

fn status_dto(state: &AppState) -> ChatGptStatusDto {
    let s = chatgpt_auth::status(state);
    let prefs = chatgpt_auth::preferences(&state.vault);
    ChatGptStatusDto {
        state: s.state,
        email: s.email,
        plan_usage: s.plan_usage,
        message: s.message,
        use_for_copilot: prefs.use_for_copilot,
        model: prefs.model,
    }
}

/// A sign-in was started and its outcome (connected, failed, cancelled) is not recorded yet.
pub(crate) static SIGN_IN_PENDING: AtomicBool = AtomicBool::new(false);

/// The part of an account email after the last `@`, lowercased: enough to tell which account kind
/// signed in, without recording who. `None` when there is no usable domain.
pub(crate) fn email_domain(email: &str) -> Option<String> {
    let domain = email.rsplit_once('@')?.1.trim().to_ascii_lowercase();
    (!domain.is_empty() && domain.len() <= 100 && domain.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')).then_some(domain)
}

fn account_text(email: Option<&str>) -> String {
    match email.and_then(email_domain) {
        Some(domain) => format!(" (account domain {domain})"),
        None => String::new(),
    }
}

/// What a status poll after a browser sign-in says happened: (summary, ok, details), or `None`
/// while it is still going. `sign_in_error` is the failure message of the sign-in itself: when a
/// refresh token from an earlier sign-in is stored the status stays `connected` after a failed
/// attempt, so the status alone cannot tell a failed re-sign-in from a completed one.
fn sign_in_outcome(status: &chatgpt_auth::ChatGptStatus, sign_in_error: Option<&str>) -> Option<(String, bool, Option<String>)> {
    let failed = |message: Option<&str>| ("ChatGPT sign-in failed".to_string(), false, message.map(|m| crate::commands::settings::scrub_error(m, &[])));
    Some(match status.state.as_str() {
        "waiting" => return None,
        "connected" => match sign_in_error {
            Some(message) => failed(Some(message)),
            None => (format!("ChatGPT sign-in completed{}", account_text(status.email.as_deref())), true, None),
        },
        "error" => failed(status.message.as_deref()),
        "signed_out" => ("ChatGPT sign-in ended without signing in".to_string(), false, None),
        _ => return None,
    })
}

/// The browser sign-in finishes in the background, so its outcome is recorded the first time the
/// status is looked at after it ended: `chatgpt.sign_in` with the account's email domain only.
pub(crate) fn note_sign_in_outcome(state: &AppState, status: &chatgpt_auth::ChatGptStatus) {
    if !SIGN_IN_PENDING.load(Ordering::SeqCst) {
        return;
    }
    let error = chatgpt_auth::sign_in_error(state);
    let Some((summary, ok, details)) = sign_in_outcome(status, error.as_deref()) else { return };
    if SIGN_IN_PENDING.compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
        crate::activity::record(state, "user", "chatgpt.sign_in", "system", "chatgpt", &summary, ok, details);
    }
}

#[tauri::command]
pub async fn chatgpt_status(state: State<'_, AppState>) -> Result<ChatGptStatusDto, String> {
    note_sign_in_outcome(&state, &chatgpt_auth::status(&state));
    Ok(status_dto(&state))
}

/// Start "Sign in with ChatGPT": opens the browser and returns the URL so the UI
/// can show it if the browser did not open.
#[tauri::command]
pub async fn chatgpt_sign_in(state: State<'_, AppState>) -> Result<String, String> {
    let url = match chatgpt_auth::begin_sign_in(&state).await {
        Ok(url) => url,
        Err(error) => {
            crate::activity::record(
                &state,
                "user",
                "chatgpt.sign_in",
                "system",
                "chatgpt",
                "ChatGPT sign-in could not start",
                false,
                Some(crate::commands::settings::scrub_error(&error, &[])),
            );
            return Err(error);
        }
    };
    SIGN_IN_PENDING.store(true, Ordering::SeqCst);
    crate::activity::record(&state, "user", "chatgpt.sign_in", "system", "chatgpt", "ChatGPT sign-in started in the browser", true, None);
    if let Err(error) = open_in_browser(&url) {
        tracing::warn!(%error, "Could not open the system browser for ChatGPT sign-in");
    }
    // The browser gets the full URL; anything shown in the UI has account hints removed.
    Ok(chatgpt_auth::redact_authorize_url(&url))
}

/// Abandon a pending browser sign-in.
#[tauri::command]
pub async fn chatgpt_cancel_sign_in(state: State<'_, AppState>) -> Result<ChatGptStatusDto, String> {
    chatgpt_auth::cancel_sign_in(&state);
    if SIGN_IN_PENDING.swap(false, Ordering::SeqCst) {
        crate::activity::record(&state, "user", "chatgpt.sign_in", "system", "chatgpt", "ChatGPT sign-in cancelled", false, None);
    }
    Ok(status_dto(&state))
}

#[tauri::command]
pub async fn chatgpt_sign_out(state: State<'_, AppState>) -> Result<String, String> {
    sign_out_audited(&state, "user").await
}

/// Sign out and write the `chatgpt.sign_out` activity entry (the account's email domain only).
pub(crate) async fn sign_out_audited(state: &AppState, actor: &str) -> Result<String, String> {
    let account = account_text(chatgpt_auth::status(state).email.as_deref());
    let result = async {
        let note = chatgpt_auth::sign_out(state).await?;
        chatgpt_auth::set_preferences(&state.vault, Some(false), None)?;
        Ok::<String, String>(note)
    }
    .await;
    match &result {
        Ok(_) => crate::activity::record(state, actor, "chatgpt.sign_out", "system", "chatgpt", &format!("ChatGPT signed out{account}"), true, None),
        Err(error) => crate::activity::record(
            state,
            actor,
            "chatgpt.sign_out",
            "system",
            "chatgpt",
            "ChatGPT sign-out failed",
            false,
            Some(crate::commands::settings::scrub_error(error, &[])),
        ),
    }
    result
}

#[tauri::command]
pub async fn chatgpt_list_models(state: State<'_, AppState>) -> Result<Vec<ChatGptModelDto>, String> {
    Ok(chatgpt_auth::list_models(&state)
        .await?
        .into_iter()
        .map(|(slug, name)| ChatGptModelDto { slug, name })
        .collect())
}

/// Choose whether the copilot runs on the ChatGPT plan, and which model.
#[tauri::command]
pub async fn chatgpt_set_preferences(
    use_for_copilot: Option<bool>,
    model: Option<String>,
    state: State<'_, AppState>,
) -> Result<ChatGptStatusDto, String> {
    if use_for_copilot == Some(true) {
        let s = chatgpt_auth::status(&state);
        if s.state != "connected" || !s.plan_usage {
            return Err("Sign in with ChatGPT (and allow plan usage) first.".into());
        }
    }
    if let Some(model) = &model {
        crate::validation::validate_string_length(model, "model")?;
    }
    chatgpt_auth::set_preferences(&state.vault, use_for_copilot, model)?;
    Ok(status_dto(&state))
}

/// Hosts the app may open in the user's browser. Anything else is refused so a
/// compromised page cannot launch arbitrary URLs.
const OPENABLE_HOSTS: &[&str] = &["auth.openai.com", "kite.zerodha.com", "chatgpt.com"];

fn openable(url: &str) -> bool {
    // Whitespace, quotes and control characters have no place in these URLs and are
    // what shells and argument parsers trip over.
    if url.len() > 4096
        || url
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>' | '^' | '|' | '\\'))
    {
        return false;
    }
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    !host.contains('@') && !host.contains(':') && OPENABLE_HOSTS.contains(&host)
}

fn open_in_browser(url: &str) -> Result<(), String> {
    if !openable(url) {
        return Err("That address is not allowed to be opened.".into());
    }
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    // Never go through cmd.exe: `&` in a query string would be run as a second command.
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");
    command.arg(url).spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Open an allowlisted https page in the system browser.
#[tauri::command]
pub async fn open_external_url(url: String) -> Result<(), String> {
    open_in_browser(&url)
}

#[cfg(test)]
mod tests {
    use super::{openable, sign_in_outcome};
    use crate::chatgpt_auth::ChatGptStatus;

    fn status(state: &str, email: Option<&str>) -> ChatGptStatus {
        ChatGptStatus { state: state.into(), email: email.map(String::from), plan_usage: true, message: None }
    }

    #[test]
    fn a_failed_re_sign_in_is_not_recorded_as_completed() {
        // Already signed in (refresh token stored): the status stays connected after a failed attempt.
        let (summary, ok, details) = sign_in_outcome(&status("connected", Some("a@b.com")), Some("State mismatch")).unwrap();
        assert!(!ok, "{summary}");
        assert!(summary.contains("failed"), "{summary}");
        assert_eq!(details.as_deref(), Some("State mismatch"));
        // A real completion still is one, and a pending sign-in records nothing yet.
        let (summary, ok, _) = sign_in_outcome(&status("connected", Some("a@b.com")), None).unwrap();
        assert!(ok && summary.contains("completed") && summary.contains("b.com"), "{summary}");
        assert!(sign_in_outcome(&status("waiting", None), None).is_none());
        // First sign-in with no stored token: the status itself says error.
        let mut failed = status("error", None);
        failed.message = Some("Timed out".into());
        let (_, ok, details) = sign_in_outcome(&failed, None).unwrap();
        assert!(!ok);
        assert_eq!(details.as_deref(), Some("Timed out"));
    }

    #[test]
    fn only_allowlisted_https_hosts_open() {
        assert!(openable("https://auth.openai.com/api/accounts/authorize?client_id=x"));
        assert!(openable("https://kite.zerodha.com/connect/login?v=3&api_key=k"));
        assert!(!openable("http://auth.openai.com/x"));
        assert!(!openable("https://evil.example/auth.openai.com"));
        assert!(!openable("https://auth.openai.com@evil.example/"));
        assert!(!openable("file:///etc/passwd"));
        assert!(!openable("javascript:alert(1)"));
        // Characters a Windows shell would treat as commands or separators.
        assert!(!openable("https://auth.openai.com/?x&calc.exe b"));
        assert!(!openable("https://auth.openai.com/?x|calc"));
        assert!(!openable("https://auth.openai.com/?x^y"));
        assert!(!openable("https://auth.openai.com/\"quoted"));
        assert!(!openable("https://auth.openai.com:8443/"));
        // The real URLs, which legitimately contain & and %, still open.
        assert!(openable("https://auth.openai.com/api/accounts/authorize?client_id=a&redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback&state=S"));
    }
}
