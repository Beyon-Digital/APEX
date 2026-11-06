//! Immutable audit ledger: every change to a strategy, ML code, dataset, model, automation or
//! notebook is a NEW VERSION of the same object. Nothing is overwritten or erased, because
//! overwriting history would be an audit violation.
//!
//! Layout under `<data_dir>/ledger/`:
//!
//! ```text
//! blobs/ab/<sha256>            content-addressed, written once, mode 0444, hash re-checked on read
//! records-0001.jsonl ...       append-only version records, hash-chained, rolled at ~16 MB
//! streams/activity.jsonl ...   hash-chained event streams ("activity", "orders"), rolled the same way
//! ```
//!
//! Tamper evidence. Each record carries `prev_hash` and `hash = sha256(canonical fields + prev_hash)`.
//! The chain continues across segment files. Event streams are chained the same way, one chain per
//! stream. [`Ledger::verify`] recomputes everything: record hashes, chain links, sequence numbers,
//! every blob, every stream.
//!
//! HONEST LIMIT. A hash chain detects an edit, a swap or a deletion in the middle of a log. It cannot
//! detect that the newest entries were cut off, or that someone with full write access to the data
//! directory rewrote the whole thing consistently. For that reason the head hashes are exposed
//! ([`LedgerStatus`]) so they can be copied somewhere else and compared later.
//!
//! The working copy on disk (strategies/foo.py ...) is a convenience mirror of the HEAD version, because
//! the strategy runner and the trainer read files. The ledger is the source of truth.
//! [`save_working_copy`] and [`reconcile_file`] keep the two in step without losing anything.
//!
//! Nothing in here panics on I/O failure and nothing is purged. Appends are guarded by one mutex and
//! are a single `write_all` of one line.

// The public API is consumed by the command modules that are written against it.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock, Weak};

/// `prev_hash` of the very first record of a chain.
pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// Largest content the ledger stores itself. Bigger content is referenced by hash (`put_reference`).
pub const MAX_BLOB_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_ROLL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 1_000_000;
const MAX_ID_BYTES: usize = 2048;
const MAX_SUMMARY_CHARS: usize = 1000;
const MAX_PROBLEMS: usize = 200;
const ORDERS_STREAM: &str = "orders";

// ── Kinds ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Strategy,
    Estimator,
    Recipe,
    Dataset,
    Model,
    Automation,
    Notebook,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Strategy => "strategy",
            Kind::Estimator => "estimator",
            Kind::Recipe => "recipe",
            Kind::Dataset => "dataset",
            Kind::Model => "model",
            Kind::Automation => "automation",
            Kind::Notebook => "notebook",
        }
    }
}

impl FromStr for Kind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s {
            "strategy" => Kind::Strategy,
            "estimator" => Kind::Estimator,
            "recipe" => Kind::Recipe,
            "dataset" => Kind::Dataset,
            "model" => Kind::Model,
            "automation" => Kind::Automation,
            "notebook" => Kind::Notebook,
            other => return Err(format!("unknown ledger kind '{other}'")),
        })
    }
}

// ── Records and reports ───────────────────────────────────────────────

/// One version of one object. Serializes to the TypeScript `VersionDto`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionRecord {
    pub seq: u64,
    pub kind: String,
    pub object_id: String,
    pub version: u32,
    /// "create" | "update" | "delete" | "restore" | "external"
    pub action: String,
    pub actor: String,
    pub ts: String,
    pub content_hash: Option<String>,
    pub size: u64,
    pub retained: bool,
    pub parent_version: Option<u32>,
    pub restored_from: Option<u32>,
    pub summary: String,
    pub prev_hash: String,
    pub hash: String,
}

fn js(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

fn opt_str(v: &Option<String>) -> String {
    v.as_deref().map(js).unwrap_or_else(|| "null".to_string())
}

fn opt_num(v: &Option<u32>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "null".to_string())
}

impl VersionRecord {
    /// Hash over a canonical text of the fields in a fixed order (not dependent on serde or map order)
    /// plus `prev_hash`.
    pub fn compute_hash(&self) -> String {
        let text = format!(
            "apex-ledger-record-v1\nseq={}\nkind={}\nobject_id={}\nversion={}\naction={}\nactor={}\nts={}\ncontent_hash={}\nsize={}\nretained={}\nparent_version={}\nrestored_from={}\nsummary={}\nprev_hash={}\n",
            self.seq,
            js(&self.kind),
            js(&self.object_id),
            self.version,
            js(&self.action),
            js(&self.actor),
            js(&self.ts),
            opt_str(&self.content_hash),
            self.size,
            self.retained,
            opt_num(&self.parent_version),
            opt_num(&self.restored_from),
            js(&self.summary),
            js(&self.prev_hash),
        );
        Ledger::sha256_hex(text.as_bytes())
    }

    pub fn is_tombstone(&self) -> bool {
        self.action == "delete"
    }
}

/// One entry of an event stream. Serializes flat: `{seq, ts, prev_hash, hash, ...payload}`, which is
/// the shape of the TypeScript `OrderEventDto` for the "orders" stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventRecord {
    pub seq: u64,
    pub ts: String,
    pub prev_hash: String,
    pub hash: String,
    #[serde(flatten)]
    pub value: Value,
}

/// Serializes to the TypeScript `VersionedObjectDto`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectSummary {
    pub kind: String,
    pub object_id: String,
    pub head_version: u32,
    pub deleted: bool,
    pub versions: u32,
    pub created_at: String,
    pub updated_at: String,
}

/// Serializes to the TypeScript `LedgerProblemDto`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Problem {
    /// "record_hash" | "chain_link" | "blob_missing" | "blob_hash" | "stream_hash" | "stream_link" | "parse"
    pub kind: String,
    #[serde(rename = "where")]
    pub location: String,
    pub detail: String,
}

/// Serializes to the TypeScript `LedgerVerifyDto`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifyReport {
    pub ok: bool,
    pub checked_records: u64,
    pub checked_blobs: u64,
    pub checked_events: u64,
    pub problems: Vec<Problem>,
    pub head_hash: Option<String>,
    pub verified_at: String,
}

/// Serializes to the TypeScript `LedgerStatusDto`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LedgerStatus {
    /// null = not verified since the app started.
    pub ok: Option<bool>,
    pub records: u64,
    pub objects: u64,
    pub head_hash: Option<String>,
    pub last_verified_at: Option<String>,
    pub stream_heads: BTreeMap<String, Option<String>>,
}

fn now_ts() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn clip_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    }
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

// ── Canonical JSON (stable key order, independent of serde_json features) ──

fn canon_into(out: &mut String, v: &Value) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&js(k));
                out.push(':');
                canon_into(out, &map[k]);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canon_into(out, item);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

fn canon(v: &Value) -> String {
    let mut s = String::new();
    canon_into(&mut s, v);
    s
}

fn event_hash(stream: &str, seq: u64, ts: &str, prev_hash: &str, payload: &Value) -> String {
    let text = format!(
        "apex-ledger-event-v1\nstream={}\nseq={}\nts={}\nprev_hash={}\npayload={}\n",
        js(stream),
        seq,
        js(ts),
        js(prev_hash),
        canon(payload)
    );
    Ledger::sha256_hex(text.as_bytes())
}

/// A parsed stream line: chain fields plus the payload (the line without the four chain fields).
struct ParsedEvent {
    seq: u64,
    ts: String,
    prev_hash: String,
    hash: String,
    payload: Value,
    full: Value,
}

fn parse_event(line: &[u8]) -> Result<ParsedEvent, String> {
    let full: Value = serde_json::from_slice(line).map_err(|e| e.to_string())?;
    let mut payload = match &full {
        Value::Object(m) => m.clone(),
        _ => return Err("line is not a JSON object".into()),
    };
    let seq = payload.remove("seq").and_then(|v| v.as_u64()).ok_or("missing seq")?;
    let ts = payload.remove("ts").and_then(|v| v.as_str().map(String::from)).ok_or("missing ts")?;
    let prev_hash = payload.remove("prev_hash").and_then(|v| v.as_str().map(String::from)).ok_or("missing prev_hash")?;
    let hash = payload.remove("hash").and_then(|v| v.as_str().map(String::from)).ok_or("missing hash")?;
    Ok(ParsedEvent { seq, ts, prev_hash, hash, payload: Value::Object(payload), full })
}

// ── State ─────────────────────────────────────────────────────────────

struct StreamState {
    head_hash: String,
    next_seq: u64,
    seg: u32,
    seg_size: u64,
    dirty: bool,
}

impl StreamState {
    fn new() -> Self {
        Self { head_hash: GENESIS_HASH.into(), next_seq: 1, seg: 1, seg_size: 0, dirty: false }
    }
}

struct State {
    records: Vec<VersionRecord>,
    by_object: HashMap<(String, String), Vec<usize>>,
    head_hash: String,
    next_seq: u64,
    seg: u32,
    seg_size: u64,
    dirty: bool,
    streams: HashMap<String, StreamState>,
}

impl State {
    fn head_of(&self, kind: &str, id: &str) -> Option<&VersionRecord> {
        self.by_object
            .get(&(kind.to_string(), id.to_string()))
            .and_then(|ix| ix.last())
            .map(|i| &self.records[*i])
    }
}

struct Inner {
    dir: PathBuf,
    roll_bytes: u64,
    state: Mutex<State>,
    last_verify: Mutex<Option<VerifyReport>>,
}

/// Cheap to clone (an `Arc`). All clones share one lock, so appends from any thread stay a single chain.
#[derive(Clone)]
pub struct Ledger {
    inner: Arc<Inner>,
}

/// One writer per directory per process. Opening the same directory twice returns the live instance,
/// because two independent mutexes over one chain would fork it.
static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<Inner>>>> = OnceLock::new();
static GLOBAL: OnceLock<Ledger> = OnceLock::new();

/// The process-wide ledger, once [`install`] has run. Code without `AppState` (adapters) uses this.
pub fn global() -> Option<&'static Ledger> {
    GLOBAL.get()
}

/// Set the process-wide ledger. Only the first call succeeds.
pub fn install(l: Ledger) -> Result<(), String> {
    GLOBAL.set(l).map_err(|_| "the ledger is already installed".to_string())
}

// ── File naming (never derived from object ids) ──────────────────────

fn records_path(dir: &Path, seg: u32) -> PathBuf {
    dir.join(format!("records-{seg:04}.jsonl"))
}

fn stream_path(dir: &Path, name: &str, seg: u32) -> PathBuf {
    if seg <= 1 {
        dir.join("streams").join(format!("{name}.jsonl"))
    } else {
        dir.join("streams").join(format!("{name}.{seg:04}.jsonl"))
    }
}

fn valid_stream_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 32 && name.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_'))
}

