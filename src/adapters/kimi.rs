//! Kimi Code's indexed session directories and main-agent wire stream.
use super::{
    bounded_redacted_output, dedup_paths, redacted_truncate, root_scope, title_from_messages,
    Adapter, Discovered, Store,
};
use crate::model::{Message, Role, Session, ToolCall, ToolResult};
use crate::util::{read_to_string_capped, short_id};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub struct KimiCode {
    home: Option<PathBuf>,
}

impl Default for KimiCode {
    fn default() -> Self {
        Self {
            home: std::env::var_os("KIMI_CODE_HOME")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .or_else(|| dirs::home_dir().map(|p| p.join(".kimi-code"))),
        }
    }
}

impl KimiCode {
    pub fn in_home(home: impl Into<PathBuf>) -> Self {
        Self {
            home: Some(home.into()),
        }
    }

    fn home(&self) -> Option<PathBuf> {
        self.home.as_deref().map(absolute)
    }

    fn indexed_cwd(&self, path: &Path) -> Option<PathBuf> {
        let id = path.file_name()?.to_str()?;
        // `show` reparses paths using the default adapter even when discovery
        // used in_home. Infer only the two native layouts, never walk upwards
        // through arbitrary projects looking for another installation.
        let parent = path.parent()?;
        let sessions = if parent.file_name().is_some_and(|s| s == "sessions") {
            Some(parent)
        } else {
            parent
                .parent()
                .filter(|p| p.file_name().is_some_and(|s| s == "sessions"))
        };
        let home = sessions
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| self.home())?;
        let text = read_to_string_capped(&home.join("session_index.jsonl")).ok()?;
        // A named directory remains readable even after its index tombstone.
        // Recover the last recorded cwd rather than applying listing filters.
        text.lines()
            .rev()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|record| record["sessionId"].as_str() == Some(id))
            .filter_map(|record| record["workDir"].as_str().map(PathBuf::from))
            .find(|cwd| cwd.is_absolute())
    }

    fn index(&self, had_error: &mut bool) -> BTreeMap<String, (PathBuf, PathBuf)> {
        let mut indexed = BTreeMap::new();
        let Some(home) = self.home() else {
            return indexed;
        };
        let path = home.join("session_index.jsonl");
        let text = match read_to_string_capped(&path) {
            Ok(text) => text,
            Err(_) => {
                if !matches!(path.try_exists(), Ok(false)) {
                    *had_error = true;
                }
                return indexed;
            }
        };
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(record) = serde_json::from_str::<Value>(line) else {
                *had_error = true;
                continue;
            };
            let Some(id) = record["sessionId"]
                .as_str()
                .filter(|id| valid_component(id))
            else {
                *had_error = true;
                continue;
            };
            // The append-only index's last complete entry wins, including
            // tombstones. Never resurrect an unindexed directory by walking it.
            if record["deleted"].as_bool() == Some(true) {
                indexed.remove(id);
                continue;
            }
            let (Some(dir), Some(cwd)) =
                (record["sessionDir"].as_str(), record["workDir"].as_str())
            else {
                *had_error = true;
                continue;
            };
            let dir = PathBuf::from(dir);
            if !dir.is_absolute() || dir.file_name().and_then(|s| s.to_str()) != Some(id) {
                *had_error = true;
                continue;
            }
            indexed.insert(id.to_owned(), (dir, PathBuf::from(cwd)));
        }
        indexed
    }
}

