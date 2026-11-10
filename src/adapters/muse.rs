//! Muse's native event log, including conversation held in retained frames.
//! Active logs are useful to search: malformed records and unfinished tails
//! are skipped without requiring an importable, completed turn.
//! Format: Muse v1 session envelopes and retained frames; runtime.session
//! payloads v1/v2, user-intent/metadata/name payloads v1.

use super::{
    bounded_redacted_output, dedup_paths, ok_or_flag, redacted_truncate, title_from_messages,
    Adapter, Discovered, Store,
};
use crate::model::{Message, Role, Session, ToolCall, ToolResult};
use anyhow::{ensure, Result};
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// An adapter for native Muse data or one isolated Muse configuration home.
pub struct Muse {
    sessions_root: Option<PathBuf>,
}

impl Default for Muse {
    fn default() -> Self {
        Self {
            sessions_root: dirs::data_dir().map(|data| data.join("muse/sessions")),
        }
    }
}

impl Muse {
    /// `home` is Muse's configuration directory, not its data directory.
    /// The native configuration uses XDG data storage; private profiles keep
    /// their data under `<home>/.data/muse/sessions`.
    pub fn in_home(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        Self {
            sessions_root: sessions_root_at(
                &home,
                dirs::config_dir().as_deref(),
                dirs::data_dir().as_deref(),
            ),
        }
    }

    /// Read an explicit native sessions directory.
    pub fn in_sessions_root(root: impl Into<PathBuf>) -> Self {
        Self {
            sessions_root: Some(root.into()),
        }
    }
}

fn sessions_root_at(home: &Path, config: Option<&Path>, data: Option<&Path>) -> Option<PathBuf> {
    if config.is_some_and(|config| home == config.join("muse")) {
        data.map(|data| data.join("muse/sessions"))
    } else {
        Some(home.join(".data/muse/sessions"))
    }
}

impl Adapter for Muse {
    fn name(&self) -> &'static str {
        "muse"
    }

    fn root(&self) -> Option<PathBuf> {
        let root = self.sessions_root.as_ref()?;
        Some(crate::util::canonicalize_as_recorded(root).unwrap_or_else(|_| root.clone()))
    }

    fn reconcile_scope(&self) -> Option<String> {
        super::root_scope(self.root().as_deref())
    }

    fn discover(&self) -> Discovered {
        let Some(root) = self.root() else {
            return Vec::new().into();
        };
        match std::fs::symlink_metadata(&root) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Vec::new().into();
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Discovered {
                    files: Vec::new(),
                    had_error: true,
                };
            }
            _ => {}
        }
        let mut had_error = false;
        let mut files = Vec::new();
        // Exactly YYYY/MM/DD/<id>/session.jsonl. Never descend into subagent
        // directories, or follow a symlink out of this home's canonical root.
        for entry in WalkDir::new(&root)
            .follow_links(false)
            .max_depth(5)
            .into_iter()
            .filter_entry(|entry| match entry.depth() {
                0 => true,
                1..=4 => entry.file_type().is_dir() && valid_directory(entry.path(), entry.depth()),
                5 => entry.file_type().is_file() && entry.file_name() == "session.jsonl",
                _ => false,
            })
        {
            let Some(entry) = ok_or_flag(entry, &mut had_error) else {
                continue;
            };
            if entry.depth() == 5 && entry.file_type().is_file() {
                files.push(entry.into_path());
            }
        }
        files.sort();
        Discovered { files, had_error }
    }

    fn store(&self) -> Option<Store> {
        let Discovered {
            files,
            mut had_error,
        } = self.discover();
        let mut keys = Vec::with_capacity(files.len());
        for file in &files {
            let Some(modified) =
                ok_or_flag(file.metadata().and_then(|m| m.modified()), &mut had_error)
            else {
                continue;
            };
            // Keys match Mj's former adapter; tokens use milliseconds as
            // required by Store's scan reporting, catching subsecond updates.
            let token = modified
                .duration_since(std::time::UNIX_EPOCH)
                .map(|age| age.as_millis().min(i64::MAX as u128) as i64)
                .unwrap_or_default();
            keys.push((file.to_string_lossy().into_owned(), token));
        }
        Some(Store {
            keys,
            files,
            had_error,
        })
    }

    fn parse_key(&self, key: &str) -> Result<Session> {
        ensure!(!key.is_empty(), "no Muse session path in key");
        self.parse(Path::new(key))
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        let metadata = std::fs::symlink_metadata(path)?;
        ensure!(metadata.is_file(), "Muse session is not a regular file");
        let (lines, _windowed) = crate::util::session_lines(path)?;
        // A retained permission frame may precede the first ordinary record.
        // Determine the outer stream first to exclude unrelated child streams.
        let stream = lines.iter().find_map(|line| {
            let value: Value = serde_json::from_str(line).ok()?;
            if value.get("retained_frame").is_some() || !supported_record(&value) {
                return None;
            }
            value.pointer("/stream/id")?.as_str().map(str::to_owned)
        });
        let mut projection = Projection::default();
        for line in lines {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let mut budget = FrameBudget {
                bytes: 16 * 1024 * 1024,
                records: 10_000,
            };
            flatten(&value, false, 0, &mut budget, &mut |record, retained| {
                if supported_record(record)
                    && stream.as_deref().is_none_or(|stream| {
                        record.pointer("/stream/id").and_then(Value::as_str) == Some(stream)
                    })
                {
                    projection.record(record, retained);
                }
            });
        }
        let title = projection
            .title
            .unwrap_or_else(|| title_from_messages(&projection.messages));
        Ok(Session {
            id: crate::util::short_id(&path.to_string_lossy()),
            tool: "muse",
            path: path.to_path_buf(),
            project: projection.cwd.unwrap_or_default(),
            started: projection.started,
            ended: projection.ended,
            title,
            subagent: false,
            messages: projection.messages,
            touched: dedup_paths(projection.touched),
            edits: Vec::new(),
        })
    }
}

