use crate::chatgpt_auth;
use crate::state::AppState;
use serde::Serialize;
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

#[tauri::command]
pub async fn chatgpt_status(state: State<'_, AppState>) -> Result<ChatGptStatusDto, String> {
    Ok(status_dto(&state))
}

/// Start "Sign in with ChatGPT": opens the browser and returns the URL so the UI
/// can show it if the browser did not open.
#[tauri::command]
pub async fn chatgpt_sign_in(state: State<'_, AppState>) -> Result<String, String> {
    let url = chatgpt_auth::begin_sign_in(&state).await?;
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
    Ok(status_dto(&state))
}

#[tauri::command]
pub async fn chatgpt_sign_out(state: State<'_, AppState>) -> Result<String, String> {
    let note = chatgpt_auth::sign_out(&state).await?;
    chatgpt_auth::set_preferences(&state.vault, Some(false), None)?;
    Ok(note)
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
    use super::openable;

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