fn list_record_segments(dir: &Path) -> Vec<u32> {
    let mut out: Vec<u32> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.strip_prefix("records-")?.strip_suffix(".jsonl")?.parse::<u32>().ok()
        })
        .collect();
    out.sort_unstable();
    out
}

/// stream name -> sorted segment numbers
fn list_stream_segments(dir: &Path) -> BTreeMap<String, Vec<u32>> {
    let mut out: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for entry in std::fs::read_dir(dir.join("streams")).into_iter().flatten().flatten() {
        let file = entry.file_name().to_string_lossy().to_string();
        let Some(stem) = file.strip_suffix(".jsonl") else { continue };
        let (name, seg) = match stem.split_once('.') {
            None => (stem, 1u32),
            Some((n, s)) => match s.parse::<u32>() {
                Ok(v) if v >= 2 => (n, v),
                _ => continue,
            },
        };
        if valid_stream_name(name) {
            out.entry(name.to_string()).or_default().push(seg);
        }
    }
    for segs in out.values_mut() {
        segs.sort_unstable();
    }
    out
}

/// The first `len` bytes of an append-only file (a snapshot taken without holding the ledger lock).
fn read_prefix(path: &Path, len: u64) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    std::fs::File::open(path)?.take(len).read_to_end(&mut out)?;
    Ok(out)
}

fn split_lines(bytes: &[u8]) -> impl Iterator<Item = (usize, &[u8])> {
    bytes
        .split(|b| *b == b'\n')
        .enumerate()
        .map(|(i, l)| (i + 1, l))
        .filter(|(_, l)| !l.iter().all(|c| c.is_ascii_whitespace()))
}

/// One `write_all` of one line. `prefix_newline` terminates a torn previous line first.
fn append_line(path: &Path, line: &str, prefix_newline: bool, sync: bool) -> std::io::Result<()> {
    let mut buf = String::with_capacity(line.len() + 2);
    if prefix_newline {
        buf.push('\n');
    }
    buf.push_str(line);
    buf.push('\n');
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(buf.as_bytes())?;
    file.flush()?;
    if sync {
        file.sync_data()?;
    }
    Ok(())
}

