//! Native DeepSeek Harness CLI logs. A session owns a directory; retained
//! immutable generations inside it are snapshots, never separate sessions.

use super::super::{
    bounded_redacted_output, clean_path, dedup_paths, ok_or_flag, redacted_truncate,
    title_from_messages, Store,
};
use crate::model::{EditEvent, EditKind, Message, Role, Session, ToolCall, ToolResult};
use crate::util::{session_lines, short_id};
use anyhow::{bail, ensure, Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const CURRENT_VERSION: u64 = 2;

struct Generation {
    path: PathBuf,
    version: u64,
}

/// Enumerate <home>/sessions/<project>/<encoded-id>/session[.vN].jsonl[.zstd].
/// Listing never reads transcript bodies. Any partial walk disables deletion
/// reconciliation, including a directory replaced by a symlink.
pub(super) fn sources(root: &Path) -> Store {
    let mut store = Store {
        keys: Vec::new(),
        files: Vec::new(),
        had_error: false,
    };
    match fs::symlink_metadata(root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return store,
        Ok(m) if m.is_dir() => {}
        _ => {
            store.had_error = true;
            return store;
        }
    }
    for project in directories(root, &mut store.had_error) {
        for dir in directories(&project, &mut store.had_error) {
            let selected = match select_generation(&dir) {
                Ok(Some(selected)) => selected,
                Ok(None) => continue,
                Err(_) => {
                    store.had_error = true;
                    continue;
                }
            };
            let Some(meta) = ok_or_flag(regular_file(&selected.path), &mut store.had_error) else {
                continue;
            };
            let Some(token) = ok_or_flag(change_token(&selected.path, &meta), &mut store.had_error)
            else {
                continue;
            };
            store.keys.push((dir.to_string_lossy().into_owned(), token));
            store.files.push(selected.path);
        }
    }
    store.keys.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    store.files.sort_unstable();
    store
}

fn directories(path: &Path, had_error: &mut bool) -> Vec<PathBuf> {
    let Some(entries) = ok_or_flag(fs::read_dir(path), had_error) else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let Some(entry) = ok_or_flag(entry, had_error) else {
            continue;
        };
        let Some(kind) = ok_or_flag(entry.file_type(), had_error) else {
            continue;
        };
        if kind.is_symlink() {
            *had_error = true;
        } else if kind.is_dir() {
            dirs.push(entry.path());
        }
    }
    dirs
}

/// Recognize only committed canonical names, matching dsh-session-format's
/// filename.ts. .zst is also accepted for logs archived with the usual suffix.
fn generation_version(name: &str) -> Option<u64> {
    let name = name
        .strip_suffix(".zstd")
        .or_else(|| name.strip_suffix(".zst"))
        .unwrap_or(name);
    if name == "session.jsonl" {
        return Some(0);
    }
    let digits = name.strip_prefix("session.v")?.strip_suffix(".jsonl")?;
    if digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let version = digits.parse::<u64>().ok()?;
    (version > 0 && version <= 9_007_199_254_740_991).then_some(version)
}

fn select_generation(dir: &Path) -> Result<Option<Generation>> {
    ensure!(
        fs::symlink_metadata(dir)?.is_dir(),
        "native session is not a directory"
    );
    let mut selected: Option<Generation> = None;
    let mut ambiguous = false;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let Some(version) = entry.file_name().to_str().and_then(generation_version) else {
            continue;
        };
        match selected.as_ref().map(|s| s.version) {
            Some(previous) if previous > version => {}
            Some(previous) if previous == version => ambiguous = true,
            _ => {
                selected = Some(Generation {
                    path: entry.path(),
                    version,
                });
                ambiguous = false;
            }
        }
    }
    ensure!(
        !ambiguous,
        "native session has multiple encodings of its newest generation"
    );
    if let Some(selected) = &selected {
        regular_file(&selected.path)?;
    }
    Ok(selected)
}

fn regular_file(path: &Path) -> Result<Metadata> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(meta.is_file(), "native generation is not a regular file");
    Ok(meta)
}

