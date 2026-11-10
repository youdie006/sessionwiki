//! Grok Build stores one conversation per `sessions/<encoded-cwd>/<uuid>`.
use super::{
    bounded_redacted_output, dedup_paths, redacted_truncate, root_scope, title_from_messages,
    Adapter, Discovered, Store,
};
use crate::model::{Message, Role, Session, ToolCall, ToolResult};
use crate::util::{read_to_string_capped, short_id};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub struct GrokBuild {
    home: Option<PathBuf>,
    configured_paths: bool,
}

impl Default for GrokBuild {
    fn default() -> Self {
        Self {
            home: std::env::var_os("GROK_HOME")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .or_else(|| dirs::home_dir().map(|p| p.join(".grok"))),
            configured_paths: false,
        }
    }
}

impl GrokBuild {
    pub fn in_home(home: impl Into<PathBuf>) -> Self {
        Self {
            home: Some(home.into()),
            configured_paths: false,
        }
    }

    /// Retain the configured home spelling in persistent keys and scopes.
    /// Embedders replacing readers that kept symlinked paths can preserve
    /// those identities; the default retains existing canonical SW keys.
    /// Parsing an existing key always preserves the supplied spelling.
    pub fn keep_configured_paths(mut self) -> Self {
        self.configured_paths = true;
        self
    }
}