fn ends_without_newline(path: &Path) -> bool {
    use std::io::{Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let Ok(len) = f.metadata().map(|m| m.len()) else { return false };
    if len == 0 {
        return false;
    }
    let mut last = [0u8; 1];
    f.seek(SeekFrom::Start(len - 1)).and_then(|_| f.read_exact(&mut last)).map(|_| last[0] != b'\n').unwrap_or(false)
}

enum Content<'a> {
    Bytes(&'a [u8]),
    Reference { hash: &'a str, size: u64 },
}

impl Ledger {
    pub fn sha256_hex(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    pub fn open(dir: &Path) -> Result<Ledger, String> {
        Self::open_with(dir, DEFAULT_ROLL_BYTES)
    }

    /// Like [`Ledger::open`] with a custom segment size (tests).
    pub fn open_with(dir: &Path, roll_bytes: u64) -> Result<Ledger, String> {
        std::fs::create_dir_all(dir.join("blobs")).map_err(|e| format!("could not create ledger directory: {e}"))?;
        std::fs::create_dir_all(dir.join("streams")).map_err(|e| format!("could not create ledger directory: {e}"))?;
        let key = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
        let mut reg = lock(registry);
        if let Some(inner) = reg.get(&key).and_then(|w| w.upgrade()) {
            return Ok(Ledger { inner });
        }
        let state = load_state(&key)?;
        let inner = Arc::new(Inner { dir: key.clone(), roll_bytes: roll_bytes.max(256), state: Mutex::new(state), last_verify: Mutex::new(None) });
        reg.retain(|_, w| w.strong_count() > 0);
        reg.insert(key, Arc::downgrade(&inner));
        Ok(Ledger { inner })
    }

    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    // ── blobs ──

    fn blob_path(&self, hash: &str) -> Result<PathBuf, String> {
        if !is_hex64(hash) {
            return Err("content hash is not a SHA-256 hex string".into());
        }
        Ok(self.inner.dir.join("blobs").join(&hash[..2]).join(hash))
    }

    /// Write-once. An existing blob with the right hash is left untouched. An existing blob with the
    /// wrong hash (a torn or tampered file) is moved aside, never deleted, and the correct bytes written.
    fn store_blob(&self, hash: &str, bytes: &[u8]) -> Result<(), String> {
        let path = self.blob_path(hash)?;
        let quarantine = if path.exists() {
            match std::fs::read(&path) {
                Ok(existing) if Self::sha256_hex(&existing) == hash => return Ok(()),
                _ => true,
            }
        } else {
            false
        };
        let parent = path.parent().ok_or("bad blob path")?;
        std::fs::create_dir_all(parent).map_err(|e| format!("could not create blob directory: {e}"))?;
        let tmp = parent.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
        let write = || -> std::io::Result<()> {
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o444))?;
            }
            Ok(())
        };
        if let Err(e) = write() {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("could not write ledger content: {e}"));
        }
        if quarantine {
            let aside = parent.join(format!("{hash}.corrupt-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
            std::fs::rename(&path, &aside).map_err(|e| format!("could not move a damaged blob aside: {e}"))?;
        }
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("could not store ledger content: {e}")
        })
    }

    fn read_blob(&self, hash: &str) -> Result<Vec<u8>, String> {
        let path = self.blob_path(hash)?;
        let bytes = std::fs::read(&path).map_err(|e| format!("ledger content {hash} is not readable: {e}"))?;
        if Self::sha256_hex(&bytes) != hash {
            return Err(format!("ledger content {hash} does not match its hash (damaged or altered)"));
        }
        Ok(bytes)
    }

    // ── versions ──

    /// Append a version. Identical content to a live head is a no-op that returns the head.
    pub fn put(&self, kind: Kind, id: &str, bytes: &[u8], actor: &str, summary: &str) -> Result<VersionRecord, String> {
        self.put_inner(kind, id, Content::Bytes(bytes), actor, None, summary)
    }

    /// Like [`Ledger::put`] with an explicit action ("external" for edits picked up from disk).
    pub fn put_as(&self, kind: Kind, id: &str, bytes: &[u8], actor: &str, action: &str, summary: &str) -> Result<VersionRecord, String> {
        self.put_inner(kind, id, Content::Bytes(bytes), actor, Some(action), summary)
    }

    /// For large write-once files: record the hash and size only (`retained = false`).
    pub fn put_reference(&self, kind: Kind, id: &str, content_hash: &str, size: u64, actor: &str, summary: &str) -> Result<VersionRecord, String> {
        self.put_inner(kind, id, Content::Reference { hash: content_hash, size }, actor, None, summary)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn put_reference_as(&self, kind: Kind, id: &str, content_hash: &str, size: u64, actor: &str, action: &str, summary: &str) -> Result<VersionRecord, String> {
        self.put_inner(kind, id, Content::Reference { hash: content_hash, size }, actor, Some(action), summary)
    }

    fn put_inner(&self, kind: Kind, id: &str, content: Content<'_>, actor: &str, action: Option<&str>, summary: &str) -> Result<VersionRecord, String> {
        validate_id(id)?;
        let (hash, size, retained) = match &content {
            Content::Bytes(b) => {
                if b.len() > MAX_BLOB_BYTES {
                    return Err(format!(
                        "content is {} bytes, over the {} MB ledger limit; record it with put_reference (hash only) instead",
                        b.len(),
                        MAX_BLOB_BYTES / 1024 / 1024
                    ));
                }
                (Self::sha256_hex(b), b.len() as u64, true)
            }
            Content::Reference { hash, size } => {
                if !is_hex64(hash) {
                    return Err("content hash must be 64 lowercase hex characters".into());
                }
                ((*hash).to_string(), *size, false)
            }
        };
        let mut st = lock(&self.inner.state);
        let head = st.head_of(kind.as_str(), id).cloned();
        if let Some(h) = &head {
            if !h.is_tombstone() && h.content_hash.as_deref() == Some(hash.as_str()) {
                return Ok(h.clone());
            }
        }
        if let Content::Bytes(b) = &content {
            // Durable content first, so a record never points at a missing blob.
            self.store_blob(&hash, b)?;
        }
        let action = match action {
            Some(a) => a.to_string(),
            None if head.as_ref().map(|h| h.is_tombstone()).unwrap_or(true) => "create".to_string(),
            None => "update".to_string(),
        };
        let rec = VersionRecord {
            seq: 0,
            kind: kind.as_str().into(),
            object_id: id.into(),
            version: head.as_ref().map(|h| h.version + 1).unwrap_or(1),
            action,
            actor: actor.into(),
            ts: String::new(),
            content_hash: Some(hash),
            size,
            retained,
            parent_version: head.as_ref().map(|h| h.version),
            restored_from: None,
            summary: clip_chars(summary, MAX_SUMMARY_CHARS),
            prev_hash: String::new(),
            hash: String::new(),
        };
        self.commit_record(&mut st, rec)
    }

    /// Mark the object deleted. History stays readable; the object can be restored or re-created.
    pub fn tombstone(&self, kind: Kind, id: &str, actor: &str, summary: &str) -> Result<VersionRecord, String> {
        validate_id(id)?;
        let mut st = lock(&self.inner.state);
        let Some(head) = st.head_of(kind.as_str(), id).cloned() else {
            return Err(format!("no {} '{}' in the ledger", kind.as_str(), id));
        };
        if head.is_tombstone() {
            return Ok(head);
        }
        let rec = VersionRecord {
            seq: 0,
            kind: kind.as_str().into(),
            object_id: id.into(),
            version: head.version + 1,
            action: "delete".into(),
            actor: actor.into(),
            ts: String::new(),
            content_hash: None,
            size: 0,
            retained: true,
            parent_version: Some(head.version),
            restored_from: None,
            summary: clip_chars(summary, MAX_SUMMARY_CHARS),
            prev_hash: String::new(),
            hash: String::new(),
        };
        self.commit_record(&mut st, rec)
    }

    /// Append a new version whose content equals an older one and return that content so the caller can
    /// rewrite the working copy. Restoring content equal to a live head is a no-op.
    pub fn restore(&self, kind: Kind, id: &str, version: u32, actor: &str) -> Result<(VersionRecord, Vec<u8>), String> {
        let mut st = lock(&self.inner.state);
        let head = st.head_of(kind.as_str(), id).cloned().ok_or_else(|| format!("no {} '{}' in the ledger", kind.as_str(), id))?;
        let target = find_version(&st, kind.as_str(), id, version)?;
        let hash = match (&target.content_hash, target.is_tombstone()) {
            (Some(h), false) => h.clone(),
            _ => return Err(format!("version {version} is a delete marker and has no content to restore")),
        };
        if !target.retained {
            return Err(format!("version {version} was too large to keep in the ledger (only its hash is recorded), so it cannot be restored from here"));
        }
        let bytes = self.read_blob(&hash)?;
        if !head.is_tombstone() && head.content_hash.as_deref() == Some(hash.as_str()) {
            return Ok((head, bytes));
        }
        let rec = VersionRecord {
            seq: 0,
            kind: kind.as_str().into(),
            object_id: id.into(),
            version: head.version + 1,
            action: "restore".into(),
            actor: actor.into(),
            ts: String::new(),
            content_hash: Some(hash),
            size: bytes.len() as u64,
            retained: true,
            parent_version: Some(head.version),
            restored_from: Some(version),
            summary: format!("Restored version {version}"),
            prev_hash: String::new(),
            hash: String::new(),
        };
        let rec = self.commit_record(&mut st, rec)?;
        Ok((rec, bytes))
    }

    pub fn head(&self, kind: Kind, id: &str) -> Option<VersionRecord> {
        lock(&self.inner.state).head_of(kind.as_str(), id).cloned()
    }

    /// Oldest first.
    pub fn history(&self, kind: Kind, id: &str) -> Vec<VersionRecord> {
        let st = lock(&self.inner.state);
        st.by_object
            .get(&(kind.as_str().to_string(), id.to_string()))
            .map(|ix| ix.iter().map(|i| st.records[*i].clone()).collect())
            .unwrap_or_default()
    }

    /// The bytes of one version, with the blob hash verified.
    pub fn content(&self, kind: Kind, id: &str, version: u32) -> Result<Vec<u8>, String> {
        let rec = {
            let st = lock(&self.inner.state);
            find_version(&st, kind.as_str(), id, version)?
        };
        let Some(hash) = rec.content_hash.as_ref().filter(|_| !rec.is_tombstone()) else {
            return Err(format!("version {version} is a delete marker and has no content"));
        };
        if !rec.retained {
            return Err(format!("version {version} was too large to keep in the ledger; only its hash {hash} is recorded"));
        }
        self.read_blob(hash)
    }

    pub fn objects(&self, kind: Kind, include_deleted: bool) -> Vec<ObjectSummary> {
        let st = lock(&self.inner.state);
        let mut out: Vec<ObjectSummary> = st
            .by_object
            .iter()
            .filter(|((k, _), _)| k == kind.as_str())
            .filter_map(|((k, id), ix)| {
                let first = &st.records[*ix.first()?];
                let head = &st.records[*ix.last()?];
                if head.is_tombstone() && !include_deleted {
                    return None;
                }
                Some(ObjectSummary {
                    kind: k.clone(),
                    object_id: id.clone(),
                    head_version: head.version,
                    deleted: head.is_tombstone(),
                    versions: ix.len() as u32,
                    created_at: first.ts.clone(),
                    updated_at: head.ts.clone(),
                })
            })
            .collect();
        out.sort_by(|a, b| a.object_id.cmp(&b.object_id));
        out
    }

    fn commit_record(&self, st: &mut State, mut rec: VersionRecord) -> Result<VersionRecord, String> {
        rec.seq = st.next_seq;
        rec.prev_hash = st.head_hash.clone();
        rec.ts = now_ts();
        rec.hash = rec.compute_hash();
        let line = serde_json::to_string(&rec).map_err(|e| format!("could not encode ledger record: {e}"))?;
        if st.seg_size >= self.inner.roll_bytes {
            st.seg += 1;
            st.seg_size = 0;
            st.dirty = false;
        }
        let path = records_path(&self.inner.dir, st.seg);
        if let Err(e) = append_line(&path, &line, st.dirty, true) {
            st.dirty = true;
            return Err(format!("could not write the audit ledger: {e}"));
        }
        st.dirty = false;
        st.seg_size += line.len() as u64 + 1;
        st.next_seq = rec.seq + 1;
        st.head_hash = rec.hash.clone();
        let idx = st.records.len();
        st.by_object.entry((rec.kind.clone(), rec.object_id.clone())).or_default().push(idx);
        st.records.push(rec.clone());
        Ok(rec)
    }

    // ── event streams ──

    /// Append one event to a hash-chained stream ("activity", "orders", ...). The payload must be a JSON
    /// object. Its `ts` string, when present, becomes the line's timestamp (used when importing old logs);
    /// `seq`, `prev_hash` and `hash` are reserved. Order events are fsynced.
    pub fn append_event(&self, stream: &str, value: Value) -> Result<EventRecord, String> {
        if !valid_stream_name(stream) {
            return Err("stream names are 1-32 characters of a-z, 0-9 and _".into());
        }
        let Value::Object(mut payload) = value else {
            return Err("an event must be a JSON object".into());
        };
        for reserved in ["seq", "prev_hash", "hash"] {
            if payload.contains_key(reserved) {
                return Err(format!("'{reserved}' is reserved for the ledger chain"));
            }
        }
        let ts = match payload.remove("ts") {
            Some(Value::String(s)) => s,
            Some(_) => return Err("'ts' must be a string".into()),
            None => now_ts(),
        };
        // Hash what a reader will parse back, so a float that does not survive a text round trip bit for
        // bit cannot make verify disagree with the writer.
        let payload: Value = serde_json::from_str(&canon(&Value::Object(payload))).map_err(|e| format!("could not encode event: {e}"))?;
        let Value::Object(payload_map) = &payload else {
            return Err("an event must be a JSON object".into());
        };

        let mut st = lock(&self.inner.state);
        let roll = self.inner.roll_bytes;
        let dir = self.inner.dir.clone();
        let s = st.streams.entry(stream.to_string()).or_insert_with(StreamState::new);
        let seq = s.next_seq;
        let prev_hash = s.head_hash.clone();
        let hash = event_hash(stream, seq, &ts, &prev_hash, &payload);
        let mut full = payload_map.clone();
        full.insert("seq".into(), seq.into());
        full.insert("ts".into(), Value::String(ts.clone()));
        full.insert("prev_hash".into(), Value::String(prev_hash.clone()));
        full.insert("hash".into(), Value::String(hash.clone()));
        let line = canon(&Value::Object(full));
        if line.len() > MAX_EVENT_BYTES {
            return Err("event is too large".into());
        }
        if s.seg_size >= roll {
            s.seg += 1;
            s.seg_size = 0;
            s.dirty = false;
        }
        let path = stream_path(&dir, stream, s.seg);
        if let Err(e) = append_line(&path, &line, s.dirty, stream == ORDERS_STREAM) {
            s.dirty = true;
            return Err(format!("could not write the {stream} audit stream: {e}"));
        }
        s.dirty = false;
        s.seg_size += line.len() as u64 + 1;
        s.next_seq = seq + 1;
        s.head_hash = hash.clone();
        Ok(EventRecord { seq, ts, prev_hash, hash, value: payload })
    }

    /// Make everything appended to a stream durable (streams other than "orders" are only flushed per append).
    pub fn flush_stream(&self, stream: &str) -> Result<(), String> {
        if !valid_stream_name(stream) {
            return Err("bad stream name".into());
        }
        let st = lock(&self.inner.state);
        let Some(s) = st.streams.get(stream) else { return Ok(()) };
        let path = stream_path(&self.inner.dir, stream, s.seg);
        std::fs::File::open(&path).and_then(|f| f.sync_all()).map_err(|e| format!("could not flush the {stream} audit stream: {e}"))
    }

    /// Newest first. The filter sees the whole line (`seq`, `ts`, `prev_hash`, `hash` and the payload).
    pub fn read_events(&self, stream: &str, filter: &dyn Fn(&Value) -> bool, limit: usize) -> Vec<EventRecord> {
        let mut out = Vec::new();
        if limit == 0 || !valid_stream_name(stream) {
            return out;
        }
        // Only the segment list and each file's current length are taken under the lock (cheap).
        // The files are append-only, so reading the first `len` bytes of each later, outside the
        // lock, is a consistent snapshot: a scan of a long history never stalls an append (the
        // fail-closed live `submitted` write, the activity log, ledger puts).
        let snapshot: Vec<(u32, u64)> = {
            let st = lock(&self.inner.state);
            let Some(s) = st.streams.get(stream) else { return out };
            (1..=s.seg)
                .rev()
                .map(|seg| (seg, std::fs::metadata(stream_path(&self.inner.dir, stream, seg)).map(|m| m.len()).unwrap_or(0)))
                .collect()
        };
        for (seg, len) in snapshot {
            let Ok(bytes) = read_prefix(&stream_path(&self.inner.dir, stream, seg), len) else { continue };
            let lines: Vec<&[u8]> = split_lines(&bytes).map(|(_, l)| l).collect();
            for line in lines.into_iter().rev() {
                let Ok(ev) = parse_event(line) else { continue };
                if !filter(&ev.full) {
                    continue;
                }
                out.push(EventRecord { seq: ev.seq, ts: ev.ts, prev_hash: ev.prev_hash, hash: ev.hash, value: ev.payload });
                if out.len() >= limit {
                    return out;
                }
            }
        }
        out
    }

    // ── status and verification ──

    pub fn status(&self) -> LedgerStatus {
        let st = lock(&self.inner.state);
        let last = lock(&self.inner.last_verify).clone();
        let mut stream_heads: BTreeMap<String, Option<String>> = BTreeMap::new();
        stream_heads.insert("activity".into(), None);
        stream_heads.insert(ORDERS_STREAM.into(), None);
        for (name, s) in &st.streams {
            stream_heads.insert(name.clone(), (s.next_seq > 1).then(|| s.head_hash.clone()));
        }
        LedgerStatus {
            ok: last.as_ref().map(|r| r.ok),
            records: st.records.len() as u64,
            objects: st.by_object.len() as u64,
            head_hash: (!st.records.is_empty()).then(|| st.head_hash.clone()),
            last_verified_at: last.map(|r| r.verified_at),
            stream_heads,
        }
    }

    /// Recompute every record hash, chain link, sequence number, blob hash and stream chain from the
    /// files on disk (not from memory). The result is also kept for [`Ledger::status`].
    pub fn verify(&self) -> VerifyReport {
        let mut problems: Vec<Problem> = Vec::new();
        let add = |problems: &mut Vec<Problem>, kind: &str, location: String, detail: String| {
            if problems.len() < MAX_PROBLEMS {
                problems.push(Problem { kind: kind.into(), location, detail });
            }
        };

        // Under the lock only the file list and each file's current length are taken, so no append is
        // half-visible. The files are append-only: the first `len` bytes are read afterwards, outside
        // the lock, so verifying a long history never stalls order placement or other appends.
        type Files = Vec<(String, Vec<u8>)>;
        type Planned = Vec<(String, std::path::PathBuf, u64)>;
        let len_of = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let (record_plan, stream_plan): (Planned, BTreeMap<String, Planned>) = {
            let _st = lock(&self.inner.state);
            let mut rf: Planned = Vec::new();
            for seg in list_record_segments(&self.inner.dir) {
                let path = records_path(&self.inner.dir, seg);
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let len = len_of(&path);
                rf.push((name, path, len));
            }
            let mut sf: BTreeMap<String, Planned> = BTreeMap::new();
            for (stream, segs) in list_stream_segments(&self.inner.dir) {
                for seg in segs {
                    let path = stream_path(&self.inner.dir, &stream, seg);
                    let name = format!("streams/{}", path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
                    let len = len_of(&path);
                    sf.entry(stream.clone()).or_default().push((name, path, len));
                }
            }
            (rf, sf)
        };
        let mut record_files: Files = Vec::new();
        for (name, path, len) in record_plan {
            match read_prefix(&path, len) {
                Ok(b) => record_files.push((name, b)),
                Err(e) => add(&mut problems, "parse", name, format!("could not read segment: {e}")),
            }
        }
        let mut stream_files: BTreeMap<String, Files> = BTreeMap::new();
        for (stream, plan) in stream_plan {
            for (name, path, len) in plan {
                match read_prefix(&path, len) {
                    Ok(b) => stream_files.entry(stream.clone()).or_default().push((name, b)),
                    Err(e) => add(&mut problems, "parse", name, format!("could not read segment: {e}")),
                }
            }
        }

        // Records.
        let mut exp_seq = 1u64;
        let mut exp_prev = GENESIS_HASH.to_string();
        let mut checked_records = 0u64;
        let mut head_hash: Option<String> = None;
        let mut blobs: BTreeMap<String, String> = BTreeMap::new();
        let nfiles = record_files.len();
        for (fi, (name, bytes)) in record_files.iter().enumerate() {
            let torn = !bytes.is_empty() && !bytes.ends_with(b"\n");
            let last_line_no = bytes.split(|b| *b == b'\n').count();
            for (line_no, line) in split_lines(bytes) {
                let loc = format!("{name}:{line_no}");
                let rec: VersionRecord = match serde_json::from_slice(line) {
                    Ok(r) => r,
                    Err(e) => {
                        let tail = torn && fi + 1 == nfiles && line_no == last_line_no;
                        let detail = if tail {
                            format!("last line is incomplete (the app was probably interrupted while writing): {e}")
                        } else {
                            format!("line is not a valid record: {e}")
                        };
                        add(&mut problems, "parse", loc, detail);
                        continue;
                    }
                };
                if rec.seq != exp_seq {
                    add(&mut problems, "chain_link", loc.clone(), format!("sequence number gap or reorder: expected {exp_seq}, found {}", rec.seq));
                }
                if rec.prev_hash != exp_prev {
                    add(&mut problems, "chain_link", loc.clone(), "prev_hash does not match the previous record".into());
                }
                if rec.compute_hash() != rec.hash {
                    add(&mut problems, "record_hash", loc.clone(), format!("record for {} (seq {}) was altered: its hash does not match its content", rec.object_id, rec.seq));
                }
                exp_seq = rec.seq.saturating_add(1);
                exp_prev = rec.hash.clone();
                head_hash = Some(rec.hash.clone());
                checked_records += 1;
                if rec.retained && !rec.is_tombstone() {
                    if let Some(h) = &rec.content_hash {
                        blobs.entry(h.clone()).or_insert(loc);
                    }
                }
            }
        }

        // Streams.
        let mut checked_events = 0u64;
        for (stream, files) in &stream_files {
            let mut exp_seq = 1u64;
            let mut exp_prev = GENESIS_HASH.to_string();
            let nfiles = files.len();
            for (fi, (name, bytes)) in files.iter().enumerate() {
                let torn = !bytes.is_empty() && !bytes.ends_with(b"\n");
                let last_line_no = bytes.split(|b| *b == b'\n').count();
                for (line_no, line) in split_lines(bytes) {
                    let loc = format!("{name}:{line_no}");
                    let ev = match parse_event(line) {
                        Ok(e) => e,
                        Err(e) => {
                            let tail = torn && fi + 1 == nfiles && line_no == last_line_no;
                            let detail = if tail {
                                format!("last line is incomplete (the app was probably interrupted while writing): {e}")
                            } else {
                                format!("line is not a valid event: {e}")
                            };
                            add(&mut problems, "parse", loc, detail);
                            continue;
                        }
                    };
                    if ev.seq != exp_seq {
                        add(&mut problems, "stream_link", loc.clone(), format!("{stream}: sequence number gap or reorder: expected {exp_seq}, found {}", ev.seq));
                    }
                    if ev.prev_hash != exp_prev {
                        add(&mut problems, "stream_link", loc.clone(), format!("{stream}: prev_hash does not match the previous event"));
                    }
                    if event_hash(stream, ev.seq, &ev.ts, &ev.prev_hash, &ev.payload) != ev.hash {
                        add(&mut problems, "stream_hash", loc, format!("{stream}: event {} was altered: its hash does not match its content", ev.seq));
                    }
                    exp_seq = ev.seq.saturating_add(1);
                    exp_prev = ev.hash.clone();
                    checked_events += 1;
                }
            }
        }

        // Blobs (immutable, so they can be hashed without holding the lock).
        let mut checked_blobs = 0u64;
        for (hash, first_ref) in &blobs {
            let path = match self.blob_path(hash) {
                Ok(p) => p,
                Err(e) => {
                    add(&mut problems, "blob_missing", first_ref.clone(), e);
                    continue;
                }
            };
            match std::fs::read(&path) {
                Err(_) => add(&mut problems, "blob_missing", first_ref.clone(), format!("content {hash} is missing")),
                Ok(b) => {
                    checked_blobs += 1;
                    if Self::sha256_hex(&b) != *hash {
                        add(&mut problems, "blob_hash", first_ref.clone(), format!("content {hash} was altered: its hash does not match"));
                    }
                }
            }
        }

        let report = VerifyReport {
            ok: problems.is_empty(),
            checked_records,
            checked_blobs,
            checked_events,
            problems,
            head_hash,
            verified_at: now_ts(),
        };
        *lock(&self.inner.last_verify) = Some(report.clone());
        report
    }
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("object id is empty".into());
    }
    if id.len() > MAX_ID_BYTES {
        return Err(format!("object id is longer than {MAX_ID_BYTES} bytes"));
    }
    Ok(())
}

