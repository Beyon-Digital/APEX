//! Chat modes for the AI copilot: how much it may do on its own.
//!
//! * **Ask**: read-only. Write tools are not even offered to the model, and a call that gets
//!   through anyway is refused.
//! * **Review**: every write-class call pauses for the user's approval (the original behaviour).
//! * **Agent**: write-class calls run without asking so the copilot can carry a task through
//!   (write a strategy, backtest it, iterate). Anything that involves a live (non-paper) broker
//!   still pauses for approval, and the live-trading guard still applies underneath.
//!
//! The mode is chosen by the user in the chat panel and enforced here on the backend; the model
//! cannot change it.

use super::{copilot, copilot_more, copilot_tools};
use crate::commands::python_runtime::RuntimePaths;
use crate::state::AppState;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatMode {
    Ask,
    Review,
    Agent,
}

/// Tools that change state or run code, beyond the lists owned by the other copilot modules.
const OTHER_WRITE_TOOLS: &[&str] = &["stop_strategy", "add_to_watchlist", "remove_from_watchlist"];

impl ChatMode {
    /// The requested mode, or the configured default when none was sent. An unknown string is an
    /// error rather than a silent fallback, so a typo can never grant more freedom than intended.
    pub fn resolve(requested: Option<&str>, require_approval_default: bool) -> Result<Self, String> {
        match requested.map(|m| m.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") => Ok(if require_approval_default { Self::Review } else { Self::Agent }),
            Some("ask") => Ok(Self::Ask),
            Some("review") => Ok(Self::Review),
            Some("agent") => Ok(Self::Agent),
            Some(other) => Err(format!("unknown copilot mode `{other}` (expected ask, review or agent)")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Review => "review",
            Self::Agent => "agent",
        }
    }

    /// What the model is told about its own freedom.
    pub fn prompt_note(self) -> &'static str {
        match self {
            Self::Ask => {
                "MODE: ASK. You are read-only. Only read tools are available and nothing you do changes anything. \
                 If the user wants something done, say exactly what you would do and tell them to switch to Review or Agent mode."
            }
            Self::Review => {
                "MODE: REVIEW. Write-class tools (orders, strategies, automations, alerts, training, notebooks, data-feed pins) \
                 pause for the user's approval; say what you are about to do and wait. Do not retry a call the user declined."
            }
            Self::Agent => {
                "MODE: AGENT. Carry the task through on your own: call tools, check each result and keep going until the job is done. \
                 Research and paper-trading actions run without asking. Anything involving a live (non-paper) broker still pauses \
                 for the user's approval: propose it clearly and wait. Do not retry a call the user declined."
            }
        }
    }

    /// How many model/tool rounds one message may take.
    pub fn max_rounds(self) -> usize {
        match self {
            Self::Ask | Self::Review => 10,
            Self::Agent => 25,
        }
    }
}

/// Whether a tool changes state or runs code.
pub fn is_write_tool(name: &str) -> bool {
    copilot::WRITE_TOOLS.contains(&name)
        || copilot_tools::EXTRA_WRITE_TOOLS.contains(&name)
        || copilot_more::WRITE_TOOLS.contains(&name)
        || OTHER_WRITE_TOOLS.contains(&name)
}

