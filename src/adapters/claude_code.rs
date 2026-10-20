use super::{
    bounded_redacted_output, clean_path, dedup_paths, ok_or_flag, parse_ts, redacted_truncate,
    title_from_messages, Adapter, Discovered,
};
use crate::model::{Message, Role, Session, ToolCall, ToolResult};
use crate::util::short_id;
use anyhow::Result;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Claude Code stores one JSONL file per session under
/// `~/.claude/projects/<sanitized-cwd>/<session-uuid>.jsonl`.
/// Each line is an event: user/assistant messages, tool results,
/// summaries, and harness bookkeeping.
///
/// `ClaudeCode::default()` reads the stock `~/.claude` install. An embedder
/// that runs several installs in different homes builds one adapter per
/// install with [`ClaudeCode::in_home`].
#[derive(Default)]
pub struct ClaudeCode {
    /// Explicit projects directory, or `None` for the stock location.
    root: Option<PathBuf>,
}

impl ClaudeCode {
    /// An adapter for the Claude Code install rooted at `home` (e.g.
    /// `~/.claude4`). The projects sub-directory layout is the adapter's
    /// business, not the caller's.
    pub fn in_home(home: impl Into<PathBuf>) -> Self {
        ClaudeCode {
            root: Some(home.into().join("projects")),
        }
    }
}

/// Under `subagents/` only `agent-*.jsonl` is a transcript. A workflow keeps
/// its bookkeeping there too (`workflows/wf_*/journal.jsonl`: which agent
/// started and what it returned), which is not a conversation.
fn is_transcript(path: &Path) -> bool {
    !path.components().any(|c| c.as_os_str() == "subagents")
        || path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("agent-"))
}

impl Adapter for ClaudeCode {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn root(&self) -> Option<PathBuf> {
        match &self.root {
            Some(root) => Some(root.clone()),
            None => Some(dirs::home_dir()?.join(".claude").join("projects")),
        }
    }

    /// Every install speaks only for the rows under the root it scans.
    fn reconcile_scope(&self) -> Option<String> {
        super::root_scope(self.root().as_deref())
    }

