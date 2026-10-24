use super::{
    bounded_redacted_output, clean_path, dedup_paths, ok_or_flag, redacted_truncate,
    title_from_messages, Adapter, Discovered, Store,
};
use crate::model::{EditEvent, EditKind, Message, Role, Session, ToolCall, ToolResult};
use crate::util::short_id;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// OpenCode (sst/opencode) keeps its data under `$XDG_DATA_HOME/opencode`
/// (default `~/.local/share/opencode`, the same on macOS - it uses
/// xdg-basedir, not the platform data dir). Two storage generations exist:
///
///   - **SQLite (v1.2.0+, the current default):** `opencode.db` (and
///     `opencode-<channel>.db`) with `session` / `message` / `part` tables.
///     This is the source of truth on any recent install, so when a db is
///     present we index it as a shared [`Store`] and ignore the JSON below.
///   - **Legacy JSON:** `storage/session|message|part/**.json`, one file each.
///     Still read on pre-1.2.0 installs that never migrated.
pub struct OpenCode;

impl Adapter for OpenCode {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn root(&self) -> Option<PathBuf> {
        // The opencode data dir - exists for both the SQLite and JSON layouts,
        // so `scan` presence and deletion reconciliation work either way.
        data_dir()
    }

    fn discover(&self) -> Discovered {
        // Legacy JSON only. When a db exists, `store()` is Some and the indexer
        // never calls this (the JSON files are stale post-migration leftovers).
        let Some(dir) = data_dir() else {
            return Vec::new().into();
        };
        let sessions = dir.join("storage").join("session");
        if !sessions.exists() {
            return Vec::new().into(); // no legacy layout - normal
        }
        let mut had_error = false;
        let files = WalkDir::new(sessions)
            .min_depth(2)
            .max_depth(2)
            .into_iter()
            .filter_map(|e| ok_or_flag(e, &mut had_error))
            .filter(|e| e.file_type().is_file())
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .map(|e| e.into_path())
            .collect();
        Discovered { files, had_error }
    }

    fn store(&self) -> Option<Store> {
        let dbs: Vec<PathBuf> = db_paths().into_iter().filter(|p| p.is_file()).collect();
        if dbs.is_empty() {
            return None;
        }
        let mut keys: Vec<(String, i64)> = Vec::new();
        let mut had_error = false;
        for db in &dbs {
            let conn = match open_ro(db) {
                Ok(c) => c,
                Err(_) => {
                    had_error = true;
                    continue;
                }
            };
            let mut seen = std::collections::HashSet::new();

            let has_v2 = match table_exists(&conn, "session_v2") {
                Ok(b) => b,
                Err(_) => {
                    had_error = true;
                    continue;
                }
            };
            let has_v1 = match table_exists(&conn, "session") {
                Ok(b) => b,
                Err(_) => {
                    had_error = true;
                    continue;
                }
            };

            if !has_v2 && !has_v1 {
                had_error = true;
                continue;
            }

            // OpenCode v2.x schema (session_v2 table)
            if has_v2 {
                match conn.prepare("SELECT id, time_updated, time_created FROM session_v2") {
                    Ok(mut stmt) => {
                        match stmt.query_map([], |r| {
                            let id: String = r.get(0)?;
                            let updated: Option<i64> = r.get(1)?;
                            let created: Option<i64> = r.get(2)?;
                            Ok((id, updated.or(created).unwrap_or(0)))
                        }) {
                            Ok(rows) => {
                                for row in rows {
                                    match row {
                                        Ok((id, token)) => {
                                            if seen.insert(id.clone()) {
                                                keys.push((make_key(db, &id), token));
                                            }
                                        }
                                        Err(_) => had_error = true,
                                    }
                                }
                            }
                            Err(_) => had_error = true,
                        }
                    }
                    Err(_) => had_error = true,
                }
            }

            // OpenCode v1.x schema (session table)
            if has_v1 {
                match conn.prepare("SELECT id, time_updated, time_created FROM session") {
                    Ok(mut stmt) => {
                        match stmt.query_map([], |r| {
                            let id: String = r.get(0)?;
                            let updated: Option<i64> = r.get(1)?;
                            let created: Option<i64> = r.get(2)?;
                            Ok((id, updated.or(created).unwrap_or(0)))
                        }) {
                            Ok(rows) => {
                                for row in rows {
                                    match row {
                                        Ok((id, token)) => {
                                            if seen.insert(id.clone()) {
                                                keys.push((make_key(db, &id), token));
                                            }
                                        }
                                        Err(_) => had_error = true,
                                    }
                                }
                            }
                            Err(_) => had_error = true,
                        }
                    }
                    Err(_) => had_error = true,
                }
            }
        }
        Some(Store {
            keys,
            files: dbs,
            had_error,
        })
    }

