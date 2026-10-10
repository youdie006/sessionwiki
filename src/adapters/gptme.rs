use super::{
    bounded_redacted_output, ok_or_flag, parse_ts, title_from_messages, Adapter, Discovered,
};
use crate::model::{Message, Role, Session, ToolCall, ToolResult};
use crate::util::short_id;
use anyhow::Result;
use chrono::NaiveDateTime;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// gptme stores one JSONL file per session under
/// `~/.local/share/gptme/logs/<session-name>/conversation.jsonl`.
/// Each line is a message with `role`, `content`, and `timestamp` fields.
/// Lines with `"pinned": true` are system-prompt boilerplate injected at
/// startup. A system-role message after an assistant tool block is its result;
/// unpaired system messages are context injections or compaction notices.
pub struct Gptme;

impl Adapter for Gptme {
    fn name(&self) -> &'static str {
        "gptme"
    }

    fn root(&self) -> Option<PathBuf> {
        Some(dirs::data_local_dir()?.join("gptme").join("logs"))
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
            .max_depth(2)
            .into_iter()
            .filter_map(|e| ok_or_flag(e, &mut had_error))
            .filter(|e| e.file_type().is_file())
            .filter(|e| e.file_name().to_string_lossy() == "conversation.jsonl")
            .map(|e| e.into_path())
            .collect();
        Discovered { files, had_error }
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        // Over the byte cap: window (head+tail) instead of dropping the session.
        let (lines, windowed) = crate::util::session_lines(path)?;

        let mut messages: Vec<Message> = Vec::new();
        let mut pending_calls = VecDeque::new();
        let mut started = None;
        let mut ended = None;

        for (line_index, line) in lines.iter().enumerate() {
            let Ok(Value::Object(mut v)) = serde_json::from_str::<Value>(line) else {
                continue;
            };

            // Drop pinned lines (system-prompt boilerplate injected at startup).
            if v.get("pinned").and_then(Value::as_bool).unwrap_or(false) {
                continue;
            }

            let ts = v
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_gptme_ts);

            let role = v.get("role").and_then(Value::as_str).map(str::to_owned);
            let text = match v.remove("content") {
                Some(Value::String(s)) => s,
                Some(Value::Array(blocks)) => blocks
                    .into_iter()
                    .filter_map(|b| match b {
                        Value::Object(mut map) => match map.remove("content") {
                            Some(Value::String(s)) => Some(s),
                            _ => None,
                        },
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => continue,
            };

            match role.as_deref() {
                Some("user") => {
                    pending_calls.clear();
                    record_time(ts, &mut started, &mut ended);
                    push(&mut messages, Role::User, &text, ts);
                }
                Some("assistant") => {
                    // A later assistant turn closes an earlier call group. Any
                    // calls left without results remain pending summaries.
                    pending_calls.clear();
                    record_time(ts, &mut started, &mut ended);
                    let mut call_index = 0;
                    for part in assistant_parts(&text) {
                        match part {
                            AssistantPart::Text(text) => {
                                push(&mut messages, Role::Assistant, &text, ts);
                            }
                            AssistantPart::ToolCall { name, args } => {
                                let id = format!("gptme-{line_index}-{call_index}");
                                call_index += 1;
                                pending_calls.push_back(id.clone());
                                messages.push(Message::tool_call(
                                    ts,
                                    ToolCall {
                                        id: Some(id),
                                        name,
                                        args,
                                    },
                                ));
                            }
                        }
                    }
                }
                Some("system") => {
                    if let Some(call_id) = pending_calls.pop_front() {
                        record_time(ts, &mut started, &mut ended);
                        messages.push(Message::tool_result(
                            ts,
                            ToolResult {
                                call_id: Some(call_id),
                                text: bounded_redacted_output(&tool_output_text(&text)),
                                // gptme persists tool output as plain system
                                // content; it does not store a result status.
                                is_error: false,
                            },
                        ));
                    }
                }
                // System messages without a preceding recognized call are
                // context injections/compaction notices, as before.
                _ => {}
            }
        }

        // Label by session directory name (human-readable slug).
        let project = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let title = if windowed {
            format!("[large] {}", title_from_messages(&messages))
        } else {
            title_from_messages(&messages)
        };

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
            touched: Vec::new(),
            edits: Vec::new(),
        })
    }
}

enum AssistantPart {
    Text(String),
    ToolCall { name: String, args: Value },
}