impl Adapter for GrokBuild {
    fn name(&self) -> &'static str {
        "grok-build"
    }

    fn root(&self) -> Option<PathBuf> {
        let root = absolute(&self.home.as_ref()?.join("sessions"));
        Some(if self.configured_paths {
            root
        } else {
            root.canonicalize().unwrap_or(root)
        })
    }

    fn reconcile_scope(&self) -> Option<String> {
        root_scope(self.root().as_deref())
    }

    fn modified_at(&self, key: &str) -> Option<DateTime<Utc>> {
        let path = Path::new(key);
        let mut files = [
            "chat_history.jsonl",
            "history.jsonl",
            "summary.json",
            "events.jsonl",
        ]
        .into_iter()
        .map(|name| path.join(name))
        .collect::<Vec<_>>();
        if let Some(parent) = path.parent() {
            files.push(parent.join(".cwd"));
        }
        files
            .iter()
            .filter_map(|file| fs::metadata(file).ok()?.modified().ok())
            .max()
            .map(DateTime::<Utc>::from)
    }

    fn discover(&self) -> Discovered {
        let store = self.store().expect("Grok is a shared store");
        Discovered {
            files: store
                .keys
                .into_iter()
                .map(|(key, _)| PathBuf::from(key))
                .collect(),
            had_error: store.had_error,
        }
    }

    fn store(&self) -> Option<Store> {
        let mut store = Store {
            keys: Vec::new(),
            files: Vec::new(),
            had_error: false,
        };
        let Some(root) = self.root() else {
            return Some(store);
        };
        for cwd_dir in directories(&root, &mut store.had_error) {
            for path in directories(&cwd_dir, &mut store.had_error) {
                // Native Grok uses chat_history.jsonl. Accept history.jsonl
                // from older exporters too; shared keys always stay directories.
                let mut files = Vec::new();
                for name in [
                    "chat_history.jsonl",
                    "history.jsonl",
                    "summary.json",
                    "events.jsonl",
                ] {
                    track_file(&path.join(name), &mut files, &mut store.had_error);
                }
                if !files.iter().any(|p| {
                    matches!(
                        p.file_name().and_then(|s| s.to_str()),
                        Some("chat_history.jsonl" | "history.jsonl")
                    )
                }) {
                    continue;
                }
                track_file(&cwd_dir.join(".cwd"), &mut files, &mut store.had_error);
                let key = absolute(&path).to_string_lossy().into_owned();
                store.keys.push((key, change_token(&files)));
                store.files.extend(files);
            }
        }
        store.keys.sort_by(|a, b| a.0.cmp(&b.0));
        store.files.sort();
        store.files.dedup();
        Some(store)
    }

    fn parse_key(&self, key: &str) -> Result<Session> {
        self.parse(Path::new(key))
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        let path = absolute(path);
        // A bad summary must not hide a recoverable history or encoded cwd.
        let summary = read_to_string_capped(&path.join("summary.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .unwrap_or(Value::Null);
        let cwd = summary
            .pointer("/info/cwd")
            .and_then(Value::as_str)
            .filter(|p| Path::new(p).is_absolute())
            .map(str::to_owned)
            .or_else(|| path.parent().and_then(decode_cwd));
        let history = if path.join("chat_history.jsonl").is_file() {
            path.join("chat_history.jsonl")
        } else {
            path.join("history.jsonl")
        };
        let (lines, windowed) = crate::util::session_lines_keeping(&history, |line| {
            serde_json::from_str::<Value>(line).is_ok_and(|r| {
                matches!(r["type"].as_str(), Some("user" | "assistant" | "reasoning"))
            })
        })
        .with_context(|| format!("read Grok Build history {}", history.display()))?;
        let mut started = summary
            .pointer("/info/created_at")
            .and_then(timestamp)
            .or_else(|| timestamp(&summary["created_at"]));
        let mut ended = summary
            .pointer("/info/updated_at")
            .and_then(timestamp)
            .or_else(|| timestamp(&summary["updated_at"]));
        let mut messages = Vec::new();
        let mut edit_calls = HashMap::new();
        let mut completed = HashSet::new();
        for line in lines {
            let Ok(record) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let ts = ["timestamp", "time", "created_at"]
                .into_iter()
                .find_map(|k| timestamp(&record[k]));
            if let Some(t) = ts {
                started = Some(started.map_or(t, |old| old.min(t)));
                ended = Some(ended.map_or(t, |old| old.max(t)));
            }
            match record["type"].as_str() {
                // Human is omitted on write; interjections are also actual
                // user input. Exclude runtime reminders and compaction text.
                Some("user")
                    if matches!(
                        record["synthetic_reason"].as_str(),
                        None | Some("human" | "interjection")
                    ) =>
                {
                    push_text(
                        &mut messages,
                        Role::User,
                        &content_text(&record["content"]),
                        ts,
                    );
                }
                Some("assistant") => {
                    push_text(
                        &mut messages,
                        Role::Assistant,
                        &content_text(&record["content"]),
                        ts,
                    );
                    for call in record["tool_calls"].as_array().into_iter().flatten() {
                        let id = call["id"].as_str().map(str::to_owned);
                        let name = call["name"].as_str().unwrap_or("?");
                        let mut args = match &call["arguments"] {
                            Value::String(text) => serde_json::from_str(text)
                                .unwrap_or_else(|_| Value::String(text.clone())),
                            value => value.clone(),
                        };
                        if name == "search_replace" {
                            if let (Some(id), Some(path)) = (&id, args["file_path"].as_str()) {
                                edit_calls.insert(id.clone(), path.to_owned());
                            }
                        }
                        redact_json(&mut args);
                        messages.push(Message::tool_call(
                            ts,
                            ToolCall {
                                id,
                                name: crate::redact::redact(name).into_owned(),
                                args,
                            },
                        ));
                    }
                }
                Some("reasoning") => {
                    let text = record["summary"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|p| p["type"].as_str() == Some("summary_text"))
                        .filter_map(|p| p["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    push_text(&mut messages, Role::Assistant, &text, ts);
                }
                Some("tool_result") => {
                    let call_id = record["tool_call_id"].as_str().map(str::to_owned);
                    // Native results carry no success flag. As in mj's reader,
                    // completion is evidence that search_replace actually ran.
                    let is_error = record["is_error"].as_bool().unwrap_or(false);
                    if !is_error {
                        if let Some(id) = &call_id {
                            completed.insert(id.clone());
                        }
                    }
                    messages.push(Message::tool_result(
                        ts,
                        ToolResult {
                            call_id,
                            is_error,
                            text: bounded_redacted_output(&content_text(&record["content"])),
                        },
                    ));
                }
                _ => {}
            }
        }
        let title = ["generated_title", "session_summary"]
            .into_iter()
            .filter_map(|key| summary[key].as_str())
            .find(|t| !t.trim().is_empty())
            .map(|t| redacted_truncate(t, 80))
            .unwrap_or_else(|| title_from_messages(&messages));
        // ConversationItem lacks timestamps in current Grok releases. Use the
        // native history's mtime when summary/records supply none.
        let modified = fs::metadata(&history)
            .ok()
            .and_then(|m| m.modified().ok())
            .map(DateTime::<Utc>::from);
        let touched = if windowed {
            // The result proving an edit ran may fall outside the text window.
            history_edits(&history)?
        } else {
            edit_calls
                .into_iter()
                .filter(|(id, _)| completed.contains(id))
                .map(|(_, path)| path)
                .collect()
        };
        Ok(Session {
            id: short_id(&path.to_string_lossy()),
            tool: self.name(),
            path,
            project: cwd.unwrap_or_default(),
            started: started.or(modified),
            ended: ended.or(modified),
            title: if windowed {
                format!("[windowed] {title}")
            } else {
                title
            },
            subagent: false,
            messages,
            touched: dedup_paths(touched),
            edits: Vec::new(),
        })
    }
}

fn history_edits(path: &Path) -> Result<Vec<String>> {
    let mut calls = HashMap::new();
    let mut completed = HashSet::new();
    crate::util::for_each_line(path, |line| {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            return;
        };
        match record["type"].as_str() {
            Some("assistant") => {
                for call in record["tool_calls"].as_array().into_iter().flatten() {
                    if call["name"].as_str() != Some("search_replace") {
                        continue;
                    }
                    let args = match &call["arguments"] {
                        Value::String(text) => serde_json::from_str(text).unwrap_or(Value::Null),
                        value => value.clone(),
                    };
                    if let (Some(id), Some(path)) =
                        (call["id"].as_str(), args["file_path"].as_str())
                    {
                        calls.insert(id.to_owned(), path.to_owned());
                    }
                }
            }
            Some("tool_result") if record["is_error"].as_bool() != Some(true) => {
                if let Some(id) = record["tool_call_id"].as_str() {
                    completed.insert(id.to_owned());
                }
            }
            _ => {}
        }
    })?;
    Ok(calls
        .into_iter()
        .filter(|(id, _)| completed.contains(id))
        .map(|(_, path)| path)
        .collect())
}

fn absolute(path: &Path) -> PathBuf {
    // A configured profile's spelling is part of its persistent session key.
    // Resolving an alias here would duplicate rows written by native readers
    // that used read_dir under that profile, and escape their old scope.
    if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    }
}

fn directories(path: &Path, had_error: &mut bool) -> Vec<PathBuf> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(_) => {
            *had_error = true;
            return Vec::new();
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        match entry.and_then(|e| Ok((e.path(), e.file_type()?))) {
            Ok((path, kind)) if kind.is_dir() => paths.push(path),
            Ok(_) => {}
            Err(_) => *had_error = true,
        }
    }
    paths.sort();
    paths
}