/// A metadata fingerprint notices migrations even when generation files have
/// identical sizes and mtimes. The high bit keeps this opaque token positive
/// and outside chrono's timestamp range (scan must not invent an activity date).
fn change_token(path: &Path, meta: &Metadata) -> Result<i64> {
    let modified = meta.modified()?.duration_since(UNIX_EPOCH)?;
    let mut hash = 0xcbf29ce484222325u64;
    for b in path
        .to_string_lossy()
        .bytes()
        .chain(meta.len().to_le_bytes())
        .chain(modified.as_secs().to_le_bytes())
        .chain(modified.subsec_nanos().to_le_bytes())
    {
        hash = (hash ^ u64::from(b)).wrapping_mul(0x100000001b3);
    }
    Ok(((hash & ((1 << 62) - 1)) | (1 << 62)) as i64)
}

pub(super) fn parse(key: &str) -> Result<Session> {
    let dir = Path::new(key);
    // Do not follow a project/session symlink, including when a formerly
    // discovered directory was replaced between enumeration and parsing.
    for path in dir.ancestors().take(3) {
        ensure!(
            fs::symlink_metadata(path)?.is_dir(),
            "native session path is not a directory"
        );
    }
    let selected = select_generation(dir)?.context("native session has no committed generation")?;
    ensure!(
        selected.version <= CURRENT_VERSION,
        "unsupported native session generation"
    );
    let before = change_token(&selected.path, &regular_file(&selected.path)?)?;
    let (lines, windowed) = session_lines(&selected.path)?;
    let mut rows = lines.iter().filter(|line| !line.trim().is_empty());
    let header: Value = serde_json::from_str(rows.next().context("native session has no header")?)
        .context("invalid native session header")?;
    ensure!(
        header["type"] == "session"
            && header["version"].as_u64() == Some(selected.version)
            && header["id"].as_str().is_some_and(|id| !id.is_empty())
            && timestamp(&header["createdAt"]).is_some()
            && header["delegationDepth"].as_u64().is_some()
            && header.get("origin").is_none_or(|v| v == "subagent")
            && ["parentSession", "agentPreset"]
                .iter()
                .all(|field| { header.get(field).is_none_or(Value::is_string) })
            && (selected.version < 2 || header["isSeeded"].is_boolean()),
        "invalid native session header"
    );
    let id = header["id"].as_str().unwrap_or_default();
    ensure!(
        dir.file_name().and_then(|n| n.to_str()) == Some(encode_segment(id).as_str()),
        "native session header id does not match its directory"
    );
    let project = match header.get("cwd") {
        Some(Value::String(cwd)) => crate::redact::redact(cwd).into_owned(),
        None => String::new(),
        _ => bail!("invalid native session cwd"),
    };
    let started = timestamp(&header["createdAt"]);
    let mut ended = started;
    let subagent = header["origin"] == "subagent"
        || header["delegationDepth"]
            .as_u64()
            .is_some_and(|depth| depth > 0);
    let mut messages = Vec::new();
    let mut title = None;
    let mut human_title = None;
    let mut seen_sequences = HashMap::new();
    let mut calls = HashSet::new();
    let mut results = HashSet::new();
    let mut failed = HashSet::new();
    let mut edits = Vec::new();

    for line in rows {
        // Refuse a damaged selected generation instead of replacing the old
        // index with an empty or stale historical transcript.
        let row: Value = serde_json::from_str(line).context("invalid native session event")?;
        let Some(kind) = row["type"].as_str() else {
            bail!("native session event has no type");
        };
        if let Some(seq) = row["seq"].as_u64() {
            match seen_sequences.insert(seq, line.as_str()) {
                Some(previous) if previous != line => bail!("conflicting native event sequence"),
                Some(_) => continue,
                None => {}
            }
        }
        let ts = timestamp(&row["time"]);
        if let Some(ts) = ts {
            ended = Some(ended.map_or(ts, |old| old.max(ts)));
        }
        let data = &row["data"];
        match kind {
            "user/message" => {
                if human_title.is_none() && data["source"]["kind"] == "user" {
                    let text = text_content(&data["content"]);
                    if !text.trim().is_empty() {
                        human_title = Some(redacted_truncate(&text, 80));
                    }
                }
                push_text(&mut messages, Role::User, &data["content"], ts);
            }
            "assistant/message" => {
                // Stream deltas and embedded tool-call blocks duplicate the
                // assembled message and authoritative tool/call events.
                push_text(
                    &mut messages,
                    Role::Assistant,
                    &data["message"]["content"],
                    ts,
                );
            }
            "tool/call" | "tool/code-dispatch-start" => {
                let id_field = if kind == "tool/call" {
                    "callId"
                } else {
                    "subCallId"
                };
                add_call(data, id_field, ts, &mut calls, &mut messages, &mut edits);
            }
            "tool/code-dispatch" => {
                // Older logs may only contain the settlement (no start event).
                add_call(data, "subCallId", ts, &mut calls, &mut messages, &mut edits);
                add_result(
                    data["subCallId"].as_str(),
                    &data["content"],
                    data["isError"].as_bool().unwrap_or(false),
                    ts,
                    &mut results,
                    &mut failed,
                    &mut messages,
                );
            }
            "tool/result" => {
                if let Some(blocks) = data["message"]["content"].as_array() {
                    for block in blocks.iter().filter(|b| b["type"] == "tool-result") {
                        add_result(
                            block["toolCallId"]
                                .as_str()
                                .or_else(|| data["message"]["source"]["callId"].as_str()),
                            &block["content"],
                            block["isError"].as_bool().unwrap_or(false)
                                || data.get("error").is_some_and(|v| !v.is_null()),
                            ts,
                            &mut results,
                            &mut failed,
                            &mut messages,
                        );
                    }
                }
            }
            "session/title" => {
                if let Some(text) = data["title"].as_str().filter(|s| !s.trim().is_empty()) {
                    title = Some(redacted_truncate(text, 80));
                }
            }
            // Packed v0/v1 assistant chunks and v2 streams are intentionally
            // omitted: the assembled assistant/message is the conversation.
            _ => {}
        }
    }

    let after = select_generation(dir)?.context("native generation disappeared while reading")?;
    ensure!(
        after.path == selected.path
            && change_token(&after.path, &regular_file(&after.path)?)? == before,
        "native session changed while reading; retry on the next sync"
    );
    let edits: Vec<_> = edits
        .into_iter()
        // An omitted middle result cannot establish a pending edit's success.
        .filter(|(id, _)| !failed.contains(id) && (!windowed || results.contains(id)))
        .map(|(_, e)| e)
        .collect();
    let touched = dedup_paths(edits.iter().map(|edit| edit.path.clone()).collect());
    let title = title
        .or(human_title)
        .unwrap_or_else(|| title_from_messages(&messages));
    Ok(Session {
        id: short_id(key),
        tool: "dsh",
        path: dir.to_path_buf(),
        project,
        started,
        ended,
        title: if windowed {
            format!("[large] {title}")
        } else {
            title
        },
        subagent,
        messages,
        touched,
        edits,
    })
}

