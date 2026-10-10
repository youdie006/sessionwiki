use super::{
    bounded_redacted_output, ok_or_flag, parse_ts, title_from_messages, Adapter, Discovered,
};
use crate::model::{Message, Role, Session, ToolCall, ToolResult};
use crate::util::short_id;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Gemini CLI stores one JSON document per saved chat under
/// `~/.gemini/tmp/<project>/chats/session-*.json`:
/// `{ sessionId, startTime, lastUpdated, messages: [{ type, content, timestamp }] }`.
pub struct Gemini;

/// The store dir is named by a slug (older versions: a hash of the path), not
/// the path itself; newer versions record the real directory in
/// `.project_root` beside `chats/`. That path is what other tools' projects,
/// `--project` and the recall hook compare against.
pub(crate) fn project_root(chat: &Path) -> Option<String> {
    let root =
        crate::util::read_to_string_capped(&chat.ancestors().nth(2)?.join(".project_root")).ok()?;
    let root = root.trim();
    Path::new(root).is_absolute().then(|| root.to_string())
}

impl Adapter for Gemini {
    fn name(&self) -> &'static str {
        "gemini"
    }

    fn root(&self) -> Option<PathBuf> {
        Some(dirs::home_dir()?.join(".gemini").join("tmp"))
    }

    fn discover(&self) -> Discovered {
        let Some(root) = self.root() else {
            return Vec::new().into();
        };
        if !root.exists() {
            return Vec::new().into(); // no store on this machine - normal
        }
        let mut had_error = false;
        let files = WalkDir::new(root)
            .max_depth(3)
            .into_iter()
            .filter_map(|e| ok_or_flag(e, &mut had_error))
            .filter(|e| e.file_type().is_file())
            .filter(|e| {
                e.path().extension().is_some_and(|x| x == "json")
                    && e.path()
                        .parent()
                        .and_then(|p| p.file_name())
                        .is_some_and(|d| d == "chats")
            })
            .map(|e| e.into_path())
            .collect();
        Discovered { files, had_error }
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        let raw = crate::util::read_to_string_capped(path)?;
        let v: Value =
            serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;

        let started = v
            .get("startTime")
            .and_then(Value::as_str)
            .and_then(parse_ts);
        let ended = v
            .get("lastUpdated")
            .and_then(Value::as_str)
            .and_then(parse_ts);

        let mut messages: Vec<Message> = Vec::new();
        // Gemini CLI records both toolCalls[].result and, in newer transcripts,
        // the same functionResponse in the following user message. Keep the
        // per-turn ids so one execution is not indexed twice.
        let mut recorded_results = HashSet::new();
        if let Some(Value::Array(items)) = v.get("messages") {
            for m in items {
                let kind = m.get("type").and_then(Value::as_str);
                let role = match kind {
                    Some("user") => Role::User,
                    Some("gemini") | Some("assistant") | Some("model") => Role::Assistant,
                    _ => continue,
                };
                let ts = m
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(parse_ts);

                let tool_calls = m.get("toolCalls").and_then(Value::as_array);
                if role == Role::Assistant {
                    recorded_results.clear();
                }

                match m.get("content") {
                    Some(Value::String(text)) => push(&mut messages, role, text, ts),
                    Some(Value::Array(blocks)) => {
                        let has_recorded_calls = tool_calls.is_some_and(|calls| !calls.is_empty());
                        for block in blocks {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                push(&mut messages, role, text, ts);
                            }

                            // Older and alternate transcript records retain the
                            // Gemini wire-format functionCall/functionResponse
                            // parts directly in content instead of toolCalls[].
                            if let Some(call) = block.get("functionCall") {
                                if role == Role::Assistant && !has_recorded_calls {
                                    emit_function_call(&mut messages, call, ts);
                                }
                            }
                            if let Some(result) = block.get("functionResponse") {
                                let id = result
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .filter(|id| !id.is_empty())
                                    .map(str::to_owned);
                                if id.as_ref().is_some_and(|id| recorded_results.contains(id)) {
                                    continue;
                                }
                                let response = result.get("response");
                                messages.push(Message::tool_result(
                                    ts,
                                    ToolResult {
                                        call_id: id,
                                        text: bounded_redacted_output(&tool_output_text(response)),
                                        is_error: response_has_error(response),
                                    },
                                ));
                            }
                        }
                    }
                    _ => {}
                }

                if role == Role::Assistant {
                    if let Some(calls) = tool_calls {
                        for call in calls {
                            let id = call
                                .get("id")
                                .and_then(Value::as_str)
                                .filter(|id| !id.is_empty())
                                .map(str::to_owned);
                            let name = call
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("?")
                                .to_owned();
                            let call_ts = call
                                .get("timestamp")
                                .and_then(Value::as_str)
                                .and_then(parse_ts)
                                .or(ts);
                            let mut args = call.get("args").cloned().unwrap_or_else(|| json!({}));
                            redact_json_strings(&mut args);
                            messages.push(Message::tool_call(
                                call_ts,
                                ToolCall {
                                    id: id.clone(),
                                    name,
                                    args,
                                },
                            ));

                            let result = call.get("result").filter(|result| !result.is_null());
                            let status = call
                                .get("status")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            let is_error = matches!(status, "error" | "cancelled");
                            if result.is_some() || is_error {
                                messages.push(Message::tool_result(
                                    call_ts,
                                    ToolResult {
                                        call_id: id.clone(),
                                        text: bounded_redacted_output(&tool_output_text(result)),
                                        is_error,
                                    },
                                ));
                                if let Some(id) = id {
                                    recorded_results.insert(id);
                                }
                            }
                        }
                    }
                }
            }
        }

        // ~/.gemini/tmp/<project>/chats/file.json
        let project = project_root(path)
            .or_else(|| {
                path.ancestors()
                    .nth(2)
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_default();
        let title = title_from_messages(&messages);

        Ok(Session {
            id: short_id(&path.to_string_lossy()),
            tool: self.name(),
            path: path.to_path_buf(),
            project,
            started,
            ended,
            title,
            subagent: false,
            messages,
            // Gemini CLI chat logs do not record structured file edits, so
            // there is nothing to link to the codebase here.
            touched: Vec::new(),
            edits: Vec::new(),
        })
    }
}

