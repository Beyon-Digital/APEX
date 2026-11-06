//! What an automation does with a model's class: the class-to-position mapping, the position
//! state machine, and the live-shorting gates. Everything here is pure so it can be tested
//! without an app, a broker or a model.
//!
//! Class meaning
//! - label kind `direction` (0/1): 1 = long, 0 = short if the rule allows shorting, else flat.
//! - label kind `return_threshold` (-1/0/1): 1 = long, -1 = short if allowed else flat, 0 = flat.
//! - no label spec (older models): 1 = long, 0 = flat, -1 = short if allowed else flat.
//!
//! Closing a short is a BUY (a "cover"), and closing a long is a SELL. The state machine never
//! reverses in one order: while a position is open and the desired side differs, it sends one
//! closing order. The opening order follows on a later run, once the position is flat and no
//! order is still working.

use std::path::Path;

use apex_core::domain::models::{BrokerOrderParams, OrderSide};
use serde_json::Value;

/// The position an automation wants to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Long,
    Short,
    Flat,
}

/// Position the model class asks for, or `None` for a class this policy does not know (the rule
/// then does nothing, rather than guessing).
pub fn target_position(label_kind: Option<&str>, signal: i64, allow_short: bool) -> Option<Target> {
    let bearish = if allow_short { Target::Short } else { Target::Flat };
    match signal {
        1 => Some(Target::Long),
        // `direction` models only have 0/1, where 0 is "down". Every other label kind
        // (return_threshold, or an older model with no spec) uses 0 for "no clear move".
        0 => Some(match label_kind {
            Some("direction") => bearish,
            _ => Target::Flat,
        }),
        -1 => Some(bearish),
        _ => None,
    }
}

/// The label kind recorded in a model's metadata JSON (`label.kind`). `None` for older files
/// that have no label, for a missing or malformed file, and for any unexpected shape.
pub fn label_kind_from_metadata(metadata_json: &str) -> Option<String> {
    let doc: Value = serde_json::from_str(metadata_json).ok()?;
    doc.get("label")?
        .get("kind")?
        .as_str()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string)
}

/// Read a model's label kind from `<models_dir>/<model_id>.json`. Tolerates a missing file,
/// an invalid id, and old metadata without a label.
pub fn read_label_kind(models_dir: &Path, model_id: &str) -> Option<String> {
    crate::commands::ml::validate_model_id(model_id).ok()?;
    let text = std::fs::read_to_string(models_dir.join(format!("{model_id}.json"))).ok()?;
    label_kind_from_metadata(&text)
}

/// Whether an automation may short through `broker_id`. Paper always can. A live broker needs
/// `[automations] allow_live_shorting` and an adapter that supports it; only Zerodha does (via
/// the MIS intraday product, since CNC delivery cannot be sold short). Every other adapter is
/// treated conservatively as unable.
pub fn broker_can_short(broker_id: &str, allow_live_shorting: bool) -> bool {
    broker_id == "paper" || (allow_live_shorting && adapter_supports_shorting(broker_id))
}

/// Whether the live adapter can send a sell without holding the instrument.
pub fn adapter_supports_shorting(broker_id: &str) -> bool {
    matches!(broker_id, "zerodha")
}

/// What an order is for. The broker side is the same for `Sell` and `Short` (a sell), and for
/// `Buy` and `Cover` (a buy); the intent decides the margin product and the wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Open a long.
    Buy,
    /// Close a long.
    Sell,
    /// Open a short.
    Short,
    /// Close a short. This is a buy.
    Cover,
}

impl Intent {
    pub fn side(self) -> OrderSide {
        match self {
            Intent::Buy | Intent::Cover => OrderSide::Buy,
            Intent::Sell | Intent::Short => OrderSide::Sell,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Intent::Buy => "BUY",
            Intent::Sell => "SELL",
            Intent::Short => "SHORT",
            Intent::Cover => "COVER",
        }
    }

    pub fn is_short_side(self) -> bool {
        matches!(self, Intent::Short | Intent::Cover)
    }
}

/// What the state machine decided for one run.
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    /// Nothing to do; the text says why.
    NoOp(String),
    /// A short or cover the broker or configuration does not permit; nothing is sent.
    Blocked(String),
    /// Send one market order.
    Order { intent: Intent, quantity: f64 },
}