fn timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .as_i64()
        .filter(|ms| *ms >= 0)
        .and_then(DateTime::from_timestamp_millis)
}

fn text_content(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| matches!(block["type"].as_str(), Some("text" | "reasoning")))
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn push_text(messages: &mut Vec<Message>, role: Role, content: &Value, ts: Option<DateTime<Utc>>) {
    let text = text_content(content);
    if !text.trim().is_empty() {
        messages.push(Message {
            role,
            text: crate::redact::redact(&text).into_owned(),
            ts,
            tool: None,
        });
    }
}

fn add_call(
    data: &Value,
    id_field: &str,
    ts: Option<DateTime<Utc>>,
    calls: &mut HashSet<String>,
    messages: &mut Vec<Message>,
    edits: &mut Vec<(String, EditEvent)>,
) {
    let (Some(id), Some(name)) = (data[id_field].as_str(), data["name"].as_str()) else {
        return;
    };
    if !calls.insert(id.to_owned()) {
        return;
    }
    let mut args = match &data["arguments"] {
        Value::String(raw) => {
            serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.clone()))
        }
        value => value.clone(),
    };
    redact_values(&mut args);
    if let Some(edit) = edit_event(name, &args, ts) {
        edits.push((id.to_owned(), edit));
    }
    messages.push(Message::tool_call(
        ts,
        ToolCall {
            id: Some(id.to_owned()),
            name: crate::redact::redact(name).into_owned(),
            args,
        },
    ));
}

