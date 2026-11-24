//! v2 SQLite messages embed their content and tool events in each row.
use super::{display_path, push, redact_json_strings};
use crate::adapters::{bounded_redacted_output, redacted_truncate, title_from_messages};
use crate::model::{Message, Role, Session, ToolCall, ToolResult};
use crate::util::short_id;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde_json::Value;
use std::path::PathBuf;

fn has_message_updated(conn: &Connection) -> rusqlite::Result<bool> {
    // Older schemas and synthetic databases can omit this message column.
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('session_message')
                       WHERE name = 'time_updated')",
        [],
        |row| row.get(0),
    )
}

pub(super) fn inventory_sql(conn: &Connection) -> rusqlite::Result<&'static str> {
    Ok(if has_message_updated(conn)? {
        "SELECT s.id, s.time_updated, s.time_created, m.updated
         FROM session_v2 AS s
         LEFT JOIN (
             SELECT session_id, max(time_updated) AS updated
             FROM session_message GROUP BY session_id
         ) AS m ON m.session_id = s.id"
    } else {
        "SELECT id, time_updated, time_created, NULL FROM session_v2"
    })
}

pub(super) fn latest_message_update(conn: &Connection, sid: &str) -> rusqlite::Result<Option<i64>> {
    if has_message_updated(conn)? {
        conn.query_row(
            "SELECT max(time_updated) FROM session_message WHERE session_id = ?1",
            [sid],
            |row| row.get(0),
        )
    } else {
        Ok(None)
    }
}

pub(super) fn activity_millis(
    created: Option<i64>,
    updated: Option<i64>,
    message_updated: Option<i64>,
) -> Option<i64> {
    updated.max(message_updated).or(created)
}

pub(super) fn parse(
    tool: &'static str,
    conn: &Connection,
    sid: &str,
    key: &str,
) -> Result<Session> {
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

    let message_updated = latest_message_update(conn, sid)?;
    let mut stmt = conn.prepare(
        "SELECT type, time_created, data FROM session_message
         WHERE session_id = ?1 ORDER BY seq ASC, id ASC LIMIT 200000",
    )?;

    let mut messages: Vec<Message> = Vec::new();

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
                                emit_tool_part_v2(item, ts, &mut messages);
                            }
                            _ => {}
                        }
                    }
                }
            }
            "shell" => {
                if let Some(command) = data.get("command").and_then(Value::as_str) {
                    let id = data
                        .get("shellID")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let mut args = serde_json::json!({"command":command});
                    redact_json_strings(&mut args);
                    messages.push(Message::tool_call(
                        ts,
                        ToolCall {
                            id: id.clone(),
                            name: "shell".into(),
                            args,
                        },
                    ));
                    let status = data.get("status").and_then(Value::as_str);
                    if data.get("output").is_some()
                        || matches!(status, Some("exited" | "timeout" | "killed" | "error"))
                    {
                        let text = data
                            .get("output")
                            .and_then(|output| {
                                output
                                    .as_str()
                                    .or_else(|| output.get("output").and_then(Value::as_str))
                            })
                            .unwrap_or_default();
                        messages.push(Message::tool_result(
                            ts,
                            ToolResult {
                                call_id: id,
                                text: bounded_redacted_output(text),
                                is_error: data
                                    .get("exit")
                                    .and_then(Value::as_i64)
                                    .is_some_and(|exit| exit != 0)
                                    || matches!(status, Some("timeout" | "killed" | "error")),
                            },
                        ));
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
        tool,
        path: PathBuf::from(display_path(key)),
        project,
        started: created.and_then(DateTime::from_timestamp_millis),
        ended: activity_millis(created, updated, message_updated)
            .and_then(DateTime::from_timestamp_millis),
        title,
        subagent: parent.is_some_and(|p| !p.is_empty()),
        messages,
    })
}

fn emit_tool_part_v2(part: &Value, ts: Option<DateTime<Utc>>, messages: &mut Vec<Message>) {
    let name = part
        .get("name")
        .or_else(|| part.get("tool"))
        .and_then(Value::as_str)
        .unwrap_or("?");
    let input = part.pointer("/state/input").or_else(|| part.get("input"));

    let state = part.get("state");
    let status = state
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str);
    let has_error = state.is_some_and(|value| value.get("error").is_some());
    let is_error = matches!(status, Some("error" | "failed")) || has_error;

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
            }
        }
        if let Some(error) = s.get("error").and_then(|value| {
            value
                .as_str()
                .or_else(|| value.get("message").and_then(Value::as_str))
        }) {
            output_text = Some(match output_text {
                Some(output) if !output.is_empty() => format!("{output}\n{error}"),
                _ => error.to_owned(),
            });
        }
    }

    if output_text.is_some()
        || matches!(status, Some("completed" | "error" | "failed"))
        || has_error
    {
        let text = output_text.unwrap_or_default();
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