fn valid_directory(path: &Path, depth: usize) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if depth == 4 {
        return !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            && !matches!(name, "." | "..");
    }
    let width = if depth == 1 { 4 } else { 2 };
    if name.len() != width || !name.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    match depth {
        2 => name
            .parse::<u32>()
            .is_ok_and(|month| (1..=12).contains(&month)),
        3 => {
            let Some(month) = path.parent() else {
                return false;
            };
            let Some(year) = month.parent() else {
                return false;
            };
            let date = format!(
                "{}-{}-{name}",
                year.file_name().unwrap_or_default().to_string_lossy(),
                month.file_name().unwrap_or_default().to_string_lossy()
            );
            NaiveDate::parse_from_str(&date, "%Y-%m-%d").is_ok()
        }
        _ => true,
    }
}

fn supported_record(value: &Value) -> bool {
    value["schema_version"] == 1
        && value.pointer("/stream/kind").and_then(Value::as_str) == Some("session")
        && value
            .pointer("/stream/id")
            .and_then(Value::as_str)
            .is_some()
        && value["payload"].is_object()
        && match value["payload_type"].as_str() {
            Some("runtime.session") => {
                matches!(value["payload_schema_version"].as_u64(), Some(1 | 2))
            }
            Some(
                "runtime.session.metadata"
                | "runtime.user_intent.accepted"
                | "runtime.user_intent.materialized"
                | "session.name.changed",
            ) => value["payload_schema_version"] == 1,
            _ => false,
        }
}

struct FrameBudget {
    bytes: usize,
    records: usize,
}

/// Only decode the designated frame child fields, never JSON inside prompts
/// or tool output. Depth and aggregate decoded bytes bound nested encodings.
fn flatten(
    value: &Value,
    retained: bool,
    depth: usize,
    budget: &mut FrameBudget,
    emit: &mut impl FnMut(&Value, bool),
) {
    if depth > 8 || budget.records == 0 {
        return;
    }
    budget.records -= 1;
    if value.get("retained_frame").is_none() {
        emit(value, retained);
        return;
    }
    if value["frame_schema_version"] != 1 {
        return;
    }
    if let Some(children) = value.get("children").or_else(|| value.get("children_json")) {
        frame_children(children, depth + 1, budget, emit);
    }
}

