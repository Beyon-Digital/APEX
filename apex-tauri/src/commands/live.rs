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
    set_live_armed_as(&state, armed, "user")?;
    Ok(status(&state))
}

/// Arm or disarm and write the `live.arm` / `live.disarm` activity entry (the order trail gets its
/// own `live_armed` / `live_disarmed` event from `set_live_armed_audited`). A refusal is an entry too.
pub(crate) fn set_live_armed_as(state: &AppState, armed: bool, actor: &str) -> Result<(), String> {
    let action = if armed { "live.arm" } else { "live.disarm" };
    let was_armed = state.live_guard.is_armed();
    let result = (|| {
        if armed {
            if state.risk.is_halted() {
                return Err("Trading is halted by the risk engine. Reset the halt first.".to_string());
            }
            if !state.vault.is_unlocked() {
                return Err("Unlock the vault before arming live trading.".to_string());
            }
        }
        state.set_live_armed_audited(armed, actor, "ui")
    })();
    let caps = format!(
        "max_orders_per_day={} max_qty_per_order={}",
        state.live_guard.max_orders_per_day(),
        state.live_guard.max_qty_per_order()
    );
    match &result {
        Ok(()) if armed == was_armed => crate::activity::record(
            state, actor, action, "system", "live", if armed { "Live trading was already armed" } else { "Live trading was already disarmed" }, true, Some(caps),
        ),
        Ok(()) => crate::activity::record(
            state, actor, action, "system", "live", if armed { "Live trading armed" } else { "Live trading disarmed" }, true, Some(caps),
        ),
        Err(error) => crate::activity::record(
            state,
            actor,
            action,
            "system",
            "live",
            if armed { "Live trading was not armed" } else { "Live trading was not disarmed" },
            false,
            Some(crate::commands::settings::scrub_error(error, &[])),
        ),
    }
    result
}

/// Emergency stop: disarm, halt trading and cancel all working live orders.
#[tauri::command]
pub async fn kill_switch(state: State<'_, AppState>) -> Result<KillSwitchResultDto, String> {
    let (cancelled, failed) = state.kill_switch_as("user").await;
    crate::activity::record(
        &state,
        "user",
        "system.kill_switch",
        "system",
        "live",
        "Kill switch used: live trading disarmed, trading halted",
        failed.is_empty(),
        Some(format!("cancelled={} failed_to_cancel={}", cancelled.len(), failed.len())),
    );
    Ok(KillSwitchResultDto { cancelled, failed })
}