fn track_file(path: &Path, files: &mut Vec<PathBuf>, had_error: &mut bool) {
    match fs::metadata(path) {
        Ok(m) if m.is_file() => files.push(path.to_owned()),
        Ok(_) => *had_error = true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => *had_error = true,
    }
}

/// Every backing file contributes, even when another file has a newer mtime.
/// `modified_at` supplies the backing-file date independently of this opaque
/// fingerprint, for cutoff filtering and store reports.
fn change_token(files: &[PathBuf]) -> i64 {
    let mut hash = 0xcbf29ce484222325u64;
    for path in files {
        for byte in path.to_string_lossy().bytes() {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
        if let Ok(meta) = fs::metadata(path) {
            let time = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            for byte in time
                .to_le_bytes()
                .into_iter()
                .chain(meta.len().to_le_bytes())
            {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            }
        }
    }
    ((hash & i64::MAX as u64) | (1 << 62)) as i64
}

fn decode_cwd(directory: &Path) -> Option<String> {
    let name = directory.file_name()?.to_str()?;
    let decoded = percent_decode(name).filter(|p| Path::new(p).is_absolute());
    decoded.or_else(|| {
        read_to_string_capped(&directory.join(".cwd"))
            .ok()
            .map(|text| text.trim().to_owned())
            .filter(|p| Path::new(p).is_absolute())
    })
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            output.push(u8::from_str_radix(text.get(i + 1..i + 3)?, 16).ok()?);
            i += 3;
        } else {
            output.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(output).ok()
}

fn timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .as_i64()
        .and_then(DateTime::from_timestamp_millis)
        .or_else(|| value.as_str().and_then(super::parse_ts))
}

