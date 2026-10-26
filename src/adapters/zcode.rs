use super::{
    bounded_redacted_output, clean_path, dedup_paths, redacted_truncate, title_from_messages,
    Adapter, Discovered, Store,
};
use crate::model::{EditEvent, EditKind, Message, Role, Session, ToolCall, ToolResult};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct ZCode;

const KEY_SEP: char = '\u{1f}';
const SNIPPET_CAP: usize = 200;

fn db_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ZCODE_DB").map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(h) = std::env::var_os("ZCODE_HOME").map(PathBuf::from) {
        let c1 = h.join("cli").join("db").join("db.sqlite");
        if c1.is_file() {
            return Some(c1);
        }
        let c2 = h.join("db").join("db.sqlite");
        if c2.is_file() {
            return Some(c2);
        }
    }
    let home = dirs::home_dir()?;
    let p = home.join(".zcode").join("cli").join("db").join("db.sqlite");
    if p.is_file() {
        return Some(p);
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

fn tool_file_path(_name: &str, input: Option<&Value>) -> Option<String> {
    let input = input?;
    for key in ["file_path", "filePath", "path"] {
        if let Some(p) = input.get(key).and_then(Value::as_str) {
            if !p.is_empty() {
                return clean_path(p);
            }
        }
    }
    None
}

fn edit_event(name: &str, input: Option<&Value>, ts: Option<DateTime<Utc>>) -> Option<EditEvent> {
    let writes = matches!(name, "Edit" | "Write" | "edit" | "write");
    if !writes {
        return None;
    }
    let input = input?;
    let path = tool_file_path(name, Some(input))?;
    let kind = match name {
        "Edit" | "edit" => EditKind::Edit,
        "Write" | "write" => EditKind::Write,
        _ => return None,
    };
    let raw = match kind {
        EditKind::Edit => input
            .get("new_string")
            .or_else(|| input.get("new_str"))
            .or_else(|| input.get("replacement"))
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

impl Adapter for ZCode {
    fn name(&self) -> &'static str {
        "zcode"
    }

    fn root(&self) -> Option<PathBuf> {
        db_path().and_then(|p| p.parent().and_then(|d| d.parent()).map(|d| d.to_path_buf()))
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
            .prepare("SELECT id, time_updated, time_created FROM session")
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
        let (db, sid) = key.split_once(KEY_SEP).context("malformed zcode key")?;
        let conn = open_ro(Path::new(db)).with_context(|| format!("open {db}"))?;

        let has_task_type = conn
            .prepare("SELECT task_type FROM session LIMIT 0")
            .is_ok();

        let (directory, title, created_ms, updated_ms, parent_id, task_type): (
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<String>,
            Option<String>,
        ) = if has_task_type {
            conn.query_row(
                "SELECT directory, title, time_created, time_updated, parent_id, task_type
                 FROM session WHERE id = ?1",
                params![sid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )?
        } else {
            conn.query_row(
                "SELECT directory, title, time_created, time_updated, parent_id
                 FROM session WHERE id = ?1",
                params![sid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, None)),
            )?
        };

        let started = created_ms.and_then(ms_to_datetime);
        let ended = updated_ms.and_then(ms_to_datetime).or(started);
        let subagent = parent_id.is_some() || task_type.as_deref() == Some("subagent_child");

        let mut mstmt = conn.prepare(
            "SELECT id, time_created, data FROM message
             WHERE session_id = ?1 ORDER BY sequence IS NULL, sequence, time_created, rowid LIMIT 50000",
        )?;
        let messages_meta: Vec<(String, Option<i64>, Value)> = mstmt
            .query_map(params![sid], |r| {
                let id: String = r.get(0)?;
                let tc: Option<i64> = r.get(1)?;
                let data_str: String = r.get(2)?;
                let data = serde_json::from_str(&data_str).unwrap_or(Value::Null);
                Ok((id, tc, data))
            })?
            .filter_map(|x| x.ok())
            .collect();

        let mut pstmt = conn.prepare(
            "SELECT message_id, data FROM part
             WHERE session_id = ?1 ORDER BY sequence IS NULL, sequence, time_created, id LIMIT 200000",
        )?;
        let mut parts_by_msg: HashMap<String, Vec<Value>> = HashMap::new();
        let prows = pstmt.query_map(params![sid], |r| {
            let mid: String = r.get(0)?;
            let data: String = r.get(1)?;
            Ok((mid, data))
        })?;
        for (mid, data) in prows.flatten() {
            if let Ok(v) = serde_json::from_str::<Value>(&data) {
                parts_by_msg.entry(mid).or_default().push(v);
            }
        }

        let mut messages: Vec<Message> = Vec::new();
        let mut touched: Vec<String> = Vec::new();
        let mut edits: Vec<EditEvent> = Vec::new();

        for (mid, tc, mdata) in messages_meta {
            let role = match mdata.get("role").and_then(Value::as_str) {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                _ => continue,
            };

            let msg_ts = mdata
                .get("time")
                .and_then(|t| t.get("created"))
                .and_then(Value::as_i64)
                .or(tc)
                .and_then(ms_to_datetime);

            if let Some(parts) = parts_by_msg.get(&mid) {
                for part in parts {
                    let ptype = part.get("type").and_then(Value::as_str).unwrap_or("");
                    match ptype {
                        "text" => {
                            if let Some(t) = part.get("text").and_then(Value::as_str) {
                                let text = t.trim();
                                if !text.is_empty() {
                                    messages.push(Message {
                                        role,
                                        text: text.to_string(),
                                        ts: msg_ts,
                                        tool: None,
                                    });
                                }
                            }
                        }
                        "tool" => {
                            let name = part.get("tool").and_then(Value::as_str).unwrap_or("?");
                            let call_id = part
                                .get("callID")
                                .or_else(|| part.get("call_id"))
                                .and_then(Value::as_str)
                                .map(str::to_owned);
                            let state = part.get("state");
                            let input = state.and_then(|s| s.get("input"));
                            let status =
                                state.and_then(|s| s.get("status")).and_then(Value::as_str);

                            if let Some(ev) = edit_event(name, input, msg_ts) {
                                touched.push(ev.path.clone());
                                edits.push(ev);
                            }
                            if let Some(p) = tool_file_path(name, input) {
                                touched.push(p);
                            }

                            let mut args = input.cloned().unwrap_or_else(|| serde_json::json!({}));
                            redact_json_strings(&mut args);

                            messages.push(Message::tool_call(
                                msg_ts,
                                ToolCall {
                                    id: call_id.clone(),
                                    name: name.to_string(),
                                    args,
                                },
                            ));

                            if let Some(st) = state {
                                let is_error = status == Some("error");
                                let output_str = if is_error {
                                    st.get("error")
                                        .and_then(Value::as_str)
                                        .map(str::to_owned)
                                        .or_else(|| st.get("error").map(|e| e.to_string()))
                                        .unwrap_or_default()
                                } else {
                                    st.get("output")
                                        .and_then(Value::as_str)
                                        .map(str::to_owned)
                                        .or_else(|| st.get("output").map(|o| o.to_string()))
                                        .unwrap_or_default()
                                };

                                if !output_str.is_empty() || is_error {
                                    let output = bounded_redacted_output(&output_str);
                                    messages.push(Message::tool_result(
                                        msg_ts,
                                        ToolResult {
                                            call_id,
                                            text: output,
                                            is_error,
                                        },
                                    ));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            } else if let Some(t) = mdata
                .get("text")
                .or_else(|| mdata.get("content"))
                .and_then(Value::as_str)
            {
                let text = t.trim();
                if !text.is_empty() {
                    messages.push(Message {
                        role,
                        text: text.to_string(),
                        ts: msg_ts,
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
            tool: "zcode",
            path: PathBuf::from(display_path(key)),
            project: directory.unwrap_or_else(|| "zcode".to_string()),
            started,
            ended,
            title: final_title,
            subagent,
            messages,
            touched: dedup_paths(touched),
            edits,
        })
    }

    fn parse(&self, _path: &Path) -> Result<Session> {
        anyhow::bail!("zcode sessions are indexed from sqlite store, not direct files")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_zcode_edit_event() {
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

    #[test]
    fn test_zcode_parse_key() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_file = temp_dir.path().join("db.sqlite");
        let conn = Connection::open(&db_file).expect("open disk db");

        conn.execute_batch(
            "CREATE TABLE session (
                id text primary key,
                directory text,
                title text,
                time_created integer,
                time_updated integer,
                parent_id text,
                task_type text
            );
            CREATE TABLE message (
                id text primary key,
                session_id text,
                time_created integer,
                data text,
                sequence integer
            );
            CREATE TABLE part (
                id text primary key,
                message_id text,
                session_id text,
                time_created integer,
                data text,
                sequence integer
            );",
        )
        .expect("create tables");

        conn.execute(
            "INSERT INTO session (id, directory, title, time_created, time_updated, parent_id, task_type)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                "sess_12345",
                "D:/myproject",
                "Test ZCode Session",
                1782644889000i64,
                1782644999000i64,
                Option::<String>::None,
                "interactive",
            ],
        )
        .expect("insert session");

        conn.execute(
            "INSERT INTO message (id, session_id, time_created, data, sequence)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                "msg_user_1",
                "sess_12345",
                1782644889100i64,
                json!({ "role": "user" }).to_string(),
                1,
            ],
        )
        .expect("insert user msg");

        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, data, sequence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                "part_1",
                "msg_user_1",
                "sess_12345",
                1782644889100i64,
                json!({ "type": "text", "text": "Please fix lib.rs" }).to_string(),
                1,
            ],
        )
        .expect("insert user part");

        conn.execute(
            "INSERT INTO message (id, session_id, time_created, data, sequence)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                "msg_asst_1",
                "sess_12345",
                1782644890000i64,
                json!({ "role": "assistant" }).to_string(),
                2,
            ],
        )
        .expect("insert assistant msg");

        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, data, sequence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                "part_2",
                "msg_asst_1",
                "sess_12345",
                1782644890100i64,
                json!({
                    "type": "tool",
                    "tool": "Edit",
                    "callID": "call_edit_1",
                    "state": {
                        "status": "completed",
                        "input": {
                            "file_path": "D:/myproject/src/lib.rs",
                            "new_string": "fixed code"
                        },
                        "output": "Edit successful"
                    }
                }).to_string(),
                1,
            ],
        )
        .expect("insert tool part");

        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, data, sequence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                "part_3",
                "msg_asst_1",
                "sess_12345",
                1782644891000i64,
                json!({ "type": "text", "text": "I fixed the issue." }).to_string(),
                2,
            ],
        )
        .expect("insert assistant text part");

        drop(conn);

        let key = make_key(&db_file, "sess_12345");
        let zcode = ZCode;
        let session = zcode.parse_key(&key).expect("parse_key must succeed");

        assert_eq!(session.id, "sess_12345");
        assert_eq!(session.tool, "zcode");
        assert_eq!(session.title, "Test ZCode Session");
        assert_eq!(session.project, "D:/myproject");
        assert!(!session.subagent);
        assert_eq!(session.touched, vec!["D:/myproject/src/lib.rs"]);
        assert_eq!(session.edits.len(), 1);
        assert_eq!(session.edits[0].path, "D:/myproject/src/lib.rs");
        assert_eq!(session.edits[0].snippet, "fixed code");
        assert_eq!(session.messages.len(), 4); // user text, tool call, tool result, assistant text
    }

    #[test]
    fn test_zcode_real_db_if_present() {
        if db_path().is_some() {
            let zcode = ZCode;
            if let Some(store) = zcode.store() {
                if let Some((key, _)) = store.keys.first() {
                    let sess = zcode.parse_key(key).expect("parse real zcode session");
                    assert_eq!(sess.tool, "zcode");
                    assert!(!sess.id.is_empty());
                    println!("Parsed real zcode session: id={}, title={}, msgs={}, touched={:?}", sess.id, sess.title, sess.messages.len(), sess.touched);
                }
            }
        }
    }
}
