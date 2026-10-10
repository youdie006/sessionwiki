use super::{
    bounded_redacted_output, dedup_paths, ok_or_flag, redacted_truncate, title_from_messages,
    Adapter, Discovered,
};
use crate::model::{Message, Role, Session, ToolCall, ToolResult};
use crate::util::short_id;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Continue (github continuedev/continue) stores one session per file at
/// `~/.continue/sessions/<sessionId>.json`, plus a `sessions.json` index. The
/// session file has no timestamps; the only time signal is `dateCreated`
/// (epoch-ms as a string) in the index, which we read for `started`.
///
/// `history` is an array of items shaped `{ message: {role, content, toolCalls},
/// toolCallStates: [...] }` - role/text live under `message`. File edits are in
/// `toolCallStates[].parsedArgs.filepath` (already parsed) or, failing that, in
/// `message.toolCalls[].function.arguments` (a JSON-encoded string).
pub struct Continue;

impl Adapter for Continue {
    fn name(&self) -> &'static str {
        "continue"
    }

    fn root(&self) -> Option<PathBuf> {
        Some(continue_dir()?.join("sessions"))
    }

    fn discover(&self) -> Discovered {
        let Some(dir) = self.root() else {
            return Vec::new().into();
        };
        if !dir.exists() {
            return Vec::new().into(); // no store on this machine - normal
        }
        let mut had_error = false;
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            // The directory exists but cannot be listed: a PARTIAL (empty)
            // result. Without the flag, reconciliation would archive every
            // Continue session over a transient permission error.
            Err(_) => {
                return Discovered {
                    files: vec![],
                    had_error: true,
                }
            }
        };
        let files = entries
            .filter_map(|e| ok_or_flag(e, &mut had_error))
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            // The index lives beside the sessions; it is not one.
            .filter(|p| p.file_name().is_some_and(|n| n != "sessions.json"))
            .collect();
        Discovered { files, had_error }
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        parse_session(self.name(), path)
    }
}

/// `$CONTINUE_GLOBAL_DIR` overrides the tree on every OS; otherwise `~/.continue`.
fn continue_dir() -> Option<PathBuf> {
    std::env::var_os("CONTINUE_GLOBAL_DIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| dirs::home_dir().map(|h| h.join(".continue")))
}

/// Tools whose args name a file the session created or edited. The path is
/// always `filepath`. Read/search/ls tools are excluded.
const EDIT_TOOLS: &[&str] = &[
    "create_new_file",
    "edit_existing_file",
    "single_find_and_replace",
    "multi_edit",
];

fn parse_session(tool: &'static str, path: &Path) -> Result<Session> {
    let raw = crate::util::read_to_string_capped(path)?;
    let s: Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;

    let project = s
        .get("workspaceDirectory")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let stored_title = s
        .get("title")
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty() && *t != "New Session")
        .map(|t| redacted_truncate(t, 80));

    let mut messages: Vec<Message> = Vec::new();
    let mut touched: Vec<String> = Vec::new();

    if let Some(history) = s.get("history").and_then(Value::as_array) {
        let explicit_result_ids: HashSet<&str> = history
            .iter()
            .filter(|item| item.pointer("/message/role").and_then(Value::as_str) == Some("tool"))
            .filter_map(|item| item.pointer("/message/toolCallId").and_then(Value::as_str))
            .collect();

        for item in history {
            let msg = item.get("message");
            let role = match msg.and_then(|m| m.get("role")).and_then(Value::as_str) {
                Some("user") => Role::User,
                Some("assistant") | Some("thinking") => Role::Assistant,
                Some("tool") => Role::Tool,
                // system / unknown - skip.
                _ => continue,
            };

            if role == Role::Tool {
                if let Some(content) = msg.and_then(|m| m.get("content")) {
                    let call_id = msg
                        .and_then(|m| m.get("toolCallId"))
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty())
                        .map(str::to_owned);
                    let is_error = call_id
                        .as_deref()
                        .is_some_and(|id| tool_status_is_error(history, id));
                    messages.push(Message::tool_result(
                        None,
                        ToolResult {
                            call_id,
                            text: bounded_redacted_output(&content_text(content)),
                            is_error,
                        },
                    ));
                }
                continue;
            }

            if let Some(text) = msg.and_then(|m| m.get("content")).map(content_text) {
                push(&mut messages, role, &text);
            }

            if role != Role::Assistant {
                continue;
            }

            for mut call in tool_calls(item) {
                redact_json_strings(&mut call.args);
                let filepath = call
                    .args
                    .get("filepath")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if EDIT_TOOLS.contains(&call.name.as_str()) && !filepath.is_empty() {
                    touched.push(filepath.to_owned());
                }
                messages.push(Message::tool_call(None, call));
            }

            if let Some(states) = item.get("toolCallStates").and_then(Value::as_array) {
                for state in states {
                    let Some(id) = state
                        .get("toolCallId")
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty())
                    else {
                        continue;
                    };
                    if explicit_result_ids.contains(id) {
                        continue;
                    }
                    let Some(output) = state_output(state) else {
                        continue;
                    };
                    messages.push(Message::tool_result(
                        None,
                        ToolResult {
                            call_id: Some(id.to_owned()),
                            text: bounded_redacted_output(&output),
                            is_error: status_is_error(state.get("status").and_then(Value::as_str)),
                        },
                    ));
                }
            }
        }
    }

    let title = stored_title.unwrap_or_else(|| title_from_messages(&messages));
    let started = started_from_index(path);

    Ok(Session {
        id: short_id(&path.to_string_lossy()),
        tool,
        path: path.to_path_buf(),
        project,
        started,
        ended: None,
        title,
        subagent: false,
        messages,
        touched: dedup_paths(touched),
        edits: Vec::new(),
    })
}