    fn discover(&self) -> Discovered {
        // Main sessions live at <project>/<uuid>.jsonl; subagent transcripts
        // at <project>/<uuid>/subagents/agent-*.jsonl and nest further when
        // subagents spawn subagents, so no depth limit here.
        let Some(root) = self.root() else {
            return Vec::new().into();
        };
        if !root.exists() {
            return Vec::new().into(); // no store on this machine - normal
        }
        let mut had_error = false;
        let files = WalkDir::new(root)
            .into_iter()
            .filter_map(|e| ok_or_flag(e, &mut had_error))
            .filter(|e| e.file_type().is_file())
            .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
            .filter(|e| is_transcript(e.path()))
            .map(|e| e.into_path())
            .collect();
        Discovered { files, had_error }
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        // A session over the byte cap is WINDOWED (head+tail) rather than
        // dropped, so the biggest sessions - the ones most worth referencing -
        // stay searchable and partially readable.
        // Past the cap, the conversation is still read from the middle; only
        // its tool output is windowed.
        let (lines, windowed) = crate::util::session_lines_keeping(path, |line| {
            (line.contains("\"type\":\"user\"") && !line.contains("\"tool_result\""))
                || (line.contains("\"type\":\"assistant\"") && line.contains("\"type\":\"text\""))
        })?;

        let mut messages: Vec<Message> = Vec::new();
        let mut touched: Vec<String> = Vec::new();
        let mut edits: Vec<crate::model::EditEvent> = Vec::new();
        let mut cwd: Option<String> = None;
        let mut summary: Option<String> = None;
        let mut ai_title: Option<String> = None;
        let mut started = None;
        let mut ended = None;

        for line in &lines {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };

            if cwd.is_none() {
                if let Some(c) = v.get("cwd").and_then(Value::as_str) {
                    cwd = Some(c.to_string());
                }
            }
            if let Some(ts) = v
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_ts)
            {
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
                Some("ai-title") => {
                    if ai_title.is_none() {
                        ai_title = v.get("aiTitle").and_then(Value::as_str).map(String::from);
                    }
                }
                Some("user") => {
                    // Skip harness meta lines; keep real prompts and tool results.
                    if v.get("isMeta").and_then(Value::as_bool) == Some(true) {
                        continue;
                    }
                    let ts = v
                        .get("timestamp")
                        .and_then(Value::as_str)
                        .and_then(parse_ts);
                    let Some(content) = v.pointer("/message/content") else {
                        continue;
                    };
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
                                        let output =
                                            bounded_redacted_output(&block_text(b.get("content")));
                                        messages.push(Message::tool_result(
                                            ts,
                                            ToolResult {
                                                call_id: b
                                                    .get("tool_use_id")
                                                    .and_then(Value::as_str)
                                                    .filter(|id| !id.is_empty())
                                                    .map(str::to_owned),
                                                text: output,
                                                is_error: b
                                                    .get("is_error")
                                                    .and_then(Value::as_bool)
                                                    .unwrap_or(false),
                                            },
                                        ));
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Some("assistant") => {
                    let ts = v
                        .get("timestamp")
                        .and_then(Value::as_str)
                        .and_then(parse_ts);
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
                                if let Some(ev) = edit_event(name, b.get("input"), ts) {
                                    touched.push(ev.path.clone());
                                    edits.push(ev);
                                }
                                let mut args = b.get("input").cloned().unwrap_or_else(|| json!({}));
                                redact_json_strings(&mut args);
                                messages.push(Message::tool_call(
                                    ts,
                                    ToolCall {
                                        id: b
                                            .get("id")
                                            .and_then(Value::as_str)
                                            .filter(|id| !id.is_empty())
                                            .map(str::to_owned),
                                        name: name.to_owned(),
                                        args,
                                    },
                                ));
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        // The window drops the middle of the file, and with it the files the
        // session edited there. Those are read from the whole file: only lines
        // that carry a tool call are parsed.
        if windowed {
            let mut all_touched = Vec::new();
            let mut all_edits = Vec::new();
            let read = crate::util::for_each_line(path, |line| {
                if !line.contains("\"tool_use\"") {
                    return;
                }
                let Ok(v) = serde_json::from_str::<Value>(line) else {
                    return;
                };
                if v.get("type").and_then(Value::as_str) != Some("assistant") {
                    return;
                }
                let ts = v
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(parse_ts);
                let Some(Value::Array(blocks)) = v.pointer("/message/content") else {
                    return;
                };
                for b in blocks {
                    if b.get("type").and_then(Value::as_str) != Some("tool_use") {
                        continue;
                    }
                    let name = b.get("name").and_then(Value::as_str).unwrap_or("?");
                    if let Some(ev) = edit_event(name, b.get("input"), ts) {
                        all_touched.push(ev.path.clone());
                        all_edits.push(ev);
                    }
                }
            });
            if read.is_ok() {
                touched = all_touched;
                edits = all_edits;
            }
        }

        let project = cwd.unwrap_or_else(|| {
            // Fall back to the sanitized directory name.
            path.parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        let base_title = summary
            .or(ai_title)
            .map(|s| redacted_truncate(&s, 80))
            .unwrap_or_else(|| title_from_messages(&messages));
        // Flag a windowed session so search/list/window make the partial read obvious.
        let title = if windowed {
            format!("[large] {base_title}")
        } else {
            base_title
        };
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
            touched: dedup_paths(touched),
            edits,
        })
    }
}

/// Pull the file a Claude Code edit tool acted on from its `input`. Only the
/// tools that write to disk count; reads, searches and shell commands do not
/// establish authorship. The field name varies by tool (`file_path`,
/// `notebook_path`, or the generic `path`).
fn edited_path(name: &str, input: Option<&Value>) -> Option<String> {
    let writes = matches!(
        name,
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" | "str_replace_based_edit_tool"
    );
    if !writes {
        return None;
    }
    let input = input?;
    for key in ["file_path", "notebook_path", "path"] {
        if let Some(p) = input.get(key).and_then(Value::as_str) {
            if !p.is_empty() {
                return Some(p.to_string());
            }
        }
    }
    None
}

const SNIPPET_CAP: usize = 200;

/// Extract the structured edit evidence from one write tool call - the richer
/// sibling of `edited_path`: not just WHICH file, but what kind of change and a
/// bounded snippet of it. Returns None for non-write tools.
fn edit_event(
    name: &str,
    input: Option<&Value>,
    ts: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<crate::model::EditEvent> {
    use crate::model::{EditEvent, EditKind};
    // clean_path applies the SAME hygiene dedup_paths gives `touched`, so an
    // edits row never survives for a path touched would have dropped.
    let path = clean_path(&edited_path(name, input)?)?;
    let input = input?;
    // Every name edited_path() accepts must map here, so `edits` and `touched`
    // never diverge (a touched path with no evidence, or vice versa).
    let kind = match name {
        "Edit" | "str_replace_based_edit_tool" => EditKind::Edit,
        "Write" => EditKind::Write,
        "MultiEdit" => EditKind::MultiEdit,
        "NotebookEdit" => EditKind::NotebookEdit,
        _ => return None,
    };
    let raw = match kind {
        // str_replace_based_edit_tool names the field new_str / file_text.
        EditKind::Edit => str_field(input, &["new_string", "new_str", "file_text"]),
        EditKind::Write => str_field(input, &["content"]),
        EditKind::MultiEdit => input
            .get("edits")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|e| e.get("new_string").and_then(Value::as_str))
            .unwrap_or(""),
        EditKind::NotebookEdit => str_field(input, &["new_source"]),
    };
    let snippet = redacted_truncate(raw.trim(), SNIPPET_CAP);
    Some(EditEvent {
        path,
        kind,
        snippet,
        ts,
    })
}

/// First present string among `keys`, or "".
fn str_field<'a>(v: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter()
        .find_map(|k| v.get(k).and_then(Value::as_str))
        .unwrap_or("")
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
            text: text.to_string(),
            ts,
            tool: None,
        });
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
            .join("\n"),
        _ => String::new(),
    }
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
    use crate::model::EditKind;
    use serde_json::json;

    #[test]
    fn edit_event_captures_kind_and_new_string_snippet() {
        let input = json!({
            "file_path": "/repo/src/auth.rs",
            "old_string": "let x = 1;",
            "new_string": "let x = 2; // fixed",
        });
        let ev = edit_event("Edit", Some(&input), None).expect("Edit yields an edit event");
        assert_eq!(ev.path, "/repo/src/auth.rs");
        assert_eq!(ev.kind, EditKind::Edit);
        assert!(
            ev.snippet.contains("let x = 2;"),
            "snippet should show the new code, got: {:?}",
            ev.snippet
        );
    }

    #[test]
    fn write_event_snippet_comes_from_content() {
        let input = json!({ "file_path": "/repo/new.rs", "content": "fn main() {}\n" });
        let ev = edit_event("Write", Some(&input), None).expect("Write yields an edit event");
        assert_eq!(ev.kind, EditKind::Write);
        assert!(ev.snippet.contains("fn main()"), "got: {:?}", ev.snippet);
    }

    #[test]
    fn multiedit_event_snippet_comes_from_first_edit() {
        let input = json!({
            "file_path": "/repo/a.rs",
            "edits": [
                { "old_string": "a", "new_string": "alpha" },
                { "old_string": "b", "new_string": "beta" },
            ],
        });
        let ev =
            edit_event("MultiEdit", Some(&input), None).expect("MultiEdit yields an edit event");
        assert_eq!(ev.kind, EditKind::MultiEdit);
        assert!(ev.snippet.contains("alpha"), "got: {:?}", ev.snippet);
    }

    #[test]
    fn non_write_tools_are_not_edit_events() {
        let input = json!({ "command": "ls", "description": "list" });
        assert!(edit_event("Bash", Some(&input), None).is_none());
        assert!(edit_event("Read", Some(&json!({ "file_path": "/x" })), None).is_none());
    }

    #[test]
    fn edit_event_cleans_the_path_like_touched() {
        // Whitespace is trimmed so edits.path matches the dedup_paths-cleaned
        // touched path (they must not diverge).
        let ev = edit_event(
            "Edit",
            Some(&json!({ "file_path": "  /repo/a.rs  ", "new_string": "x" })),
            None,
        )
        .expect("a trimmable path still yields an edit");
        assert_eq!(ev.path, "/repo/a.rs");
        // A path the touched filter rejects (embedded newline) yields no edit
        // either - never an edits-only record touched would have dropped.
        assert!(edit_event(
            "Edit",
            Some(&json!({ "file_path": "/re\npo/a.rs", "new_string": "x" })),
            None
        )
        .is_none());
    }

    #[test]
    fn parse_populates_edits_alongside_touched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess.jsonl");
        let line = r#"{"type":"assistant","timestamp":"2026-07-01T10:00:00Z","message":{"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/repo/src/auth.rs","old_string":"a","new_string":"let fixed = true;"}}]}}"#;
        std::fs::write(&path, format!("{line}\n")).unwrap();

        let session = super::super::parse_session(&ClaudeCode::default(), &path).unwrap();

        assert_eq!(session.edits.len(), 1, "one edit event extracted");
        assert_eq!(session.edits[0].path, "/repo/src/auth.rs");
        assert_eq!(session.edits[0].kind, EditKind::Edit);
        assert!(session.edits[0].snippet.contains("let fixed = true;"));
        // touched stays consistent with edits (same path).
        assert_eq!(session.touched, vec!["/repo/src/auth.rs".to_string()]);
    }

    /// An embedder points one adapter at each Claude Code install. The adapter
    /// must find that install's transcripts and claim only that install's rows
    /// for deletion reconciliation.
    #[test]
    fn in_home_discovers_that_installs_sessions_and_scopes_reconciliation() {
        let home = tempfile::tempdir().unwrap();
        let project = home.path().join("projects").join("-repo");
        std::fs::create_dir_all(&project).unwrap();
        let file = project.join("11111111-2222-3333-4444-555555555555.jsonl");
        std::fs::write(&file, "{\"type\":\"user\",\"cwd\":\"/repo\"}\n").unwrap();

        let adapter = ClaudeCode::in_home(home.path());
        let found = adapter.discover();
        assert!(!found.had_error);
        assert_eq!(found.files, vec![file.clone()]);

        let scope = adapter
            .reconcile_scope()
            .expect("an explicit install is scoped");
        assert!(
            file.to_string_lossy().starts_with(&scope),
            "{scope} must be a prefix of the discovered {}",
            file.display()
        );
        assert_eq!(
            ClaudeCode::default().reconcile_scope(),
            crate::adapters::root_scope(ClaudeCode::default().root().as_deref()),
            "the stock adapter speaks only for its own install"
        );
    }

    /// A session over the byte cap is indexed from its head and tail, and the
    /// files it edited in between were dropped with the rest: on one machine
    /// a 352 MB session with 1,999 Edit calls traced to none of them. Every
    /// edit in the file is kept, however the text is windowed.
    #[test]
    fn a_windowed_session_keeps_every_file_it_edited() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir
            .path()
            .join("11111111-2222-3333-4444-555555555555.jsonl");
        let mut body = String::new();
        for i in 0..30 {
            body.push_str(&format!(
                "{{\"type\":\"assistant\",\"timestamp\":\"2026-10-01T00:00:{i:02}Z\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"t{i}\",\"name\":\"Edit\",\"input\":{{\"file_path\":\"/repo/src/f{i}.rs\",\"old_string\":\"a\",\"new_string\":\"b\"}}}}]}}}}\n"
            ));
        }
        std::fs::write(&file, body).unwrap();
        crate::util::TEST_WINDOW.with(|w| w.set(Some((2_000, 600, 600))));
        let session = ClaudeCode::default().parse(&file).unwrap();
        crate::util::TEST_WINDOW.with(|w| w.set(None));
        assert!(session.title.starts_with("[large]"), "{}", session.title);
        assert_eq!(session.touched.len(), 30, "{:?}", session.touched);
        assert_eq!(session.edits.len(), 30);
        assert!(session.touched.contains(&"/repo/src/f15.rs".to_string()));
    }

    /// A windowed session kept only its first and last turns' text, so search
    /// could not find what was asked in between: one 352 MB session had 1,733
    /// prompts and 8 of them were indexed. Its conversation text is kept from
    /// the whole file; bulky tool output in the middle is still dropped.
    #[test]
    fn a_windowed_session_keeps_the_conversation_in_between() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir
            .path()
            .join("11111111-2222-3333-4444-555555555555.jsonl");
        let mut body = String::new();
        for i in 0..30 {
            body.push_str(&format!(
                "{{\"type\":\"user\",\"timestamp\":\"2026-10-01T00:00:{i:02}Z\",\"message\":{{\"content\":\"question number {i}\"}}}}\n"
            ));
            body.push_str(&format!(
                "{{\"type\":\"user\",\"message\":{{\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"t{i}\",\"content\":\"bulky output {i}\"}}]}}}}\n"
            ));
            body.push_str(&format!(
                "{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"answer number {i}\"}}]}}}}\n"
            ));
        }
        std::fs::write(&file, body).unwrap();
        crate::util::TEST_WINDOW.with(|w| w.set(Some((2_000, 600, 600))));
        let session = ClaudeCode::default().parse(&file).unwrap();
        crate::util::TEST_WINDOW.with(|w| w.set(None));
        assert!(session.title.starts_with("[large]"), "{}", session.title);
        let text = |role: Role| -> Vec<String> {
            session
                .messages
                .iter()
                .filter(|m| m.role == role)
                .map(|m| m.text.clone())
                .collect()
        };
        let questions: Vec<String> = (0..30).map(|i| format!("question number {i}")).collect();
        let answers: Vec<String> = (0..30).map(|i| format!("answer number {i}")).collect();
        assert_eq!(text(Role::User), questions);
        assert_eq!(text(Role::Assistant), answers);
        let results = session.messages.iter().filter(|m| m.tool.is_some()).count();
        assert!(
            results < 30,
            "tool output in the middle stays windowed: {results}"
        );
    }
}
