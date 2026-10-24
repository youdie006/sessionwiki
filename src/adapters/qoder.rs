use super::{
    bounded_redacted_output, clean_path, dedup_paths, redacted_truncate, title_from_messages,
    Adapter, Discovered, Store,
};
use crate::model::{EditEvent, EditKind, Message, Role, Session, ToolCall, ToolResult};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct Qoder;

const KEY_SEP: char = '\u{1f}';
const SNIPPET_CAP: usize = 200;

fn db_path() -> Option<PathBuf> {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let p = PathBuf::from(appdata)
            .join("com.qoder.app.stable")
            .join("main.sqlite");
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn open_ro(path: &Path) -> Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .context("open sqlite db")
}

fn make_key(db: &Path, id: &str) -> String {
    format!("{}{KEY_SEP}{id}", db.display())
}

fn display_path(key: &str) -> String {
    key.replace(KEY_SEP, "#")
}

fn redact_json_strings(value: &mut Value) {
    match value {
        Value::String(text) => *text = crate::redact::redact(text).into_owned(),
        Value::Array(values) => values.iter_mut().for_each(redact_json_strings),
        Value::Object(values) => values.values_mut().for_each(redact_json_strings),
        _ => {}
    }
}

fn edited_path(name: &str, input: Option<&Value>) -> Option<String> {
    let writes = matches!(name, "Edit" | "Write");
    if !writes {
        return None;
    }
    let input = input?;
    for key in ["file_path", "path"] {
        if let Some(p) = input.get(key).and_then(Value::as_str) {
            if !p.is_empty() {
                return Some(p.to_string());
            }
        }
    }
    None
}

fn edit_event(name: &str, input: Option<&Value>, ts: Option<DateTime<Utc>>) -> Option<EditEvent> {
    let path = clean_path(&edited_path(name, input)?)?;
    let input = input?;
    let kind = match name {
        "Edit" => EditKind::Edit,
        "Write" => EditKind::Write,
        _ => return None,
    };
    let raw = match kind {
        EditKind::Edit => input
            .get("new_string")
            .or_else(|| input.get("new_str"))
            .and_then(Value::as_str)
            .unwrap_or(""),
        EditKind::Write => input.get("content").and_then(Value::as_str).unwrap_or(""),
        _ => "",
    };
    let snippet = redacted_truncate(raw.trim(), SNIPPET_CAP);
    Some(EditEvent {
        path,
        kind,
        snippet,
        ts,
    })
}

fn ms_to_datetime(ms: i64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(ms / 1000, ((ms % 1000) * 1_000_000) as u32)
}

