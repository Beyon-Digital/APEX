use crate::dto::{KillSwitchResultDto, LiveStatusDto};
use crate::state::AppState;
use tauri::State;

fn status(state: &AppState) -> LiveStatusDto {
    LiveStatusDto {
        armed: state.live_guard.is_armed(),
        orders_today: state.live_guard.orders_today(),
        max_orders_per_day: state.live_guard.max_orders_per_day(),
        max_qty_per_order: state.live_guard.max_qty_per_order(),
        halted: state.risk.is_halted(),
    }
}

#[tauri::command]
pub async fn get_live_status(state: State<'_, AppState>) -> Result<LiveStatusDto, String> {
    Ok(status(&state))
}

/// Arm or disarm live trading. Arming is refused while the risk engine is
/// halted or the vault is locked (no live adapter can be configured then).
#[tauri::command]
pub async fn set_live_armed(
    armed: bool,
    state: State<'_, AppState>,
) -> Result<LiveStatusDto, String> {
    if armed {
        if state.risk.is_halted() {
            return Err("Trading is halted by the risk engine. Reset the halt first.".into());
        }
        if !state.vault.is_unlocked() {
            return Err("Unlock the vault before arming live trading.".into());
        }
    }
    state.live_guard.set_armed(armed);
    Ok(status(&state))
}

/// Emergency stop: disarm, halt trading and cancel all working live orders.
#[tauri::command]
pub async fn kill_switch(state: State<'_, AppState>) -> Result<KillSwitchResultDto, String> {
    let (cancelled, failed) = state.kill_switch().await;
    Ok(KillSwitchResultDto { cancelled, failed })
}