fn flat(position: f64) -> bool {
    position.abs() < 1e-9
}

/// The position state machine. `signed_position` is positive for a long and negative for a
/// short. Rules, in order:
/// 1. an order from an earlier run still working: do nothing;
/// 2. already holding the desired side: do nothing;
/// 3. desired flat while flat: do nothing;
/// 4. holding something other than the desired position: ONE closing order of
///    `min(quantity, held)` (never an order that would reverse through zero);
/// 5. flat and wanting a long or short: ONE opening order of `quantity`.
///
/// A short or cover on a broker that cannot short is [`Plan::Blocked`], not an order.
pub fn plan_action(
    desired: Target,
    signed_position: f64,
    working_order: bool,
    allow_short: bool,
    broker_can_short: bool,
    quantity: f64,
    symbol: &str,
) -> Plan {
    if working_order {
        return Plan::NoOp(format!("no-op: an earlier {symbol} order is still working"));
    }
    let held = signed_position.abs();
    let is_flat = flat(signed_position);
    let is_long = !is_flat && signed_position > 0.0;
    let is_short = !is_flat && signed_position < 0.0;

    let intent = match desired {
        Target::Long if is_long => {
            return Plan::NoOp(format!("no-op: already long {held} {symbol}"));
        }
        Target::Short if is_short => {
            return Plan::NoOp(format!("no-op: already short {held} {symbol}"));
        }
        Target::Flat if is_flat => {
            return Plan::NoOp(format!("no-op: no {symbol} position to exit"));
        }
        // Holding the other side (or the wrong one): close it first. Reversing happens on a
        // later run, after this order has filled.
        _ if is_long => Intent::Sell,
        _ if is_short => Intent::Cover,
        Target::Long => Intent::Buy,
        Target::Short => {
            if !allow_short {
                return Plan::NoOp(format!(
                    "no-op: the model wants {symbol} short but this rule does not allow shorting"
                ));
            }
            Intent::Short
        }
        Target::Flat => unreachable!("flat target while flat returned above"),
    };

    if intent.is_short_side() && !broker_can_short {
        return Plan::Blocked(format!(
            "blocked: live shorting is disabled, so no {} {symbol} order was sent \
             (enable [automations] allow_live_shorting and use a broker that supports it; \
             an open short must then be closed by hand)",
            intent.label()
        ));
    }

    let quantity = match intent {
        // Never close more than is held: an oversized order would flip the position.
        Intent::Sell | Intent::Cover => quantity.min(held),
        Intent::Buy | Intent::Short => quantity,
    };
    Plan::Order { intent, quantity }
}

