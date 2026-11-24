//! Antigravity IDE and agy CLI native JSONL histories.
use super::{
    bounded_redacted_output, clean_path, dedup_paths, parse_ts, redacted_truncate, root_scope,
    title_from_messages, Adapter, Discovered, Store, UnavailableMetadata,
};
use crate::model::{EditEvent, EditKind, Message, Role, Session, ToolCall, ToolResult};
use crate::util::{canonicalize_as_recorded, short_id};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Antigravity {
    home: Option<PathBuf>,
    variant: &'static str,
    degraded: std::sync::Arc<std::sync::Mutex<HashMap<String, UnavailableMetadata>>>,
    summaries: std::sync::Arc<std::sync::Mutex<HashMap<String, Option<Summary>>>>,
}
impl Default for Antigravity {
    fn default() -> Self {
        Self::ide()
    }
}
impl Antigravity {
    pub fn ide() -> Self {
        Self {
            home: None,
            degraded: Default::default(),
            variant: "antigravity",
            summaries: Default::default(),
        }
    }
    pub fn cli() -> Self {
        Self {
            home: None,
            degraded: Default::default(),
            variant: "antigravity-cli",
            summaries: Default::default(),
        }
    }
    /// The Gemini configuration directory (normally ~/.gemini), as passed to
    /// agy's --gemini-dir. This is not the OS user's home directory.
    pub fn in_home(home: impl Into<PathBuf>) -> Self {
        Self {
            home: Some(home.into()),
            ..Self::ide()
        }
    }
    pub fn cli_in_home(home: impl Into<PathBuf>) -> Self {
        Self {
            home: Some(home.into()),
            ..Self::cli()
        }
    }
}

const FILES: [&str; 4] = [
    ".system_generated/logs/transcript_full.jsonl",
    ".system_generated/logs/transcript.jsonl",
    "transcript_full.jsonl",
    "transcript.jsonl",
];

