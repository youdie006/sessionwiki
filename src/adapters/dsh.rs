//! DeepSeek Harness native logs and dsh-acp bridge session records.
//!
//! The CLI stores generation logs beneath `$DSH_HOME/sessions`; dsh-acp
//! stores a flat `sessions/<id>.json` beneath `$DSH_ACP_DATA_DIR`. A private
//! profile can hold either format, so an explicit home reads both.

mod native;

use super::{
    bounded_redacted_output, dedup_paths, ok_or_flag, parse_ts, redacted_truncate, root_scope,
    title_from_messages, Adapter, Discovered, Store,
};
use crate::model::{Message, Role, Session, ToolCall, ToolResult};
use anyhow::{Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct Dsh {
    home: Option<PathBuf>,
    acp: bool,
}

impl Dsh {
    /// Read either native or bridge sessions beneath one profile home.
    pub fn in_home(home: impl Into<PathBuf>) -> Self {
        Self {
            home: Some(home.into()),
            acp: false,
        }
    }

    /// The bridge's standalone store, separate from the native CLI home.
    pub fn acp_default() -> Self {
        Self {
            home: None,
            acp: true,
        }
    }

    fn home(&self) -> Option<PathBuf> {
        self.home.clone().or_else(|| {
            let variable = if self.acp {
                "DSH_ACP_DATA_DIR"
            } else {
                "DSH_HOME"
            };
            std::env::var_os(variable)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .or_else(|| {
                    if self.acp {
                        std::env::current_dir().ok().map(|cwd| cwd.join(".dsh-acp"))
                    } else {
                        dirs::home_dir().map(|home| home.join(".dsh"))
                    }
                })
        })
    }
}

impl Adapter for Dsh {
    fn name(&self) -> &'static str {
        "dsh"
    }

    fn root(&self) -> Option<PathBuf> {
        let root = self.home()?.join("sessions");
        Some(crate::util::canonicalize_as_recorded(&root).unwrap_or(root))
    }

    fn discover(&self) -> Discovered {
        Vec::new().into()
    }

    fn store(&self) -> Option<Store> {
        let Some(root) = self.root() else {
            return Some(Store {
                keys: vec![],
                files: vec![],
                had_error: false,
            });
        };
        let mut store = native::sources(&root);
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) => {
                store.had_error |= error.kind() != std::io::ErrorKind::NotFound;
                return Some(store);
            }
        };
        for entry in entries {
            let Some(entry) = ok_or_flag(entry, &mut store.had_error) else {
                continue;
            };
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Some(metadata) = ok_or_flag(std::fs::symlink_metadata(&path), &mut store.had_error)
            else {
                continue;
            };
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                continue;
            }
            let Some(modified) = ok_or_flag(metadata.modified(), &mut store.had_error) else {
                continue;
            };
            let token = modified
                .duration_since(std::time::UNIX_EPOCH)
                .map(|time| time.as_millis().min(i64::MAX as u128) as i64)
                .unwrap_or_default();
            store
                .keys
                .push((path.to_string_lossy().into_owned(), token));
            store.files.push(path);
        }
        Some(store)
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        if path.is_dir() {
            native::parse(&path.to_string_lossy())
        } else {
            parse_acp(path)
        }
    }

    fn parse_key(&self, key: &str) -> Result<Session> {
        self.parse(Path::new(key))
    }

    fn modified_at(&self, key: &str) -> Option<chrono::DateTime<chrono::Utc>> {
        let path = Path::new(key);
        if path.is_dir() {
            native::modified_at(key)
        } else {
            std::fs::metadata(path)
                .ok()?
                .modified()
                .ok()
                .map(Into::into)
        }
    }

    fn reconcile_scope(&self) -> Option<String> {
        root_scope(self.root().as_deref())
    }
}