fn find_version(st: &State, kind: &str, id: &str, version: u32) -> Result<VersionRecord, String> {
    st.by_object
        .get(&(kind.to_string(), id.to_string()))
        .and_then(|ix| ix.iter().map(|i| &st.records[*i]).find(|r| r.version == version))
        .cloned()
        .ok_or_else(|| format!("{kind} '{id}' has no version {version}"))
}

fn load_state(dir: &Path) -> Result<State, String> {
    let mut st = State {
        records: Vec::new(),
        by_object: HashMap::new(),
        head_hash: GENESIS_HASH.into(),
        next_seq: 1,
        seg: 1,
        seg_size: 0,
        dirty: false,
        streams: HashMap::new(),
    };
    let segs = list_record_segments(dir);
    for seg in &segs {
        let path = records_path(dir, *seg);
        let bytes = std::fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
        for (_, line) in split_lines(&bytes) {
            // Unreadable lines (a torn tail) are skipped here; verify reports them.
            if let Ok(rec) = serde_json::from_slice::<VersionRecord>(line) {
                let idx = st.records.len();
                st.next_seq = rec.seq.saturating_add(1);
                st.head_hash = rec.hash.clone();
                st.by_object.entry((rec.kind.clone(), rec.object_id.clone())).or_default().push(idx);
                st.records.push(rec);
            }
        }
        if Some(seg) == segs.last() {
            st.seg = *seg;
            st.seg_size = bytes.len() as u64;
            st.dirty = !bytes.is_empty() && !bytes.ends_with(b"\n");
        }
    }
    // Versions of one object are kept in version order even if the file was tampered with.
    let records = &st.records;
    for ix in st.by_object.values_mut() {
        ix.sort_by_key(|i| records[*i].version);
    }
    // Streams: only the newest readable line of each is needed.
    for (name, segs) in list_stream_segments(dir) {
        let mut s = StreamState::new();
        let last = *segs.last().unwrap_or(&1);
        s.seg = last;
        s.seg_size = std::fs::metadata(stream_path(dir, &name, last)).map(|m| m.len()).unwrap_or(0);
        s.dirty = ends_without_newline(&stream_path(dir, &name, last));
        'segments: for seg in segs.iter().rev() {
            let Ok(bytes) = std::fs::read(stream_path(dir, &name, *seg)) else { continue };
            let lines: Vec<&[u8]> = split_lines(&bytes).map(|(_, l)| l).collect();
            for line in lines.into_iter().rev() {
                if let Ok(ev) = parse_event(line) {
                    s.next_seq = ev.seq.saturating_add(1);
                    s.head_hash = ev.hash;
                    break 'segments;
                }
            }
        }
        st.streams.insert(name, s);
    }
    Ok(st)
}

// ── Working copies ────────────────────────────────────────────────────

/// The temp file next to a working copy. The only other path these helpers touch.
pub fn pending_path(path: &Path) -> Result<PathBuf, String> {
    let name = path.file_name().ok_or("working copy path has no file name")?;
    let mut name = name.to_os_string();
    name.push(".apexnew");
    Ok(path.with_file_name(name))
}

struct FileSnap {
    hash: String,
    size: u64,
    /// Present when the file is small enough for the ledger to keep.
    bytes: Option<Vec<u8>>,
}

fn snapshot_file(path: &Path) -> Result<Option<FileSnap>, String> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not inspect {}: {e}", path.display())),
    };
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let io = |e: std::io::Error| format!("could not read {}: {e}", path.display());
    if meta.len() as usize <= MAX_BLOB_BYTES {
        return match std::fs::read(path) {
            Ok(b) => Ok(Some(FileSnap { hash: Ledger::sha256_hex(&b), size: b.len() as u64, bytes: Some(b) })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io(e)),
        };
    }
    let mut f = std::fs::File::open(path).map_err(io)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut size = 0u64;
    loop {
        let n = f.read(&mut buf).map_err(io)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok(Some(FileSnap { hash: hex::encode(hasher.finalize()), size, bytes: None }))
}

/// What [`reconcile_file_detailed`] found and did.
#[derive(Debug, Default)]
pub struct ReconcileOutcome {
    /// The version appended for an external edit, adoption or removal.
    pub record: Option<VersionRecord>,
    /// A pending file matching the head was moved into place.
    pub completed_pending: bool,
    /// A pending file that was not used: left alone, with the reason.
    pub pending_left: Option<String>,
}