fn sources(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for name in FILES {
        let path = dir.join(name);
        match std::fs::metadata(&path) {
            Ok(m) if m.is_file() => {
                anyhow::ensure!(
                    canonicalize_as_recorded(&path)?.starts_with(canonicalize_as_recorded(dir)?),
                    "transcript escapes its conversation directory"
                );
                out.push(path);
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                for node in path.ancestors().take_while(|p| *p != dir) {
                    anyhow::ensure!(
                        !std::fs::symlink_metadata(node).is_ok_and(|m| m.file_type().is_symlink()),
                        "unreadable transcript symlink in {}",
                        path.display()
                    );
                }
            }
            Err(e) => return Err(e).with_context(|| format!("stat {}", path.display())),
        }
    }
    Ok(out)
}
fn preferred(dir: &Path) -> Result<PathBuf> {
    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    struct Coverage {
        last_step: Option<u64>,
        terminal_steps: usize,
        full: bool,
    }
    let mut best: Option<(PathBuf, Coverage)> = None;
    for path in sources(dir)? {
        if path.metadata()?.len() == 0 {
            continue;
        }
        // Prefer coverage, then completion, then richer full output. Compact
        // logs normally flush slightly later even when their state is identical.
        let mut last_step = None;
        let mut terminal_steps = 0;
        crate::util::for_each_line(&path, |line| {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                if let Some(step) = record_id(&v) {
                    last_step = Some(last_step.map_or(step, |old: u64| old.max(step)));
                }
                if matches!(
                    v.get("status").and_then(Value::as_str),
                    Some("DONE" | "ERROR" | "FAILED" | "CANCELLED")
                ) {
                    terminal_steps += 1;
                }
            }
        })?;
        let score = Coverage {
            last_step,
            terminal_steps,
            full: path
                .file_name()
                .is_some_and(|n| n == "transcript_full.jsonl"),
        };
        let newer = best.as_ref().is_none_or(|(_, old)| score > *old);
        if newer {
            best = Some((path, score));
        }
    }
    best.map(|(path, _)| path)
        .with_context(|| format!("no readable transcript in {}", dir.display()))
}
struct Token(u64);
impl Token {
    fn new() -> Self {
        Self(0xcbf29ce484222325)
    }
    fn bytes(&mut self, bytes: &[u8]) {
        for b in (bytes.len() as u64).to_le_bytes().iter().chain(bytes) {
            self.0 = (self.0 ^ u64::from(*b)).wrapping_mul(0x100000001b3);
        }
    }
    fn finish(&self) -> u64 {
        self.0
    }
}
fn change_token(files: &[PathBuf]) -> Result<i64> {
    let mut h = Token::new();
    for path in files {
        h.bytes(path.as_os_str().as_encoded_bytes());
        let m = path.metadata()?;
        h.bytes(&m.len().to_le_bytes());
        let nanos = m
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        h.bytes(&nanos.to_le_bytes());
    }
    Ok(((h.finish() & ((1 << 62) - 1)) | (1 << 62)) as i64)
}
impl Adapter for Antigravity {
    fn name(&self) -> &'static str {
        "antigravity"
    }
    fn root(&self) -> Option<PathBuf> {
        let home = self
            .home
            .clone()
            .or_else(|| dirs::home_dir().map(|p| p.join(".gemini")))?;
        let root = home.join(self.variant).join("brain");
        Some(canonicalize_as_recorded(&root).unwrap_or(root))
    }
    fn reconcile_scope(&self) -> Option<String> {
        root_scope(self.root().as_deref())
    }
    fn discover(&self) -> Discovered {
        Vec::new().into()
    }
    fn store(&self) -> Option<Store> {
        let root = self.root()?;
        if !root.exists() {
            return None;
        }
        let mut keys = Vec::new();
        let mut files = Vec::new();
        let mut had_error = false;
        let db = root.parent()?.join("conversation_summaries.db");
        let (summaries, whole_failure) = match read_summaries(&db, None) {
            Ok(rows) => (rows, false),
            Err(e) => {
                eprintln!("[antigravity] summary DB {} failed: {e:#}; existing indexed metadata preserved; new conversations indexed with transcript metadata", db.display());
                (HashMap::new(), true)
            }
        };
        let mut degraded = HashMap::new();
        let mut cache = HashMap::new();
        match std::fs::read_dir(&root) {
            Err(_) => had_error = true,
            Ok(entries) => {
                for entry in entries {
                    let entry = match entry {
                        Ok(e) => e,
                        Err(_) => {
                            had_error = true;
                            continue;
                        }
                    };
                    match entry.file_type() {
                        Ok(kind) if kind.is_dir() => {}
                        Ok(_) => continue,
                        Err(_) => {
                            had_error = true;
                            continue;
                        }
                    }
                    let dir = entry.path();
                    match sources(&dir).and_then(|paths| Ok((change_token(&paths)?, paths))) {
                        Ok((token, paths)) if !paths.is_empty() => {
                            let key = dir.to_string_lossy().into_owned();
                            let summary = dir
                                .file_name()
                                .and_then(|n| n.to_str())
                                .and_then(|id| summaries.get(id))
                                .cloned();
                            let unavailable = if whole_failure {
                                UnavailableMetadata {
                                    title: true,
                                    project: true,
                                    subagent: true,
                                }
                            } else {
                                summary.as_ref().map(|s| s.degraded).unwrap_or_default()
                            };
                            degraded.insert(key.clone(), unavailable);
                            let mut h = Token::new();
                            h.bytes(&[
                                u8::from(unavailable.title),
                                u8::from(unavailable.project),
                                u8::from(unavailable.subagent),
                            ]);
                            h.bytes(&token.to_le_bytes());
                            h.bytes(&[u8::from(summary.is_some())]);
                            if let Some(s) = &summary {
                                for field in [&s.title, &s.project, &s.parent] {
                                    h.bytes(field.as_bytes());
                                }
                                h.bytes(&[u8::from(s.updated.is_some())]);
                                if let Some(t) = s.updated {
                                    h.bytes(&t.timestamp_millis().to_le_bytes());
                                }
                            }
                            let token = ((h.finish() & ((1 << 62) - 1)) | (1 << 62)) as i64;
                            cache.insert(key.clone(), summary);
                            keys.push((key, token));
                            files.extend(paths);
                        }
                        Ok(_) => {}
                        Err(_) => had_error = true,
                    }
                }
            }
        }
        *self.degraded.lock().unwrap_or_else(|e| e.into_inner()) = degraded;
        keys.sort();
        *self.summaries.lock().unwrap_or_else(|e| e.into_inner()) = cache;
        Some(Store {
            keys,
            files,
            had_error,
        })
    }
    fn unavailable_metadata(&self, key: &str) -> UnavailableMetadata {
        self.degraded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .copied()
            .unwrap_or_default()
    }
    fn modified_at(&self, key: &str) -> Option<DateTime<Utc>> {
        let log_time = sources(Path::new(key))
            .ok()?
            .iter()
            .filter_map(|p| p.metadata().ok()?.modified().ok())
            .max()
            .map(DateTime::<Utc>::from);
        log_time
            .into_iter()
            .chain(self.summary(key).and_then(|s| s.updated))
            .max()
    }
    fn sync_activity_at(&self, key: &str, already_indexed: bool) -> Option<DateTime<Utc>> {
        if !already_indexed {
            return self.modified_at(key);
        }
        let db = Path::new(key)
            .parent()?
            .parent()?
            .join("conversation_summaries.db");
        [db.clone(), PathBuf::from(format!("{}-wal", db.display()))]
            .iter()
            .filter_map(|p| p.metadata().ok()?.modified().ok())
            .map(DateTime::<Utc>::from)
            .chain(self.modified_at(key))
            .max()
    }
    fn parse_key(&self, key: &str) -> Result<Session> {
        let session = parse_transcript(&preferred(Path::new(key))?, key)?;
        Ok(self.with_summary(session, key))
    }
    fn parse(&self, path: &Path) -> Result<Session> {
        if path.is_dir() {
            return self.parse_key(&path.to_string_lossy());
        }
        let parent = path.parent().context("transcript has no parent")?;
        let dir = if parent.file_name().is_some_and(|n| n == "logs") {
            parent
                .parent()
                .and_then(Path::parent)
                .context("invalid Antigravity log path")?
        } else {
            parent
        };
        let key = dir.to_string_lossy();
        Ok(self.with_summary(parse_transcript(path, &key)?, &key))
    }
}
#[derive(Clone, Debug)]
struct Summary {
    degraded: UnavailableMetadata,
    title: String,
    project: String,
    parent: String,
    updated: Option<DateTime<Utc>>,
}
impl Antigravity {
    fn with_summary(&self, mut session: Session, key: &str) -> Session {
        let summary = self.summary(key);
        if let Some(summary) = summary {
            if !summary.project.is_empty() {
                session.project = summary.project;
            }
            if !summary.title.trim().is_empty() {
                session.title = redacted_truncate(&summary.title, 80);
            }
            session.subagent = !summary.parent.is_empty();
        }
        session
    }
    fn summary(&self, key: &str) -> Option<Summary> {
        let cached = self
            .summaries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned();
        cached.unwrap_or_else(|| {
            let dir = Path::new(key);
            let id = dir.file_name()?.to_str()?;
            let db = dir.parent()?.parent()?.join("conversation_summaries.db");
            read_summaries(&db, Some(id)).ok()?.remove(id)
        })
    }
}
fn read_summaries(db: &Path, id: Option<&str>) -> Result<HashMap<String, Summary>> {
    use rusqlite::{Connection, OpenFlags};
    match db.metadata() {
        Ok(metadata) => anyhow::ensure!(metadata.is_file(), "summary database is not a file"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(e.into()),
    }
    let read = |conn: Connection| -> Result<HashMap<String, Summary>> {
        let columns = conn
            .prepare("PRAGMA table_info(conversation_summaries)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        anyhow::ensure!(
            columns.iter().any(|c| c == "conversation_id"),
            "missing conversation_summaries/conversation_id"
        );
        let projection = |name: &str| {
            if columns.iter().any(|c| c == name) {
                name.to_owned()
            } else {
                "NULL".to_owned()
            }
        };
        let title = projection("title");
        let workspace = projection("workspace_uris");
        let parent = projection("parent_conversation_id");
        let updated = projection("last_modified_time");
        let missing_title = title == "NULL";
        let missing_project = workspace == "NULL";
        let missing_parent = parent == "NULL";
        let mut stmt = conn.prepare(&format!(
            "SELECT conversation_id,{title},{workspace},{parent},{updated} FROM conversation_summaries {}",
            if id.is_some() {
                "WHERE conversation_id=?1"
            } else {
                "WHERE ?1 IS NULL"
            }
        ))?;
        let rows = stmt.query_map([id], |r| {
            Ok((
                r.get::<_, String>(0).ok(),
                r.get::<_, Option<String>>(1),
                r.get::<_, Option<String>>(2),
                r.get::<_, Option<String>>(3),
                r.get::<_, rusqlite::types::Value>(4)?,
            ))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (id, title, uris, parent, updated) = row?;
            let Some(id) = id else {
                eprintln!("[antigravity] skipped summary with invalid ID");
                anyhow::bail!("undecodable summary row ID; metadata cannot be attributed safely");
            };
            let mut degraded = UnavailableMetadata {
                title: missing_title || title.is_err(),
                project: missing_project || uris.is_err(),
                subagent: missing_parent || parent.is_err(),
            };
            let title = title.ok().flatten().unwrap_or_default();
            let uris = uris.ok().flatten().unwrap_or_default();
            let parent = parent.ok().flatten().unwrap_or_default();
            let updated = match updated {
                rusqlite::types::Value::Text(s) => {
                    parse_ts(&s.replacen(' ', "T", 1)).or_else(|| {
                        DateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S%.f %:z")
                            .ok()
                            .map(|t| t.with_timezone(&Utc))
                    })
                }
                rusqlite::types::Value::Integer(n) => {
                    if n.unsigned_abs() > 100_000_000_000 {
                        DateTime::from_timestamp_millis(n)
                    } else {
                        DateTime::from_timestamp(n, 0)
                    }
                }
                _ => None,
            };
            let uris: Vec<String> = if uris.trim().is_empty() {
                Vec::new()
            } else {
                match serde_json::from_str(&uris) {
                    Ok(uris) => uris,
                    Err(_) => {
                        degraded.project = true;
                        Vec::new()
                    }
                }
            };
            if degraded.any() {
                eprintln!("[antigravity] summary {id} has unavailable fields; existing indexed metadata preserved; fresh sessions use available fields");
            }
            let project = uris.first().and_then(|s| file_uri(s)).unwrap_or_default();
            out.insert(
                id,
                Summary {
                    degraded,
                    title,
                    project,
                    parent,
                    updated,
                },
            );
        }
        Ok(out)
    };
    read(Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?)
}
fn file_uri(uri: &str) -> Option<String> {
    let path = uri.strip_prefix("file://")?;
    let path = path.strip_prefix("localhost").unwrap_or(path);
    if !path.starts_with('/') {
        return None;
    }
    let mut bytes = Vec::new();
    let mut i = 0;
    while i < path.len() {
        if path.as_bytes()[i] == b'%' && i + 2 < path.len() {
            if let (Some(hi), Some(lo)) = (
                (path.as_bytes()[i + 1] as char).to_digit(16),
                (path.as_bytes()[i + 2] as char).to_digit(16),
            ) {
                bytes.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        bytes.push(path.as_bytes()[i]);
        i += 1;
    }
    let decoded = String::from_utf8(bytes).ok()?;
    Some(if decoded.as_bytes().get(2) == Some(&b':') {
        decoded[1..].to_owned()
    } else {
        decoded
    })
}

fn stamp(v: &Value) -> Option<DateTime<Utc>> {
    v.get("created_at")?.as_str().and_then(parse_ts)
}
fn kind(v: &Value) -> &str {
    v.get("type").and_then(Value::as_str).unwrap_or("")
}
// Sort indexed entries within their original slots; missing indices retain line order.
fn order_steps(steps: &mut [Value]) {
    let mut ordered: Vec<Value> = steps
        .iter()
        .filter(|v| record_id(v).is_some())
        .cloned()
        .collect();
    ordered.sort_by_key(record_id);
    let mut ordered = ordered.into_iter();
    for step in steps {
        if record_id(step).is_some() {
            *step = ordered.next().unwrap();
        }
    }
}
fn is_tool_result_step(v: &Value) -> bool {
    if matches!(
        v.get("source").and_then(Value::as_str),
        Some("SYSTEM" | "USER_EXPLICIT")
    ) {
        return false;
    }
    if matches!(
        kind(v),
        "" | "UNSPECIFIED"
            | "USER_INPUT"
            | "PLANNER_RESPONSE"
            | "EPHEMERAL_MESSAGE"
            | "CONVERSATION_HISTORY"
            | "CHECKPOINT"
            | "SYSTEM_MESSAGE"
            | "TASK_BOUNDARY"
            | "KNOWLEDGE_ARTIFACTS"
            | "KNOWLEDGE_GENERATION"
            | "DIRECTORY_RULES"
            | "TOOL_CALL_PROPOSAL"
            | "TOOL_CALL_CHOICE"
            | "PROPOSAL_FEEDBACK"
            | "MANAGER_FEEDBACK"
            | "SUGGESTED_RESPONSES"
            | "CODE_ACKNOWLEDGEMENT"
            | "ERROR_MESSAGE"
            | "WAIT"
            | "PLAN_INPUT"
            | "BRAIN_UPDATE"
            | "KI_INSERTION"
            | "FINISH"
            | "DUMMY"
            | "CIDER_AGENT_DUMMY"
    ) {
        return false;
    }
    v.get("status").and_then(Value::as_str).is_some()
        || v.get("is_error").and_then(Value::as_bool).is_some()
        || v.get("content").and_then(Value::as_str).is_some()
}
fn record_id(v: &Value) -> Option<u64> {
    v.get("step_index").and_then(Value::as_u64)
}
fn args(tc: &Value) -> Value {
    let value = tc
        .get("args")
        .or_else(|| tc.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    match value {
        Value::String(ref text) => serde_json::from_str(text).unwrap_or(value),
        _ => value,
    }
}
fn redact_args(v: &mut Value) {
    match v {
        Value::String(s) => *s = crate::redact::redact(s).into_owned(),
        Value::Array(a) => a.iter_mut().for_each(redact_args),
        Value::Object(o) => o.values_mut().for_each(redact_args),
        _ => {}
    }
}
fn user_text(s: &str) -> &str {
    s.split_once("<USER_REQUEST>")
        .and_then(|(_, s)| s.split_once("</USER_REQUEST>").map(|(s, _)| s))
        .unwrap_or(s)
        .trim()
}
fn edit(name: &str, input: &Value, ts: Option<DateTime<Utc>>) -> Option<EditEvent> {
    let kind = match name {
        "write_file" | "write_to_file" => EditKind::Write,
        "edit_file" | "replace_file_content" | "multi_replace_file_content" => EditKind::Edit,
        _ => return None,
    };
    let path = ["Path", "path", "file_path", "TargetFile"]
        .iter()
        .find_map(|k| input.get(k)?.as_str())?;
    let text = [
        "Replacement",
        "replacement",
        "ReplacementContent",
        "Content",
        "content",
        "CodeContent",
    ]
    .iter()
    .find_map(|k| input.get(k).and_then(Value::as_str))
    .unwrap_or("");
    let old = input
        .get("TargetContent")
        .and_then(Value::as_str)
        .unwrap_or("");
    let text = if old.is_empty() {
        text.to_owned()
    } else {
        format!("-{old}\n+{text}")
    };
    Some(EditEvent {
        path: clean_path(path)?,
        kind,
        ts,
        snippet: redacted_truncate(&text, 200),
    })
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum ToolFamily {
    Generic,
    Edit,
    Directory,
    View,
    Command,
    Search,
    Web,
    Other(String),
}
fn call_family(name: &str) -> ToolFamily {
    match name {
        "write_file"
        | "write_to_file"
        | "edit_file"
        | "replace_file_content"
        | "multi_replace_file_content"
        | "code_action"
        | "file_change"
        | "edit_notebook"
        | "write_blob" => ToolFamily::Edit,
        "list_dir" | "list_directory" => ToolFamily::Directory,
        "view_file" | "read_file" | "view_file_outline" | "view_code_item" => ToolFamily::View,
        "run_command" | "command_status" | "shell_exec" => ToolFamily::Command,
        "grep_search" | "find_by_name" | "code_search" => ToolFamily::Search,
        "search_web" => ToolFamily::Web,
        _ => ToolFamily::Other(name.to_owned()),
    }
}
fn result_family(name: &str) -> ToolFamily {
    match name {
        "GENERIC" => ToolFamily::Generic,
        "CODE_ACTION" | "FILE_CHANGE" | "EDIT_NOTEBOOK" | "WRITE_BLOB" => ToolFamily::Edit,
        "LIST_DIRECTORY" => ToolFamily::Directory,
        "VIEW_FILE" | "VIEW_FILE_OUTLINE" | "VIEW_CODE_ITEM" => ToolFamily::View,
        "RUN_COMMAND" | "SHELL_EXEC" | "COMMAND_STATUS" => ToolFamily::Command,
        "GREP_SEARCH" | "FIND" | "CODE_SEARCH" => ToolFamily::Search,
        "SEARCH_WEB" => ToolFamily::Web,
        _ => ToolFamily::Other(name.to_ascii_lowercase()),
    }
}
fn take_pending(pending: &mut VecDeque<Pending>, family: &ToolFamily) -> Option<Pending> {
    let index = if *family == ToolFamily::Generic {
        0
    } else {
        pending.iter().position(|p| p.family == *family)?
    };
    pending.remove(index)
}
enum CompactStepKind {
    User,
    Planner(Vec<(Option<EditEvent>, Option<String>, ToolFamily)>),
    Result(bool, ToolFamily),
    Other,
}
struct CompactEditStep {
    step_index: Option<u64>,
    ts: Option<DateTime<Utc>>,
    cwd: Option<String>,
    kind: CompactStepKind,
}
impl CompactEditStep {
    fn placeholder() -> Self {
        Self {
            step_index: Some(u64::MAX),
            ts: None,
            cwd: None,
            kind: CompactStepKind::Other,
        }
    }
}
struct Pending {
    family: ToolFamily,
    id: String,
    edit: Option<EditEvent>,
}
fn parse_transcript(path: &Path, key: &str) -> Result<Session> {
    let (lines, windowed) = crate::util::session_lines_keeping(path, |line| {
        serde_json::from_str::<Value>(line)
            .is_ok_and(|v| matches!(kind(&v), "USER_INPUT" | "PLANNER_RESPONSE"))
    })?;
    let mut retained: Vec<Value> = lines
        .iter()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    order_steps(&mut retained);
    let wanted: HashSet<u64> = retained
        .iter()
        .filter(|v| is_tool_result_step(v))
        .filter_map(record_id)
        .collect();
    let mut links: HashMap<u64, String> = HashMap::new();
    let mut pending = VecDeque::<Pending>::new();
    let mut edits = Vec::new();
    let mut started: Option<DateTime<Utc>> = None;
    let mut ended: Option<DateTime<Utc>> = None;
    let mut project = None;
    let mut all = Vec::new();
    crate::util::for_each_line(path, |line| {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let ts = stamp(&v);
        if let Some(t) = ts {
            started = Some(started.map_or(t, |s| s.min(t)));
            ended = Some(ended.map_or(t, |s| s.max(t)));
        }
        let cwd = v
            .get("cwd")
            .or_else(|| v.get("workspace"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let kind = match kind(&v) {
            "USER_INPUT" => CompactStepKind::User,
            "PLANNER_RESPONSE" => {
                let calls = v
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|tc| {
                        let input = args(tc);
                        let cwd = input
                            .get("Cwd")
                            .or_else(|| input.get("cwd"))
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        (
                            edit(
                                tc.get("name").and_then(Value::as_str).unwrap_or("?"),
                                &input,
                                ts,
                            ),
                            cwd,
                            call_family(tc.get("name").and_then(Value::as_str).unwrap_or("?")),
                        )
                    })
                    .collect();
                CompactStepKind::Planner(calls)
            }
            _ if is_tool_result_step(&v) => CompactStepKind::Result(
                v.get("status").and_then(Value::as_str) == Some("DONE")
                    && !v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                result_family(kind(&v)),
            ),
            _ => CompactStepKind::Other,
        };
        all.push(CompactEditStep {
            step_index: record_id(&v),
            ts,
            cwd,
            kind,
        });
    })?;
    // Keep unindexed entries in their original slots, as in the message pass.
    let mut indexed: Vec<_> = all
        .iter_mut()
        .filter(|s| s.step_index.is_some())
        .map(|s| std::mem::replace(s, CompactEditStep::placeholder()))
        .collect();
    indexed.sort_by_key(|s| s.step_index);
    let mut indexed = indexed.into_iter();
    for slot in &mut all {
        if slot.step_index == Some(u64::MAX) {
            *slot = indexed.next().unwrap();
        }
    }
    for step in all {
        if let Some(cwd) = step.cwd {
            project = Some(cwd);
        }
        match step.kind {
            CompactStepKind::User => pending.clear(),
            CompactStepKind::Planner(calls) => {
                for (i, (edit, cwd, family)) in calls.into_iter().enumerate() {
                    if project.is_none() {
                        project = cwd;
                    }
                    if pending.len() == 4096 {
                        pending.pop_front();
                    }
                    pending.push_back(Pending {
                        family,
                        id: format!("{}:{i}", step.step_index.unwrap_or(0)),
                        edit,
                    });
                }
            }
            CompactStepKind::Result(done, family) => {
                if let Some(call) = take_pending(&mut pending, &family) {
                    if done && matches!(family, ToolFamily::Generic | ToolFamily::Edit) {
                        if let Some(mut edit) = call.edit {
                            edit.ts = step.ts.or(edit.ts);
                            edits.push(edit);
                        }
                    }
                    if let Some(id) = step.step_index.filter(|id| wanted.contains(id)) {
                        links.insert(id, call.id);
                    }
                }
            }
            CompactStepKind::Other => {}
        }
    }
    let retained_calls: HashSet<String> = retained
        .iter()
        .filter(|v| kind(v) == "PLANNER_RESPONSE")
        .flat_map(|v| {
            v.get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
                .map(move |(i, _)| format!("{}:{i}", record_id(v).unwrap_or(0)))
        })
        .collect();
    let mut messages = Vec::new();
    for v in &retained {
        let ts = stamp(v);
        let content = v.get("content").and_then(Value::as_str).unwrap_or("");
        match kind(v) {
            "USER_INPUT" | "PLANNER_RESPONSE" => {
                let text = if kind(v) == "USER_INPUT" {
                    user_text(content)
                } else {
                    content.trim()
                };
                if !text.is_empty() {
                    messages.push(Message {
                        role: if kind(v) == "USER_INPUT" {
                            Role::User
                        } else {
                            Role::Assistant
                        },
                        text: crate::redact::redact(text).into_owned(),
                        ts,
                        tool: None,
                    });
                }
                if let Some(calls) = v.get("tool_calls").and_then(Value::as_array) {
                    for (i, tc) in calls.iter().enumerate() {
                        let mut input = args(tc);
                        redact_args(&mut input);
                        messages.push(Message::tool_call(
                            ts,
                            ToolCall {
                                id: Some(format!("{}:{i}", record_id(v).unwrap_or(0))),
                                name: tc
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("?")
                                    .to_owned(),
                                args: input,
                            },
                        ));
                    }
                }
            }
            _ if is_tool_result_step(v) => {
                let call_id = record_id(v).and_then(|s| links.get(&s)).cloned();
                // A window may have dropped the corresponding call. Do not
                // pair its output with an unrelated retained tool invocation.
                if windowed
                    && call_id
                        .as_ref()
                        .is_none_or(|id| !retained_calls.contains(id))
                {
                    continue;
                }
                messages.push(Message::tool_result(
                    ts,
                    ToolResult {
                        call_id,
                        text: bounded_redacted_output(content),
                        is_error: matches!(
                            v.get("status").and_then(Value::as_str),
                            Some("ERROR" | "FAILED")
                        ) || v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                    },
                ));
            }
            _ => {}
        }
    }
    Ok(Session {
        id: short_id(key),
        tool: "antigravity",
        path: PathBuf::from(key),
        project: project.unwrap_or_else(|| "antigravity".into()),
        started,
        ended,
        title: title_from_messages(&messages),
        subagent: false,
        messages,
        touched: dedup_paths(edits.iter().map(|e| e.path.clone()).collect()),
        edits,
    })
}

#[cfg(test)]
mod tests;