    fn parse_key(&self, key: &str) -> Result<Session> {
        let (db, sid) = key.split_once(KEY_SEP).context("malformed opencode key")?;
        let conn = open_ro(Path::new(db)).with_context(|| format!("open {db}"))?;

        if table_exists(&conn, "session_v2").unwrap_or(false) {
            let in_v2 = conn
                .query_row(
                    "SELECT 1 FROM session_v2 WHERE id = ?1",
                    params![sid],
                    |_| Ok(true),
                )
                .optional()
                .with_context(|| format!("check session_v2 in {db}"))?
                .unwrap_or(false);

            if in_v2 {
                return self.parse_v2(&conn, db, sid, key);
            }
        }

        self.parse_v1(&conn, db, sid, key)
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        parse_json(self.name(), path)
    }
}

fn table_exists(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![table],
        |_| Ok(true),
    )
    .optional()
    .map(|o| o.unwrap_or(false))
}

impl OpenCode {
    fn parse_v2(&self, conn: &Connection, _db: &str, sid: &str, key: &str) -> Result<Session> {
        let (project, raw_title, created, updated, parent): (
            String,
            String,
            Option<i64>,
            Option<i64>,
            Option<String>,
        ) = conn.query_row(
            "SELECT directory, title, time_created, time_updated, parent_id
             FROM session_v2 WHERE id = ?1",
            params![sid],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                ))
            },
        )?;

        let mut stmt = conn.prepare(
            "SELECT type, time_created, data FROM session_message
             WHERE session_id = ?1 ORDER BY seq ASC, id ASC LIMIT 200000",
        )?;

        let mut messages: Vec<Message> = Vec::new();
        let mut touched: Vec<String> = Vec::new();
        let mut edits: Vec<EditEvent> = Vec::new();

        let rows = stmt.query_map(params![sid], |r| {
            let mtype: String = r.get(0)?;
            let tc: Option<i64> = r.get(1)?;
            let data: String = r.get(2)?;
            Ok((mtype, tc, data))
        })?;

        for row in rows {
            let (mtype, tc, data_str) = row?;
            let ts = tc.and_then(DateTime::from_timestamp_millis);
            let data: Value = serde_json::from_str(&data_str)
                .with_context(|| format!("malformed message json in opencode v2 session {sid}"))?;

            match mtype.as_str() {
                "user" => {
                    let text = data
                        .get("text")
                        .or_else(|| data.get("content"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim();
                    if !text.is_empty() {
                        push(&mut messages, Role::User, text, ts);
                    }
                }
                "assistant" => {
                    if let Some(content) = data.get("content").and_then(Value::as_array) {
                        for item in content {
                            let itype = item.get("type").and_then(Value::as_str).unwrap_or("");
                            match itype {
                                "text" => {
                                    if let Some(t) = item.get("text").and_then(Value::as_str) {
                                        let text = t.trim();
                                        if !text.is_empty() {
                                            push(&mut messages, Role::Assistant, text, ts);
                                        }
                                    }
                                }
                                "tool" => {
                                    emit_tool_part_v2(item, ts, &mut messages, &mut touched, &mut edits);
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        let title = if raw_title.trim().is_empty() || raw_title.starts_with("New session") {
            title_from_messages(&messages)
        } else {
            redacted_truncate(&raw_title, 80)
        };

        Ok(Session {
            id: short_id(key),
            tool: self.name(),
            path: PathBuf::from(display_path(key)),
            project,
            started: created.and_then(DateTime::from_timestamp_millis),
            ended: updated.and_then(DateTime::from_timestamp_millis),
            title,
            subagent: parent.is_some_and(|p| !p.is_empty()),
            messages,
            touched: dedup_paths(touched),
            edits,
        })
    }

    fn parse_v1(&self, conn: &Connection, _db: &str, sid: &str, key: &str) -> Result<Session> {
        let (project, raw_title, created, updated): (String, String, Option<i64>, Option<i64>) =
            conn.query_row(
                "SELECT directory, title, time_created, time_updated
                 FROM session WHERE id = ?1",
                params![sid],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        r.get(2)?,
                        r.get(3)?,
                    ))
                },
            )?;

        let parent: Option<String> = conn
            .query_row(
                "SELECT parent_id FROM session WHERE id = ?1",
                params![sid],
                |r| r.get(0),
            )
            .unwrap_or(None);

        let mut mstmt = conn.prepare(
            "SELECT id, time_created, data FROM message
             WHERE session_id = ?1 ORDER BY time_created, id LIMIT 200000",
        )?;
        let messages_meta: Vec<(String, Option<i64>, Value)> = mstmt
            .query_map(params![sid], |r| {
                let id: String = r.get(0)?;
                let tc: Option<i64> = r.get(1)?;
                let data: String = r.get(2)?;
                Ok((id, tc, serde_json::from_str(&data).unwrap_or(Value::Null)))
            })?
            .filter_map(|x| x.ok())
            .collect();

        let mut pstmt = conn.prepare(
            "SELECT message_id, data FROM part
             WHERE session_id = ?1 ORDER BY time_created, id LIMIT 200000",
        )?;
        let mut parts: HashMap<String, Vec<Value>> = HashMap::new();
        let prows = pstmt.query_map(params![sid], |r| {
            let mid: String = r.get(0)?;
            let data: String = r.get(1)?;
            Ok((mid, data))
        })?;
        for (mid, data) in prows.flatten() {
            if let Ok(v) = serde_json::from_str::<Value>(&data) {
                parts.entry(mid).or_default().push(v);
            }
        }

        let mut messages: Vec<Message> = Vec::new();
        let mut touched: Vec<String> = Vec::new();
        for (mid, tc, mdata) in &messages_meta {
            let role = match mdata.get("role").and_then(Value::as_str) {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                _ => continue,
            };
            let ts = tc.and_then(DateTime::from_timestamp_millis);
            let Some(ps) = parts.get(mid) else { continue };
            for p in ps {
                match p.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = p.get("text").and_then(Value::as_str) {
                            push(&mut messages, role, t, ts);
                        }
                    }
                    Some("tool") => {
                        emit_tool_part(p, ts, &mut messages, &mut touched);
                    }
                    Some("patch") => {
                        if let Some(Value::Array(files)) = p.get("files") {
                            for f in files {
                                if let Some(fp) = f.as_str() {
                                    touched.push(fp.to_string());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        let title = if raw_title.trim().is_empty() || raw_title.starts_with("New session") {
            title_from_messages(&messages)
        } else {
            redacted_truncate(&raw_title, 80)
        };

        Ok(Session {
            id: short_id(key),
            tool: self.name(),
            path: PathBuf::from(display_path(key)),
            project,
            started: created.and_then(DateTime::from_timestamp_millis),
            ended: updated.and_then(DateTime::from_timestamp_millis),
            title,
            subagent: parent.is_some_and(|p| !p.is_empty()),
            messages,
            touched: dedup_paths(touched),
            edits: Vec::new(),
        })
    }
}

/// Key encoding for a shared-store session: `<db path><US><session id>`. The
/// unit separator never appears in a path or a uuid.
const KEY_SEP: char = '\u{1f}';
fn make_key(db: &Path, session_id: &str) -> String {
    format!("{}{KEY_SEP}{session_id}", db.display())
}

/// A human-facing form of a shared-store key for `Session.path` (shown in
/// `show`/`brief`/web). The raw key carries a U+001F separator that must not
/// leak into output or an LLM briefing; render it as `<db>#<id>`.
fn display_path(key: &str) -> String {
    key.replace(KEY_SEP, "#")
}

/// `$XDG_DATA_HOME/opencode`, or `~/.local/share/opencode`.
fn data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("share")))?;
    Some(base.join("opencode"))
}

/// The session databases: `opencode.db` plus any `opencode-<channel>.db`, or a
/// single path from `$OPENCODE_DB` (absolute, or relative to the data dir).
fn db_paths() -> Vec<PathBuf> {
    if let Some(over) = std::env::var_os("OPENCODE_DB") {
        let p = PathBuf::from(&over);
        if !p.as_os_str().is_empty() && p.as_path() != Path::new(":memory:") {
            return vec![if p.is_absolute() {
                p
            } else {
                data_dir().map(|d| d.join(&p)).unwrap_or(p)
            }];
        }
    }
    let Some(dir) = data_dir() else {
        return vec![];
    };
    let mut out = vec![dir.join("opencode.db")];
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with("opencode-") && name.ends_with(".db") {
                out.push(p);
            }
        }
    }
    out
}

/// Open another tool's database strictly read-only - never creating, writing,
/// checkpointing, or recovering it. A plain read-only handle can fail on a WAL
/// db when the shared-memory file is missing/locked; the fallback retries with
/// an `immutable=1` URI, which tells SQLite to assume the file cannot change and
/// skip all locking and shared-memory, so it still cannot write. (Trade-off:
/// changes a running OpenCode makes mid-read may be missed - fine for indexing.)
fn open_ro(db: &Path) -> Result<Connection> {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .or_else(|_| {
            let uri = format!("file:{}?immutable=1", db.display());
            Connection::open_with_flags(
                &uri,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
            )
        })
        .map_err(Into::into)
}

// --- legacy JSON layout (pre-1.2.0) ---

fn parse_json(tool: &'static str, path: &Path) -> Result<Session> {
    let raw = crate::util::read_to_string_capped(path)?;
    let s: Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;

    let session_id = s
        .get("id")
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_else(|| stem(path));
    let project = s
        .get("directory")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let started = s.pointer("/time/created").and_then(epoch_ms);
    let ended = s.pointer("/time/updated").and_then(epoch_ms);
    let subagent = s.get("parentID").is_some();
    let session_title = s
        .get("title")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty() && !t.starts_with("New session"))
        .map(|t| redacted_truncate(t, 80));

    let store = path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent());

    let mut messages: Vec<Message> = Vec::new();
    let mut touched: Vec<String> = Vec::new();

    if let Some(store) = store {
        for msg_path in sorted_json(&store.join("message").join(&session_id)) {
            let Ok(mraw) = crate::util::read_to_string_capped(&msg_path) else {
                continue;
            };
            let Ok(m) = serde_json::from_str::<Value>(&mraw) else {
                continue;
            };
            let role = match m.get("role").and_then(Value::as_str) {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                _ => continue,
            };
            let mid = m
                .get("id")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| stem(&msg_path));
            let ts = m.pointer("/time/created").and_then(epoch_ms);

            for part_path in sorted_json(&store.join("part").join(&mid)) {
                let Ok(praw) = crate::util::read_to_string_capped(&part_path) else {
                    continue;
                };
                let Ok(p) = serde_json::from_str::<Value>(&praw) else {
                    continue;
                };
                match p.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = p.get("text").and_then(Value::as_str) {
                            push(&mut messages, role, t, ts);
                        }
                    }
                    Some("tool") => {
                        emit_tool_part(&p, ts, &mut messages, &mut touched);
                    }
                    Some("patch") => {
                        if let Some(Value::Array(files)) = p.get("files") {
                            for f in files {
                                if let Some(fp) = f.as_str() {
                                    touched.push(fp.to_string());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let title = session_title.unwrap_or_else(|| title_from_messages(&messages));

    Ok(Session {
        id: short_id(&path.to_string_lossy()),
        tool,
        path: path.to_path_buf(),
        project,
        started,
        ended,
        title,
        subagent,
        messages,
        touched: dedup_paths(touched),
        edits: Vec::new(),
    })
}

/// Both layouts name an edited file in `state.input.filePath` or `path` for the
/// `edit`/`write`/`multiedit` tools. Reads are not authorship.
fn edited_path(tool: &str, input: Option<&Value>) -> Option<String> {
    if !matches!(tool, "edit" | "write" | "multiedit") {
        return None;
    }
    let input = input?;
    for key in ["filePath", "path", "file_path"] {
        if let Some(p) = input.get(key).and_then(Value::as_str) {
            if !p.is_empty() {
                return Some(p.to_string());
            }
        }
    }
    None
}

fn edit_event(tool: &str, input: Option<&Value>, ts: Option<DateTime<Utc>>) -> Option<EditEvent> {
    let path = clean_path(&edited_path(tool, input)?)?;
    let input = input?;
    let kind = match tool {
        "edit" | "multiedit" => EditKind::Edit,
        "write" => EditKind::Write,
        _ => return None,
    };
    let raw = match kind {
        EditKind::Edit => input
            .get("newString")
            .or_else(|| input.get("new_string"))
            .or_else(|| input.get("new_str"))
            .and_then(Value::as_str)
            .unwrap_or(""),
        EditKind::Write => input.get("content").and_then(Value::as_str).unwrap_or(""),
        _ => "",
    };
    let snippet = redacted_truncate(raw.trim(), 200);
    Some(EditEvent {
        path,
        kind,
        snippet,
        ts,
    })
}

fn emit_tool_part(
    part: &Value,
    ts: Option<DateTime<Utc>>,
    messages: &mut Vec<Message>,
    touched: &mut Vec<String>,
) {
    let name = part.get("tool").and_then(Value::as_str).unwrap_or("?");
    let input = part.pointer("/state/input");
    // If the tool is edit/write, record touched files for the session header.
    let tool_lower = name.to_ascii_lowercase();
    if matches!(tool_lower.as_str(), "edit" | "write") {
        if let Some(path) = input
            .and_then(|v| v.get("filePath").or_else(|| v.get("path")))
            .and_then(Value::as_str)
        {
            if !path.is_empty() {
                touched.push(path.to_string());
            }
        }
    }

    let mut args = input.cloned().unwrap_or_else(|| serde_json::json!({}));
    redact_json_strings(&mut args);
    messages.push(Message::tool_call(
        ts,
        ToolCall {
            id: part
                .get("callID")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned),
            name: name.to_owned(),
            args,
        },
    ));

    let status = part.pointer("/state/status").and_then(Value::as_str);
    let output = part.pointer("/state/output").and_then(Value::as_str);
    let error = part.pointer("/state/error").and_then(Value::as_str);
    let has_error_field = part
        .get("state")
        .is_some_and(|value| value.get("error").is_some());
    let is_terminal = matches!(status, Some("completed" | "error"));

    if is_terminal || output.is_some() || error.is_some() || has_error_field {
        messages.push(Message::tool_result(
            ts,
            ToolResult {
                call_id: part
                    .get("callID")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned),
                text: bounded_redacted_output(output.or(error).unwrap_or_default()),
                is_error: status == Some("error") || has_error_field,
            },
        ));
    }
}

fn emit_tool_part_v2(
    part: &Value,
    ts: Option<DateTime<Utc>>,
    messages: &mut Vec<Message>,
    touched: &mut Vec<String>,
    edits: &mut Vec<EditEvent>,
) {
    let name = part
        .get("name")
        .or_else(|| part.get("tool"))
        .and_then(Value::as_str)
        .unwrap_or("?");
    let input = part.pointer("/state/input").or_else(|| part.get("input"));
    let tool_lower = name.to_ascii_lowercase();

    if let Some(path) = edited_path(&tool_lower, input) {
        if let Some(clean) = clean_path(&path) {
            touched.push(clean);
            if let Some(ev) = edit_event(&tool_lower, input, ts) {
                edits.push(ev);
            }
        }
    }

    let mut args = input.cloned().unwrap_or_else(|| serde_json::json!({}));
    redact_json_strings(&mut args);
    let call_id = part
        .get("id")
        .or_else(|| part.get("callID"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned);

    messages.push(Message::tool_call(
        ts,
        ToolCall {
            id: call_id.clone(),
            name: name.to_owned(),
            args,
        },
    ));

    let state = part.get("state");
    let status = state
        .and_then(|v| v.get("status"))
        .and_then(Value::as_str);

    let mut output_text: Option<String> = None;
    if let Some(s) = state {
        if let Some(c) = s.get("content").and_then(Value::as_array) {
            let texts: Vec<&str> = c
                .iter()
                .filter_map(|x| x.get("text").and_then(Value::as_str))
                .collect();
            if !texts.is_empty() {
                output_text = Some(texts.join("\n"));
            }
        }
        if output_text.is_none() {
            if let Some(out) = s.get("output").and_then(Value::as_str) {
                output_text = Some(out.to_string());
            } else if let Some(err) = s.get("error").and_then(Value::as_str) {
                output_text = Some(err.to_string());
            }
        }
    }

    let is_error = status == Some("error") || status == Some("failed");
    if let Some(text) = output_text {
        messages.push(Message::tool_result(
            ts,
            ToolResult {
                call_id,
                text: bounded_redacted_output(&text),
                is_error,
            },
        ));
    }
}

fn redact_json_strings(value: &mut Value) {
    match value {
        Value::String(text) => *text = crate::redact::redact(text).into_owned(),
        Value::Array(values) => values.iter_mut().for_each(redact_json_strings),
        Value::Object(values) => values.values_mut().for_each(redact_json_strings),
        _ => {}
    }
}

fn sorted_json(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    v.sort();
    v
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Timestamps are epoch milliseconds (numbers), not RFC3339 strings.
fn epoch_ms(v: &Value) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis(v.as_i64()?)
}

fn push(messages: &mut Vec<Message>, role: Role, text: &str, ts: Option<DateTime<Utc>>) {
    let text = text.trim();
    if !text.is_empty() {
        messages.push(Message {
            role,
            text: text.to_string(),
            ts,
            tool: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{make_key, OpenCode};
    use crate::adapters::Adapter;
    use chrono::DateTime;
    use rusqlite::Connection;
    use std::path::PathBuf;

    #[test]
    fn parses_a_sqlite_session() {
        // Build a minimal opencode.db (session/message/part) and read one
        // session back out of it through the SQLite path.
        let db = std::env::temp_dir().join(format!("sw-oc-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE session(id TEXT PRIMARY KEY, parent_id TEXT, project_id TEXT,
                     directory TEXT, title TEXT, time_created INTEGER, time_updated INTEGER);
                 CREATE TABLE message(id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
                 CREATE TABLE part(id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, time_created INTEGER, data TEXT);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO session VALUES ('ses1', NULL, 'p1', '/home/dev/app', 'Add retry', 1718630400000, 1718630500000)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message VALUES ('m1','ses1',1718630400000,'{\"role\":\"user\"}')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message VALUES ('m2','ses1',1718630450000,'{\"role\":\"assistant\"}')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO part VALUES ('pa','m1','ses1',1718630400000,'{\"type\":\"text\",\"text\":\"add retry to the client\"}')",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO part VALUES ('pb','m2','ses1',1718630450000,'{\"type\":\"text\",\"text\":\"I will add it.\"}')",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO part VALUES ('pc','m2','ses1',1718630451000,'{\"type\":\"tool\",\"tool\":\"edit\",\"state\":{\"input\":{\"filePath\":\"src/client.ts\"}}}')",
                [],
            ).unwrap();
        }

        let key = make_key(&db, "ses1");
        let s = OpenCode.parse_key(&key).unwrap();

        assert_eq!(s.tool, "opencode");
        assert_eq!(s.project, "/home/dev/app");
        assert_eq!(s.title, "Add retry");
        assert!(!s.subagent);
        let roles: Vec<_> = s.messages.iter().map(|m| m.role.label()).collect();
        assert_eq!(roles, ["user", "assistant", "tool"]);
        assert_eq!(s.touched, ["src/client.ts"]);
        assert_eq!(s.started, DateTime::from_timestamp_millis(1718630400000));
        assert_eq!(s.ended, DateTime::from_timestamp_millis(1718630500000));

        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(db.with_extension("db-wal"));
        let _ = std::fs::remove_file(db.with_extension("db-shm"));
    }

    #[test]
    fn parses_a_sqlite_session_v2() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode_v2.db");

        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE session_v2 (
                    id TEXT PRIMARY KEY,
                    parent_id TEXT,
                    project_id TEXT,
                    directory TEXT,
                    title TEXT,
                    time_created INTEGER,
                    time_updated INTEGER
                 );
                 CREATE TABLE session_message (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    type TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    time_created INTEGER,
                    data TEXT NOT NULL
                 );",
            )
            .unwrap();

            conn.execute(
                "INSERT INTO session_v2 VALUES ('ses2', NULL, 'p2', '/home/dev/v2app', 'V2 Migration', 1718630400000, 1718630500000)",
                [],
            )
            .unwrap();

            conn.execute(
                "INSERT INTO session_message VALUES ('m1', 'ses2', 'user', 1, 1718630400000, '{\"text\":\"Please fix disk issue\"}')",
                [],
            )
            .unwrap();

            conn.execute(
                "INSERT INTO session_message VALUES ('m2', 'ses2', 'assistant', 2, 1718630450000, '{\"content\":[{\"type\":\"text\",\"text\":\"Fixing it now.\"},{\"type\":\"tool\",\"name\":\"write\",\"state\":{\"input\":{\"path\":\"src/disk.ts\",\"content\":\"const ok = true;\"}}}]}')",
                [],
            )
            .unwrap();
        }

        let key = make_key(&db, "ses2");
        let s = OpenCode.parse_key(&key).unwrap();

        assert_eq!(s.tool, "opencode");
        assert_eq!(s.project, "/home/dev/v2app");
        assert_eq!(s.title, "V2 Migration");
        assert!(!s.subagent);
        let roles: Vec<_> = s.messages.iter().map(|m| m.role.label()).collect();
        assert_eq!(roles, ["user", "assistant", "tool"]);
        assert_eq!(s.touched, ["src/disk.ts"]);
        assert_eq!(s.edits.len(), 1);
        assert_eq!(s.edits[0].path, "src/disk.ts");
        assert_eq!(s.started, DateTime::from_timestamp_millis(1718630400000));
        assert_eq!(s.ended, DateTime::from_timestamp_millis(1718630500000));

        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(db.with_extension("db-wal"));
        let _ = std::fs::remove_file(db.with_extension("db-shm"));
    }

    #[test]
    fn parses_sanitized_v2_fixture() {
        // Identified OpenCode version: v2.1.1 (session_v2 & session_message schema)
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixture_db = root.join("tests/fixtures/opencode/opencode_v2_1_1.db");
        assert!(fixture_db.exists(), "fixture db must exist: {:?}", fixture_db);

        let key = make_key(&fixture_db, "ses_v2_sanitized_001");
        let s = OpenCode.parse_key(&key).expect("parse sanitized v2 fixture");

        assert_eq!(s.tool, "opencode");
        assert_eq!(s.project, "/home/developer/opencode-v2-demo");
        assert_eq!(s.title, "Migrate authentication to JWT");
        assert!(!s.subagent);

        let roles: Vec<_> = s.messages.iter().map(|m| m.role.label()).collect();
        assert_eq!(roles, ["user", "assistant", "tool", "tool", "assistant"]);
        assert_eq!(s.touched, ["src/auth.ts"]);
        assert_eq!(s.edits.len(), 1);
        assert_eq!(s.edits[0].path, "src/auth.ts");
        assert_eq!(s.started, DateTime::from_timestamp_millis(1718630400000));
        assert_eq!(s.ended, DateTime::from_timestamp_millis(1718630500000));
    }

    #[test]
    fn mixed_store_indexes_and_parses_both_v1_and_v2() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("mixed.db");

        {
            let conn = Connection::open(&db).unwrap();
            // Create both v1 and v2 tables
            conn.execute_batch(
                "CREATE TABLE session_v2 (
                    id TEXT PRIMARY KEY,
                    parent_id TEXT,
                    project_id TEXT,
                    directory TEXT,
                    title TEXT,
                    time_created INTEGER,
                    time_updated INTEGER
                 );
                 CREATE TABLE session_message (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    type TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    time_created INTEGER,
                    data TEXT NOT NULL
                 );
                 CREATE TABLE session (
                    id TEXT PRIMARY KEY,
                    parent_id TEXT,
                    project_id TEXT,
                    directory TEXT,
                    title TEXT,
                    time_created INTEGER,
                    time_updated INTEGER
                 );
                 CREATE TABLE message (
                    id TEXT PRIMARY KEY,
                    session_id TEXT,
                    time_created INTEGER,
                    data TEXT
                 );
                 CREATE TABLE part (
                    id TEXT PRIMARY KEY,
                    message_id TEXT,
                    session_id TEXT,
                    time_created INTEGER,
                    data TEXT
                 );",
            )
            .unwrap();

            // Insert a v2 session
            conn.execute(
                "INSERT INTO session_v2 VALUES ('ses_v2', NULL, 'p2', '/home/dev/v2', 'V2 Title', 1718630400000, 1718630500000)",
                [],
            )
            .unwrap();
            conn.execute(
                r#"INSERT INTO session_message VALUES ('m1', 'ses_v2', 'user', 1, 1718630400000, '{"text":"v2 query"}')"#,
                [],
            )
            .unwrap();

            // Insert a v1 session
            conn.execute(
                "INSERT INTO session VALUES ('ses_v1', NULL, 'p1', '/home/dev/v1', 'V1 Title', 1718630400000, 1718630500000)",
                [],
            )
            .unwrap();
            conn.execute(
                r#"INSERT INTO message VALUES ('m_old', 'ses_v1', 1718630400000, '{"role":"user"}')"#,
                [],
            )
            .unwrap();
            conn.execute(
                r#"INSERT INTO part VALUES ('p_old', 'm_old', 'ses_v1', 1718630400000, '{"type":"text","text":"v1 query"}')"#,
                [],
            )
            .unwrap();

            // Insert a duplicated ID into v1 table (migrated session): should dedup during discovery
            conn.execute(
                "INSERT INTO session VALUES ('ses_v2', NULL, 'p2_dup', '/home/dev/dup', 'Old V2 Title', 1718630400000, 1718630500000)",
                [],
            )
            .unwrap();
        }

        // Test discovery via OPENCODE_DB
        std::env::set_var("OPENCODE_DB", &db);
        let store = OpenCode.store().expect("store must be found");
        assert!(!store.had_error, "clean store discovery must not have errors");
        let session_ids: Vec<_> = store
            .keys
            .iter()
            .map(|(k, _)| k.split_once(super::KEY_SEP).unwrap().1)
            .collect();
        assert_eq!(session_ids, ["ses_v2", "ses_v1"], "v2 and v1 sessions discovered, duplicate deduped");

        // Parse v2 key
        let key_v2 = make_key(&db, "ses_v2");
        let s2 = OpenCode.parse_key(&key_v2).expect("parse v2 from mixed store");
        assert_eq!(s2.title, "V2 Title");
        assert_eq!(s2.project, "/home/dev/v2");

        // Parse v1 key
        let key_v1 = make_key(&db, "ses_v1");
        let s1 = OpenCode.parse_key(&key_v1).expect("parse v1 from mixed store");
        assert_eq!(s1.title, "V1 Title");
        assert_eq!(s1.project, "/home/dev/v1");

        std::env::remove_var("OPENCODE_DB");
    }

    #[test]
    fn v2_parse_failure_propagates_and_is_not_masked_by_v1() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("corrupt_v2.db");

        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE session_v2 (
                    id TEXT PRIMARY KEY,
                    parent_id TEXT,
                    project_id TEXT,
                    directory TEXT,
                    title TEXT,
                    time_created INTEGER,
                    time_updated INTEGER
                 );
                 CREATE TABLE session_message (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    type TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    time_created INTEGER,
                    data TEXT NOT NULL
                 );
                 CREATE TABLE session (
                    id TEXT PRIMARY KEY,
                    parent_id TEXT,
                    project_id TEXT,
                    directory TEXT,
                    title TEXT,
                    time_created INTEGER,
                    time_updated INTEGER
                 );",
            )
            .unwrap();

            // Insert a v2 session whose message data is corrupted (invalid JSON)
            conn.execute(
                "INSERT INTO session_v2 VALUES ('corrupt_ses', NULL, 'p2', '/home/dev/bad', 'Corrupt V2', 1718630400000, 1718630500000)",
                [],
            )
            .unwrap();
            conn.execute(
                r#"INSERT INTO session_message VALUES ('m1', 'corrupt_ses', 'user', 1, 1718630400000, 'NOT_VALID_JSON{:::')"#,
                [],
            )
            .unwrap();
        }

        let key = make_key(&db, "corrupt_ses");
        let err = OpenCode.parse_key(&key).unwrap_err();
        let err_msg = format!("{err:#}");
        assert!(
            err_msg.contains("malformed message json in opencode v2 session corrupt_ses"),
            "expected v2 JSON error, got: {err_msg}"
        );
        // Ensure it did not fall back to v1 (which would complain about QueryReturnedNoRows in session table)
        assert!(
            !err_msg.contains("QueryReturnedNoRows"),
            "v2 parse failure must NOT fall back to v1 row search"
        );
    }

    #[test]
    fn discovery_keeps_had_error_on_sql_or_row_failure() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("broken_rows.db");

        {
            let conn = Connection::open(&db).unwrap();
            // Table exists, but schema/data violates expected types (time_created TEXT not INTEGER)
            conn.execute_batch(
                "CREATE TABLE session_v2 (
                    id TEXT PRIMARY KEY,
                    time_updated TEXT,
                    time_created TEXT
                 );
                 INSERT INTO session_v2 VALUES ('s_bad', 'not_an_int', 'neither_is_this');",
            )
            .unwrap();
        }

        std::env::set_var("OPENCODE_DB", &db);
        let store = OpenCode.store().expect("store discovered");
        assert!(store.had_error, "store discovery must keep had_error = true on row failure");
        std::env::remove_var("OPENCODE_DB");
    }
}