impl Adapter for Qoder {
    fn name(&self) -> &'static str {
        "qoder"
    }

    fn root(&self) -> Option<PathBuf> {
        db_path().and_then(|p| p.parent().map(|d| d.to_path_buf()))
    }

    fn reconcile_scope(&self) -> Option<String> {
        super::root_scope(self.root().as_deref())
    }

    fn discover(&self) -> Discovered {
        Vec::new().into()
    }

    fn store(&self) -> Option<Store> {
        let db = db_path()?;
        let conn = open_ro(&db).ok()?;
        let mut stmt = conn
            .prepare("SELECT session_id, updated_at, created_at FROM chat_sessions")
            .ok()?;
        let mut keys: Vec<(String, i64)> = Vec::new();
        let mut had_error = false;
        let rows = stmt.query_map([], |r| {
            let id: String = r.get(0)?;
            let updated: Option<i64> = r.get(1)?;
            let created: Option<i64> = r.get(2)?;
            Ok((id, updated.or(created).unwrap_or(0)))
        });
        match rows {
            Ok(rows) => {
                for (id, token) in rows.flatten() {
                    keys.push((make_key(&db, &id), token));
                }
            }
            Err(_) => had_error = true,
        }
        Some(Store {
            keys,
            files: vec![db],
            had_error,
        })
    }

    fn parse_key(&self, key: &str) -> Result<Session> {
        let (db, sid) = key.split_once(KEY_SEP).context("malformed qoder key")?;
        let conn = open_ro(Path::new(db)).with_context(|| format!("open {db}"))?;

        let (cwd, title, created_ms, updated_ms): (
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<i64>,
        ) = conn.query_row(
            "SELECT cwd, title, created_at, updated_at FROM chat_sessions WHERE session_id = ?1",
            params![sid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;

        let started = created_ms.and_then(ms_to_datetime);
        let ended = updated_ms.and_then(ms_to_datetime).or(started);

        let mut stmt = conn.prepare(
            "SELECT role, created_at, payload_json FROM chat_session_messages
             WHERE session_id = ?1 ORDER BY sequence ASC LIMIT 50000",
        )?;

        let mut messages: Vec<Message> = Vec::new();
        let mut touched: Vec<String> = Vec::new();
        let mut edits: Vec<EditEvent> = Vec::new();

        let rows = stmt.query_map(params![sid], |r| {
            let role: String = r.get(0)?;
            let created: Option<i64> = r.get(1)?;
            let payload: String = r.get(2)?;
            Ok((role, created, payload))
        })?;

        for row in rows.flatten() {
            let (role_str, ts_ms, payload_json) = row;
            let ts = ts_ms.and_then(ms_to_datetime);
            let Ok(data) = serde_json::from_str::<Value>(&payload_json) else {
                continue;
            };

            let default_role = if role_str == "assistant" {
                Role::Assistant
            } else {
                Role::User
            };

            if let Some(parts) = data.get("parts").and_then(Value::as_array) {
                for part in parts {
                    let ptype = part.get("type").and_then(Value::as_str).unwrap_or("");
                    match ptype {
                        "text" => {
                            if let Some(t) = part.get("text").and_then(Value::as_str) {
                                let text = t.trim();
                                if !text.is_empty() {
                                    messages.push(Message {
                                        role: default_role,
                                        text: text.to_string(),
                                        ts,
                                        tool: None,
                                    });
                                }
                            }
                        }
                        "tool" => {
                            if let Some(tool_obj) = part.get("tool") {
                                let name =
                                    tool_obj.get("name").and_then(Value::as_str).unwrap_or("?");
                                let input =
                                    tool_obj.get("input").or_else(|| tool_obj.get("arguments"));

                                if let Some(ev) = edit_event(name, input, ts) {
                                    touched.push(ev.path.clone());
                                    edits.push(ev);
                                }

                                let mut args =
                                    input.cloned().unwrap_or_else(|| serde_json::json!({}));
                                redact_json_strings(&mut args);
                                let call_id = tool_obj
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned);
                                messages.push(Message::tool_call(
                                    ts,
                                    ToolCall {
                                        id: call_id.clone(),
                                        name: name.to_string(),
                                        args,
                                    },
                                ));

                                if let Some(resp) = tool_obj.get("response").and_then(Value::as_str)
                                {
                                    let output = bounded_redacted_output(resp);
                                    messages.push(Message::tool_result(
                                        ts,
                                        ToolResult {
                                            call_id,
                                            text: output,
                                            is_error: tool_obj
                                                .get("status")
                                                .and_then(Value::as_str)
                                                == Some("failed"),
                                        },
                                    ));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            } else if let Some(t) = data.get("text").and_then(Value::as_str) {
                let text = t.trim();
                if !text.is_empty() {
                    messages.push(Message {
                        role: default_role,
                        text: text.to_string(),
                        ts,
                        tool: None,
                    });
                }
            }
        }

        let computed_title = title_from_messages(&messages);
        let final_title = title
            .filter(|t| !t.trim().is_empty())
            .unwrap_or(computed_title);

        Ok(Session {
            id: sid.to_string(),
            tool: "qoder",
            path: PathBuf::from(display_path(key)),
            project: cwd.unwrap_or_else(|| "qoder".to_string()),
            started,
            ended,
            title: final_title,
            subagent: false,
            messages,
            touched: dedup_paths(touched),
            edits,
        })
    }

    fn parse(&self, _path: &Path) -> Result<Session> {
        anyhow::bail!("qoder sessions are indexed from sqlite store, not direct files")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_qoder_edit_event() {
        let input = json!({
            "file_path": "d:/project/lib.rs",
            "old_string": "foo",
            "new_string": "bar"
        });
        let ev = edit_event("Edit", Some(&input), None).expect("edit_event should parse Edit");
        assert_eq!(ev.path, "d:/project/lib.rs");
        assert_eq!(ev.snippet, "bar");

        let write_input = json!({
            "file_path": "d:/project/new.rs",
            "content": "pub fn hello() {}"
        });
        let wev =
            edit_event("Write", Some(&write_input), None).expect("edit_event should parse Write");
        assert_eq!(wev.path, "d:/project/new.rs");
        assert_eq!(wev.snippet, "pub fn hello() {}");
    }
}
