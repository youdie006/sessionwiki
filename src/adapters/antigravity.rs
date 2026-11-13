//! Antigravity IDE and agy CLI native JSONL histories.
use super::{
    bounded_redacted_output, clean_path, dedup_paths, parse_ts, redacted_truncate, root_scope,
    title_from_messages, Adapter, Discovered, Store,
};
use crate::model::{EditEvent, EditKind, Message, Role, Session, ToolCall, ToolResult};
use crate::util::{canonicalize_as_recorded, short_id};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Antigravity {
    home: Option<PathBuf>,
    variant: &'static str,
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
            variant: "antigravity",
            summaries: Default::default(),
        }
    }
    pub fn cli() -> Self {
        Self {
            home: None,
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
    let mut best: Option<(PathBuf, Option<u64>, std::time::SystemTime)> = None;
    for path in sources(dir)? {
        let metadata = path.metadata()?;
        if metadata.len() == 0 {
            continue;
        }
        // Full logs retain richer output, but can lag behind the compact log
        // during a live conversation. Prefer coverage before format, retaining
        // the FILES order (full first) when both reach the same native step.
        let mut last_step = None;
        crate::util::for_each_line(&path, |line| {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                if let Some(step) = record_id(&v) {
                    last_step = Some(last_step.map_or(step, |old: u64| old.max(step)));
                }
            }
        })?;
        let modified = metadata.modified()?;
        let newer =
            best.as_ref()
                .is_none_or(|(_, old_step, old_modified)| match (*old_step, last_step) {
                    (Some(old), Some(new)) => new > old,
                    _ => modified > *old_modified,
                });
        if newer {
            best = Some((path, last_step, modified));
        }
    }
    best.map(|(path, _, _)| path)
        .with_context(|| format!("no readable transcript in {}", dir.display()))
}
fn change_token(files: &[PathBuf]) -> Result<i64> {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for path in files {
        path.file_name().hash(&mut h);
        let m = path.metadata()?;
        m.len().hash(&mut h);
        m.modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .hash(&mut h);
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
        let summaries =
            match read_summaries(&root.parent()?.join("conversation_summaries.db"), None) {
                Ok(summaries) => summaries,
                Err(_) => {
                    // A transient metadata failure must not replace indexed titles,
                    // workspaces or parent relationships with transcript fallbacks.
                    return Some(Store {
                        keys,
                        files,
                        had_error: true,
                    });
                }
            };
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
                            let mut h = std::collections::hash_map::DefaultHasher::new();
                            token.hash(&mut h);
                            summary.hash(&mut h);
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
        keys.sort();
        *self.summaries.lock().unwrap_or_else(|e| e.into_inner()) = cache;
        Some(Store {
            keys,
            files,
            had_error,
        })
    }
    fn modified_at(&self, key: &str) -> Option<DateTime<Utc>> {
        let log_time = sources(Path::new(key))
            .ok()?
            .iter()
            .filter_map(|p| p.metadata().ok()?.modified().ok())
            .max()
            .map(DateTime::<Utc>::from);
        let db = Path::new(key)
            .parent()?
            .parent()?
            .join("conversation_summaries.db");
        let metadata_time = [db.clone(), PathBuf::from(format!("{}-wal", db.display()))]
            .iter()
            .filter_map(|p| p.metadata().ok()?.modified().ok())
            .max()
            .map(DateTime::<Utc>::from);
        log_time
            .into_iter()
            .chain(self.summary(key).and_then(|s| s.updated))
            // Row timestamps need not advance on metadata edits or deletion.
            // Database freshness affects eligibility, never the per-row token.
            .chain(metadata_time)
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
#[derive(Clone, Debug, Hash)]
struct Summary {
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
        let has_parent = conn
            .prepare("PRAGMA table_info(conversation_summaries)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|s| s == "parent_conversation_id");
        let has_updated = conn
            .prepare("PRAGMA table_info(conversation_summaries)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|s| s == "last_modified_time");
        let updated = if has_updated {
            "last_modified_time"
        } else {
            "NULL"
        };
        let parent = if has_parent {
            "parent_conversation_id"
        } else {
            "''"
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT conversation_id,title,workspace_uris,{parent},{updated} FROM conversation_summaries {}",
            if id.is_some() {
                "WHERE conversation_id=?1"
            } else {
                "WHERE ?1 IS NULL"
            }
        ))?;
        let rows = stmt.query_map([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                r.get::<_, rusqlite::types::Value>(4)?,
            ))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (id, title, uris, parent, updated) = row?;
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
            let uris: Vec<String> = serde_json::from_str(&uris).unwrap_or_default();
            let project = uris.first().and_then(|s| file_uri(s)).unwrap_or_default();
            out.insert(
                id,
                Summary {
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
    Some(EditEvent {
        path: clean_path(path)?,
        kind,
        ts,
        snippet: redacted_truncate(text, 200),
    })
}
struct Pending {
    id: String,
    edit: Option<EditEvent>,
}
fn parse_transcript(path: &Path, key: &str) -> Result<Session> {
    let (lines, windowed) = crate::util::session_lines_keeping(path, |line| {
        serde_json::from_str::<Value>(line)
            .is_ok_and(|v| matches!(kind(&v), "USER_INPUT" | "PLANNER_RESPONSE"))
    })?;
    let retained: Vec<Value> = lines
        .iter()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let wanted: HashSet<u64> = retained
        .iter()
        .filter(|v| kind(v) == "GENERIC")
        .filter_map(record_id)
        .collect();
    let mut links: HashMap<u64, String> = HashMap::new();
    let mut pending = VecDeque::<Pending>::new();
    let mut edits = Vec::new();
    let mut started: Option<DateTime<Utc>> = None;
    let mut ended: Option<DateTime<Utc>> = None;
    let mut project = None;
    // Stream the complete source for provenance, including middle tool steps
    // outside a conversation window. Pending input is reduced to bounded edits.
    crate::util::for_each_line(path, |line| {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let ts = stamp(&v);
        if let Some(t) = ts {
            started = Some(started.map_or(t, |s| s.min(t)));
            ended = Some(ended.map_or(t, |s| s.max(t)));
        }
        if let Some(cwd) = v
            .get("cwd")
            .or_else(|| v.get("workspace"))
            .and_then(Value::as_str)
        {
            project = Some(cwd.to_owned());
        }
        match kind(&v) {
            "USER_INPUT" => {
                pending.clear();
            }
            "PLANNER_RESPONSE" => {
                if let Some(calls) = v.get("tool_calls").and_then(Value::as_array) {
                    for (i, tc) in calls.iter().enumerate() {
                        let input = args(tc);
                        if project.is_none() {
                            project = input
                                .get("Cwd")
                                .or_else(|| input.get("cwd"))
                                .and_then(Value::as_str)
                                .map(str::to_owned);
                        }
                        if pending.len() == 4096 {
                            pending.pop_front();
                        }
                        pending.push_back(Pending {
                            id: format!("{}:{i}", record_id(&v).unwrap_or(0)),
                            edit: edit(
                                tc.get("name").and_then(Value::as_str).unwrap_or("?"),
                                &input,
                                ts,
                            ),
                        });
                    }
                }
            }
            "GENERIC" => {
                if let Some(call) = pending.pop_front() {
                    if v.get("status").and_then(Value::as_str) == Some("DONE")
                        && !v.get("is_error").and_then(Value::as_bool).unwrap_or(false)
                    {
                        if let Some(mut edit) = call.edit {
                            edit.ts = ts.or(edit.ts);
                            edits.push(edit);
                        }
                    }
                    if let Some(step) = record_id(&v).filter(|step| wanted.contains(step)) {
                        links.insert(step, call.id);
                    }
                }
            }
            _ => {}
        }
    })?;
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
            "GENERIC" => {
                let call_id = record_id(v).and_then(|s| links.get(&s)).cloned();
                // A window may have dropped the corresponding call. Do not
                // pair its output with an unrelated retained tool invocation.
                if windowed && call_id.is_none() {
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
