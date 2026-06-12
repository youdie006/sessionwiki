use super::{parse_ts, title_from_messages, Adapter};
use crate::model::{Message, Role, Session};
use crate::util::{short_id, truncate};
use anyhow::{Context, Result};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Claude Code stores one JSONL file per session under
/// `~/.claude/projects/<sanitized-cwd>/<session-uuid>.jsonl`.
/// Each line is an event: user/assistant messages, tool results,
/// summaries, and harness bookkeeping.
pub struct ClaudeCode;

impl Adapter for ClaudeCode {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn root(&self) -> Option<PathBuf> {
        Some(dirs::home_dir()?.join(".claude").join("projects"))
    }

    fn discover(&self) -> Vec<PathBuf> {
        // Main sessions live at <project>/<uuid>.jsonl; subagent transcripts
        // at <project>/<uuid>/subagents/agent-*.jsonl. Both are sessions.
        let Some(root) = self.root() else { return vec![] };
        WalkDir::new(root)
            .max_depth(4)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
            .map(|e| e.into_path())
            .collect()
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let reader = BufReader::new(file);

        let mut messages: Vec<Message> = Vec::new();
        let mut cwd: Option<String> = None;
        let mut summary: Option<String> = None;
        let mut started = None;
        let mut ended = None;

        for line in reader.lines() {
            let Ok(line) = line else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };

            if cwd.is_none() {
                if let Some(c) = v.get("cwd").and_then(Value::as_str) {
                    cwd = Some(c.to_string());
                }
            }
            if let Some(ts) = v.get("timestamp").and_then(Value::as_str).and_then(parse_ts) {
                if started.is_none() {
                    started = Some(ts);
                }
                ended = Some(ts);
            }

            match v.get("type").and_then(Value::as_str) {
                Some("summary") => {
                    if summary.is_none() {
                        summary = v.get("summary").and_then(Value::as_str).map(String::from);
                    }
                }
                Some("user") => {
                    // Skip harness meta lines; keep real prompts and tool results.
                    if v.get("isMeta").and_then(Value::as_bool) == Some(true) {
                        continue;
                    }
                    let ts = v.get("timestamp").and_then(Value::as_str).and_then(parse_ts);
                    let Some(content) = v.pointer("/message/content") else { continue };
                    match content {
                        Value::String(s) => push(&mut messages, Role::User, s, ts),
                        Value::Array(blocks) => {
                            for b in blocks {
                                match b.get("type").and_then(Value::as_str) {
                                    Some("text") => {
                                        if let Some(t) = b.get("text").and_then(Value::as_str) {
                                            push(&mut messages, Role::User, t, ts);
                                        }
                                    }
                                    Some("tool_result") => {
                                        let t = block_text(b.get("content"));
                                        if !t.is_empty() {
                                            push(&mut messages, Role::Tool, &truncate(&t, 500), ts);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Some("assistant") => {
                    let ts = v.get("timestamp").and_then(Value::as_str).and_then(parse_ts);
                    let Some(Value::Array(blocks)) = v.pointer("/message/content") else {
                        continue;
                    };
                    for b in blocks {
                        match b.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                if let Some(t) = b.get("text").and_then(Value::as_str) {
                                    push(&mut messages, Role::Assistant, t, ts);
                                }
                            }
                            Some("tool_use") => {
                                let name = b.get("name").and_then(Value::as_str).unwrap_or("?");
                                let input = b.get("input").map(|i| i.to_string()).unwrap_or_default();
                                let text = format!("{name} {}", truncate(&input, 300));
                                push(&mut messages, Role::Tool, &text, ts);
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        let project = cwd.unwrap_or_else(|| {
            // Fall back to the sanitized directory name.
            path.parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        let title = summary.map(|s| truncate(&s, 80)).unwrap_or_else(|| title_from_messages(&messages));
        let subagent = path.to_string_lossy().contains("/subagents/");

        Ok(Session {
            id: short_id(&path.to_string_lossy()),
            tool: self.name(),
            path: path.to_path_buf(),
            project,
            started,
            ended,
            title,
            subagent,
            messages,
        })
    }
}

fn push(messages: &mut Vec<Message>, role: Role, text: &str, ts: Option<chrono::DateTime<chrono::Utc>>) {
    let text = text.trim();
    if !text.is_empty() {
        messages.push(Message { role, text: text.to_string(), ts });
    }
}

/// tool_result content is either a string or an array of text blocks.
fn block_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}