impl Adapter for KimiCode {
    fn name(&self) -> &'static str {
        "kimi-code"
    }

    fn root(&self) -> Option<PathBuf> {
        self.home().map(|p| absolute(&p.join("sessions")))
    }

    fn reconcile_scope(&self) -> Option<String> {
        root_scope(self.root().as_deref())
    }

    fn discover(&self) -> Discovered {
        let store = self.store().expect("Kimi is a shared store");
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
        let Some(home) = self.home() else {
            return Some(store);
        };
        let index = home.join("session_index.jsonl");
        track_file(&index, &mut store.files, &mut store.had_error);
        let indexed = self.index(&mut store.had_error);
        let Some(root) = self.root() else {
            return Some(store);
        };
        for (_, (path, cwd)) in indexed {
            let meta = match fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    store.had_error = true;
                    continue;
                }
            };
            if meta.file_type().is_symlink() || !meta.is_dir() {
                continue;
            }
            let path = match path.canonicalize() {
                Ok(path) => path,
                Err(_) => {
                    store.had_error = true;
                    continue;
                }
            };
            if !path.starts_with(&root) {
                continue;
            }
            match read_json_optional(&path.join("state.json")) {
                Ok(state) if state["archived"].as_bool() == Some(true) => continue,
                Ok(_) => {}
                Err(_) => {
                    store.had_error = true;
                    continue;
                }
            }
            // The shared index is counted once above. Its mtime changes for
            // unrelated sessions; only this entry's semantic metadata belongs
            // in this session's token.
            let mut files = Vec::new();
            track_file(&path.join("state.json"), &mut files, &mut store.had_error);
            track_file(&path.join("wire.jsonl"), &mut files, &mut store.had_error);
            // Subagents are not separate sessions, but their files are real
            // backing data and must count towards size and invalidation.
            for agent in directories(&path.join("agents"), &mut store.had_error) {
                track_file(&agent.join("wire.jsonl"), &mut files, &mut store.had_error);
            }
            store.keys.push((
                path.to_string_lossy().into_owned(),
                change_token(&files, &path, &cwd),
            ));
            store.files.extend(files);
        }
        store.files.sort();
        store.files.dedup();
        Some(store)
    }

    fn parse_key(&self, key: &str) -> Result<Session> {
        self.parse(Path::new(key))
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        let path = absolute(path);
        let state = read_json_optional(&path.join("state.json"))?;
        let cwd = ["workDir", "cwd"]
            .into_iter()
            .filter_map(|key| state[key].as_str())
            .map(PathBuf::from)
            .find(|p| p.is_absolute())
            .or_else(|| self.indexed_cwd(&path));
        let wire = if path.join("agents/main/wire.jsonl").is_file() {
            path.join("agents/main/wire.jsonl")
        } else {
            path.join("wire.jsonl")
        };
        let (lines, windowed) = crate::util::session_lines_keeping(&wire, |line| {
            serde_json::from_str::<Value>(line).is_ok_and(|r| {
                matches!(r["type"].as_str(), Some("turn.prompt" | "turn.steer"))
                    || r.pointer("/event/part/type").and_then(Value::as_str) == Some("text")
            })
        })?;
        let mut messages = Vec::new();
        let mut started = timestamp(&state["createdAt"]);
        let mut ended = timestamp(&state["updatedAt"]);
        let mut edit_calls = HashMap::new();
        let mut completed = HashSet::new();
        for line in lines {
            let Ok(record) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let ts = record_ts(&record);
            if let Some(t) = ts {
                started = Some(started.map_or(t, |old| old.min(t)));
                ended = Some(ended.map_or(t, |old| old.max(t)));
            }
            match record["type"].as_str() {
                Some("turn.prompt" | "turn.steer")
                    if record.pointer("/origin/kind").and_then(Value::as_str) == Some("user") =>
                {
                    push_text(
                        &mut messages,
                        Role::User,
                        &content_text(&record["input"]),
                        ts,
                        false,
                    );
                }
                Some("context.append_loop_event") => {
                    let event = &record["event"];
                    match event["type"].as_str() {
                        Some("content.part")
                            if event.pointer("/part/type").and_then(Value::as_str)
                                == Some("text") =>
                        {
                            push_text(
                                &mut messages,
                                Role::Assistant,
                                event["part"]["text"].as_str().unwrap_or(""),
                                ts,
                                true,
                            );
                        }
                        Some("tool.call") => {
                            let id = event["toolCallId"].as_str().map(str::to_owned);
                            let name = event["name"].as_str().unwrap_or("?");
                            let mut args = event.get("args").cloned().unwrap_or(Value::Null);
                            if matches!(name, "Edit" | "Write") {
                                if let (Some(id), Some(path)) = (
                                    &id,
                                    args["path"].as_str().or_else(|| args["file_path"].as_str()),
                                ) {
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
                        Some("tool.result") => {
                            let call_id = event["toolCallId"].as_str().map(str::to_owned);
                            let result = &event["result"];
                            let is_error = result["isError"].as_bool().unwrap_or(false);
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
                                    text: bounded_redacted_output(&content_text(&result["output"])),
                                },
                            ));
                        }
                        _ => {}
                    }
                }
                // context.append_message repeats the same conversation. Only
                // the wire's original prompts and loop events are projected.
                _ => {}
            }
        }
        let title = if state["isCustomTitle"].is_boolean() {
            state["title"].as_str()
        } else {
            state["customTitle"]
                .as_str()
                .or_else(|| state["title"].as_str())
        };
        for message in &mut messages {
            message.text = crate::redact::redact(&message.text).into_owned();
        }
        let title = title
            .filter(|t| !t.trim().is_empty())
            .map(|t| redacted_truncate(t, 80))
            .unwrap_or_else(|| title_from_messages(&messages));
        let mut touched: Vec<String> = edit_calls
            .into_iter()
            .filter(|(id, _)| completed.contains(id))
            .map(|(_, path)| path)
            .collect();
        if windowed {
            // Text is windowed, but provenance must cover the whole stream.
            touched = wire_edits(&wire)?;
        }
        let mut listing_error = false;
        for agent in directories(&path.join("agents"), &mut listing_error) {
            let sub_wire = agent.join("wire.jsonl");
            if sub_wire != wire && sub_wire.is_file() {
                // Agent ids have their own call-id namespace. Read only their
                // edit evidence; their conversations are not the main session.
                touched.extend(wire_edits(&sub_wire)?);
            }
        }
        Ok(Session {
            id: short_id(&path.to_string_lossy()),
            tool: self.name(),
            path,
            project: cwd
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            started,
            ended,
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

fn wire_edits(path: &Path) -> Result<Vec<String>> {
    let mut calls = HashMap::new();
    let mut completed = HashSet::new();
    crate::util::for_each_line(path, |line| {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            return;
        };
        if record["type"].as_str() != Some("context.append_loop_event") {
            return;
        }
        let event = &record["event"];
        let Some(id) = event["toolCallId"].as_str() else {
            return;
        };
        match event["type"].as_str() {
            Some("tool.call") if matches!(event["name"].as_str(), Some("Edit" | "Write")) => {
                if let Some(path) = event["args"]["path"]
                    .as_str()
                    .or_else(|| event["args"]["file_path"].as_str())
                {
                    calls.insert(id.to_owned(), path.to_owned());
                }
            }
            Some("tool.result") if event["result"]["isError"].as_bool() != Some(true) => {
                completed.insert(id.to_owned());
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

fn valid_component(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && !id.contains(['/', '\\', '\0'])
}

fn absolute(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir().unwrap_or_default().join(path)
        }
    })
}

fn read_json_optional(path: &Path) -> Result<Value> {
    match path.try_exists() {
        Ok(false) => Ok(Value::Null),
        _ => {
            let value: Value = serde_json::from_str(&read_to_string_capped(path)?)
                .with_context(|| format!("parse {}", path.display()))?;
            anyhow::ensure!(value.is_object(), "invalid Kimi state {}", path.display());
            Ok(value)
        }
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

/// Include every file's nanosecond mtime and size, rather than only the newest
/// mtime: restoring a transcript must not hide a later metadata-only edit.
/// Keep opaque tokens positive and above epoch seconds for `sync --since`;
/// `scan` rejects these out-of-range values as wall-clock milliseconds.
fn change_token(files: &[PathBuf], session_path: &Path, indexed_cwd: &Path) -> i64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in session_path
        .to_string_lossy()
        .bytes()
        .chain([0])
        .chain(indexed_cwd.to_string_lossy().bytes())
    {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
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

fn timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .as_i64()
        .and_then(DateTime::from_timestamp_millis)
        .or_else(|| value.as_str().and_then(super::parse_ts))
}

fn record_ts(record: &Value) -> Option<DateTime<Utc>> {
    ["timestamp", "time", "created_at"]
        .into_iter()
        .find_map(|key| timestamp(&record[key]))
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

fn push_text(
    messages: &mut Vec<Message>,
    role: Role,
    text: &str,
    ts: Option<DateTime<Utc>>,
    append: bool,
) {
    if text.is_empty() {
        return;
    }
    if append {
        if let Some(last) = messages
            .last_mut()
            .filter(|m| m.role == role && m.tool.is_none())
        {
            last.text.push_str(text);
            return;
        }
    }
    // Redact after concatenating all chunks, so split credentials stay whole.
    messages.push(Message {
        role,
        text: text.to_owned(),
        ts,
        tool: None,
    });
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

    fn fixture(home: &Path, id: &str, state: Value, records: &[Value]) -> PathBuf {
        let path = home.join("sessions/workspace").join(id);
        fs::create_dir_all(path.join("agents/main")).unwrap();
        fs::write(path.join("state.json"), state.to_string()).unwrap();
        fs::write(
            path.join("agents/main/wire.jsonl"),
            records
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        path.canonicalize().unwrap()
    }

    fn index(home: &Path, entries: &[Value]) {
        fs::write(
            home.join("session_index.jsonl"),
            entries
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
    }

    fn entry(path: &Path) -> Value {
        json!({"sessionId": path.file_name().unwrap().to_str().unwrap(), "sessionDir": path, "workDir": "/repo/from-index"})
    }

    fn prompt(text: &str) -> Value {
        json!({"type":"turn.prompt", "origin":{"kind":"user"}, "input":[{"type":"text","text":text}],"time":1700000000000i64})
    }

    fn event(event: Value) -> Value {
        json!({"type":"context.append_loop_event", "event":event,"time":1700000001000i64})
    }

    #[test]
    fn index_tombstones_archives_and_unindexed_directories_control_discovery() {
        let tmp = tempfile::tempdir().unwrap();
        let live = fixture(tmp.path(), "session-live", json!({"cwd":"/repo"}), &[]);
        let deleted = fixture(tmp.path(), "session-deleted", json!({}), &[]);
        let archived = fixture(
            tmp.path(),
            "session-archived",
            json!({"archived":true}),
            &[],
        );
        fixture(tmp.path(), "session-unindexed", json!({}), &[]);
        index(
            tmp.path(),
            &[
                entry(&live),
                entry(&deleted),
                entry(&archived),
                json!({"sessionId":"session-deleted","deleted":true}),
            ],
        );
        let adapter = KimiCode::in_home(tmp.path());
        let store = adapter.store().unwrap();
        assert!(!store.had_error);
        assert_eq!(store.keys.len(), 1);
        assert_eq!(store.keys[0].0, live.to_string_lossy());
        assert!(store.files.iter().all(|p| p.is_file()));
        assert_eq!(store.files.len(), 3); // shared index, state, main wire
        assert_eq!(adapter.discover().files, vec![live]);
    }

    #[test]
    fn cwd_and_legacy_title_metadata_fall_back_to_index() {
        let tmp = tempfile::tempdir().unwrap();
        let path = fixture(
            tmp.path(),
            "session-one",
            json!({"workDir":"relative", "customTitle":"Legacy title", "title":"Generated", "createdAt":1700000000000i64,"updatedAt":1700000001000i64}),
            &[prompt("hello")],
        );
        index(tmp.path(), &[entry(&path)]);
        let adapter = KimiCode::in_home(tmp.path());
        let session = adapter.parse_key(&path.to_string_lossy()).unwrap();
        assert_eq!(session.id, short_id(&path.to_string_lossy()));
        assert_eq!(session.path, path);
        assert_eq!(session.project, "/repo/from-index");
        assert_eq!(session.title, "Legacy title");
        assert_eq!(session.started.unwrap().timestamp_millis(), 1700000000000);
        assert_eq!(session.ended.unwrap().timestamp_millis(), 1700000001000);
        let other_home = tempfile::tempdir().unwrap();
        let other_adapter = KimiCode::in_home(other_home.path());
        assert_eq!(
            other_adapter.parse(&path).unwrap().project,
            "/repo/from-index"
        );
        fs::write(path.join("state.json"), r#"{"isCustomTitle":false,"title":"Current title","customTitle":"Stale title","cwd":"/state-cwd"}"#).unwrap();
        let current = adapter.parse(&path).unwrap();
        assert_eq!(current.title, "Current title");
        assert_eq!(current.project, "/state-cwd");
    }

    #[test]
    fn conversation_is_not_duplicated_and_tools_remain_structured_and_redacted() {
        let tmp = tempfile::tempdir().unwrap();
        let secret = format!("sk-{}", "a".repeat(48));
        let records = vec![
            prompt(&format!("fix {secret}")),
            json!({"type":"context.append_message","message":{"role":"user","content":"duplicate"}}),
            json!({"type":"turn.prompt","origin":{"kind":"agent"},"input":[{"type":"text","text":"synthetic"}]}),
            event(json!({"type":"content.part","part":{"type":"text","text":"Done "}})),
            event(json!({"type":"content.part","part":{"type":"text","text":secret}})),
            event(
                json!({"type":"tool.call","toolCallId":"ok","name":"Edit","args":{"path":"src/lib.rs","nested":[secret]}}),
            ),
            event(
                json!({"type":"tool.result","toolCallId":"ok","result":{"output":[{"type":"text","text":secret}],"isError":false}}),
            ),
            event(
                json!({"type":"tool.call","toolCallId":"failed","name":"Write","args":{"path":"failed.rs"}}),
            ),
            event(
                json!({"type":"tool.result","toolCallId":"failed","result":{"output":"failed","isError":true}}),
            ),
            event(
                json!({"type":"tool.call","toolCallId":"pending","name":"Write","args":{"path":"pending.rs"}}),
            ),
        ];
        let path = fixture(
            tmp.path(),
            "session-tools",
            json!({"cwd":"/repo","title":secret}),
            &records,
        );
        let adapter = KimiCode::in_home(tmp.path());
        let session = adapter.parse(&path).unwrap();
        assert_eq!(session.tool, "kimi-code");
        assert_eq!(
            session
                .messages
                .iter()
                .filter(|m| m.role == Role::User)
                .count(),
            1
        );
        assert!(session
            .messages
            .iter()
            .any(|m| m.role == Role::Assistant && m.text.starts_with("Done ")));
        assert!(
            matches!(&session.messages[2].tool, Some(ToolEvent::Part(ToolPart::Call(call))) if call.id.as_deref() == Some("ok") && call.args["path"] == "src/lib.rs")
        );
        assert!(
            matches!(&session.messages[3].tool, Some(ToolEvent::Part(ToolPart::Result(result))) if !result.is_error && result.call_id.as_deref() == Some("ok"))
        );
        assert_eq!(session.touched, vec!["src/lib.rs"]);
        assert!(!serde_json::to_string(&session).unwrap().contains(&secret));
        let folded = super::super::parse_session_key(&adapter, &path.to_string_lossy()).unwrap();
        assert!(folded
            .messages
            .iter()
            .any(|m| matches!(m.tool, Some(ToolEvent::Summary(_)))));
    }

    #[test]
    fn malformed_index_or_state_marks_listing_partial_but_bad_wire_lines_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let path = fixture(tmp.path(), "session-one", json!({}), &[prompt("survives")]);
        index(tmp.path(), &[entry(&path)]);
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(tmp.path().join("session_index.jsonl"))
            .unwrap()
            .write_all(b"\n{truncated")
            .unwrap();
        let adapter = KimiCode::in_home(tmp.path());
        let store = adapter.store().unwrap();
        assert_eq!(store.keys.len(), 1);
        assert!(store.had_error);
        fs::OpenOptions::new()
            .append(true)
            .open(path.join("agents/main/wire.jsonl"))
            .unwrap()
            .write_all(b"\ninvalid\n{\"type\":\"unknown\"}\n")
            .unwrap();
        assert_eq!(adapter.parse(&path).unwrap().messages.len(), 1);
        fs::write(path.join("state.json"), "[]").unwrap();
        assert!(adapter.store().unwrap().had_error);
        assert!(adapter.parse(&path).is_err());
        assert!(
            !KimiCode::in_home(tmp.path().join("missing"))
                .store()
                .unwrap()
                .had_error
        );
    }

    #[test]
    fn all_backing_metadata_contributes_to_incremental_token() {
        let tmp = tempfile::tempdir().unwrap();
        let path = fixture(tmp.path(), "session-one", json!({"title":"A"}), &[]);
        index(tmp.path(), &[entry(&path)]);
        let wire = path.join("agents/main/wire.jsonl");
        let future = std::time::UNIX_EPOCH + std::time::Duration::from_secs(2000000000);
        fs::File::open(&wire)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(future))
            .unwrap();
        let adapter = KimiCode::in_home(tmp.path());
        let first = adapter.store().unwrap().keys[0].1;
        let state = fs::File::open(path.join("state.json")).unwrap();
        state
            .set_times(
                fs::FileTimes::new().set_modified(
                    std::time::UNIX_EPOCH + std::time::Duration::from_secs(1700000000),
                ),
            )
            .unwrap();
        let second = adapter.store().unwrap().keys[0].1;
        assert_ne!(
            first, second,
            "state mtime matters even with a newer transcript"
        );
        assert!(second > 2000000000);
        assert!(DateTime::<Utc>::from_timestamp_millis(second).is_none());
        let dir_mtime = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&wire, "changed").unwrap();
        assert_ne!(second, adapter.store().unwrap().keys[0].1);
        assert_eq!(dir_mtime, fs::metadata(&path).unwrap().modified().unwrap());
    }

    #[test]
    fn unrelated_index_activity_does_not_reparse_existing_histories() {
        let tmp = tempfile::tempdir().unwrap();
        let path = fixture(tmp.path(), "session-one", json!({}), &[]);
        index(tmp.path(), &[entry(&path)]);
        let adapter = KimiCode::in_home(tmp.path());
        let token = |store: Store| {
            store
                .keys
                .into_iter()
                .find(|(key, _)| key == &*path.to_string_lossy())
                .unwrap()
                .1
        };
        let original = token(adapter.store().unwrap());
        let unrelated = fixture(tmp.path(), "session-other", json!({}), &[]);
        use std::io::Write;
        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .open(tmp.path().join("session_index.jsonl"))
                .unwrap(),
            "\n{}",
            entry(&unrelated)
        )
        .unwrap();
        let store = adapter.store().unwrap();
        assert_eq!(
            store
                .files
                .iter()
                .filter(|p| p.file_name().is_some_and(|s| s == "session_index.jsonl"))
                .count(),
            1
        );
        assert_eq!(original, token(store));
        let mut changed = entry(&path);
        changed["workDir"] = json!("/repo/changed-cwd");
        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .open(tmp.path().join("session_index.jsonl"))
                .unwrap(),
            "{changed}"
        )
        .unwrap();
        assert_ne!(original, token(adapter.store().unwrap()));
        assert_eq!(adapter.parse(&path).unwrap().project, "/repo/changed-cwd");
    }

    #[test]
    fn scope_uses_canonical_home_and_never_swallows_neighbor_home() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let ab = tmp.path().join("ab");
        fs::create_dir_all(a.join("sessions")).unwrap();
        fs::create_dir_all(ab.join("sessions")).unwrap();
        let scope = KimiCode::in_home(&a).reconcile_scope().unwrap();
        assert!(a.join("sessions/one").to_string_lossy().starts_with(&scope));
        assert!(!ab
            .join("sessions/one")
            .to_string_lossy()
            .starts_with(&scope));
        #[cfg(unix)]
        {
            let alias = tmp.path().join("alias");
            std::os::unix::fs::symlink(&a, &alias).unwrap();
            assert_eq!(KimiCode::in_home(alias).reconcile_scope(), Some(scope));
        }
    }

    #[test]
    fn windows_large_wire_and_redacts_credentials_split_between_chunks() {
        let tmp = tempfile::tempdir().unwrap();
        let path = fixture(
            tmp.path(),
            "session-one",
            json!({}),
            &[
                prompt("head"),
                event(json!({"type":"content.part","part":{"type":"text","text":"sk-"}})),
                event(json!({"type":"content.part","part":{"type":"text","text":"a".repeat(48)}})),
                prompt("middle user"),
                event(json!({"type":"tool.result","result":{"output":"x".repeat(5000)}})),
                prompt("tail"),
            ],
        );
        crate::util::TEST_WINDOW.with(|w| w.set(Some((100, 100, 300))));
        let parsed = KimiCode::in_home(tmp.path()).parse(&path);
        crate::util::TEST_WINDOW.with(|w| w.set(None));
        let session = parsed.unwrap();
        assert!(session.title.starts_with("[windowed]"));
        assert!(session.messages.iter().any(|m| m.text == "middle user"));
        assert!(!serde_json::to_string(&session)
            .unwrap()
            .contains(&format!("sk-{}", "a".repeat(48))));
    }

    #[test]
    fn windowed_and_subagent_edits_keep_complete_provenance_in_separate_call_namespaces() {
        let tmp = tempfile::tempdir().unwrap();
        let call = event(
            json!({"type":"tool.call","toolCallId":"same-id","name":"Edit","args":{"path":"main.rs"}}),
        );
        let result = event(
            json!({"type":"tool.result","toolCallId":"same-id","result":{"output":"x".repeat(10000)}}),
        );
        let path = fixture(
            tmp.path(),
            "session-one",
            json!({}),
            &[prompt("head"), call, result, prompt("tail")],
        );
        let sub = path.join("agents/sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("wire.jsonl"), [
            event(json!({"type":"tool.call","toolCallId":"same-id","name":"Write","args":{"path":"sub.rs"}})).to_string(),
            event(json!({"type":"tool.result","toolCallId":"same-id","result":{"output":"ok"}})).to_string(),
            event(json!({"type":"content.part","part":{"type":"text","text":"subagent text"}})).to_string(),
        ].join("\n")).unwrap();
        crate::util::TEST_WINDOW.with(|w| w.set(Some((100, 100, 200))));
        let parsed = KimiCode::in_home(tmp.path()).parse(&path);
        crate::util::TEST_WINDOW.with(|w| w.set(None));
        let session = parsed.unwrap();
        let mut touched = session.touched;
        touched.sort();
        assert_eq!(touched, vec!["main.rs", "sub.rs"]);
        assert!(!session.messages.iter().any(|m| m.text == "subagent text"));
    }
}