fn decode_frame_json(text: &str, budget: &mut FrameBudget) -> Option<Value> {
    if text.len() > budget.bytes {
        return None;
    }
    budget.bytes -= text.len();
    serde_json::from_str(text).ok()
}

fn frame_children(
    children: &Value,
    depth: usize,
    budget: &mut FrameBudget,
    emit: &mut impl FnMut(&Value, bool),
) {
    if depth > 8 || budget.records == 0 {
        return;
    }
    if let Some(text) = children.as_str() {
        if let Some(decoded) = decode_frame_json(text, budget) {
            frame_children(&decoded, depth + 1, budget, emit);
        }
        return;
    }
    let Some(children) = children.as_array() else {
        return;
    };
    for child in children {
        if budget.records == 0 {
            break;
        }
        if let Some(text) = child
            .get("record_json")
            .and_then(Value::as_str)
            .or_else(|| child.as_str())
        {
            if let Some(record) = decode_frame_json(text, budget) {
                flatten(&record, true, depth + 1, budget, emit);
            }
        } else {
            flatten(child, true, depth + 1, budget, emit);
        }
    }
}

#[derive(Default)]
struct Projection {
    cwd: Option<String>,
    title: Option<String>,
    started: Option<DateTime<Utc>>,
    ended: Option<DateTime<Utc>>,
    messages: Vec<Message>,
    touched: Vec<String>,
    records: HashSet<String>,
    intents: HashSet<String>,
    assistants: HashSet<String>,
    calls: HashSet<String>,
    results: HashSet<String>,
}