fn content_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_acp(path: &Path) -> Result<Session> {
    let body = crate::util::read_to_string_capped(path)?;
    let record: Value = serde_json::from_str(&body)
        .with_context(|| format!("parse DSH session {}", path.display()))?;
    anyhow::ensure!(
        record.is_object() && record.get("messages").is_some_and(Value::is_array),
        "DSH session has no messages array"
    );
    let started = record
        .get("created_at")
        .and_then(Value::as_str)
        .and_then(parse_ts);
    let ended = record
        .get("updated_at")
        .and_then(Value::as_str)
        .and_then(parse_ts);
    let mut messages = Vec::new();
    let mut touched = Vec::new();
    for message in record["messages"].as_array().into_iter().flatten() {
        let role = match message.get("role").and_then(Value::as_str) {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            Some("tool") => Role::Tool,
            _ => continue,
        };
        let text = content_text(&message["content"]);
        if role == Role::Tool {
            let call_id = message.get("tool_call_id").and_then(Value::as_str);
            let outcome = call_id.and_then(|id| record.get("tool_outcomes")?.get(id));
            messages.push(Message::tool_result(
                None,
                ToolResult {
                    call_id: call_id.map(str::to_owned),
                    text: bounded_redacted_output(&text),
                    is_error: outcome
                        .and_then(|o| o.get("status"))
                        .and_then(Value::as_str)
                        == Some("failed"),
                },
            ));
            continue;
        }
        if !text.trim().is_empty() {
            messages.push(Message {
                role,
                text: crate::redact::redact(&text).into_owned(),
                ts: None,
                tool: None,
            });
        }
        if role != Role::Assistant {
            continue;
        }
        for call in message
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(name) = call.pointer("/function/name").and_then(Value::as_str) else {
                continue;
            };
            let raw = call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let redacted = crate::redact::redact(raw);
            let args: Value = serde_json::from_str(&redacted)
                .unwrap_or_else(|_| Value::String(redacted.into_owned()));
            if matches!(name, "write_file" | "edit_file") {
                if let Some(path) = args.get("path").and_then(Value::as_str) {
                    touched.push(path.to_owned());
                }
            }
            messages.push(Message::tool_call(
                None,
                ToolCall {
                    id: call.get("id").and_then(Value::as_str).map(str::to_owned),
                    name: name.to_owned(),
                    args,
                },
            ));
        }
    }
    let title = record
        .get("title")
        .and_then(Value::as_str)
        .filter(|title| !title.trim().is_empty())
        .map(|title| redacted_truncate(title, 80))
        .unwrap_or_else(|| title_from_messages(&messages));
    Ok(Session {
        id: crate::util::short_id(&path.to_string_lossy()),
        tool: "dsh",
        path: path.to_path_buf(),
        project: record
            .get("cwd")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        started,
        ended,
        title,
        subagent: record
            .get("parent_session_id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty()),
        messages,
        touched: dedup_paths(touched),
        edits: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ToolEvent;
    use serde_json::json;

    #[test]
    fn bridge_messages_tools_metadata_and_redaction() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("sessions");
        std::fs::create_dir(&root).unwrap();
        let path = root.join("sample.json");
        std::fs::write(&path, json!({
            "id":"sample", "cwd":"/work/project", "title":"Repair search", "parent_session_id":"parent",
            "created_at":"2026-11-10T12:00:00Z", "updated_at":"2026-11-10T12:01:00Z",
            "messages":[
                {"role":"system","content":"private instructions"},
                {"role":"user","content":[{"type":"text","text":"repair search"},{"type":"image_url","image_url":{"url":"data:ignored"}}]},
                {"role":"assistant","content":"I will edit it", "reasoning_content":"private reasoning", "tool_calls":[
                    {"id":"call1","type":"function","function":{"name":"edit_file","arguments":"{\"path\":\"src/search.rs\",\"new_text\":\"fixed\"}"}}
                ]},
                {"role":"tool","tool_call_id":"call1","content":"failed sk-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ"},
                {"role":"assistant","content":"retry"}
            ], "tool_outcomes":{"call1":{"status":"failed"}},
            "pending_steering":[{"content":"not submitted yet"}]
        }).to_string()).unwrap();
        let adapter = Dsh::in_home(home.path());
        let store = adapter.store().unwrap();
        assert!(!store.had_error);
        assert_eq!(store.keys.len(), 1);
        assert_eq!(store.files, vec![path.clone()]);
        assert!(store.keys[0]
            .0
            .starts_with(&adapter.reconcile_scope().unwrap()));
        let session = super::super::parse_session_key(&adapter, &store.keys[0].0).unwrap();
        assert_eq!(session.project, "/work/project");
        assert_eq!(session.title, "Repair search");
        assert!(session.started.is_some() && session.ended > session.started);
        assert!(session.subagent);
        assert_eq!(session.touched, ["src/search.rs"]);
        assert_eq!(
            session
                .messages
                .iter()
                .filter(|m| m.role == Role::User)
                .count(),
            1
        );
        let tool = session
            .messages
            .iter()
            .find_map(|m| match &m.tool {
                Some(ToolEvent::Summary(s)) => Some(s),
                _ => None,
            })
            .unwrap();
        assert_eq!(tool.name, "edit_file");
        assert_eq!(tool.is_error, Some(true));
        let serialized = serde_json::to_string(&session).unwrap();
        for excluded in [
            "private instructions",
            "private reasoning",
            "not submitted yet",
            "sk-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ",
            "data:ignored",
        ] {
            assert!(!serialized.contains(excluded), "included {excluded}");
        }
    }

    #[test]
    fn invalid_bridge_record_does_not_become_empty_session() {
        let home = tempfile::tempdir().unwrap();
        let file = home.path().join("broken.json");
        for data in ["{", "{}", "[]", "{\"messages\":null}"] {
            std::fs::write(&file, data).unwrap();
            assert!(Dsh::in_home(home.path()).parse(&file).is_err());
        }
    }

    #[test]
    fn missing_store_is_normal_but_non_directory_is_a_listing_error() {
        let home = tempfile::tempdir().unwrap();
        let adapter = Dsh::in_home(home.path());
        assert!(!adapter.store().unwrap().had_error);
        std::fs::write(home.path().join("sessions"), "not a directory").unwrap();
        assert!(adapter.store().unwrap().had_error);
    }

    #[cfg(unix)]
    #[test]
    fn discovery_does_not_follow_bridge_symlinks() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("sessions");
        std::fs::create_dir(&root).unwrap();
        let external = home.path().join("external.json");
        std::fs::write(&external, "{\"messages\":[]}").unwrap();
        std::os::unix::fs::symlink(&external, root.join("link.json")).unwrap();
        let store = Dsh::in_home(home.path()).store().unwrap();
        // Native discovery conservatively protects old rows when a former
        // project/session directory might have been replaced by a symlink.
        assert!(store.had_error);
        assert!(store.keys.is_empty());
    }
}
