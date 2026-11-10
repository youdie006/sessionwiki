//! v2 SQLite messages embed their content and tool events in each row.
use super::{display_path, push, redact_json_strings};
use crate::adapters::{
    bounded_redacted_output, clean_path, dedup_paths, redacted_truncate, title_from_messages,
};
use crate::model::{EditEvent, EditKind, Message, Role, Session, ToolCall, ToolResult};
use crate::util::short_id;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde_json::Value;
use std::path::PathBuf;

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
                                emit_tool_part_v2(
                                    item,
                                    ts,
                                    &mut messages,
                                    &mut touched,
                                    &mut edits,
                                );
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
        tool,
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

// v2 embeds tools in session_message.data.content. Keep its input aliases
// separate from the existing v1/JSON tool-part contract.
fn edited_path_v2(tool: &str, input: Option<&Value>) -> Option<String> {
    if !matches!(tool, "edit" | "write" | "multiedit") {
        return None;
    }
    let input = input?;
    ["filePath", "path", "file_path"]
        .into_iter()
        .find_map(|key| {
            input
                .get(key)
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty())
                .map(str::to_owned)
        })
}

fn edit_event_v2(
    tool: &str,
    input: Option<&Value>,
    ts: Option<DateTime<Utc>>,
) -> Option<EditEvent> {
    let path = clean_path(&edited_path_v2(tool, input)?)?;
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

    let state = part.get("state");
    let status = state
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str);
    let has_error = state.is_some_and(|value| value.get("error").is_some());
    let is_error = matches!(status, Some("error" | "failed")) || has_error;
    if let Some(path) = edited_path_v2(&tool_lower, input).filter(|_| !is_error) {
        if let Some(clean) = clean_path(&path) {
            touched.push(clean);
            if let Some(ev) = edit_event_v2(&tool_lower, input, ts) {
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
