use super::{dedup_paths, title_from_messages, Adapter};
use crate::model::{Message, Role, Session};
use crate::util::{short_id, truncate};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// OpenCode (sst/opencode) stores one session as several JSON files under
/// `$XDG_DATA_HOME/opencode/storage` (default `~/.local/share/opencode/storage`,
/// the same on macOS - it uses xdg-basedir, not the platform data dir):
///   session/<projectID>/<sessionID>.json   - session metadata
///   message/<sessionID>/<messageID>.json    - one per message (role, time)
///   part/<messageID>/<partID>.json          - message content pieces
/// IDs are time-ordered, so a lexicographic filename sort is conversation order.
pub struct OpenCode;

impl Adapter for OpenCode {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn root(&self) -> Option<PathBuf> {
        // xdg-basedir semantics on every platform: $XDG_DATA_HOME or
        // ~/.local/share (NOT the OS data dir, which differs on macOS).
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("share")))?;
        Some(base.join("opencode").join("storage"))
    }

    fn discover(&self) -> Vec<PathBuf> {
        // Session files are session/<projectID>/<sessionID>.json. One per
        // session; parse() reads its message/part files relative to the store.
        let Some(root) = self.root() else {
            return vec![];
        };
        WalkDir::new(root.join("session"))
            .min_depth(2)
            .max_depth(2)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .map(|e| e.into_path())
            .collect()
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("open {}", path.display()))?;
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
            .map(|t| truncate(t, 80));

        // storage/session/<proj>/<id>.json -> storage root is three up.
        let store = path
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent());

        let mut messages: Vec<Message> = Vec::new();
        let mut touched: Vec<String> = Vec::new();

        if let Some(store) = store {
            for msg_path in sorted_json(&store.join("message").join(&session_id)) {
                let Ok(mraw) = std::fs::read_to_string(&msg_path) else {
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
                    let Ok(praw) = std::fs::read_to_string(&part_path) else {
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
                            let tool = p.get("tool").and_then(Value::as_str).unwrap_or("?");
                            if let Some(fp) = edited_path(tool, p.pointer("/state/input")) {
                                touched.push(fp);
                            }
                            let input = p
                                .pointer("/state/input")
                                .map(|i| i.to_string())
                                .unwrap_or_default();
                            push(
                                &mut messages,
                                Role::Tool,
                                &format!("{tool} {}", truncate(&input, 300)),
                                ts,
                            );
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
                        // reasoning / step-start / step-finish / snapshot / agent
                        // are turn bookkeeping or model noise - skip.
                        _ => {}
                    }
                }
            }
        }

        let title = session_title.unwrap_or_else(|| title_from_messages(&messages));

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
            touched: dedup_paths(touched),
        })
    }
}

/// OpenCode's edit/write tools name the file in `state.input.filePath`. read is
/// not a write, and other tools (bash, ...) do not establish authorship.
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

/// All `*.json` files in a dir, sorted by filename. OpenCode ids are
/// time-ordered, so this is chronological order. Missing dir -> empty.
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

/// OpenCode timestamps are epoch milliseconds (numbers), not RFC3339 strings.
fn epoch_ms(v: &Value) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis(v.as_i64()?)
}

fn push(messages: &mut Vec<Message>, role: Role, text: &str, ts: Option<DateTime<Utc>>) {
    let text = text.trim();
    if !text.is_empty() {
        messages.push(Message {
            role,
            text: text.to_string(),
            ts,
        });
    }
}