#[allow(clippy::too_many_arguments)]
fn add_result(
    id: Option<&str>,
    content: &Value,
    is_error: bool,
    ts: Option<DateTime<Utc>>,
    results: &mut HashSet<String>,
    failed: &mut HashSet<String>,
    messages: &mut Vec<Message>,
) {
    if let Some(id) = id {
        if is_error {
            failed.insert(id.to_owned());
        }
        if !results.insert(id.to_owned()) {
            return;
        }
    }
    messages.push(Message::tool_result(
        ts,
        ToolResult {
            call_id: id.map(str::to_owned),
            text: bounded_redacted_output(&text_content(content)),
            is_error,
        },
    ));
}

fn edit_event(name: &str, args: &Value, ts: Option<DateTime<Utc>>) -> Option<EditEvent> {
    let (kind, snippet) = match name {
        "write" => (
            EditKind::Write,
            args["content"].as_str().unwrap_or_default(),
        ),
        "edit" => (
            EditKind::Edit,
            args["new_string"].as_str().unwrap_or_default(),
        ),
        "str_replace_editor" => match args["command"].as_str()? {
            "create" => (
                EditKind::Write,
                args["file_text"].as_str().unwrap_or_default(),
            ),
            "str_replace" | "insert" => {
                (EditKind::Edit, args["new_str"].as_str().unwrap_or_default())
            }
            _ => return None,
        },
        _ => return None,
    };
    let path = clean_path(
        args["file_path"]
            .as_str()
            .or_else(|| args["path"].as_str())?,
    )?;
    Some(EditEvent {
        path,
        kind,
        snippet: redacted_truncate(snippet.trim(), 2000),
        ts,
    })
}

fn redact_values(value: &mut Value) {
    match value {
        Value::String(s) => *s = crate::redact::redact(s).into_owned(),
        Value::Array(values) => values.iter_mut().for_each(redact_values),
        Value::Object(values) => values.values_mut().for_each(redact_values),
        _ => {}
    }
}