fn content_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p["type"].as_str() == Some("text"))
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn push_text(messages: &mut Vec<Message>, role: Role, text: &str, ts: Option<DateTime<Utc>>) {
    if !text.is_empty() {
        messages.push(Message {
            role,
            text: crate::redact::redact(text).into_owned(),
            ts,
            tool: None,
        });
    }
}

fn redact_json(value: &mut Value) {
    match value {
        Value::String(text) => *text = crate::redact::redact(text).into_owned(),
        Value::Array(values) => values.iter_mut().for_each(redact_json),
        Value::Object(values) => values.values_mut().for_each(redact_json),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ToolEvent, ToolPart};
    use serde_json::json;

    fn fixture(home: &Path, cwd: &str, id: &str, summary: Value, records: &[Value]) -> PathBuf {
        let path = home.join("sessions").join(cwd).join(id);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("summary.json"), summary.to_string()).unwrap();
        fs::write(
            path.join("chat_history.jsonl"),
            records
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        path.canonicalize().unwrap()
    }

    #[test]
    fn discovers_session_directories_and_accounts_for_real_backing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let path = fixture(
            tmp.path(),
            "%2Frepo",
            "uuid",
            json!({"session_summary":"A title","info":{"cwd":"/summary/repo"}}),
            &[],
        );
        fs::write(tmp.path().join("sessions/search.db"), "not a session").unwrap();
        fs::create_dir_all(tmp.path().join("sessions/%2Frepo/not-a-session")).unwrap();
        let adapter = GrokBuild::in_home(tmp.path());
        let store = adapter.store().unwrap();
        assert!(!store.had_error);
        assert_eq!(store.keys.len(), 1);
        assert_eq!(store.keys[0].0, path.to_string_lossy());
        assert!(store.files.iter().all(|p| p.is_file()));
        assert_eq!(store.files.len(), 2);
        let session = adapter.parse_key(&store.keys[0].0).unwrap();
        assert_eq!(session.path, path);
        assert_eq!(session.id, short_id(&store.keys[0].0));
        assert_eq!(session.tool, "grok-build");
        assert_eq!(session.project, "/summary/repo");
        assert_eq!(session.title, "A title");
        assert!(session.started.is_some()); // mtime fallback for untimed native items
        assert_eq!(adapter.discover().files, vec![path]);
    }

    #[test]
    fn native_generated_title_and_summary_timestamps_are_preserved() {
        let tmp = tempfile::tempdir().unwrap();
        let path = fixture(
            tmp.path(),
            "%2Frepo",
            "uuid",
            json!({"generated_title":"Renamed session","session_summary":"Old prompt","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z"}),
            &[],
        );
        let session = GrokBuild::in_home(tmp.path()).parse(&path).unwrap();
        assert_eq!(session.title, "Renamed session");
        assert_eq!(
            session.started,
            super::super::parse_ts("2026-01-01T00:00:00Z")
        );
        assert_eq!(
            session.ended,
            super::super::parse_ts("2026-01-02T00:00:00Z")
        );
    }

    #[test]
    fn encoded_and_hashed_cwds_survive_missing_or_malformed_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let encoded = fixture(
            tmp.path(),
            "%2Ftmp%2Fhello%20world%2F%E6%97%A5%E6%9C%AC",
            "one",
            json!({}),
            &[],
        );
        let hashed = fixture(tmp.path(), "repo-123456", "two", json!({}), &[]);
        fs::write(
            hashed.parent().unwrap().join(".cwd"),
            "/long/original/path\n",
        )
        .unwrap();
        fs::write(encoded.join("summary.json"), "{truncated").unwrap();
        fs::remove_file(hashed.join("summary.json")).unwrap();
        let adapter = GrokBuild::in_home(tmp.path());
        assert_eq!(
            adapter.parse(&encoded).unwrap().project,
            "/tmp/hello world/日本"
        );
        assert_eq!(
            adapter.parse(&hashed).unwrap().project,
            "/long/original/path"
        );
        assert_eq!(percent_decode("%F0%9F%98%80"), Some("😀".into()));
        for malformed in ["%", "%Q0", "%0", "%FF", "%日"] {
            assert!(percent_decode(malformed).is_none());
        }
    }

    #[test]
    fn projects_conversation_tools_reasoning_and_completed_edits_without_synthetic_input() {
        let tmp = tempfile::tempdir().unwrap();
        let secret = format!("sk-{}", "b".repeat(48));
        let records = vec![
            json!({"type":"system","content":"system prompt"}),
            json!({"type":"user","content":[{"type":"text","text":format!("fix {secret}")},{"type":"image","url":"private-image"}],"timestamp":"2024-01-01T00:00:00Z"}),
            json!({"type":"user","synthetic_reason":"compaction_meta","content":[{"type":"text","text":"injected"}]}),
            json!({"type":"reasoning","summary":[{"type":"summary_text","text":"Think"}]}),
            json!({"type":"assistant","content":"Answer","tool_calls":[{"id":"one","name":"search_replace","arguments":json!({"file_path":"lib.rs","new_string":secret}).to_string()},{"id":"two","name":"search_replace","arguments":"{\"file_path\":\"pending.rs\"}"}]}),
            json!({"type":"tool_result","tool_call_id":"one","content":secret}),
            json!({"type":"user","synthetic_reason":"interjection","content":[{"type":"text","text":"steering"}],"time":1704067201000i64}),
        ];
        let path = fixture(
            tmp.path(),
            "%2Frepo",
            "uuid",
            json!({"session_summary":secret}),
            &records,
        );
        let adapter = GrokBuild::in_home(tmp.path());
        let session = adapter.parse(&path).unwrap();
        assert_eq!(
            session
                .messages
                .iter()
                .filter(|m| m.role == Role::User)
                .count(),
            2
        );
        assert_eq!(session.messages[1].text, "Think");
        assert_eq!(session.messages[2].text, "Answer");
        assert!(
            matches!(&session.messages[3].tool, Some(ToolEvent::Part(ToolPart::Call(call))) if call.name == "search_replace" && call.args["file_path"] == "lib.rs")
        );
        assert!(
            matches!(&session.messages[5].tool, Some(ToolEvent::Part(ToolPart::Result(result))) if result.call_id.as_deref() == Some("one"))
        );
        assert_eq!(session.touched, vec!["lib.rs"]);
        assert_eq!(session.started.unwrap().timestamp_millis(), 1704067200000);
        assert_eq!(session.ended.unwrap().timestamp_millis(), 1704067201000);
        let serialized = serde_json::to_string(&session).unwrap();
        assert!(!serialized.contains(&secret));
        assert!(!serialized.contains("private-image"));
        let folded = super::super::parse_session_key(&adapter, &path.to_string_lossy()).unwrap();
        assert!(folded
            .messages
            .iter()
            .any(|m| matches!(m.tool, Some(ToolEvent::Summary(_)))));
    }

    #[test]
    fn malformed_lines_and_arguments_do_not_erase_readable_history() {
        let tmp = tempfile::tempdir().unwrap();
        let path = fixture(
            tmp.path(),
            "%2Frepo",
            "uuid",
            json!({}),
            &[
                json!({"type":"user","content":"hello"}),
                json!({"type":"assistant","tool_calls":[{"id":"bad","name":"unknown","arguments":"{bad"}]}),
                json!({"type":"tool_result","tool_call_id":"bad","content":"failure","is_error":true}),
            ],
        );
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(path.join("chat_history.jsonl"))
            .unwrap()
            .write_all(b"\n[]\nnot-json\n{truncated")
            .unwrap();
        let session = GrokBuild::in_home(tmp.path()).parse(&path).unwrap();
        assert_eq!(session.messages.len(), 3);
        assert_eq!(session.title, "hello");
        assert!(
            matches!(&session.messages[1].tool, Some(ToolEvent::Part(ToolPart::Call(call))) if call.args == "{bad")
        );
        assert!(session.touched.is_empty());
        fs::rename(path.join("chat_history.jsonl"), path.join("history.jsonl")).unwrap();
        assert_eq!(
            GrokBuild::in_home(tmp.path()).store().unwrap().keys.len(),
            1
        );
        assert_eq!(
            GrokBuild::in_home(tmp.path())
                .parse(&path)
                .unwrap()
                .messages
                .len(),
            3
        );
    }

    #[test]
    fn metadata_only_change_invalidates_even_when_history_has_newer_mtime() {
        let tmp = tempfile::tempdir().unwrap();
        let path = fixture(
            tmp.path(),
            "repo-hash",
            "uuid",
            json!({"session_summary":"A"}),
            &[],
        );
        fs::write(path.parent().unwrap().join(".cwd"), "/repo").unwrap();
        let future = std::time::UNIX_EPOCH + std::time::Duration::from_secs(2000000000);
        fs::File::open(path.join("chat_history.jsonl"))
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(future))
            .unwrap();
        let adapter = GrokBuild::in_home(tmp.path());
        let first = adapter.store().unwrap().keys[0].1;
        fs::File::open(path.join("summary.json"))
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
            .unwrap();
        let second = adapter.store().unwrap().keys[0].1;
        assert_ne!(first, second);
        fs::write(path.parent().unwrap().join(".cwd"), "/new-repo").unwrap();
        let third = adapter.store().unwrap().keys[0].1;
        assert_ne!(second, third);
        let dir_mtime = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(path.join("chat_history.jsonl"), "changed").unwrap();
        assert_ne!(third, adapter.store().unwrap().keys[0].1);
        assert_eq!(dir_mtime, fs::metadata(&path).unwrap().modified().unwrap());
        assert!(third > 2000000000);
    }

    #[test]
    fn modified_at_uses_backing_file_metadata_without_parsing_history_or_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let path = fixture(tmp.path(), "repo-hash", "uuid", json!({}), &[]);
        fs::write(path.join("summary.json"), "invalid JSON").unwrap();
        fs::write(path.join("chat_history.jsonl"), "invalid JSONL").unwrap();
        let adapter = GrokBuild::in_home(tmp.path());
        let future = std::time::UNIX_EPOCH + std::time::Duration::from_secs(2000000000);
        fs::File::open(path.join("chat_history.jsonl"))
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(future))
            .unwrap();
        assert_eq!(
            adapter.modified_at(&path.to_string_lossy()),
            Some(DateTime::<Utc>::from(future))
        );
        let later = future + std::time::Duration::from_secs(10);
        fs::write(path.parent().unwrap().join(".cwd"), "/repo").unwrap();
        fs::File::open(path.parent().unwrap().join(".cwd"))
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(later))
            .unwrap();
        assert_eq!(
            adapter.modified_at(&path.to_string_lossy()),
            Some(DateTime::<Utc>::from(later))
        );
    }

    #[test]
    fn missing_store_is_normal_but_unreadable_listing_is_partial_and_scope_is_per_home() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!GrokBuild::in_home(tmp.path()).store().unwrap().had_error);
        fs::write(tmp.path().join("sessions"), "not a directory").unwrap();
        assert!(GrokBuild::in_home(tmp.path()).store().unwrap().had_error);
        fs::remove_file(tmp.path().join("sessions")).unwrap();
        fs::create_dir(tmp.path().join("sessions")).unwrap();
        let adapter = GrokBuild::in_home(tmp.path());
        let scope = adapter.reconcile_scope().unwrap();
        assert!(tmp
            .path()
            .join("sessions/repo/uuid")
            .to_string_lossy()
            .starts_with(&scope));
        assert!(!tmp
            .path()
            .join("sessions-other/repo/uuid")
            .to_string_lossy()
            .starts_with(&scope));
        #[cfg(unix)]
        {
            let alias_root = tempfile::tempdir().unwrap();
            let alias = alias_root.path().join("alias");
            std::os::unix::fs::symlink(tmp.path(), &alias).unwrap();
            assert_eq!(
                GrokBuild::in_home(&alias).reconcile_scope(),
                Some(scope.clone())
            );
            let alias_adapter = GrokBuild::in_home(&alias).keep_configured_paths();
            let alias_scope = root_scope(Some(&alias.join("sessions"))).unwrap();
            assert_ne!(alias_scope, scope);
            assert_eq!(alias_adapter.reconcile_scope(), Some(alias_scope.clone()));
            assert!(alias_adapter
                .store()
                .unwrap()
                .keys
                .iter()
                .all(|(key, _)| key.starts_with(&alias_scope)));
        }
    }

    #[test]
    fn large_history_keeps_middle_conversation_and_bounds_tool_output() {
        let tmp = tempfile::tempdir().unwrap();
        let path = fixture(
            tmp.path(),
            "%2Frepo",
            "uuid",
            json!({}),
            &[
                json!({"type":"user","content":"head"}),
                json!({"type":"tool_result","content":"x".repeat(5000)}),
                json!({"type":"user","content":"middle"}),
                json!({"type":"assistant","content":"answer"}),
                json!({"type":"tool_result","content":"y".repeat(5000)}),
                json!({"type":"user","content":"tail"}),
            ],
        );
        crate::util::TEST_WINDOW.with(|w| w.set(Some((100, 100, 100))));
        let parsed = GrokBuild::in_home(tmp.path()).parse(&path);
        crate::util::TEST_WINDOW.with(|w| w.set(None));
        let session = parsed.unwrap();
        assert!(session.title.starts_with("[windowed]"));
        assert_eq!(
            session
                .messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            vec!["head", "middle", "answer", "tail"]
        );
    }

    #[test]
    fn windowed_results_still_prove_edits_in_the_middle_ran() {
        let tmp = tempfile::tempdir().unwrap();
        let path = fixture(
            tmp.path(),
            "%2Frepo",
            "uuid",
            json!({}),
            &[
                json!({"type":"user","content":"head"}),
                json!({"type":"assistant","tool_calls":[{"id":"one","name":"search_replace","arguments":"{\"file_path\":\"middle.rs\"}"}]}),
                json!({"type":"tool_result","tool_call_id":"one","content":"x".repeat(10000)}),
                json!({"type":"user","content":"tail"}),
            ],
        );
        crate::util::TEST_WINDOW.with(|w| w.set(Some((100, 100, 200))));
        let parsed = GrokBuild::in_home(tmp.path()).parse(&path);
        crate::util::TEST_WINDOW.with(|w| w.set(None));
        assert_eq!(parsed.unwrap().touched, vec!["middle.rs"]);
    }
}
