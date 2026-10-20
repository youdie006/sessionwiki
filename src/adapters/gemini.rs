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
/// `~/.gemini/tmp/<project>/chats/session-*.json` (newer CLIs write `.jsonl`, see [`fold_jsonl`]):
/// `{ sessionId, startTime, lastUpdated, messages: [{ type, content, timestamp }] }`.
pub struct Gemini;

/// A chat file: `chats/*.json`, or `chats/*.jsonl` as Gemini CLI writes now.
fn is_chat(path: &Path) -> bool {
    path.extension()
        .is_some_and(|x| x == "json" || x == "jsonl")
        && path
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|d| d == "chats")
}

/// Rebuild the chat document a JSONL chat records: its header line, `$set`
/// lines that replace fields, and message lines, a repeated id replacing the
/// earlier copy of that message as it grew.
fn fold_jsonl(raw: &str) -> Option<Value> {
    let mut doc = serde_json::Map::new();
    let mut messages: Vec<Value> = Vec::new();
    let mut read_any = false;
    for line in raw.lines() {
        let Ok(Value::Object(entry)) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        read_any = true;
        if let Some(Value::Object(set)) = entry.get("$set") {
            for (key, value) in set {
                match (key.as_str(), value) {
                    ("messages", Value::Array(list)) => messages = list.clone(),
                    _ => {
                        doc.insert(key.clone(), value.clone());
                    }
                }
            }
        } else if entry.contains_key("type") && entry.contains_key("id") {
            let message = Value::Object(entry);
            match messages
                .iter_mut()
                .find(|m| m.get("id") == message.get("id"))
            {
                Some(earlier) => *earlier = message,
                None => messages.push(message),
            }
        } else {
            doc.extend(entry);
        }
    }
    doc.insert("messages".into(), Value::Array(messages));
    read_any.then_some(Value::Object(doc))
}

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
            .filter(|e| is_chat(e.path()))
            .map(|e| e.into_path())
            .collect();
        Discovered { files, had_error }
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        let raw = crate::util::read_to_string_capped(path)?;
        // A `.jsonl` chat is a log to fold, unless the file is one whole
        // document after all.
        let v: Value = match serde_json::from_str(&raw) {
            Err(_) if path.extension().is_some_and(|x| x == "jsonl") => fold_jsonl(&raw)
                .with_context(|| format!("parse {}: no JSON lines", path.display()))?,
            parsed => parsed.with_context(|| format!("parse {}", path.display()))?,
        };

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Role;

    /// Gemini CLI now writes each chat as JSONL: a header line, `$set` lines
    /// that replace fields, and one line per message - repeated with the same id
    /// as the message grows. Only `.json` chats were read, so on one machine the
    /// 14 written since mid-June were never indexed.
    #[test]
    fn a_jsonl_chat_is_folded_into_its_messages() {
        let dir = tempfile::tempdir().unwrap();
        let chats = dir.path().join("proj").join("chats");
        std::fs::create_dir_all(&chats).unwrap();
        let file = chats.join("session-2026-07-14T02-10-bbfe7356.jsonl");
        std::fs::write(
            &file,
            [
                r#"{"sessionId":"s1","projectHash":"h","startTime":"2026-07-14T02:10:00.000Z","lastUpdated":"2026-07-14T02:10:00.000Z","kind":"main"}"#,
                r#"{"$set":{"messages":[{"id":"m1","timestamp":"2026-07-14T02:10:01.000Z","type":"user","content":[{"text":"rename the config loader"}]}],"lastUpdated":"2026-07-14T02:10:01.000Z"}}"#,
                r#"{"id":"m2","timestamp":"2026-07-14T02:10:02.000Z","type":"gemini","content":"Look"}"#,
                r#"{"id":"m2","timestamp":"2026-07-14T02:10:03.000Z","type":"gemini","content":"Looking at the loader now."}"#,
                r#"{"$set":{"lastUpdated":"2026-07-14T02:10:03.000Z"}}"#,
                r#"{"id":"m3","timestamp":"2026-07-14T02:10:04.000Z","type":"user","content":[{"text":"thanks"}]}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        assert!(is_chat(&file));
        let session = Gemini.parse(&file).unwrap();
        let said: Vec<(Role, String)> = session
            .messages
            .iter()
            .map(|m| (m.role, m.text.clone()))
            .collect();
        assert_eq!(
            said,
            vec![
                (Role::User, "rename the config loader".to_string()),
                (Role::Assistant, "Looking at the loader now.".to_string()),
                (Role::User, "thanks".to_string()),
            ]
        );
        assert_eq!(
            session.ended.map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-07-14T02:10:03+00:00")
        );
        assert!(is_chat(&chats.join("session-old.json")));
        assert!(!is_chat(&dir.path().join("proj").join("logs.json")));
    }
}
