//! Existing v1 SQLite and legacy JSON decoding.
use super::{display_path, push, redact_json_strings};
use crate::adapters::{
    bounded_redacted_output, dedup_paths, redacted_truncate, title_from_messages,
};
use crate::model::{Message, Role, Session, ToolCall, ToolResult};
use crate::util::short_id;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub(super) fn parse_sql(
    tool: &'static str,
    conn: &Connection,
    sid: &str,
    key: &str,
) -> Result<Session> {
    let (project, raw_title, created, updated): (String, String, Option<i64>, Option<i64>) = conn
        .query_row(
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
    // parent_id is fetched separately and tolerantly: an older or partial
    // schema without the column degrades to "not a subagent" rather than
    // failing the whole session.
    let parent: Option<String> = conn
        .query_row(
            "SELECT parent_id FROM session WHERE id = ?1",
            params![sid],
            |r| r.get(0),
        )
        .unwrap_or(None);

    // role per message, in transcript order.
    // LIMIT caps memory if a malicious db packs millions of rows into one
    // session; a real session is orders of magnitude below this.
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

    // content parts, grouped per message in order.
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
                // reasoning / step-* / file / snapshot - skip.
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
        tool,
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

pub(super) fn parse_json(tool: &'static str, path: &Path) -> Result<Session> {
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

/// Both layouts name an edited file in `state.input.filePath` for the
/// `edit`/`write`/`multiedit` tools. Reads are not authorship.
fn edited_path(tool: &str, input: Option<&Value>) -> Option<String> {
    if !matches!(tool, "edit" | "write" | "multiedit") {
        return None;
    }
    input?
        .get("filePath")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
        .map(String::from)
}

fn emit_tool_part(
    part: &Value,
    ts: Option<DateTime<Utc>>,
    messages: &mut Vec<Message>,
    touched: &mut Vec<String>,
) {
    let name = part.get("tool").and_then(Value::as_str).unwrap_or("?");
    let input = part.pointer("/state/input");
    let state = part.get("state");
    let status = state
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str);
    let has_error_field = state.is_some_and(|value| value.get("error").is_some());
    let is_error = status == Some("error") || has_error_field;
    // An edit that ended in error - rejected at permission, or no match -
    // changed nothing.
    if let Some(path) = edited_path(name, input).filter(|_| !is_error) {
        touched.push(path);
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

    let output = state
        .and_then(|value| value.get("output"))
        .and_then(Value::as_str);
    let error = state
        .and_then(|value| value.get("error"))
        .and_then(Value::as_str);
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
                is_error,
            },
        ));
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

#[cfg(test)]
mod tests {
    use super::super::{make_key, OpenCode};
    use crate::adapters::Adapter;
    use chrono::DateTime;
    use rusqlite::Connection;

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

    /// An edit that ended in error changed nothing: OpenCode records the
    /// user rejecting the permission and a failed match ("oldString not found")
    /// as `status: "error"`. A call still running has no outcome yet and keeps
    /// its file, as a finished one does.
    #[test]
    fn an_edit_that_ended_in_error_edited_nothing() {
        let part = |path: &str, state: serde_json::Value| {
            let mut state = state;
            state["input"] = serde_json::json!({ "filePath": path });
            serde_json::json!({"type": "tool", "tool": "edit", "callID": path, "state": state})
        };
        let parts = [
            part(
                "done.ts",
                serde_json::json!({"status": "completed", "output": "ok"}),
            ),
            part("running.ts", serde_json::json!({"status": "running"})),
            part(
                "rejected.ts",
                serde_json::json!({"status": "error", "error": "The user rejected permission to use this specific tool call."}),
            ),
            part(
                "nomatch.ts",
                serde_json::json!({"status": "error", "error": "oldString not found in content"}),
            ),
            part("unstated.ts", serde_json::json!({"error": "failed"})),
            part("bare.ts", serde_json::json!({"status": "error"})),
        ];
        let (mut messages, mut touched) = (Vec::new(), Vec::new());
        for p in &parts {
            super::emit_tool_part(p, None, &mut messages, &mut touched);
        }
        assert_eq!(touched, ["done.ts", "running.ts"]);
    }
}