/// Broker-specific order parameters for an intent. A short and its cover use Zerodha's MIS
/// (intraday) product on both legs, because the delivery product (CNC) cannot be sold short.
pub fn broker_params_for(broker_id: &str, intent: Intent) -> Option<BrokerOrderParams> {
    if broker_id == "zerodha" && intent.is_short_side() {
        return Some(BrokerOrderParams {
            product: Some("MIS".into()),
            ..BrokerOrderParams::default()
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use Target::*;

    // ── class mapping ────────────────────────────────────────────────

    #[test]
    fn direction_labels_map_one_to_long_and_zero_to_short_or_flat() {
        let k = Some("direction");
        assert_eq!(target_position(k, 1, false), Some(Long));
        assert_eq!(target_position(k, 1, true), Some(Long));
        assert_eq!(target_position(k, 0, false), Some(Flat));
        assert_eq!(target_position(k, 0, true), Some(Short));
    }

    #[test]
    fn return_threshold_labels_map_minus_one_to_short_or_flat_and_zero_to_flat() {
        let k = Some("return_threshold");
        assert_eq!(target_position(k, 1, false), Some(Long));
        assert_eq!(target_position(k, 1, true), Some(Long));
        assert_eq!(target_position(k, -1, false), Some(Flat));
        assert_eq!(target_position(k, -1, true), Some(Short));
        assert_eq!(target_position(k, 0, false), Some(Flat));
        assert_eq!(target_position(k, 0, true), Some(Flat));
    }

    #[test]
    fn legacy_models_map_one_long_zero_flat_minus_one_short_if_allowed() {
        for k in [None, Some("something_new"), Some("")] {
            assert_eq!(target_position(k, 1, false), Some(Long));
            assert_eq!(target_position(k, 1, true), Some(Long));
            assert_eq!(target_position(k, 0, false), Some(Flat));
            assert_eq!(target_position(k, 0, true), Some(Flat));
            assert_eq!(target_position(k, -1, false), Some(Flat));
            assert_eq!(target_position(k, -1, true), Some(Short));
        }
    }

    #[test]
    fn a_stray_minus_one_on_a_direction_model_follows_the_bearish_rule() {
        let k = Some("direction");
        assert_eq!(target_position(k, -1, false), Some(Flat));
        assert_eq!(target_position(k, -1, true), Some(Short));
    }

    #[test]
    fn unknown_classes_have_no_target() {
        for k in [None, Some("direction"), Some("return_threshold")] {
            for allow in [false, true] {
                for signal in [2, 3, -2, 99, i64::MIN, i64::MAX] {
                    assert_eq!(target_position(k, signal, allow), None);
                }
            }
        }
    }

    #[test]
    fn a_rule_that_cannot_short_never_targets_a_short() {
        for k in [None, Some("direction"), Some("return_threshold")] {
            for signal in -3..=3 {
                assert_ne!(target_position(k, signal, false), Some(Short));
            }
        }
    }

    #[test]
    fn long_only_rules_behave_as_before_for_every_signal() {
        // The earlier rule: 1 buys, 0 and -1 exit a long, anything else is ignored.
        for k in [None, Some("direction"), Some("return_threshold")] {
            assert_eq!(target_position(k, 1, false), Some(Long));
            assert_eq!(target_position(k, 0, false), Some(Flat));
            assert_eq!(target_position(k, -1, false), Some(Flat));
            assert_eq!(target_position(k, 2, false), None);
        }
    }

    // ── metadata ─────────────────────────────────────────────────────

    #[test]
    fn label_kind_is_read_from_metadata() {
        let doc = r#"{"model_id":"m","label":{"horizon_bars":5,"kind":"return_threshold","threshold_pct":1.0}}"#;
        assert_eq!(label_kind_from_metadata(doc).as_deref(), Some("return_threshold"));
        let doc = r#"{"label":{"kind":"direction"}}"#;
        assert_eq!(label_kind_from_metadata(doc).as_deref(), Some("direction"));
    }

    #[test]
    fn missing_or_malformed_metadata_has_no_label_kind() {
        assert_eq!(label_kind_from_metadata(r#"{"model_id":"old"}"#), None);
        assert_eq!(label_kind_from_metadata(r#"{"label":null}"#), None);
        assert_eq!(label_kind_from_metadata(r#"{"label":{}}"#), None);
        assert_eq!(label_kind_from_metadata(r#"{"label":{"kind":7}}"#), None);
        assert_eq!(label_kind_from_metadata(r#"{"label":{"kind":"  "}}"#), None);
        assert_eq!(label_kind_from_metadata(r#"{"label":"direction"}"#), None);
        assert_eq!(label_kind_from_metadata("not json"), None);
        assert_eq!(label_kind_from_metadata(""), None);
        assert_eq!(label_kind_from_metadata("[]"), None);
    }

    #[test]
    fn read_label_kind_handles_files_and_bad_ids() {
        let dir = std::env::temp_dir().join(format!("apex-signal-policy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("m1.json"), r#"{"label":{"kind":"direction"}}"#).unwrap();
        std::fs::write(dir.join("old.json"), r#"{"model_id":"old"}"#).unwrap();
        std::fs::write(dir.join("bad.json"), "{{{").unwrap();
        assert_eq!(read_label_kind(&dir, "m1").as_deref(), Some("direction"));
        assert_eq!(read_label_kind(&dir, "old"), None);
        assert_eq!(read_label_kind(&dir, "bad"), None);
        assert_eq!(read_label_kind(&dir, "missing"), None);
        assert_eq!(read_label_kind(&dir, "../m1"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── broker gates ─────────────────────────────────────────────────

    #[test]
    fn only_paper_and_a_supporting_live_broker_can_short() {
        assert!(broker_can_short("paper", false));
        assert!(broker_can_short("paper", true));
        // Zerodha needs the switch.
        assert!(!broker_can_short("zerodha", false));
        assert!(broker_can_short("zerodha", true));
        // Everything else is conservative even with the switch on.
        for broker in ["binance", "coinbase", "groww", "angel_one", "robinhood", "unknown"] {
            assert!(!broker_can_short(broker, false), "{broker}");
            assert!(!broker_can_short(broker, true), "{broker}");
        }
    }

    #[test]
    fn zerodha_shorts_and_covers_use_mis_and_nothing_else_does() {
        for intent in [Intent::Short, Intent::Cover] {
            let params = broker_params_for("zerodha", intent).expect("params");
            assert_eq!(params.product.as_deref(), Some("MIS"));
        }
        for intent in [Intent::Buy, Intent::Sell] {
            assert!(broker_params_for("zerodha", intent).is_none());
        }
        for intent in [Intent::Buy, Intent::Sell, Intent::Short, Intent::Cover] {
            assert!(broker_params_for("paper", intent).is_none());
            assert!(broker_params_for("binance", intent).is_none());
        }
    }

    #[test]
    fn intents_map_to_the_right_broker_side() {
        assert_eq!(Intent::Buy.side(), OrderSide::Buy);
        // Closing a short is a buy.
        assert_eq!(Intent::Cover.side(), OrderSide::Buy);
        assert_eq!(Intent::Sell.side(), OrderSide::Sell);
        assert_eq!(Intent::Short.side(), OrderSide::Sell);
    }

    // ── state machine ────────────────────────────────────────────────

    fn plan(desired: Target, position: f64, working: bool, allow: bool, can: bool) -> Plan {
        plan_action(desired, position, working, allow, can, 10.0, "AAPL")
    }

    fn order(intent: Intent, quantity: f64) -> Plan {
        Plan::Order { intent, quantity }
    }

    fn is_noop(p: &Plan) -> bool {
        matches!(p, Plan::NoOp(_))
    }

    #[test]
    fn flat_and_wanting_long_buys_the_rule_quantity() {
        assert_eq!(plan(Long, 0.0, false, false, true), order(Intent::Buy, 10.0));
        assert_eq!(plan(Long, 0.0, false, true, true), order(Intent::Buy, 10.0));
    }

    #[test]
    fn flat_and_wanting_short_shorts_the_rule_quantity() {
        assert_eq!(plan(Short, 0.0, false, true, true), order(Intent::Short, 10.0));
    }

    #[test]
    fn already_in_the_desired_position_is_a_clear_no_op() {
        let long = plan(Long, 4.0, false, true, true);
        assert!(matches!(&long, Plan::NoOp(m) if m.contains("already long 4 AAPL")), "{long:?}");
        let short = plan(Short, -4.0, false, true, true);
        assert!(matches!(&short, Plan::NoOp(m) if m.contains("already short 4 AAPL")), "{short:?}");
    }

    #[test]
    fn flat_and_wanting_flat_does_nothing() {
        let p = plan(Flat, 0.0, false, true, true);
        assert!(matches!(&p, Plan::NoOp(m) if m.contains("no AAPL position to exit")), "{p:?}");
    }

    #[test]
    fn wanting_flat_while_long_sells_and_while_short_covers() {
        assert_eq!(plan(Flat, 4.0, false, false, true), order(Intent::Sell, 4.0));
        assert_eq!(plan(Flat, 25.0, false, false, true), order(Intent::Sell, 10.0));
        assert_eq!(plan(Flat, -4.0, false, true, true), order(Intent::Cover, 4.0));
        assert_eq!(plan(Flat, -25.0, false, true, true), order(Intent::Cover, 10.0));
    }

    #[test]
    fn opposite_position_is_closed_first_never_reversed_in_one_order() {
        // Long while the model flips to short: sell only what is held (never held + quantity).
        assert_eq!(plan(Short, 4.0, false, true, true), order(Intent::Sell, 4.0));
        assert_eq!(plan(Short, 40.0, false, true, true), order(Intent::Sell, 10.0));
        // Short while the model flips to long.
        assert_eq!(plan(Long, -4.0, false, true, true), order(Intent::Cover, 4.0));
        assert_eq!(plan(Long, -40.0, false, false, true), order(Intent::Cover, 10.0));
        // Fractional holdings are respected.
        assert_eq!(plan(Short, 0.25, false, true, true), order(Intent::Sell, 0.25));
    }

    #[test]
    fn the_opening_order_follows_once_flat() {
        // After the close fills, the same desired target now opens.
        assert_eq!(plan(Short, 0.0, false, true, true), order(Intent::Short, 10.0));
        assert_eq!(plan(Long, 0.0, false, true, true), order(Intent::Buy, 10.0));
    }

    #[test]
    fn a_working_order_blocks_every_action() {
        for desired in [Long, Short, Flat] {
            for position in [-4.0, 0.0, 4.0] {
                let p = plan(desired, position, true, true, true);
                assert!(
                    matches!(&p, Plan::NoOp(m) if m.contains("still working")),
                    "{desired:?} {position}: {p:?}"
                );
            }
        }
    }

    #[test]
    fn a_rule_that_cannot_short_is_a_no_op_on_a_short_target() {
        let p = plan(Short, 0.0, false, false, true);
        assert!(matches!(&p, Plan::NoOp(m) if m.contains("does not allow shorting")), "{p:?}");
    }

    #[test]
    fn a_broker_that_cannot_short_blocks_shorts_and_covers_but_not_longs() {
        // allow_short on the rule, allow_live_shorting off: never an order.
        for position in [0.0, 4.0] {
            let p = plan(Short, position, false, true, false);
            if position == 0.0 {
                assert!(matches!(&p, Plan::Blocked(m) if m.contains("live shorting is disabled")), "{p:?}");
            } else {
                // Closing a long is an ordinary sell and stays allowed.
                assert_eq!(p, order(Intent::Sell, 4.0));
            }
        }
        let cover = plan(Flat, -4.0, false, true, false);
        assert!(matches!(&cover, Plan::Blocked(m) if m.contains("live shorting is disabled")), "{cover:?}");
        let cover = plan(Long, -4.0, false, true, false);
        assert!(matches!(&cover, Plan::Blocked(_)), "{cover:?}");
        // Plain long trading is unaffected.
        assert_eq!(plan(Long, 0.0, false, true, false), order(Intent::Buy, 10.0));
        assert_eq!(plan(Flat, 4.0, false, true, false), order(Intent::Sell, 4.0));
    }

    #[test]
    fn live_rule_with_allow_short_but_live_shorting_off_never_submits_a_short_side_order() {
        // End to end through the real gates: class -> target -> plan, on a live Zerodha rule
        // with allow_short = true while [automations] allow_live_shorting = false.
        let allow_live_shorting = false;
        let can = broker_can_short("zerodha", allow_live_shorting);
        assert!(!can);
        for kind in [None, Some("direction"), Some("return_threshold")] {
            for signal in -1..=1 {
                for position in [-4.0, 0.0, 4.0] {
                    let target = target_position(kind, signal, true).unwrap();
                    let p = plan_action(target, position, false, true, can, 10.0, "INFY");
                    if let Plan::Order { intent, .. } = p {
                        assert!(
                            !intent.is_short_side(),
                            "{kind:?} signal {signal} position {position} would send {intent:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn rules_without_shorting_never_open_a_short_for_any_signal_or_position() {
        for kind in [None, Some("direction"), Some("return_threshold")] {
            for signal in -2..=2 {
                for position in [-4.0, 0.0, 4.0] {
                    let Some(target) = target_position(kind, signal, false) else { continue };
                    if let Plan::Order { intent, .. } =
                        plan_action(target, position, false, false, true, 10.0, "AAPL")
                    {
                        assert_ne!(intent, Intent::Short, "{kind:?} {signal} {position}");
                    }
                }
            }
        }
    }

    #[test]
    fn long_only_sequence_matches_the_previous_buy_sell_rules() {
        // Previous behaviour for a long-only rule: 1 buys when flat, 0/-1 sell what is held up
        // to the rule quantity, repeats are no-ops.
        assert_eq!(plan(Long, 0.0, false, false, true), order(Intent::Buy, 10.0));
        assert!(is_noop(&plan(Long, 10.0, false, false, true)));
        assert_eq!(plan(Flat, 10.0, false, false, true), order(Intent::Sell, 10.0));
        assert_eq!(plan(Flat, 3.0, false, false, true), order(Intent::Sell, 3.0));
        assert!(is_noop(&plan(Flat, 0.0, false, false, true)));
    }
}
