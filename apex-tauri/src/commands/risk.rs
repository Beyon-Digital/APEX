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
    state.risk.reset_halt();
    // Resume algos that the kill switch paused; they were stopped for safety.
    state
        .automations_paused
        .store(false, std::sync::atomic::Ordering::SeqCst);
    Ok(())
}