fn push(
    messages: &mut Vec<Message>,
    role: Role,
    text: &str,
    ts: Option<chrono::DateTime<chrono::Utc>>,
) {
    let text = text.trim();
    if !text.is_empty() {
        messages.push(Message {
            role,
            text: text.to_owned(),
            ts,
            tool: None,
        });
    }
}

fn emit_function_call(
    messages: &mut Vec<Message>,
    call: &Value,
    ts: Option<chrono::DateTime<chrono::Utc>>,
) {
    let id = call
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    let name = call
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_owned();
    let mut args = call.get("args").cloned().unwrap_or_else(|| json!({}));
    redact_json_strings(&mut args);
    messages.push(Message::tool_call(ts, ToolCall { id, name, args }));
}

/// Tool outputs may be plain strings, text parts, or functionResponse wrappers.
/// Extract only textual fields so inline image/audio payloads never become
/// indexed output.
fn tool_output_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| tool_output_text(Some(value)))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Object(object)) => {
            for key in [
                "output",
                "text",
                "content",
                "message",
                "result",
                "response",
                "functionResponse",
                "parts",
                "error",
            ] {
                if let Some(text) = object
                    .get(key)
                    .map(|value| tool_output_text(Some(value)))
                    .filter(|text| !text.is_empty())
                {
                    return text;
                }
            }
            String::new()
        }
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(value)) => value.to_string(),
        _ => String::new(),
    }
}

fn response_has_error(response: Option<&Value>) -> bool {
    response.is_some_and(|response| {
        let error = response.get("error");
        error.is_some_and(|value| !value.is_null() && value != &Value::Bool(false))
            || response.get("status").and_then(Value::as_str) == Some("error")
    })
}

fn redact_json_strings(value: &mut Value) {
    match value {
        Value::String(text) => *text = crate::redact::redact(text).into_owned(),
        Value::Array(values) => values.iter_mut().for_each(redact_json_strings),
        Value::Object(values) => values.values_mut().for_each(redact_json_strings),
        _ => {}
    }
}