fn assistant_parts(content: &str) -> Vec<AssistantPart> {
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let mut offsets = Vec::with_capacity(lines.len() + 1);
    offsets.push(0);
    for line in &lines {
        offsets.push(offsets.last().copied().unwrap_or_default() + line.len());
    }

    let mut parts = Vec::new();
    let mut segment_start = 0;
    let mut line_index = 0;
    while line_index < lines.len() {
        let opening = lines[line_index]
            .trim_start()
            .trim_end_matches(['\r', '\n'])
            .strip_prefix("```");
        let Some(header) = opening else {
            line_index += 1;
            continue;
        };
        let Some(close_index) = (line_index + 1..lines.len())
            .find(|&index| lines[index].trim().trim_end_matches(['\r', '\n']).trim() == "```")
        else {
            break;
        };

        let start = offsets[line_index];
        let body_start = offsets[line_index + 1];
        let close_start = offsets[close_index];
        let end = offsets[close_index + 1];
        if segment_start < start {
            parts.push(AssistantPart::Text(
                content[segment_start..start].to_owned(),
            ));
        }

        let mut header_parts = header.split_whitespace();
        let tool = header_parts.next().unwrap_or_default();
        let header_args = header_parts.collect::<Vec<_>>().join(" ");
        if is_tool_name(tool) {
            parts.push(AssistantPart::ToolCall {
                name: tool.to_owned(),
                args: tool_args(tool, &header_args, &content[body_start..close_start]),
            });
        } else {
            parts.push(AssistantPart::Text(content[start..end].to_owned()));
        }
        segment_start = end;
        line_index = close_index + 1;
    }
    if segment_start < content.len() {
        parts.push(AssistantPart::Text(content[segment_start..].to_owned()));
    }
    parts
}

fn is_tool_name(name: &str) -> bool {
    matches!(
        name,
        "append"
            | "bash"
            | "browser"
            | "chats"
            | "computer"
            | "gh"
            | "ipython"
            | "mcp"
            | "morph"
            | "patch"
            | "python"
            | "rag"
            | "read"
            | "save"
            | "screenshot"
            | "shell"
            | "subagent"
            | "tmux"
            | "vision"
    )
}

fn tool_args(name: &str, header_args: &str, code: &str) -> Value {
    let code = code.trim();
    if matches!(name, "shell" | "bash") {
        return json!({"command": if code.is_empty() { header_args } else { code }});
    }
    if matches!(name, "patch" | "morph") {
        let mut args = serde_json::Map::new();
        if let Some(path) = header_args.split_whitespace().next() {
            args.insert("path".into(), Value::String(path.to_owned()));
        }
        args.insert("patch".into(), Value::String(code.to_owned()));
        return Value::Object(args);
    }

    let mut args = serde_json::Map::new();
    if let Some(path) = header_args.split_whitespace().next() {
        args.insert("path".into(), Value::String(path.to_owned()));
    }
    if !code.is_empty() {
        args.insert("code".into(), Value::String(code.to_owned()));
    }
    Value::Object(args)
}

fn tool_output_text(content: &str) -> String {
    let mut outputs = Vec::new();
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let mut line_index = 0;
    while line_index < lines.len() {
        let Some(header) = lines[line_index]
            .trim_start()
            .trim_end_matches(['\r', '\n'])
            .strip_prefix("```")
        else {
            line_index += 1;
            continue;
        };
        let _ = header;
        let Some(close_index) = (line_index + 1..lines.len())
            .find(|&index| lines[index].trim().trim_end_matches(['\r', '\n']).trim() == "```")
        else {
            break;
        };
        let start = lines[..=line_index]
            .iter()
            .map(|line| line.len())
            .sum::<usize>();
        let end = lines[..close_index]
            .iter()
            .map(|line| line.len())
            .sum::<usize>();
        let body = content[start..end].trim();
        if !body.is_empty() {
            outputs.push(body.to_owned());
        }
        line_index = close_index + 1;
    }
    if !outputs.is_empty() {
        return outputs.join("\n");
    }

    let content = content.trim();
    if content.starts_with("Ran command:") {
        return content
            .split_once('\n')
            .map(|(_, output)| output.trim())
            .unwrap_or_default()
            .to_owned();
    }
    content.to_owned()
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

fn record_time(
    ts: Option<chrono::DateTime<chrono::Utc>>,
    started: &mut Option<chrono::DateTime<chrono::Utc>>,
    ended: &mut Option<chrono::DateTime<chrono::Utc>>,
) {
    if let Some(ts) = ts {
        started.get_or_insert(ts);
        *ended = Some(ts);
    }
}

/// gptme uses Python's `datetime.now().isoformat()`, which produces naive
/// timestamps with no UTC offset (e.g. `2026-06-08T10:00:01.000000`).
/// Try RFC 3339 first; fall back to naive-datetime parsing and assume UTC.
fn parse_gptme_ts(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    parse_ts(s).or_else(|| s.parse::<NaiveDateTime>().ok().map(|n| n.and_utc()))
}
