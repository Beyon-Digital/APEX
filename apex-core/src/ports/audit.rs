//! Audit port for the order lifecycle.
//!
//! Orders are audit records. Every state an order passes through is reported as a NEW event and an
//! earlier event is never updated or removed. The sink behind this trait is an append-only,
//! hash-chained log (see `apex-tauri/src/order_audit.rs`); a hash chain detects edits and deletions in
//! the middle of the log, but someone with full write access to the data directory could rewrite all
//! of it, which is why the chain's head hash is exposed for copying elsewhere.
//!
//! The manager holds an optional sink. No sink means no auditing, so code that does not care (tests,
//! tools) is unaffected.

use serde::{Deserialize, Serialize};

/// Who or what produced an order, precise enough to find the exact code or model version.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OrderAuditSource {
    /// "manual" | "copilot" | "automation" | "strategy" | "kill_switch" | "unattended"
    pub kind: String,
    /// Automation id, strategy path or copilot turn id. Empty for manual orders.
    pub id: String,
    /// Ledger version of the automation or strategy that produced the order.
    pub version: Option<u32>,
    /// Content hash of that version.
    pub hash: Option<String>,
    /// Registry id of the model that produced the signal, when one did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// Ledger version of that model's record at order time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_version: Option<u32>,
}

impl OrderAuditSource {
    pub fn new(kind: &str, id: &str) -> Self {
        Self { kind: kind.into(), id: id.into(), ..Default::default() }
    }
}

/// One event of an order's life. Mirrors the UI's `OrderEventDto` (the ledger adds seq, ts and hashes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderAuditEvent {
    /// Broker order id. Empty for events that happen before the broker assigns one (submitted,
    /// risk_accepted, risk_rejected) and for system events (kill_switch, live_armed, live_disarmed).
    /// Those carry `data.request_id`, which the later `broker_ack` event repeats.
    pub order_id: String,
    pub broker_id: String,
    /// "submitted" | "risk_accepted" | "risk_rejected" | "broker_ack" | "status" | "fill" |
    /// "cancel_requested" | "cancelled" | "cancel_failed" | "modified" | "modify_failed" | "rejected" |
    /// "kill_switch" | "live_armed" | "live_disarmed"
    pub event: String,
    /// "user" | "copilot" | "automation" | "strategy" | "system" | "external" | "unattributed"
    pub actor: String,
    pub source: Option<OrderAuditSource>,
    pub data: serde_json::Value,
}

/// Where audit events go. `record` must not panic and must not block for long (a mutex-guarded file
/// append is fine). An `Err` means the event is NOT durable.
pub trait OrderAuditSink: Send + Sync {
    fn record(&self, event: OrderAuditEvent) -> Result<(), String>;
}

/// Who is acting and on whose authority, for the events one call produces.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditContext {
    pub actor: String,
    pub source: Option<OrderAuditSource>,
    /// Whether a person confirmed this live order in the UI. `None` when no confirmation applies.
    pub confirmed: Option<bool>,
}

impl AuditContext {
    pub fn new(actor: &str, source: Option<OrderAuditSource>) -> Self {
        Self { actor: actor.into(), source, confirmed: None }
    }

    pub fn confirmed(mut self, confirmed: bool) -> Self {
        self.confirmed = Some(confirmed);
        self
    }

    /// Orders from callers that did not say who they are. Visible as such in the log.
    pub fn unattributed() -> Self {
        Self::new("unattributed", None)
    }

    /// Orders that appear at a broker without APEX having placed them.
    pub fn external() -> Self {
        Self::new("external", None)
    }
}

impl Default for AuditContext {
    fn default() -> Self {
        Self::unattributed()
    }
}