impl Projection {
    fn record(&mut self, value: &Value, retained: bool) {
        if !first_id(&mut self.records, value.get("id")) {
            return;
        }
        let ts = value["recorded_at"]
            .as_i64()
            .and_then(DateTime::from_timestamp_micros);
        if let Some(ts) = ts {
            self.started = Some(self.started.map_or(ts, |old| old.min(ts)));
            self.ended = Some(self.ended.map_or(ts, |old| old.max(ts)));
        }
        let payload = &value["payload"];
        match value["payload_type"].as_str() {
            Some("runtime.session.metadata") if !retained => {
                if let Some(cwd) = payload
                    .pointer("/record/workspace_root")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                {
                    self.cwd = Some(cwd.to_owned());
                }
            }
            Some("session.name.changed") if !retained => {
                if let Some(title) = payload["new_name"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                {
                    self.title = Some(redacted_truncate(title, 80));
                }
            }
            Some("runtime.user_intent.accepted") => {
                if let Some(text) = intent_text(payload) {
                    let key = payload["intent_id"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("{}:{text}", value["sequence"]));
                    if self.intents.insert(key) {
                        self.message(Role::User, &text, ts);
                    }
                }
            }
            Some("runtime.session") if payload["kind"] == "run" => {
                let event = &payload["event"];
                match event["kind"].as_str() {
                    Some("assistant_message_committed") => {
                        if let Some(text) = event["text"].as_str().filter(|s| !s.trim().is_empty())
                        {
                            if first_id(&mut self.assistants, event.get("message_id")) {
                                self.message(Role::Assistant, text, ts);
                            }
                        }
                    }
                    Some("assistant_tool_calls_committed") => {
                        for call in event["tool_calls"].as_array().into_iter().flatten() {
                            self.call(call, ts);
                        }
                    }
                    Some("tool_result_batch_committed") => {
                        for result in event["results"].as_array().into_iter().flatten() {
                            self.result(result, ts);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn message(&mut self, role: Role, text: &str, ts: Option<DateTime<Utc>>) {
        self.messages.push(Message {
            role,
            text: crate::redact::redact(text).into_owned(),
            ts,
            tool: None,
        });
    }

    fn call(&mut self, call: &Value, ts: Option<DateTime<Utc>>) {
        let Some(name) = call["name"].as_str().filter(|s| !s.is_empty()) else {
            return;
        };
        let id = call.get("call_id").or_else(|| call.get("id"));
        if !first_id(&mut self.calls, id) {
            return;
        }
        let mut args = call.get("args").cloned().unwrap_or(Value::Null);
        if let Some(text) = args.as_str() {
            args = serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned()));
        }
        redact_json_strings(&mut args);
        let lower = name.to_ascii_lowercase();
        if !lower.contains("read")
            && [
                "write", "edit", "patch", "replace", "rename", "delete", "move",
            ]
            .iter()
            .any(|verb| lower.contains(verb))
        {
            collect_paths(&args, &mut self.touched);
        }
        self.messages.push(Message::tool_call(
            ts,
            ToolCall {
                id: id
                    .and_then(Value::as_str)
                    .map(|s| crate::redact::redact(s).into_owned()),
                name: crate::redact::redact(name).into_owned(),
                args,
            },
        ));
    }

    fn result(&mut self, result: &Value, ts: Option<DateTime<Utc>>) {
        let Some(text) = result["text"].as_str() else {
            return;
        };
        let id = result.get("tool_call_id").or_else(|| result.get("call_id"));
        if !first_id(&mut self.results, id) {
            return;
        }
        self.messages.push(Message::tool_result(
            ts,
            ToolResult {
                call_id: id
                    .and_then(Value::as_str)
                    .map(|s| crate::redact::redact(s).into_owned()),
                text: bounded_redacted_output(text),
                is_error: result["is_error"].as_bool().unwrap_or(false),
            },
        ));
    }
}

fn first_id(seen: &mut HashSet<String>, value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .is_none_or(|id| seen.insert(id.to_owned()))
}

fn intent_text(payload: &Value) -> Option<String> {
    let text_parts = |parts: Vec<&Value>| {
        parts
            .into_iter()
            .filter(|part| {
                part.get("kind")
                    .or_else(|| part.get("type"))
                    .and_then(Value::as_str)
                    == Some("text")
            })
            .filter_map(|part| part["text"].as_str())
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let messages = payload["model_messages"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .collect();
    let text = text_parts(messages);
    if !text.is_empty() {
        return Some(text);
    }
    let text = text_parts(
        payload["refill_blocks"]
            .as_array()
            .into_iter()
            .flatten()
            .collect(),
    );
    if !text.is_empty() {
        return Some(text);
    }
    payload
        .get("prompt")
        .or_else(|| payload.get("text"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
}

fn redact_json_strings(value: &mut Value) {
    match value {
        Value::String(text) => *text = crate::redact::redact(text).into_owned(),
        Value::Array(values) => values.iter_mut().for_each(redact_json_strings),
        Value::Object(values) => values.values_mut().for_each(redact_json_strings),
        _ => {}
    }
}

fn collect_paths(value: &Value, paths: &mut Vec<String>) {
    match value {
        Value::Object(values) => {
            for (key, value) in values {
                if matches!(
                    key.as_str(),
                    "path" | "file_path" | "filePath" | "target_path" | "targetPath" | "filename"
                ) {
                    if let Some(path) = value.as_str() {
                        paths.push(path.to_owned());
                    }
                }
                collect_paths(value, paths);
            }
        }
        Value::Array(values) => values.iter().for_each(|value| collect_paths(value, paths)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ToolEvent, ToolPart};
    use serde_json::json;
    use std::fs;

    const TIME: i64 = 1_788_872_000_000_000;

    // Native envelope and payload shapes from mj-checkpoint's Muse fixtures.
    fn record(sequence: u64, payload_type: &str, payload: Value) -> Value {
        json!({
            "schema_version": 1,
            "id": format!("record-{sequence}"),
            "stream": {"kind": "session", "id": "session-1"},
            "sequence": sequence,
            "recorded_at": TIME + sequence as i64,
            "record_type": "event", "durability": "durable",
            "payload_type": payload_type, "payload_schema_version": 1,
            "payload": payload,
        })
    }

    fn intent(sequence: u64, id: &str, text: &str) -> Value {
        record(
            sequence,
            "runtime.user_intent.accepted",
            json!({
                "intent_id": id,
                "model_messages": [{"role":"user", "content":[{"kind":"text", "text": text}]}],
            }),
        )
    }

    fn run(sequence: u64, event: Value) -> Value {
        record(
            sequence,
            "runtime.session",
            json!({"kind":"run", "run_id":"run-1", "event":event}),
        )
    }

    fn frame(children: Vec<Value>) -> Value {
        json!({
            "retained_frame": "session_permission_transaction", "frame_schema_version":1,
            "outer_log_ordinal":1, "transaction_id":"tx-1", "content_sha256":"sha256:opaque",
            "children": children.into_iter().enumerate().map(|(i, child)| json!({"child_index":i,"record_json":child.to_string()})).collect::<Vec<_>>(),
        })
    }

    fn write(root: &Path, relative: &str, records: &[Value]) -> PathBuf {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = records
            .iter()
            .map(|record| format!("{record}\n"))
            .collect::<String>();
        fs::write(&path, body).unwrap();
        path
    }

    fn texts(session: &Session, role: Role) -> Vec<&str> {
        session
            .messages
            .iter()
            .filter(|message| message.role == role)
            .map(|message| message.text.as_str())
            .collect()
    }

    #[test]
    fn native_and_private_config_homes_use_distinct_data_roots() {
        let config = Path::new("/xdg/config");
        let data = Path::new("/xdg/data");
        assert_eq!(
            sessions_root_at(&config.join("muse"), Some(config), Some(data)),
            Some(data.join("muse/sessions"))
        );
        let private = Path::new("/profiles/private-muse");
        assert_eq!(
            sessions_root_at(private, Some(config), Some(data)),
            Some(private.join(".data/muse/sessions"))
        );
        assert_eq!(
            Muse::in_home(private).root(),
            Some(private.join(".data/muse/sessions"))
        );
        if let Some(config) = dirs::config_dir() {
            assert_eq!(
                Muse::in_home(config.join("muse")).root(),
                Muse::default().root()
            );
        }
        assert_eq!(
            sessions_root_at(&config.join("muse"), Some(config), None),
            None
        );
    }

    #[test]
    fn discovers_only_top_level_dated_sessions_and_keeps_file_keys() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let path = write(root, "2026/09/08/session-1/session.jsonl", &[]);
        for relative in [
            "2026/09/08/session-1/subagent/child/session.jsonl",
            "2026/09/08/session-1/other.jsonl",
            "2026/9/08/session-2/session.jsonl",
            "2026/13/08/session-2/session.jsonl",
            "2026/02/30/session-2/session.jsonl",
            "202x/09/08/session-2/session.jsonl",
            "2026/09/08/invalid id/session.jsonl",
            "session.jsonl",
        ] {
            write(root, relative, &[]);
        }
        let adapter = Muse::in_sessions_root(root);
        let canonical_path = crate::util::canonicalize_as_recorded(&path).unwrap();
        let found = adapter.discover();
        assert!(!found.had_error);
        assert_eq!(found.files, vec![canonical_path.clone()]);
        let store = adapter.store().unwrap();
        assert!(!store.had_error);
        assert_eq!(store.files, found.files);
        assert_eq!(store.keys.len(), 1);
        assert_eq!(store.keys[0].0, canonical_path.to_string_lossy());
        let modified = path
            .metadata()
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert_eq!(store.keys[0].1, modified);
        let parsed = adapter.parse_key(&store.keys[0].0).unwrap();
        assert_eq!(parsed.id, crate::util::short_id(&store.keys[0].0));
        assert_eq!(parsed.path, canonical_path);
        assert!(store.keys[0]
            .0
            .starts_with(&adapter.reconcile_scope().unwrap()));
    }

    #[test]
    fn missing_roots_are_normal_but_failed_walks_protect_reconciliation() {
        let directory = tempfile::tempdir().unwrap();
        let missing = Muse::in_sessions_root(directory.path().join("missing"));
        assert!(!missing.discover().had_error);
        assert!(!missing.store().unwrap().had_error);
        let file = directory.path().join("not-a-directory");
        fs::write(&file, "").unwrap();
        let broken = Muse::in_sessions_root(file).store().unwrap();
        assert!(broken.had_error);
        assert!(broken.keys.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn canonical_scopes_and_symlink_exclusion_prevent_home_escapes() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("real");
        let native = write(&root, "2026/09/08/session-1/session.jsonl", &[]);
        let outside = directory.path().join("outside");
        let escaped = write(&outside, "2026/09/08/session-2/session.jsonl", &[]);
        symlink(outside.join("2026"), root.join("2025")).unwrap();
        symlink(
            escaped.parent().unwrap(),
            root.join("2026/09/08/linked-session"),
        )
        .unwrap();
        let linked_file = root.join("2026/09/08/linked-file");
        fs::create_dir_all(&linked_file).unwrap();
        symlink(&escaped, linked_file.join("session.jsonl")).unwrap();
        symlink(&root, directory.path().join("alias")).unwrap();
        let original = Muse::in_sessions_root(&root);
        let alias = Muse::in_sessions_root(directory.path().join("alias"));
        assert_eq!(original.root(), alias.root());
        assert_eq!(original.reconcile_scope(), alias.reconcile_scope());
        assert_eq!(alias.discover().files, vec![native.canonicalize().unwrap()]);
        assert!(!alias.discover().had_error);
        assert!(alias.parse(&linked_file.join("session.jsonl")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn partial_walk_still_returns_readable_sessions() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = write(directory.path(), "2026/09/08/session-1/session.jsonl", &[]);
        let blocked = directory.path().join("2026/10");
        fs::create_dir_all(&blocked).unwrap();
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o0)).unwrap();
        let denied = fs::read_dir(&blocked).is_err();
        let store = Muse::in_sessions_root(directory.path()).store().unwrap();
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(store.files, vec![path.canonicalize().unwrap()]);
        if denied {
            assert!(store.had_error);
        }
    }

    #[test]
    fn indexes_active_conversation_metadata_and_deduplicates_commits() {
        let directory = tempfile::tempdir().unwrap();
        let records = vec![
            record(
                1,
                "runtime.session.metadata",
                json!({"kind":"metadata", "record":{"workspace_root":"/old"}}),
            ),
            intent(2, "intent-1", "continue"),
            intent(3, "intent-1", "continue"),
            record(
                4,
                "runtime.user_intent.materialized",
                json!({"intent_id":"intent-1", "outcome":{"run_id":"run-1"}}),
            ),
            run(
                5,
                json!({"kind":"assistant_message_committed", "message_id":"answer-1", "text":"hello back"}),
            ),
            run(
                6,
                json!({"kind":"assistant_message_committed", "message_id":"answer-1", "text":"hello back"}),
            ),
            intent(7, "intent-2", "continue"),
            record(
                8,
                "runtime.session.metadata",
                json!({"kind":"metadata", "record":{"workspace_root":"/work"}}),
            ),
            record(9, "session.name.changed", json!({"new_name":"Old title"})),
            record(
                10,
                "session.name.changed",
                json!({"new_name":"Native title"}),
            ),
            run(11, json!({"kind":"started"})),
        ];
        let path = write(directory.path(), "session-1/session.jsonl", &records);
        let session = Muse::default().parse(&path).unwrap();
        assert_eq!(session.tool, "muse");
        assert_eq!(session.project, "/work");
        assert_eq!(session.title, "Native title");
        assert!(!session.subagent);
        assert_eq!(texts(&session, Role::User), ["continue", "continue"]);
        assert_eq!(texts(&session, Role::Assistant), ["hello back"]);
        assert_eq!(session.started.unwrap().timestamp_micros(), TIME + 1);
        assert_eq!(session.ended.unwrap().timestamp_micros(), TIME + 11);
        assert_eq!(session.messages[0].ts.unwrap().timestamp_micros(), TIME + 2);
        assert_eq!(session.messages[1].ts.unwrap().timestamp_micros(), TIME + 5);
    }

    #[test]
    fn retained_frames_flatten_both_child_encodings_without_project_pollution() {
        let directory = tempfile::tempdir().unwrap();
        let child = intent(1, "intent-1", "inside the frame");
        let mut foreign = intent(20, "foreign", "foreign stream");
        foreign["stream"]["id"] = json!("child-session");
        let records = vec![
            frame(vec![
                child.clone(),
                foreign,
                record(
                    21,
                    "runtime.session.metadata",
                    json!({"record":{"workspace_root":"/wrong"}}),
                ),
            ]),
            record(
                2,
                "runtime.session.metadata",
                json!({"record":{"workspace_root":"/work"}}),
            ),
            child,
            json!({"retained_frame":"permission", "frame_schema_version":1,
                "children_json": json!([
                    {"record_json":"bad json"},
                    {"record_json":run(3, json!({"kind":"assistant_message_committed", "message_id":"answer", "text":"retained answer"})).to_string()},
                    {"record_json":null}
                ]).to_string()}),
        ];
        let path = write(directory.path(), "session-1/session.jsonl", &records);
        let session = Muse::default().parse(&path).unwrap();
        assert_eq!(session.project, "/work");
        assert_eq!(session.title, "inside the frame");
        assert_eq!(texts(&session, Role::User), ["inside the frame"]);
        assert_eq!(texts(&session, Role::Assistant), ["retained answer"]);
    }

    #[test]
    fn tool_batches_keep_structured_calls_results_paths_and_redact_all_text() {
        let directory = tempfile::tempdir().unwrap();
        let secret = "sk-abcdefghijklmnopqrstuvwxyz123456789012345678901234567890";
        let records = vec![
            intent(1, "intent", &format!("fix {secret}")),
            record(
                2,
                "session.name.changed",
                json!({"new_name":format!("title {secret}")}),
            ),
            run(
                3,
                json!({"kind":"assistant_tool_calls_committed", "message_id":"tools", "tool_calls":[
                    {"id":"provider-id", "call_id":"call-1", "name":"write_file", "args":json!({"path":"src/lib.rs", "content":secret}).to_string()},
                    {"call_id":"call-2", "name":"edit_file", "args":{"path":"src/lib.rs", "replacement":secret}},
                    {"call_id":"call-3", "name":"read_file", "args":{"path":"read-only.rs"}},
                    {"call_id":"call-4", "name":"bash", "args":"malformed arguments"}
                ]}),
            ),
            run(
                4,
                json!({"kind":"tool_result_batch_committed", "batch_id":"batch", "results":[
                    {"tool_call_id":"call-1", "tool_call_index":0, "text":format!("saved {secret}")},
                    {"tool_call_id":"call-2", "text":"failed", "is_error":true},
                    {"tool_call_id":"call-3", "text":"x".repeat(20_000)}
                ]}),
            ),
            run(
                5,
                json!({"kind":"assistant_message_committed", "message_id":"answer", "text":format!("done {secret}")}),
            ),
        ];
        let path = write(directory.path(), "session-1/session.jsonl", &records);
        let adapter = Muse::default();
        let raw = adapter.parse(&path).unwrap();
        assert_eq!(raw.touched, ["src/lib.rs"]);
        assert!(!serde_json::to_string(&raw).unwrap().contains(secret));
        let Some(ToolEvent::Part(ToolPart::Call(call))) = &raw.messages[1].tool else {
            panic!("structured call missing");
        };
        assert_eq!(call.id.as_deref(), Some("call-1"));
        assert_eq!(call.args["path"], "src/lib.rs");
        assert!(raw
            .messages
            .iter()
            .filter_map(|message| match &message.tool {
                Some(ToolEvent::Part(ToolPart::Result(result))) => Some(result),
                _ => None,
            })
            .any(|result| result.is_error));
        let folded = super::super::parse_session(&adapter, &path).unwrap();
        let summaries: Vec<_> = folded
            .messages
            .iter()
            .filter_map(|message| match &message.tool {
                Some(ToolEvent::Summary(summary)) => Some(summary),
                _ => None,
            })
            .collect();
        assert_eq!(summaries.len(), 4);
        assert_eq!(summaries[0].name, "write_file");
        assert!(summaries[0].output.as_ref().unwrap().starts_with("saved "));
        assert_eq!(summaries[1].is_error, Some(true));
        assert!(summaries[2].output.as_ref().unwrap().len() <= 8192);
    }

    #[test]
    fn malformed_unknown_records_and_truncated_tail_preserve_valid_turns() {
        let directory = tempfile::tempdir().unwrap();
        let mut unknown = intent(1, "unknown", "unsupported envelope");
        unknown["schema_version"] = json!(99);
        let mut future = intent(2, "future", "unsupported payload");
        future["payload_schema_version"] = json!(99);
        let records = vec![
            Value::Null,
            json!("scalar"),
            json!([]),
            json!({"payload":{}}),
            unknown,
            future,
            record(3, "future.payload", json!({"text":"unknown"})),
            record(
                4,
                "runtime.user_intent.accepted",
                json!({"intent_id":"intent"}),
            ),
            intent(5, "intent", "valid user"),
            run(
                6,
                json!({"kind":"assistant_message_committed", "message_id":"answer", "text":false}),
            ),
            run(
                7,
                json!({"kind":"assistant_message_committed", "message_id":"answer", "text":"valid answer"}),
            ),
            json!({"retained_frame":"broken", "frame_schema_version":1, "children":42}),
            json!({"retained_frame":"future", "frame_schema_version":99, "children":[]}),
        ];
        let path = write(directory.path(), "session-1/session.jsonl", &records);
        use std::io::Write;
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "not json").unwrap();
        write!(file, "{{\"schema_version\":1,").unwrap();
        let session = Muse::default().parse(&path).unwrap();
        assert_eq!(texts(&session, Role::User), ["valid user"]);
        assert_eq!(texts(&session, Role::Assistant), ["valid answer"]);
        assert_eq!(session.title, "valid user");
    }

    #[test]
    fn intent_fallbacks_and_idless_repeated_prompts_preserve_distinct_turns() {
        let directory = tempfile::tempdir().unwrap();
        let records = vec![
            record(
                1,
                "runtime.user_intent.accepted",
                json!({"refill_blocks":[{"type":"image","text":"ignore"},{"type":"text","text":"refill"}]}),
            ),
            record(
                2,
                "runtime.user_intent.accepted",
                json!({"prompt":"continue"}),
            ),
            record(
                3,
                "runtime.user_intent.accepted",
                json!({"text":"continue"}),
            ),
            record(
                4,
                "runtime.user_intent.accepted",
                json!({"model_messages":[{"content":[{"kind":"text","text":"first"},{"type":"text","text":"second"}]}],"prompt":"ignore fallback"}),
            ),
        ];
        let path = write(directory.path(), "session-1/session.jsonl", &records);
        assert_eq!(
            texts(&Muse::default().parse(&path).unwrap(), Role::User),
            ["refill", "continue", "continue", "first\nsecond"]
        );
    }

    #[test]
    fn retained_frame_depth_and_decode_budgets_are_bounded() {
        let child = intent(1, "intent", "bounded");
        let mut nested = child.clone();
        for _ in 0..10 {
            nested = frame(vec![nested]);
        }
        let mut emitted = 0;
        flatten(
            &nested,
            false,
            0,
            &mut FrameBudget {
                bytes: 1_000_000,
                records: 100,
            },
            &mut |_, _| emitted += 1,
        );
        assert_eq!(emitted, 0);
        flatten(
            &frame(vec![child.clone()]),
            false,
            0,
            &mut FrameBudget {
                bytes: 1,
                records: 100,
            },
            &mut |_, _| emitted += 1,
        );
        assert_eq!(emitted, 0);
        flatten(
            &frame(vec![child.clone(), child]),
            false,
            0,
            &mut FrameBudget {
                bytes: 1_000_000,
                records: 2,
            },
            &mut |_, _| emitted += 1,
        );
        assert_eq!(emitted, 1);
    }

    #[test]
    fn large_sessions_use_bounded_util_windows() {
        let directory = tempfile::tempdir().unwrap();
        let mut records = vec![intent(1, "start", "first prompt")];
        for sequence in 2..20 {
            records.push(run(
                sequence,
                json!({"kind":"padding", "text":"x".repeat(1000)}),
            ));
        }
        records.push(intent(20, "end", "latest prompt"));
        let path = write(directory.path(), "session-1/session.jsonl", &records);
        crate::util::TEST_WINDOW.with(|window| window.set(Some((1024, 1024, 1024))));
        let parsed = Muse::default().parse(&path);
        crate::util::TEST_WINDOW.with(|window| window.set(None));
        assert_eq!(
            texts(&parsed.unwrap(), Role::User),
            ["first prompt", "latest prompt"]
        );
    }
}