/// Keep only the tool definitions this mode may use (Ask hides every write tool).
pub fn tools_for_mode(all: Value, mode: ChatMode) -> Value {
    if mode != ChatMode::Ask {
        return all;
    }
    Value::Array(
        all.as_array()
            .map(|tools| {
                tools
                    .iter()
                    .filter(|t| {
                        t.pointer("/function/name")
                            .and_then(|n| n.as_str())
                            .map(|n| !is_write_tool(n))
                            .unwrap_or(true)
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// Whether this call touches a live (non-paper) broker. `automation_broker` maps an automation
/// id to its broker, for the tools that act on an existing rule.
pub fn live_involved(name: &str, args: &Value, automation_broker: &dyn Fn(&str) -> Option<String>) -> bool {
    let live = |broker: Option<&str>| broker.map(|b| b != "paper").unwrap_or(false);
    let arg_broker = || args.get("broker_id").and_then(|v| v.as_str()).unwrap_or("paper").to_string();
    match name {
        "place_order" | "cancel_order" | "modify_order" | "start_strategy" | "create_automation" => {
            live(Some(&arg_broker()))
        }
        "set_automation_enabled" | "delete_automation" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
            live(automation_broker(id).as_deref())
        }
        _ => false,
    }
}

/// Whether the call must pause for the user's approval under `mode`.
pub fn requires_approval(mode: ChatMode, name: &str, args: &Value, state: &AppState, paths: &RuntimePaths) -> bool {
    match mode {
        // Writes are refused in Ask mode before this is consulted.
        ChatMode::Ask => false,
        ChatMode::Review => copilot::WRITE_TOOLS.contains(&name) || copilot_tools::needs_approval(name, args, state, paths),
        ChatMode::Agent => {
            let broker_of = |id: &str| {
                state
                    .automations
                    .list()
                    .iter()
                    .find(|rule| rule.id == id)
                    .map(|rule| rule.broker_id.clone())
            };
            live_involved(name, args, &broker_of)
                // Stopping a live session closes its position at market.
                || (name == "stop_strategy" && copilot_tools::needs_approval(name, args, state, paths))
        }
    }
}

/// Whether a call must be refused outright because the mode is read-only.
pub fn refused_in_ask(name: &str, args: &Value, state: &AppState, paths: &RuntimePaths) -> bool {
    is_write_tool(name) || copilot_tools::needs_approval(name, args, state, paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn none(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn mode_resolution_defaults_follow_config_and_typos_are_errors() {
        assert_eq!(ChatMode::resolve(Some("ASK"), false).unwrap(), ChatMode::Ask);
        assert_eq!(ChatMode::resolve(Some(" agent "), true).unwrap(), ChatMode::Agent);
        assert_eq!(ChatMode::resolve(Some("review"), false).unwrap(), ChatMode::Review);
        assert_eq!(ChatMode::resolve(None, true).unwrap(), ChatMode::Review);
        assert_eq!(ChatMode::resolve(None, false).unwrap(), ChatMode::Agent);
        assert_eq!(ChatMode::resolve(Some(""), true).unwrap(), ChatMode::Review);
        assert!(ChatMode::resolve(Some("autonomous"), true).is_err());
        assert!(ChatMode::resolve(Some("agnet"), true).is_err());
    }

    #[test]
    fn agent_gets_more_rounds_than_the_others() {
        assert!(ChatMode::Agent.max_rounds() > ChatMode::Review.max_rounds());
        assert_eq!(ChatMode::Ask.max_rounds(), ChatMode::Review.max_rounds());
    }

    #[test]
    fn ask_mode_hides_every_write_tool_and_keeps_the_reads() {
        let all = Value::Array(copilot_tools::extra_tool_schemas());
        let ask = tools_for_mode(all.clone(), ChatMode::Ask);
        let names = |v: &Value| -> Vec<String> {
            v.as_array().unwrap().iter().filter_map(|t| t["function"]["name"].as_str().map(String::from)).collect()
        };
        for name in names(&ask) {
            assert!(!is_write_tool(&name), "{name} is a write tool but is offered in Ask mode");
        }
        let ask_names = names(&ask);
        assert!(ask_names.contains(&"web_fetch".to_string()));
        assert!(ask_names.contains(&"get_system_status".to_string()));
        assert!(ask_names.contains(&"kill_switch".to_string()), "the safety stop stays available");
        for write in ["start_strategy", "modify_order", "write_notebook", "set_symbol_provider", "run_notebook_cell"] {
            assert!(names(&all).contains(&write.to_string()), "{write} should exist in the full set");
            assert!(!ask_names.contains(&write.to_string()), "{write} must be hidden in Ask");
        }
        // Review and Agent see everything.
        assert_eq!(tools_for_mode(all.clone(), ChatMode::Agent), all);
        assert_eq!(tools_for_mode(all.clone(), ChatMode::Review), all);
    }

    #[test]
    fn write_tools_are_classified() {
        for t in ["place_order", "cancel_order", "save_strategy", "train_ml_model", "create_alert", "remove_alert",
                  "create_automation", "start_strategy", "stop_strategy", "modify_order", "add_to_watchlist",
                  "write_notebook", "run_notebook_cell", "set_symbol_provider", "export_bars_csv"] {
            assert!(is_write_tool(t), "{t} should be a write tool");
        }
        for t in ["get_quote", "web_fetch", "compute_indicator", "read_notebook", "list_notebooks", "kill_switch",
                  "run_backtest", "get_system_status"] {
            assert!(!is_write_tool(t), "{t} should not be a write tool");
        }
    }

    #[test]
    fn live_orders_and_strategies_are_flagged_but_paper_is_not() {
        for tool in ["place_order", "cancel_order", "modify_order", "start_strategy", "create_automation"] {
            assert!(live_involved(tool, &json!({"broker_id": "zerodha"}), &none), "{tool} on zerodha");
            assert!(!live_involved(tool, &json!({"broker_id": "paper"}), &none), "{tool} on paper");
            assert!(!live_involved(tool, &json!({}), &none), "{tool} defaults to paper");
        }
        assert!(!live_involved("save_strategy", &json!({"broker_id": "zerodha"}), &none));
        assert!(!live_involved("get_quote", &json!({"broker_id": "zerodha"}), &none));
    }

    #[test]
    fn changing_an_existing_automation_is_judged_by_its_broker() {
        let lookup = |id: &str| match id {
            "live-rule" => Some("zerodha".to_string()),
            "paper-rule" => Some("paper".to_string()),
            _ => None,
        };
        assert!(live_involved("set_automation_enabled", &json!({"id": "live-rule", "enabled": true}), &lookup));
        assert!(live_involved("delete_automation", &json!({"id": "live-rule"}), &lookup));
        assert!(!live_involved("set_automation_enabled", &json!({"id": "paper-rule"}), &lookup));
        assert!(!live_involved("delete_automation", &json!({"id": "missing"}), &lookup));
    }

    #[test]
    fn every_mode_tells_the_model_its_limits() {
        assert!(ChatMode::Ask.prompt_note().contains("read-only"));
        assert!(ChatMode::Review.prompt_note().contains("approval"));
        let agent = ChatMode::Agent.prompt_note();
        assert!(agent.contains("live") && agent.contains("approval"));
    }
}