/// `content` is `string | MessagePart[]`; join the text parts, drop images.
fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Calls from `message.toolCalls`, enriched with parsed state args when
/// available. Older history entries containing only toolCallStates still work.
fn tool_calls(item: &Value) -> Vec<ToolCall> {
    if let Some(calls) = item
        .pointer("/message/toolCalls")
        .and_then(Value::as_array)
        .filter(|calls| !calls.is_empty())
    {
        return calls
            .iter()
            .enumerate()
            .map(|(index, call)| {
                let state =
                    matching_tool_state(item, call.get("id").and_then(Value::as_str), index);
                let id = call
                    .get("id")
                    .and_then(Value::as_str)
                    .or_else(|| state.and_then(|st| st.get("toolCallId").and_then(Value::as_str)))
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned);
                let name = call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        state
                            .and_then(|st| st.pointer("/toolCall/function/name"))
                            .and_then(Value::as_str)
                    })
                    .unwrap_or("?")
                    .to_owned();
                let args = state
                    .and_then(|st| st.get("parsedArgs"))
                    .filter(|args| !args.is_null())
                    .cloned()
                    .unwrap_or_else(|| call_arguments(call));
                ToolCall { id, name, args }
            })
            .collect();
    }

    item.get("toolCallStates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|state| {
            let tool_call = state.get("toolCall");
            let id = state
                .get("toolCallId")
                .and_then(Value::as_str)
                .or_else(|| {
                    tool_call
                        .and_then(|call| call.get("id"))
                        .and_then(Value::as_str)
                })
                .filter(|id| !id.is_empty())
                .map(str::to_owned);
            let name = tool_call
                .and_then(|call| call.pointer("/function/name"))
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_owned();
            let args = state
                .get("parsedArgs")
                .filter(|args| !args.is_null())
                .cloned()
                .unwrap_or_else(|| {
                    tool_call
                        .map(call_arguments)
                        .unwrap_or_else(|| serde_json::json!({}))
                });
            ToolCall { id, name, args }
        })
        .collect()
}

fn matching_tool_state<'a>(item: &'a Value, id: Option<&str>, index: usize) -> Option<&'a Value> {
    let states = item.get("toolCallStates")?.as_array()?;
    id.filter(|id| !id.is_empty())
        .and_then(|id| {
            states
                .iter()
                .find(|state| state.get("toolCallId").and_then(Value::as_str) == Some(id))
        })
        .or_else(|| states.get(index))
}

fn call_arguments(call: &Value) -> Value {
    match call.pointer("/function/arguments") {
        Some(Value::String(arguments)) => {
            serde_json::from_str(arguments).unwrap_or_else(|_| Value::String(arguments.clone()))
        }
        Some(arguments) => arguments.clone(),
        None => serde_json::json!({}),
    }
}

fn tool_status_is_error(history: &[Value], id: &str) -> bool {
    history
        .iter()
        .flat_map(|item| {
            item.get("toolCallStates")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .find(|state| state.get("toolCallId").and_then(Value::as_str) == Some(id))
        .is_some_and(|state| status_is_error(state.get("status").and_then(Value::as_str)))
}

fn status_is_error(status: Option<&str>) -> bool {
    status.is_some_and(|status| {
        matches!(
            status.to_ascii_lowercase().as_str(),
            "errored" | "error" | "failed" | "canceled" | "cancelled"
        )
    })
}

fn state_output(state: &Value) -> Option<String> {
    let output = state.get("output")?.as_array()?;
    let text = output
        .iter()
        .filter_map(|item| item.get("content"))
        .map(content_text)
        .filter(|content| !content.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn redact_json_strings(value: &mut Value) {
    match value {
        Value::String(text) => *text = crate::redact::redact(text).into_owned(),
        Value::Array(values) => values.iter_mut().for_each(redact_json_strings),
        Value::Object(values) => values.values_mut().for_each(redact_json_strings),
        _ => {}
    }
}

/// The session file carries no time; `sessions.json` records `dateCreated`
/// (epoch ms, as a string) per id. Match by the file's stem.
fn started_from_index(path: &Path) -> Option<DateTime<Utc>> {
    let id = path.file_stem()?.to_string_lossy();
    let index = path.parent()?.join("sessions.json");
    let raw = crate::util::read_to_string_capped(&index).ok()?;
    let list: Value = serde_json::from_str(&raw).ok()?;
    let created = list.as_array()?.iter().find_map(|e| {
        let sid = e.get("sessionId").and_then(Value::as_str)?;
        if sid == id {
            e.get("dateCreated").and_then(Value::as_str)
        } else {
            None
        }
    })?;
    DateTime::from_timestamp_millis(created.parse::<i64>().ok()?)
}

fn push(messages: &mut Vec<Message>, role: Role, text: &str) {
    let text = text.trim();
    if !text.is_empty() {
        messages.push(Message {
            role,
            text: text.to_string(),
            ts: None,
            tool: None,
        });
    }
}