/// Save a new version and mirror it to the working copy. Order: write `<path>.apexnew` -> ledger put
/// (durable) -> atomic rename into place. Anything on disk that the ledger does not know (an external
/// edit) is first recorded as its own version, so it is never overwritten unseen.
#[allow(clippy::too_many_arguments)]
pub fn save_working_copy(ledger: &Ledger, kind: Kind, id: &str, path: &Path, bytes: &[u8], actor: &str, summary: &str) -> Result<VersionRecord, String> {
    save_working_copy_with(ledger, kind, id, path, bytes, actor, summary, &|| {})
}

/// [`save_working_copy`] with a hook that runs after the ledger put and before the final check and
/// rename (tests use it to write the file at exactly that moment).
///
/// HONEST LIMIT. The working file is checked again just before the rename: if something outside APEX
/// (an editor, `git checkout`, the trainer) changed it since the save began, that content is recorded
/// as an "external" version and the rename is NOT done, so it is never replaced unseen. A plain
/// `rename` cannot be made conditional, so a write that lands in the few microseconds between that
/// check and the rename is still replaced; it is the same race every editor's atomic save has.
#[allow(clippy::too_many_arguments)]
fn save_working_copy_with(
    ledger: &Ledger,
    kind: Kind,
    id: &str,
    path: &Path,
    bytes: &[u8],
    actor: &str,
    summary: &str,
    between_put_and_rename: &dyn Fn(),
) -> Result<VersionRecord, String> {
    if bytes.len() > MAX_BLOB_BYTES {
        return Err(format!("content is over the {} MB ledger limit", MAX_BLOB_BYTES / 1024 / 1024));
    }
    let outcome = reconcile_file_detailed(ledger, kind, id, path)?;
    if let Some(why) = &outcome.pending_left {
        tracing::warn!(id, "{why}");
    }
    let hash = Ledger::sha256_hex(bytes);
    if let (Some(head), Some(snap)) = (ledger.head(kind, id), snapshot_file(path)?) {
        if !head.is_tombstone() && head.content_hash.as_deref() == Some(hash.as_str()) && snap.hash == hash {
            return Ok(head);
        }
    }
    // What the working file held once the ledger was brought up to date: the baseline for noticing
    // an outside write that lands while this save is in flight.
    let baseline = snapshot_file(path)?.map(|s| s.hash);
    let pending = pending_path(path)?;
    if pending.exists() {
        // A leftover that did not match the head. Keep it under another name rather than overwrite it.
        let aside = pending.with_extension(format!("apexnew.orphan-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
        std::fs::rename(&pending, &aside).map_err(|e| format!("could not set aside a leftover pending file: {e}"))?;
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&pending)?;
        f.write_all(bytes)?;
        f.sync_all()
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&pending);
        format!("could not write {}: {e}", pending.display())
    })?;
    let rec = match ledger.put(kind, id, bytes, actor, summary) {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_file(&pending);
            return Err(e);
        }
    };
    between_put_and_rename();
    if let Some(now) = snapshot_file(path)? {
        if baseline.as_deref() != Some(now.hash.as_str()) && now.hash != rec.content_hash.clone().unwrap_or_default() {
            // Changed outside APEX while this save was in flight. Keep it: it becomes the newest version
            // and stays in place; the version just saved remains in the history.
            let external = match &now.bytes {
                Some(b) => ledger.put_as(kind, id, b, "external", "external", "File changed outside APEX while a save was in progress")?,
                None => ledger.put_reference_as(kind, id, &now.hash, now.size, "external", "external", "File changed outside APEX while a save was in progress")?,
            };
            let _ = std::fs::remove_file(&pending);
            return Err(format!(
                "{} was changed outside APEX while it was being saved. Your save is version {} and the outside change is version {} (the current one); nothing was lost. Review the history and save again if needed.",
                path.display(),
                rec.version,
                external.version
            ));
        }
    }
    std::fs::rename(&pending, path).map_err(|e| {
        format!("saved as version {} in the audit ledger, but the file {} could not be updated ({e}); it will be completed at next start", rec.version, path.display())
    })?;
    Ok(rec)
}

/// Bring the ledger and the working copy back in step (startup, or before a save).
/// - `<path>.apexnew` whose hash equals the head's content: finish the rename.
/// - `<path>.apexnew` with another hash: left alone (see [`reconcile_file_detailed`] to read why).
/// - working file different from the head: appended as a version by actor "external", action "external".
/// - working file missing while the head is live: recorded as a delete by actor "external".
///
/// A delete done by the app (tombstone, then removing the file) that is interrupted between the two steps
/// leaves a file next to a tombstone; that is recorded as an external re-creation (nothing is lost).
pub fn reconcile_file(ledger: &Ledger, kind: Kind, id: &str, path: &Path) -> Result<Option<VersionRecord>, String> {
    let outcome = reconcile_file_detailed(ledger, kind, id, path)?;
    if let Some(why) = &outcome.pending_left {
        tracing::warn!(id, "{why}");
    }
    Ok(outcome.record)
}

