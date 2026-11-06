use crate::dto::RiskStatusDto;
use crate::state::AppState;
use tauri::State;

/// Get current risk status.
#[tauri::command]
pub async fn get_risk_status(state: State<'_, AppState>) -> Result<RiskStatusDto, String> {
    Ok(RiskStatusDto {
        session_pnl: state.risk.session_pnl(),
        is_halted: state.risk.is_halted(),
        max_daily_loss: state.risk.config().max_daily_loss,
    })
}

/// Reset the trading halt (explicit UI action only).
#[tauri::command]
pub async fn reset_halt(state: State<'_, AppState>) -> Result<(), String> {
    reset_halt_as(&state, "user");
    Ok(())
}

/// Reset the halt and write the `risk.reset_halt` activity entry: who did it, when, and the session
/// P&L and loss limit at that moment. Recorded even when nothing was halted.
pub(crate) fn reset_halt_as(state: &AppState, actor: &str) {
    let was_halted = state.risk.is_halted();
    let pnl = state.risk.session_pnl();
    state.risk.reset_halt();
    // Resume algos that the kill switch paused; they were stopped for safety.
    let was_paused = state
        .automations_paused
        .swap(false, std::sync::atomic::Ordering::SeqCst);
    crate::activity::record(
        state,
        actor,
        "risk.reset_halt",
        "system",
        "risk",
        if was_halted { "Trading halt reset" } else { "Trading halt reset (trading was not halted)" },
        true,
        Some(format!(
            "was_halted={was_halted} automations_resumed={was_paused} session_pnl={pnl:.2} max_daily_loss={}",
            state.risk.config().max_daily_loss
        )),
    );
}