/// Native ids use injective UTF-16 code-unit escaping, not URL encoding.
fn encode_segment(id: &str) -> String {
    if id == "." {
        return "~002E".into();
    }
    if id == ".." {
        return "~002E~002E".into();
    }
    let mut out = String::new();
    for unit in id.encode_utf16() {
        if unit <= 127 && ((unit as u8).is_ascii_alphanumeric() || b"._-".contains(&(unit as u8))) {
            out.push(char::from(unit as u8));
        } else {
            use std::fmt::Write;
            let _ = write!(out, "~{unit:04X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ToolEvent, ToolPart};
    use serde_json::json;

    fn fixture(id: &str) -> (tempfile::TempDir, PathBuf, Value) {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp
            .path()
            .join("sessions/--work--")
            .join(encode_segment(id));
        fs::create_dir_all(&dir).unwrap();
        let header = json!({
            "type": "session", "version": 2, "id": id, "cwd": "/work",
            "createdAt": 1_700_000_000_000i64, "isSeeded": false, "delegationDepth": 0,
        });
        (temp, dir, header)
    }

    fn write_rows(path: &Path, rows: &[Value]) {
        let text = rows
            .iter()
            .map(|row| format!("{row}\n"))
            .collect::<String>();
        fs::write(path, text).unwrap();
    }

    fn event(kind: &str, seq: u64, data: Value) -> Value {
        json!({"type": kind, "seq": seq, "time": 1_700_000_000_001i64 + seq as i64, "data": data})
    }

    fn user(seq: u64, text: &str) -> Value {
        event(
            "user/message",
            seq,
            json!({
                "role": "user", "id": format!("u{seq}"), "source": {"kind": "user"},
                "content": [{"type": "text", "text": text}],
            }),
        )
    }

    fn call(seq: u64, id: &str, name: &str, args: Value) -> Value {
        event(
            "tool/call",
            seq,
            json!({
                "turn": 1, "step": 1, "callId": id, "name": name, "arguments": args.to_string(),
            }),
        )
    }

    fn result(seq: u64, id: &str, text: &str, error: bool) -> Value {
        event(
            "tool/result",
            seq,
            json!({"turn": 1, "step": 1, "message": {
                "id": format!("r{seq}"), "role": "user", "source": {"kind": "tool", "callId": id},
                "content": [{"type": "tool-result", "toolCallId": id, "isError": error,
                    "content": [{"type": "text", "text": text}]}],
            }}),
        )
    }

    fn parsed(dir: &Path) -> Session {
        parse(dir.to_str().unwrap()).unwrap()
    }

    #[test]
    fn reads_native_messages_metadata_tools_and_applied_edits_once() {
        let (_temp, dir, mut header) = fixture("native");
        header["origin"] = json!("subagent");
        header["parentSession"] = json!("parent");
        let call_args = json!({"file_path": "/work/a.rs", "content": "let fixed = true;"});
        let rows = [
            header,
            user(0, "Fix the parser"),
            event(
                "assistant/chunk",
                1,
                json!({"chunk": {"type": "text-delta", "text": "duplicate"}}),
            ),
            event(
                "assistant/message",
                2,
                json!({"message": {"role": "assistant", "content": [
                {"type": "text", "text": "I will fix it."},
                {"type": "tool-call", "id": "c1", "name": "write", "arguments": call_args.to_string()},
            ]}, "stream": [{"type": "text-chunks", "texts": ["duplicate"]}]}),
            ),
            call(3, "c1", "write", call_args),
            result(4, "c1", "Created file", false),
            call(
                5,
                "c2",
                "edit",
                json!({"file_path": "/work/failed.rs", "new_string": "oops"}),
            ),
            result(6, "c2", "permission denied", true),
            event("session/title", 7, json!({"title": "First title"})),
            event("session/title", 8, json!({"title": "Final title"})),
            event(
                "turn/end",
                9,
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
        ];
        write_rows(&dir.join("session.v2.jsonl"), &rows);
        let mut session = parsed(&dir);
        assert_eq!(session.id, short_id(dir.to_str().unwrap()));
        assert_eq!(session.path, dir);
        assert_eq!(session.tool, "dsh");
        assert_eq!(session.project, "/work");
        assert_eq!(session.title, "Final title");
        assert!(session.subagent);
        assert_eq!(
            session.started.unwrap().timestamp_millis(),
            1_700_000_000_000
        );
        assert_eq!(session.ended.unwrap().timestamp_millis(), 1_700_000_000_010);
        assert_eq!(session.messages.len(), 6);
        assert_eq!(session.messages[1].text, "I will fix it.");
        assert_eq!(session.touched, ["/work/a.rs"]);
        assert_eq!(session.edits.len(), 1);
        assert_eq!(session.edits[0].kind, EditKind::Write);
        assert_eq!(session.edits[0].snippet, "let fixed = true;");
        crate::tool_summary::fold_tool_parts(&mut session);
        assert_eq!(session.messages.len(), 4);
        let Some(ToolEvent::Summary(summary)) = &session.messages[2].tool else {
            panic!("unpaired tool")
        };
        assert_eq!(summary.name, "write");
        assert_eq!(summary.output.as_deref(), Some("Created file"));
    }

    #[test]
    fn latest_generation_is_one_stable_directory_key_and_changes_token() {
        let (temp, dir, mut header) = fixture("migration");
        header["version"] = json!(0);
        header.as_object_mut().unwrap().remove("isSeeded");
        write_rows(
            &dir.join("session.jsonl"),
            &[header.clone(), user(0, "old snapshot")],
        );
        let first = sources(&temp.path().join("sessions"));
        let before = parsed(&dir);
        assert_eq!(first.keys.len(), 1);
        assert!(!first.had_error);
        assert_eq!(first.files, [dir.join("session.jsonl")]);
        header["version"] = json!(1);
        write_rows(
            &dir.join("session.v1.jsonl"),
            &[header.clone(), user(0, "current snapshot")],
        );
        let second = sources(&temp.path().join("sessions"));
        assert_eq!(first.keys[0].0, second.keys[0].0);
        assert_ne!(first.keys[0].1, second.keys[0].1);
        let session = parsed(&dir);
        assert_eq!(session.id, before.id);
        assert_eq!(session.title, "current snapshot");
        assert_eq!(session.messages.len(), 1);
        assert_eq!(second.files, [dir.join("session.v1.jsonl")]);
        assert!(DateTime::from_timestamp_millis(second.keys[0].1).is_none());
    }

    #[test]
    fn generation_and_size_changes_are_detected_even_with_identical_mtimes() {
        use std::fs::FileTimes;
        let (temp, dir, mut header) = fixture("mtime");
        header["version"] = json!(1);
        let old = dir.join("session.v1.jsonl");
        write_rows(&old, &[header.clone(), user(0, "same bytes")]);
        let time = fs::metadata(&old).unwrap().modified().unwrap();
        let first = sources(&temp.path().join("sessions"));
        header["version"] = json!(2);
        let new = dir.join("session.v2.jsonl");
        write_rows(&new, &[header, user(0, "same bytes")]);
        fs::File::options()
            .write(true)
            .open(&new)
            .unwrap()
            .set_times(FileTimes::new().set_modified(time))
            .unwrap();
        assert_eq!(
            fs::metadata(&old).unwrap().len(),
            fs::metadata(&new).unwrap().len()
        );
        let second = sources(&temp.path().join("sessions"));
        assert_ne!(first.keys[0].1, second.keys[0].1);
        let file = fs::File::options().append(true).open(&new).unwrap();
        use std::io::Write;
        (&file).write_all(b"\n").unwrap();
        file.set_times(FileTimes::new().set_modified(time)).unwrap();
        let third = sources(&temp.path().join("sessions"));
        assert_ne!(second.keys[0].1, third.keys[0].1);
    }

    #[test]
    fn fallback_title_prefers_the_human_prompt_over_injected_context() {
        let (_temp, dir, header) = fixture("title-context");
        let mut injected = user(0, "Synthetic environment snapshot");
        injected["data"]["source"] = json!({"kind": "plugin", "plugin": "environment"});
        write_rows(
            &dir.join("session.v2.jsonl"),
            &[header, injected, user(1, "My actual task")],
        );
        assert_eq!(parsed(&dir).title, "My actual task");
    }

    #[test]
    fn canonical_generation_names_match_the_native_persistence_tests() {
        for (name, version) in [
            ("session.jsonl", 0),
            ("session.v1.jsonl", 1),
            ("session.v27.jsonl.zstd", 27),
            ("session.v2.jsonl.zst", 2),
        ] {
            assert_eq!(generation_version(name), Some(version), "{name}");
        }
        for name in [
            "session.v0.jsonl",
            "session.v01.jsonl",
            "session.V2.jsonl",
            "session.v.jsonl",
            "session.v1.backup.jsonl",
            "session.v2.jsonl.123.tmp",
            "session.v9007199254740992.jsonl",
            "v2.jsonl",
            "metadata.json",
            "session.lock",
        ] {
            assert_eq!(generation_version(name), None, "{name}");
        }
    }

    #[test]
    fn newest_future_corrupt_and_ambiguous_generations_never_fall_back() {
        let (temp, dir, mut header) = fixture("bad-newest");
        header["version"] = json!(1);
        write_rows(
            &dir.join("session.v1.jsonl"),
            &[header.clone(), user(0, "stale")],
        );
        header["version"] = json!(3);
        write_rows(
            &dir.join("session.v3.jsonl"),
            &[header.clone(), user(0, "future")],
        );
        assert!(parse(dir.to_str().unwrap())
            .unwrap_err()
            .to_string()
            .contains("unsupported"));
        assert_eq!(sources(&temp.path().join("sessions")).keys.len(), 1);
        fs::remove_file(dir.join("session.v3.jsonl")).unwrap();
        fs::write(dir.join("session.v2.jsonl"), "corrupt\n").unwrap();
        assert!(parse(dir.to_str().unwrap()).is_err());
        header["version"] = json!(1); // Filename/header disagreement also refuses.
        write_rows(&dir.join("session.v2.jsonl"), &[header.clone()]);
        assert!(parse(dir.to_str().unwrap()).is_err());
        header["version"] = json!(2);
        write_rows(&dir.join("session.v2.jsonl"), &[header, user(0, "latest")]);
        fs::write(dir.join("session.v2.jsonl.zstd"), "alternate").unwrap();
        assert!(parse(dir.to_str().unwrap()).is_err());
        let store = sources(&temp.path().join("sessions"));
        assert!(store.had_error);
        assert!(store.keys.is_empty());
    }

    #[test]
    fn compressed_native_header_and_append_frames_are_all_read() {
        use ruzstd::encoding::{compress_to_vec, CompressionLevel};
        let (_temp, dir, header) = fixture("compressed");
        for suffix in ["zstd", "zst"] {
            let path = dir.join(format!("session.v2.jsonl.{suffix}"));
            let rows = [
                header.clone(),
                user(0, "compressed prompt"),
                event(
                    "assistant/message",
                    1,
                    json!({"message": {"content": [
                        {"type": "text", "text": "appended answer"},
                    ]}}),
                ),
            ];
            let bytes: Vec<_> = rows
                .iter()
                .flat_map(|row| {
                    compress_to_vec(format!("{row}\n").as_bytes(), CompressionLevel::Fastest)
                })
                .collect();
            fs::write(&path, &bytes).unwrap();
            let session = parsed(&dir);
            assert_eq!(session.messages.len(), 2);
            assert_eq!(session.messages[1].text, "appended answer");
            let mut torn = bytes;
            torn.extend_from_slice(&[0x28, 0xb5]);
            fs::write(&path, torn).unwrap();
            assert!(parse(dir.to_str().unwrap()).is_err());
            fs::remove_file(&path).unwrap();
        }
    }

    #[test]
    fn nested_dispatches_extract_edits_and_pair_results_without_duplicate_calls() {
        let (_temp, dir, header) = fixture("nested");
        let args = json!({"file_path": "/work/a.rs", "new_string": "new value"});
        write_rows(
            &dir.join("session.v2.jsonl"),
            &[
                header,
                event(
                    "tool/code-dispatch-start",
                    0,
                    json!({"subCallId": "nested:0", "name": "edit", "arguments": args}),
                ),
                event(
                    "tool/code-dispatch",
                    1,
                    json!({"subCallId": "nested:0", "name": "edit",
                "arguments": args, "isError": false, "content": [{"type": "text", "text": "updated"}]}),
                ),
                // Settlement-only historical nested dispatches are still useful.
                event(
                    "tool/code-dispatch",
                    2,
                    json!({"subCallId": "nested:1", "name": "write",
                "arguments": {"file_path": "/work/b.rs", "content": "created"},
                "isError": false, "content": [{"type": "text", "text": "created"}]}),
                ),
            ],
        );
        let mut session = parsed(&dir);
        assert_eq!(session.messages.len(), 4);
        assert_eq!(session.touched, ["/work/a.rs", "/work/b.rs"]);
        crate::tool_summary::fold_tool_parts(&mut session);
        assert_eq!(session.messages.len(), 2);
    }

    #[test]
    fn values_are_redacted_before_title_and_output_bounds() {
        let (_temp, dir, header) = fixture("redaction");
        let secret = format!("sk-{}", "A".repeat(140));
        write_rows(
            &dir.join("session.v2.jsonl"),
            &[
                header,
                user(0, &format!("Token {secret}")),
                call(
                    1,
                    "c",
                    "write",
                    json!({"file_path": "/work/a", "content": secret,
                "nested": {"list": [secret]}}),
                ),
                result(2, "c", &format!("{secret} {}", "x".repeat(12000)), false),
                event(
                    "session/title",
                    3,
                    json!({"title": format!("Title {secret}")}),
                ),
            ],
        );
        let session = parsed(&dir);
        let serialized = serde_json::to_string(&session).unwrap();
        assert!(!serialized.contains(&secret));
        assert!(serialized.contains("[redacted:"));
        let Some(ToolEvent::Part(ToolPart::Result(result))) = &session.messages[2].tool else {
            panic!("missing result")
        };
        assert!(result.text.len() <= 8192);
        assert!(!session.title.contains("sk-AAAA"));
    }

    #[test]
    fn fork_lineage_is_not_itself_subagent_metadata_and_encoded_ids_are_supported() {
        let (_temp, dir, mut header) = fixture("child/한😀~");
        header["parentSession"] = json!("fork-parent");
        header["isSeeded"] = json!(true);
        write_rows(
            &dir.join("session.v2.jsonl"),
            &[header.clone(), user(0, "fork prompt")],
        );
        assert!(!parsed(&dir).subagent);
        header["delegationDepth"] = json!(1);
        write_rows(&dir.join("session.v2.jsonl"), &[header.clone()]);
        assert!(parsed(&dir).subagent);
        header["id"] = json!("wrong-directory");
        write_rows(&dir.join("session.v2.jsonl"), &[header]);
        assert!(parse(dir.to_str().unwrap()).is_err());
    }

    #[test]
    fn listing_is_metadata_only_and_absent_or_unreadable_roots_are_distinguished() {
        let (temp, dir, _) = fixture("listing");
        // Discovery must not parse headers or read the session's large body.
        let file = fs::File::create(dir.join("session.v2.jsonl")).unwrap();
        file.set_len(crate::util::MAX_SESSION_FILE_BYTES + 1)
            .unwrap();
        let store = sources(&temp.path().join("sessions"));
        assert_eq!(store.keys.len(), 1);
        assert!(!store.had_error);
        assert!(!sources(&temp.path().join("missing")).had_error);
        assert!(sources(&dir.join("session.v2.jsonl")).had_error);
    }

    #[test]
    fn windowed_reads_keep_metadata_head_and_recent_tail() {
        let (_temp, dir, header) = fixture("windowed");
        let mut rows = vec![header, user(0, "first prompt")];
        rows.extend((1..100).map(|n| user(n, &format!("middle {} {n}", "x".repeat(50)))));
        rows.push(user(100, "last prompt"));
        write_rows(&dir.join("session.v2.jsonl"), &rows);
        crate::util::TEST_WINDOW.with(|w| w.set(Some((500, 700, 700))));
        let session = parse(dir.to_str().unwrap());
        crate::util::TEST_WINDOW.with(|w| w.set(None));
        let session = session.unwrap();
        assert!(session.title.starts_with("[large] first prompt"));
        assert_eq!(session.messages.first().unwrap().text, "first prompt");
        assert_eq!(session.messages.last().unwrap().text, "last prompt");
        assert!(session.messages.len() < 10);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_projects_sessions_and_generations_never_escape_the_store() {
        use std::os::unix::fs::symlink;
        let (temp, dir, header) = fixture("safe");
        let outside = tempfile::tempdir().unwrap();
        let log = outside.path().join("session.v2.jsonl");
        write_rows(&log, &[header]);
        symlink(&log, dir.join("session.v2.jsonl")).unwrap();
        let root = temp.path().join("sessions");
        assert!(sources(&root).had_error);
        assert!(sources(&root).keys.is_empty());
        assert!(parse(dir.to_str().unwrap()).is_err());
        fs::remove_file(dir.join("session.v2.jsonl")).unwrap();
        symlink(
            outside.path(),
            dir.parent().unwrap().join("escaped-session"),
        )
        .unwrap();
        symlink(outside.path(), root.join("escaped-project")).unwrap();
        assert!(sources(&root).had_error);
        assert!(sources(&root).keys.is_empty());
        assert!(parse(
            dir.parent()
                .unwrap()
                .join("escaped-session")
                .to_str()
                .unwrap()
        )
        .is_err());
        let root_link = temp.path().join("root-link");
        symlink(&root, &root_link).unwrap();
        assert!(sources(&root_link).had_error);
    }

    #[cfg(unix)]
    #[test]
    fn permission_denied_partial_listing_is_reported() {
        use std::os::unix::fs::PermissionsExt;
        let (temp, dir, header) = fixture("readable");
        write_rows(&dir.join("session.v2.jsonl"), &[header]);
        let denied = temp.path().join("sessions/denied");
        fs::create_dir(&denied).unwrap();
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).unwrap();
        let actually_denied = fs::read_dir(&denied).is_err();
        let store = sources(&temp.path().join("sessions"));
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(store.keys.len(), 1);
        // Root can list a mode-000 directory; exercise this on ordinary users.
        if actually_denied {
            assert!(store.had_error);
        }
    }
}
