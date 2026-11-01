mod broker_runtime;
mod commands;
mod config;
mod dto;
mod live_guard;
mod state;
mod tracing_setup;
mod validation;
mod vault;

use commands::strategy;
use commands::vault as vault_cmd;
use commands::{
    adapters, alerts, automation, brokers, copilot, data, graph, health, live, market, ml, news, notebook,
    orderbook, orders, quant, risk, scanner, settings,
};
use tauri::Manager;

fn main() {
    tracing_setup::init();

    tracing::info!("APEX Terminal starting...");

    tauri::Builder::default()
        .setup(|app| {
            let app_handle = app.handle().clone();
            let runtime_paths = commands::python_runtime::RuntimePaths::resolve(&app_handle)
                .expect("Runtime path resolution failed");
            let app_state_paths = runtime_paths.clone();

            tauri::async_runtime::block_on(async move {
                let app_state = state::AppState::init(app_state_paths)
                    .await
                    .expect("App state initialization failed");

                tracing::info!("App state initialized successfully");
                tracing::info!(
                    "Risk engine: max_daily_loss = {}, halted = {}",
                    app_state.risk.config().max_daily_loss,
                    app_state.risk.is_halted()
                );

                // Start real-time event push from message bus → frontend
                app_state.start_event_push(app_handle.clone());

                app_handle.manage(app_state);
            });

            app.manage(runtime_paths.clone());

            // Register ML model registry state
            app.manage(ml::ModelRegistry::new(&runtime_paths));

            // Background scheduler for automation rules
            automation::spawn_automation_loop(&app.handle());

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            market::get_quote,
            market::get_ohlcv,
            market::subscribe_symbols,
            market::unsubscribe_symbols,
            market::search_symbols,
            orders::get_holdings,
            orders::place_order,
            orders::preview_order,
            live::get_live_status,
            live::set_live_armed,
            live::kill_switch,
            orders::cancel_order,
            orders::modify_order,
            orders::get_positions,
            orders::get_open_orders,
            orders::get_orders,
            orders::get_account_balance,
            alerts::add_alert,
            alerts::remove_alert,
            alerts::get_alert_rules,
            risk::get_risk_status,
            risk::reset_halt,
            data::get_historical_data,
            data::get_watchlist_symbols,
            quant::compute_indicator,
            quant::get_quant_stats,
            quant::get_regression,
            news::get_news,
            news::search_news,
            news::list_news_feeds,
            scanner::run_scan,
            graph::get_graph,
            graph::compute_correlations,
            orderbook::get_order_book,
            copilot::copilot_chat,
            copilot::approve_copilot_tools,
            strategy::list_strategy_files,
            strategy::create_strategy_file,
            strategy::save_strategy_file,
            strategy::delete_strategy_file,
            strategy::run_strategy_file,
            commands::strategy_worker::stop_strategy_file,
            commands::strategy_worker::list_strategy_sessions,
            strategy::run_strategy_backtest,
            ml::list_ml_models,
            ml::train_ml_model,
            ml::delete_ml_model,
            ml::predict_model_signal,
            automation::create_automation,
            automation::list_automations,
            automation::delete_automation,
            automation::set_automation_enabled,
            brokers::list_broker_connections,
            brokers::set_broker_session,
            brokers::clear_broker_session,
            brokers::zerodha_login,
            brokers::zerodha_login_url,
            vault_cmd::vault_status,
            vault_cmd::vault_init,
            vault_cmd::vault_unlock,
            vault_cmd::vault_lock,
            vault_cmd::vault_change_passphrase,
            vault_cmd::import_env_credentials,
            adapters::list_adapter_schemas,
            adapters::get_adapter_configs,
            adapters::save_adapter_config,
            adapters::set_adapter_enabled,
            adapters::delete_adapter_config,
            adapters::test_adapter_connection,
            health::get_system_health,
            settings::get_app_settings,
            settings::save_app_settings,
            notebook::list_notebooks,
            notebook::load_notebook,
            notebook::create_notebook,
            notebook::save_notebook,
            notebook::run_notebook_cell,
        ])
        .run(tauri::generate_context!())
        .expect("error while running APEX Terminal");
}