pub fn reconcile_file_detailed(ledger: &Ledger, kind: Kind, id: &str, path: &Path) -> Result<ReconcileOutcome, String> {
    let pending = pending_path(path)?;
    let mut out = ReconcileOutcome::default();
    let head = ledger.head(kind, id);
    let live_hash = head.as_ref().filter(|h| !h.is_tombstone()).and_then(|h| h.content_hash.clone());

    if let Some(p) = snapshot_file(&pending)? {
        if live_hash.as_deref() == Some(p.hash.as_str()) {
            let known = match snapshot_file(path)? {
                None => true,
                Some(cur) => cur.hash == p.hash || ledger.history(kind, id).iter().any(|r| r.content_hash.as_deref() == Some(cur.hash.as_str())),
            };
            if known {
                std::fs::rename(&pending, path).map_err(|e| format!("could not finish the interrupted save of {}: {e}", path.display()))?;
                out.completed_pending = true;
                return Ok(out);
            }
            out.pending_left = Some(format!(
                "{} matches the ledger head but the working file holds unrecorded content; the external edit is recorded and the pending file is left in place",
                pending.display()
            ));
        } else {
            out.pending_left = Some(format!(
                "{} does not match the ledger head ({}); it was left in place",
                pending.display(),
                live_hash.as_deref().unwrap_or("no live version")
            ));
        }
    }

    let snap = snapshot_file(path)?;
    let append = |snap: &FileSnap, summary: &str| -> Result<VersionRecord, String> {
        match &snap.bytes {
            Some(b) => ledger.put_as(kind, id, b, "external", "external", summary),
            None => ledger.put_reference_as(kind, id, &snap.hash, snap.size, "external", "external", summary),
        }
    };
    match (&head, snap) {
        (None, None) => {}
        (None, Some(s)) => out.record = Some(append(&s, "Existing file adopted into the ledger")?),
        (Some(h), None) => {
            if !h.is_tombstone() {
                out.record = Some(ledger.tombstone(kind, id, "external", "File removed outside APEX")?);
            }
        }
        (Some(h), Some(s)) => {
            if h.is_tombstone() {
                out.record = Some(append(&s, "File re-created outside APEX")?);
            } else if h.content_hash.as_deref() != Some(s.hash.as_str()) {
                out.record = Some(append(&s, "File changed outside APEX")?);
            }
        }
    }
    Ok(out)
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    /// A temp directory that is removed when the test ends.
    struct Tmp(PathBuf);

    impl std::ops::Deref for Tmp {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tmp(name: &str) -> Tmp {
        let dir = std::env::temp_dir().join(format!("apex-ledger-{name}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Tmp(dir)
    }

    fn open(dir: &Path) -> Ledger {
        Ledger::open(&dir.join("ledger")).unwrap()
    }

    fn seg1(dir: &Path) -> PathBuf {
        dir.join("ledger").join("records-0001.jsonl")
    }

    fn lines_of(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path).unwrap().lines().map(String::from).collect()
    }

    fn write_lines(path: &Path, lines: &[String]) {
        let mut text = lines.join("\n");
        text.push('\n');
        std::fs::write(path, text).unwrap();
    }

    fn kinds(report: &VerifyReport) -> Vec<&str> {
        report.problems.iter().map(|p| p.kind.as_str()).collect()
    }

    /// Four versions of two objects plus a few events.
    fn populated(dir: &Path) -> Ledger {
        let l = open(dir);
        l.put(Kind::Strategy, "a.py", b"v1", "user", "first").unwrap();
        l.put(Kind::Strategy, "a.py", b"v2", "user", "second").unwrap();
        l.put(Kind::Estimator, "e.py", b"est", "copilot", "est").unwrap();
        l.put(Kind::Strategy, "a.py", b"v3", "user", "third").unwrap();
        for i in 0..3 {
            l.append_event("activity", serde_json::json!({"n": i})).unwrap();
            l.append_event("orders", serde_json::json!({"order_id": format!("o{i}"), "event": "submitted"})).unwrap();
        }
        assert!(l.verify().ok);
        l
    }

    #[test]
    fn versions_noop_tombstone_recreate_restore_and_history_order() {
        let dir = tmp("versions");
        let l = open(&dir);
        let v1 = l.put(Kind::Strategy, "s.py", b"one", "user", "create").unwrap();
        assert_eq!((v1.version, v1.action.as_str(), v1.parent_version), (1, "create", None));
        let again = l.put(Kind::Strategy, "s.py", b"one", "user", "same").unwrap();
        assert_eq!(again, v1, "identical content is a no-op");
        assert_eq!(l.history(Kind::Strategy, "s.py").len(), 1);
        let v2 = l.put(Kind::Strategy, "s.py", b"two", "copilot", "edit").unwrap();
        assert_eq!((v2.version, v2.action.as_str(), v2.parent_version), (2, "update", Some(1)));
        let del = l.tombstone(Kind::Strategy, "s.py", "user", "gone").unwrap();
        assert_eq!((del.version, del.action.as_str(), del.content_hash.clone()), (3, "delete", None));
        assert_eq!(l.tombstone(Kind::Strategy, "s.py", "user", "again").unwrap(), del, "deleting twice is a no-op");
        assert!(l.objects(Kind::Strategy, false).is_empty());
        let all = l.objects(Kind::Strategy, true);
        assert_eq!((all.len(), all[0].deleted, all[0].versions, all[0].head_version), (1, true, 3, 3));
        // The history of a deleted object stays readable.
        assert_eq!(l.content(Kind::Strategy, "s.py", 1).unwrap(), b"one");
        assert!(l.content(Kind::Strategy, "s.py", 3).is_err());
        // Re-creating the same id continues the same object, even with content equal to an older version.
        let v4 = l.put(Kind::Strategy, "s.py", b"two", "user", "back").unwrap();
        assert_eq!((v4.version, v4.action.as_str(), v4.parent_version), (4, "create", Some(3)));
        // Restore appends a new version with the old content.
        let (v5, bytes) = l.restore(Kind::Strategy, "s.py", 1, "user").unwrap();
        assert_eq!((v5.version, v5.action.as_str(), v5.restored_from, bytes.as_slice()), (5, "restore", Some(1), b"one".as_slice()));
        assert_eq!(v5.content_hash, v1.content_hash);
        assert_eq!(l.history(Kind::Strategy, "s.py").iter().map(|r| r.version).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
        assert_eq!(l.history(Kind::Strategy, "s.py")[0], v1, "old versions are never changed");
        assert_eq!(l.head(Kind::Strategy, "s.py").unwrap(), v5);
        assert!(l.restore(Kind::Strategy, "s.py", 3, "user").is_err(), "a tombstone has nothing to restore");
        assert!(l.restore(Kind::Strategy, "s.py", 99, "user").is_err());
        assert!(l.tombstone(Kind::Strategy, "nope", "user", "").is_err());
        // Seqs are gapless across objects and kinds.
        l.put(Kind::Model, "m1", b"{}", "user", "m").unwrap();
        let seqs: Vec<u64> = lines_of(&seg1(&dir)).iter().map(|l| serde_json::from_str::<VersionRecord>(l).unwrap().seq).collect();
        assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>());
        assert!(l.verify().ok);
    }

    #[test]
    fn a_restored_tombstoned_object_comes_back_and_references_cannot_be_restored() {
        let dir = tmp("restore-misc");
        let l = open(&dir);
        let v1 = l.put(Kind::Recipe, "r", b"x", "user", "").unwrap();
        let (rec, bytes) = l.restore(Kind::Recipe, "r", 1, "user").unwrap();
        assert_eq!((rec, bytes), (v1, b"x".to_vec()), "restoring the current content is a no-op");
        l.tombstone(Kind::Recipe, "r", "user", "").unwrap();
        let (back, _) = l.restore(Kind::Recipe, "r", 1, "user").unwrap();
        assert_eq!((back.version, back.action.as_str()), (3, "restore"));
        assert_eq!(l.objects(Kind::Recipe, false).len(), 1);
        let big = Ledger::sha256_hex(b"big dataset");
        l.put_reference(Kind::Dataset, "d.csv", &big, 20_000_000, "user", "ref").unwrap();
        assert!(!l.head(Kind::Dataset, "d.csv").unwrap().retained);
        assert!(l.restore(Kind::Dataset, "d.csv", 1, "user").is_err());
        assert!(l.content(Kind::Dataset, "d.csv", 1).is_err());
        assert!(l.put_reference(Kind::Dataset, "d.csv", "../../etc/passwd", 1, "user", "").is_err(), "reference hashes are validated");
        let r = l.verify();
        assert!(r.ok, "{:?}", r.problems);
    }

    #[test]
    fn a_clean_ledger_verifies() {
        let dir = tmp("clean");
        let l = populated(&dir);
        let r = l.verify();
        assert!(r.ok, "{:?}", r.problems);
        assert_eq!((r.checked_records, r.checked_events, r.checked_blobs), (4, 6, 4));
        assert_eq!(r.head_hash, l.status().head_hash);
        assert_eq!(l.status().ok, Some(true));
    }

    #[test]
    fn an_edited_record_line_is_reported() {
        let dir = tmp("edit");
        let l = populated(&dir);
        let mut lines = lines_of(&seg1(&dir));
        lines[1] = lines[1].replace("second", "SECOND");
        write_lines(&seg1(&dir), &lines);
        let r = l.verify();
        assert!(!r.ok);
        assert_eq!(kinds(&r), ["record_hash"]);
        assert!(r.problems[0].location.ends_with(":2"));
    }

    #[test]
    fn a_deleted_middle_line_is_reported() {
        let dir = tmp("delmid");
        let l = populated(&dir);
        let mut lines = lines_of(&seg1(&dir));
        lines.remove(1);
        write_lines(&seg1(&dir), &lines);
        let r = l.verify();
        assert!(!r.ok);
        assert!(kinds(&r).iter().all(|k| *k == "chain_link"), "{:?}", r.problems);
        assert!(r.problems.iter().any(|p| p.detail.contains("gap")));
        assert!(r.problems.iter().any(|p| p.detail.contains("prev_hash")));
    }

    #[test]
    fn swapped_lines_are_reported() {
        let dir = tmp("swap");
        let l = populated(&dir);
        let mut lines = lines_of(&seg1(&dir));
        lines.swap(1, 2);
        write_lines(&seg1(&dir), &lines);
        let r = l.verify();
        assert!(!r.ok);
        assert!(kinds(&r).contains(&"chain_link"), "{:?}", r.problems);
    }

    #[test]
    fn a_sequence_gap_with_a_consistent_hash_is_reported() {
        let dir = tmp("gap");
        let l = populated(&dir);
        let mut lines = lines_of(&seg1(&dir));
        // A forger who recomputes the hash still cannot hide the broken sequence.
        let mut last: VersionRecord = serde_json::from_str(lines.last().unwrap()).unwrap();
        last.seq += 5;
        last.hash = last.compute_hash();
        *lines.last_mut().unwrap() = serde_json::to_string(&last).unwrap();
        write_lines(&seg1(&dir), &lines);
        let r = l.verify();
        assert!(!r.ok);
        assert_eq!(kinds(&r), ["chain_link"]);
        assert!(r.problems[0].detail.contains("expected 4, found 9"), "{:?}", r.problems);
    }

    #[test]
    fn an_altered_blob_is_reported() {
        let dir = tmp("blob-alter");
        let l = populated(&dir);
        let hash = l.head(Kind::Estimator, "e.py").unwrap().content_hash.unwrap();
        let path = dir.join("ledger").join("blobs").join(&hash[..2]).join(&hash);
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"tampered").unwrap();
        let r = l.verify();
        assert!(!r.ok);
        assert_eq!(kinds(&r), ["blob_hash"]);
        assert!(l.content(Kind::Estimator, "e.py", 1).is_err(), "reads verify the blob hash too");
    }

    #[test]
    fn a_deleted_blob_is_reported() {
        let dir = tmp("blob-del");
        let l = populated(&dir);
        let hash = l.head(Kind::Estimator, "e.py").unwrap().content_hash.unwrap();
        std::fs::remove_file(dir.join("ledger").join("blobs").join(&hash[..2]).join(&hash)).unwrap();
        let r = l.verify();
        assert!(!r.ok);
        assert_eq!(kinds(&r), ["blob_missing"]);
    }

    #[test]
    fn a_truncated_tail_line_is_reported_and_appends_continue() {
        let dir = tmp("torn");
        let l = populated(&dir);
        let text = std::fs::read(seg1(&dir)).unwrap();
        std::fs::write(seg1(&dir), &text[..text.len() - 40]).unwrap();
        let r = l.verify();
        assert!(!r.ok);
        assert_eq!(kinds(&r), ["parse"]);
        assert!(r.problems[0].detail.contains("incomplete"));
        drop(l);
        // Reopen: the torn line is ignored, the next record continues the chain from the last good one,
        // and the torn fragment stays visible to verify instead of vanishing.
        let l = open(&dir);
        assert_eq!(l.history(Kind::Strategy, "a.py").len(), 2, "the torn record never counted");
        let rec = l.put(Kind::Strategy, "a.py", b"v4", "user", "after crash").unwrap();
        assert_eq!((rec.seq, rec.version), (4, 3));
        let r = l.verify();
        assert_eq!(kinds(&r), ["parse"]);
        assert_eq!(r.checked_records, 4);
        let text = std::fs::read_to_string(seg1(&dir)).unwrap();
        assert!(!text.contains("}{"), "the new record starts on its own line");
    }

    #[test]
    fn edited_and_deleted_stream_lines_are_reported() {
        let dir = tmp("stream");
        let l = populated(&dir);
        let path = dir.join("ledger").join("streams").join("orders.jsonl");
        let original = lines_of(&path);
        let mut lines = original.clone();
        lines[1] = lines[1].replace("\"o1\"", "\"oX\"");
        assert_ne!(lines[1], original[1]);
        write_lines(&path, &lines);
        let r = l.verify();
        assert!(!r.ok);
        assert_eq!(kinds(&r), ["stream_hash"]);
        let mut lines = original.clone();
        lines.remove(1);
        write_lines(&path, &lines);
        let r = l.verify();
        assert!(!r.ok);
        assert!(kinds(&r).iter().all(|k| *k == "stream_link"), "{:?}", r.problems);
        // The activity stream is untouched and still checked.
        assert_eq!(r.checked_events, 2 + 3);
    }

    #[test]
    fn concurrent_appends_keep_a_valid_gapless_chain() {
        let dir = tmp("concurrent");
        let l = open(&dir);
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let l = l.clone();
                std::thread::spawn(move || {
                    for n in 0..25 {
                        l.put(Kind::Strategy, &format!("s{t}.py"), format!("{t}-{n}").as_bytes(), "user", "x").unwrap();
                        l.append_event("activity", serde_json::json!({"t": t, "n": n})).unwrap();
                        l.append_event("orders", serde_json::json!({"order_id": format!("{t}-{n}")})).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let r = l.verify();
        assert!(r.ok, "{:?}", r.problems);
        assert_eq!((r.checked_records, r.checked_events), (200, 400));
        let seqs: Vec<u64> = lines_of(&seg1(&dir)).iter().map(|l| serde_json::from_str::<VersionRecord>(l).unwrap().seq).collect();
        assert_eq!(seqs, (1..=200).collect::<Vec<_>>());
        for t in 0..8 {
            assert_eq!(l.history(Kind::Strategy, &format!("s{t}.py")).len(), 25);
        }
    }

    #[test]
    fn a_long_scan_does_not_hold_the_lock_that_appends_need() {
        use std::sync::mpsc;
        use std::time::Duration;
        let dir = tmp("scan-lock");
        let l = Ledger::open_with(&dir.join("ledger"), 1500).unwrap();
        for n in 0..30 {
            l.append_event("orders", serde_json::json!({"order_id": format!("o{n}"), "event": "status"})).unwrap();
        }
        let (inside_tx, inside_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let reader = {
            let l = l.clone();
            std::thread::spawn(move || {
                let first = std::sync::atomic::AtomicBool::new(true);
                // The filter runs while read_events scans; hold the scan open on the first line.
                let events = l.read_events("orders", &|_| {
                    if first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                        let _ = inside_tx.send(());
                        let _ = release_rx.lock().unwrap().recv_timeout(Duration::from_secs(10));
                    }
                    true
                }, usize::MAX);
                events
            })
        };
        inside_rx.recv_timeout(Duration::from_secs(5)).expect("the scan started");

        // While the scan is open, an append (which may roll to a new segment) and a status call must finish.
        let (done_tx, done_rx) = mpsc::channel();
        let writer = {
            let l = l.clone();
            std::thread::spawn(move || {
                let appended = l.append_event("orders", serde_json::json!({"order_id": "late", "event": "status"})).is_ok();
                let _ = l.status();
                let _ = done_tx.send(appended);
            })
        };
        let finished = done_rx.recv_timeout(Duration::from_secs(3));
        let _ = release_tx.send(()); // always let the scan go, even when the assertion below fails
        assert_eq!(finished, Ok(true), "an append must not wait for a scan of the history");
        writer.join().unwrap();
        let events = reader.join().unwrap();
        // The scan saw the history as it was when it began: 30 events, newest first, a valid chain.
        assert_eq!(events.len(), 30);
        assert_eq!(events[0].value["order_id"], "o29");
        assert!(events.windows(2).all(|w| w[0].prev_hash != w[1].prev_hash && w[0].seq == w[1].seq + 1));
        assert_eq!(l.read_events("orders", &|_| true, 1)[0].value["order_id"], "late");
        let r = l.verify();
        assert!(r.ok, "{:?}", r.problems);
        assert_eq!(r.checked_events, 31);
    }

    #[test]
    fn an_outside_write_during_a_save_is_recorded_not_replaced() {
        let dir = tmp("save-race");
        let l = open(&dir);
        let path = dir.join("strategies").join("race.py");
        save_working_copy(&l, Kind::Strategy, "strategies/race.py", &path, b"v1", "user", "create").unwrap();

        // An editor writes the file after the save began and before its rename.
        let edit = || std::fs::write(&path, b"typed in an editor").unwrap();
        let err = save_working_copy_with(&l, Kind::Strategy, "strategies/race.py", &path, b"v2 from the app", "user", "edit", &edit).unwrap_err();
        assert!(err.contains("changed outside APEX"), "{err}");

        // The editor's content is still on disk and is the newest version; the app's save is in the history.
        assert_eq!(std::fs::read(&path).unwrap(), b"typed in an editor");
        let history = l.history(Kind::Strategy, "strategies/race.py");
        let contents: Vec<Vec<u8>> = history.iter().map(|r| l.content(Kind::Strategy, "strategies/race.py", r.version).unwrap()).collect();
        assert_eq!(contents, [b"v1".to_vec(), b"v2 from the app".to_vec(), b"typed in an editor".to_vec()]);
        assert_eq!((history[2].actor.as_str(), history[2].action.as_str()), ("external", "external"));
        assert!(!pending_path(&path).unwrap().exists(), "the pending file is cleaned up");
        // Nothing is left to reconcile, and the chain is intact.
        assert!(reconcile_file(&l, Kind::Strategy, "strategies/race.py", &path).unwrap().is_none());
        assert!(l.verify().ok);
    }

    #[test]
    fn a_normal_save_still_replaces_the_file_with_the_new_version() {
        let dir = tmp("save-normal");
        let l = open(&dir);
        let path = dir.join("strategies").join("plain.py");
        save_working_copy(&l, Kind::Strategy, "strategies/plain.py", &path, b"v1", "user", "create").unwrap();
        let rec = save_working_copy(&l, Kind::Strategy, "strategies/plain.py", &path, b"v2", "user", "edit").unwrap();
        assert_eq!(rec.version, 2);
        assert_eq!(std::fs::read(&path).unwrap(), b"v2");
    }

    #[test]
    fn segments_roll_and_the_chain_continues_across_them() {
        let dir = tmp("roll");
        let ledger_dir = dir.join("ledger");
        let l = Ledger::open_with(&ledger_dir, 1500).unwrap();
        for n in 0..40 {
            l.put(Kind::Notebook, "n.ipynb", format!("cell {n}").as_bytes(), "user", "edit").unwrap();
            l.append_event("activity", serde_json::json!({"n": n, "pad": "p".repeat(100)})).unwrap();
        }
        assert!(list_record_segments(&ledger_dir).len() > 2);
        assert!(list_stream_segments(&ledger_dir)["activity"].len() > 2);
        let r = l.verify();
        assert!(r.ok, "{:?}", r.problems);
        assert_eq!((r.checked_records, r.checked_events), (40, 40));
        // The first record of segment 2 references the last hash of segment 1.
        let first2: VersionRecord = serde_json::from_str(&lines_of(&ledger_dir.join("records-0002.jsonl"))[0]).unwrap();
        let last1: VersionRecord = serde_json::from_str(lines_of(&ledger_dir.join("records-0001.jsonl")).last().unwrap()).unwrap();
        assert_eq!(first2.prev_hash, last1.hash);
        // Newest-first reads span segments; reopening continues in the newest segment.
        let events = l.read_events("activity", &|_| true, 1000);
        assert_eq!(events.len(), 40);
        assert_eq!(events[0].value["n"], 39);
        let status = l.status();
        drop(l);
        let l = Ledger::open_with(&ledger_dir, 1500).unwrap();
        assert_eq!(l.status().head_hash, status.head_hash);
        l.put(Kind::Notebook, "n.ipynb", b"after", "user", "").unwrap();
        l.append_event("activity", serde_json::json!({"n": 40})).unwrap();
        assert!(l.verify().ok);
    }

    #[test]
    fn reopening_rebuilds_identical_state() {
        let dir = tmp("reopen");
        let l = populated(&dir);
        l.tombstone(Kind::Estimator, "e.py", "user", "bye").unwrap();
        let (status, hist, objs, events) = (l.status(), l.history(Kind::Strategy, "a.py"), l.objects(Kind::Estimator, true), l.read_events("orders", &|_| true, 100));
        drop(l);
        let l = open(&dir);
        let mut status2 = l.status();
        assert_eq!(status2.ok, None, "not verified since this open");
        status2.ok = status.ok;
        status2.last_verified_at = status.last_verified_at.clone();
        assert_eq!(status2, status);
        assert_eq!(l.history(Kind::Strategy, "a.py"), hist);
        assert_eq!(l.objects(Kind::Estimator, true), objs);
        assert_eq!(l.read_events("orders", &|_| true, 100), events);
        let next = l.append_event("orders", serde_json::json!({"order_id": "o9"})).unwrap();
        assert_eq!(next.seq, 4);
        assert_eq!(next.prev_hash, events[0].hash);
        assert!(l.verify().ok);
    }

    #[test]
    fn blobs_are_written_once_and_read_only() {
        let dir = tmp("blob-once");
        let l = open(&dir);
        l.put(Kind::Strategy, "a.py", b"same bytes", "user", "").unwrap();
        let hash = Ledger::sha256_hex(b"same bytes");
        let path = dir.join("ledger").join("blobs").join(&hash[..2]).join(&hash);
        let before = std::fs::metadata(&path).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        l.put(Kind::Strategy, "b.py", b"same bytes", "user", "same content, other object").unwrap();
        let after = std::fs::metadata(&path).unwrap();
        assert_eq!(before.modified().unwrap(), after.modified().unwrap(), "the blob was not rewritten");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(after.permissions().mode() & 0o777, 0o444);
        }
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1, "no temp files left behind");
        assert!(l.verify().ok);
    }

    #[test]
    fn a_damaged_blob_is_moved_aside_and_replaced_by_the_correct_bytes() {
        let dir = tmp("blob-repair");
        let l = open(&dir);
        l.put(Kind::Strategy, "a.py", b"good", "user", "").unwrap();
        let hash = Ledger::sha256_hex(b"good");
        let path = dir.join("ledger").join("blobs").join(&hash[..2]).join(&hash);
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"torn").unwrap();
        l.put(Kind::Strategy, "b.py", b"good", "user", "").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"good");
        let aside = std::fs::read_dir(path.parent().unwrap()).unwrap().flatten().any(|e| e.file_name().to_string_lossy().contains(".corrupt-"));
        assert!(aside, "the damaged copy is kept, not deleted");
    }

    #[test]
    fn content_over_the_cap_is_refused_with_a_clear_message() {
        let dir = tmp("cap");
        let l = open(&dir);
        let big = vec![7u8; MAX_BLOB_BYTES + 1];
        let err = l.put(Kind::Dataset, "big.csv", &big, "user", "").unwrap_err();
        assert!(err.contains("put_reference") && err.contains("8 MB"), "{err}");
        assert!(l.head(Kind::Dataset, "big.csv").is_none(), "nothing was recorded");
        l.put(Kind::Dataset, "ok.csv", &vec![7u8; MAX_BLOB_BYTES], "user", "exactly at the cap").unwrap();
        l.put_reference(Kind::Dataset, "big.csv", &Ledger::sha256_hex(&big), big.len() as u64, "user", "by hash").unwrap();
        assert!(l.verify().ok);
    }

    #[test]
    fn hostile_object_ids_are_data_and_never_file_names() {
        let dir = tmp("escape");
        let l = open(&dir);
        let ids = ["../../../etc/passwd", "..\\..\\evil", "/abs/path.py", "a\0b", "x/../../y", "con:aux", "名前.py"];
        for id in ids {
            l.put(Kind::Strategy, id, id.as_bytes(), "user", id).unwrap();
            assert_eq!(l.head(Kind::Strategy, id).unwrap().object_id, id);
            assert_eq!(l.content(Kind::Strategy, id, 1).unwrap(), id.as_bytes());
        }
        assert!(l.put(Kind::Strategy, "", b"x", "user", "").is_err());
        assert!(l.put(Kind::Strategy, &"x".repeat(MAX_ID_BYTES + 1), b"x", "user", "").is_err());
        let mut names: Vec<String> = Vec::new();
        fn walk(p: &Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(p).unwrap().flatten() {
                out.push(e.file_name().to_string_lossy().to_string());
                if e.path().is_dir() {
                    walk(&e.path(), out);
                }
            }
        }
        walk(&dir, &mut names);
        assert!(names.iter().all(|n| !n.contains("passwd") && !n.contains("evil") && !n.contains("..") && !n.contains('\\') && !n.contains("path")), "{names:?}");
        assert!(l.verify().ok);
        drop(l);
        assert_eq!(open(&dir).objects(Kind::Strategy, false).len(), ids.len());
    }

    #[test]
    fn events_validate_their_input_and_filter_newest_first() {
        let dir = tmp("events");
        let l = open(&dir);
        assert!(l.append_event("../x", serde_json::json!({})).is_err());
        assert!(l.append_event("orders", serde_json::json!([1])).is_err());
        assert!(l.append_event("orders", serde_json::json!({"hash": "x"})).is_err());
        assert!(l.append_event("orders", serde_json::json!({"ts": 5})).is_err());
        let e = l
            .append_event("orders", serde_json::json!({"ts": "2020-01-01T00:00:00Z", "order_id": "a", "data": {"price": 0.1, "qty": 3, "nested": {"z": 1, "a": 2}}}))
            .unwrap();
        assert_eq!(e.ts, "2020-01-01T00:00:00Z");
        l.append_event("orders", serde_json::json!({"order_id": "b"})).unwrap();
        l.append_event("orders", serde_json::json!({"order_id": "a", "event": "fill"})).unwrap();
        let got = l.read_events("orders", &|v| v["order_id"] == "a", 10);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].value["event"], "fill");
        assert_eq!(got[1].value["data"]["price"], 0.1);
        assert_eq!(l.read_events("orders", &|_| true, 2).len(), 2);
        assert!(l.read_events("nope", &|_| true, 2).is_empty());
        let flat = serde_json::to_value(&got[0]).unwrap();
        assert_eq!((flat["seq"].as_u64(), flat["order_id"].as_str()), (Some(3), Some("a")), "serializes flat like OrderEventDto");
        assert!(l.verify().ok);
    }

    #[test]
    fn the_record_hash_depends_on_every_field() {
        let base = VersionRecord {
            seq: 1,
            kind: "strategy".into(),
            object_id: "a.py".into(),
            version: 1,
            action: "create".into(),
            actor: "user".into(),
            ts: "2026-01-01T00:00:00.000Z".into(),
            content_hash: Some("ab".repeat(32)),
            size: 3,
            retained: true,
            parent_version: None,
            restored_from: None,
            summary: "s".into(),
            prev_hash: GENESIS_HASH.into(),
            hash: String::new(),
        };
        let h = base.compute_hash();
        assert_eq!(h, base.clone().compute_hash());
        assert_eq!(h.len(), 64);
        let variants: Vec<Box<dyn Fn(&mut VersionRecord)>> = vec![
            Box::new(|r| r.seq = 2),
            Box::new(|r| r.kind = "model".into()),
            Box::new(|r| r.object_id = "b.py".into()),
            Box::new(|r| r.version = 2),
            Box::new(|r| r.action = "update".into()),
            Box::new(|r| r.actor = "copilot".into()),
            Box::new(|r| r.ts = "2026-01-01T00:00:01.000Z".into()),
            Box::new(|r| r.content_hash = None),
            Box::new(|r| r.size = 4),
            Box::new(|r| r.retained = false),
            Box::new(|r| r.parent_version = Some(1)),
            Box::new(|r| r.restored_from = Some(1)),
            Box::new(|r| r.summary = "t".into()),
            Box::new(|r| r.prev_hash = "1".repeat(64)),
        ];
        for (i, change) in variants.iter().enumerate() {
            let mut r = base.clone();
            change(&mut r);
            assert_ne!(r.compute_hash(), h, "field #{i} is not covered by the hash");
        }
    }

    // ── working copies ──

    fn work(dir: &Path) -> PathBuf {
        dir.join("strategies").join("foo.py")
    }

    #[test]
    fn save_working_copy_writes_pending_then_ledger_then_renames() {
        let dir = tmp("save");
        let l = open(&dir);
        let path = work(&dir);
        let v1 = save_working_copy(&l, Kind::Strategy, "strategies/foo.py", &path, b"print(1)", "user", "create").unwrap();
        assert_eq!(v1.version, 1);
        assert_eq!(std::fs::read(&path).unwrap(), b"print(1)");
        assert!(!pending_path(&path).unwrap().exists());
        let v2 = save_working_copy(&l, Kind::Strategy, "strategies/foo.py", &path, b"print(2)", "user", "edit").unwrap();
        assert_eq!(v2.version, 2);
        assert_eq!(std::fs::read(&path).unwrap(), b"print(2)");
        // The older version is still in the ledger.
        assert_eq!(l.content(Kind::Strategy, "strategies/foo.py", 1).unwrap(), b"print(1)");
        // Saving identical content changes nothing (not even the file).
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(save_working_copy(&l, Kind::Strategy, "strategies/foo.py", &path, b"print(2)", "user", "same").unwrap(), v2);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);
        assert!(l.verify().ok);
    }

    #[test]
    fn a_failed_ledger_write_leaves_the_working_copy_untouched() {
        let dir = tmp("save-fail");
        let l = open(&dir);
        let path = work(&dir);
        save_working_copy(&l, Kind::Strategy, "s", &path, b"old", "user", "").unwrap();
        let err = save_working_copy(&l, Kind::Strategy, "", &path, b"new", "user", "").unwrap_err();
        assert!(err.contains("empty"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        assert!(!pending_path(&path).unwrap().exists(), "the temp file is cleaned up");
    }

    #[test]
    fn crash_after_ledger_put_completes_the_rename_at_startup() {
        let dir = tmp("crash-complete");
        let l = open(&dir);
        let path = work(&dir);
        save_working_copy(&l, Kind::Strategy, "s", &path, b"old", "user", "").unwrap();
        // Simulate: pending written, ledger put done, crash before the rename.
        l.put(Kind::Strategy, "s", b"new", "user", "edit").unwrap();
        std::fs::write(pending_path(&path).unwrap(), b"new").unwrap();
        let out = reconcile_file_detailed(&l, Kind::Strategy, "s", &path).unwrap();
        assert!(out.completed_pending && out.record.is_none() && out.pending_left.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(!pending_path(&path).unwrap().exists());
        assert_eq!(l.history(Kind::Strategy, "s").len(), 2, "no spurious external version");
    }

    #[test]
    fn crash_before_ledger_put_leaves_the_mismatched_pending_file_and_reports_it() {
        let dir = tmp("crash-mismatch");
        let l = open(&dir);
        let path = work(&dir);
        save_working_copy(&l, Kind::Strategy, "s", &path, b"old", "user", "").unwrap();
        std::fs::write(pending_path(&path).unwrap(), b"never recorded").unwrap();
        let out = reconcile_file_detailed(&l, Kind::Strategy, "s", &path).unwrap();
        assert!(!out.completed_pending && out.record.is_none());
        assert!(out.pending_left.as_deref().unwrap().contains("does not match"));
        assert_eq!(std::fs::read(pending_path(&path).unwrap()).unwrap(), b"never recorded", "left alone");
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        // A later save does not destroy the leftover either.
        save_working_copy(&l, Kind::Strategy, "s", &path, b"next", "user", "").unwrap();
        let found: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains("orphan")).collect();
        assert_eq!(found.len(), 1);
        assert_eq!(std::fs::read(found[0].path()).unwrap(), b"never recorded");
    }

    #[test]
    fn an_external_edit_becomes_an_external_version() {
        let dir = tmp("external");
        let l = open(&dir);
        let path = work(&dir);
        save_working_copy(&l, Kind::Strategy, "s", &path, b"mine", "user", "").unwrap();
        assert!(reconcile_file(&l, Kind::Strategy, "s", &path).unwrap().is_none(), "in step: nothing to do");
        std::fs::write(&path, b"edited in vim").unwrap();
        let rec = reconcile_file(&l, Kind::Strategy, "s", &path).unwrap().unwrap();
        assert_eq!((rec.version, rec.action.as_str(), rec.actor.as_str()), (2, "external", "external"));
        assert_eq!(l.content(Kind::Strategy, "s", 2).unwrap(), b"edited in vim");
        assert_eq!(l.content(Kind::Strategy, "s", 1).unwrap(), b"mine");
        assert!(reconcile_file(&l, Kind::Strategy, "s", &path).unwrap().is_none());
        // An app save after an unnoticed external edit records the edit first, then saves on top.
        std::fs::write(&path, b"edited again").unwrap();
        let v = save_working_copy(&l, Kind::Strategy, "s", &path, b"app version", "user", "").unwrap();
        assert_eq!(v.version, 4);
        assert_eq!(l.content(Kind::Strategy, "s", 3).unwrap(), b"edited again");
        assert_eq!(std::fs::read(&path).unwrap(), b"app version");
    }

    #[test]
    fn an_unknown_existing_file_is_adopted_and_a_missing_file_becomes_an_external_delete() {
        let dir = tmp("adopt");
        let l = open(&dir);
        let path = work(&dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"pre-existing").unwrap();
        let rec = reconcile_file(&l, Kind::Strategy, "s", &path).unwrap().unwrap();
        assert_eq!((rec.version, rec.action.as_str(), rec.actor.as_str()), (1, "external", "external"));
        std::fs::remove_file(&path).unwrap();
        let del = reconcile_file(&l, Kind::Strategy, "s", &path).unwrap().unwrap();
        assert_eq!((del.version, del.action.as_str(), del.actor.as_str()), (2, "delete", "external"));
        assert!(reconcile_file(&l, Kind::Strategy, "s", &path).unwrap().is_none(), "already recorded");
        assert!(reconcile_file(&l, Kind::Strategy, "never", &dir.join("none.py")).unwrap().is_none());
        // The file coming back is recorded too, continuing the same object.
        std::fs::write(&path, b"back").unwrap();
        let back = reconcile_file(&l, Kind::Strategy, "s", &path).unwrap().unwrap();
        assert_eq!((back.version, back.action.as_str()), (3, "external"));
        assert_eq!(l.content(Kind::Strategy, "s", 1).unwrap(), b"pre-existing");
    }

    #[test]
    fn large_working_files_are_recorded_by_hash() {
        let dir = tmp("large-file");
        let l = open(&dir);
        let path = dir.join("data").join("big.csv");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let big = vec![b'x'; MAX_BLOB_BYTES + 10];
        std::fs::write(&path, &big).unwrap();
        let rec = reconcile_file(&l, Kind::Dataset, "big.csv", &path).unwrap().unwrap();
        assert!(!rec.retained);
        assert_eq!((rec.content_hash.clone(), rec.size), (Some(Ledger::sha256_hex(&big)), big.len() as u64));
        assert!(reconcile_file(&l, Kind::Dataset, "big.csv", &path).unwrap().is_none());
    }

    #[test]
    fn a_directory_in_place_of_a_file_is_an_error_not_a_panic() {
        let dir = tmp("dir-as-file");
        let l = open(&dir);
        let path = dir.join("thing.py");
        std::fs::create_dir_all(&path).unwrap();
        assert!(reconcile_file(&l, Kind::Strategy, "s", &path).is_err());
        assert!(save_working_copy(&l, Kind::Strategy, "s", &path, b"x", "user", "").is_err());
    }

    #[test]
    fn the_same_directory_opens_to_one_shared_writer() {
        let dir = tmp("shared");
        let a = open(&dir);
        let b = open(&dir);
        a.put(Kind::Model, "m", b"1", "user", "").unwrap();
        b.put(Kind::Model, "m", b"2", "user", "").unwrap();
        assert_eq!(a.history(Kind::Model, "m").len(), 2, "one chain, not two forks");
        assert!(a.verify().ok);
    }

    #[test]
    fn kinds_round_trip() {
        for k in [Kind::Strategy, Kind::Estimator, Kind::Recipe, Kind::Dataset, Kind::Model, Kind::Automation, Kind::Notebook] {
            assert_eq!(k.as_str().parse::<Kind>().unwrap(), k);
            assert_eq!(serde_json::to_string(&k).unwrap(), format!("\"{}\"", k.as_str()));
        }
        assert!("nonsense".parse::<Kind>().is_err());
    }
}
