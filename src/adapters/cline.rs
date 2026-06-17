use super::{dedup_paths, title_from_messages, Adapter};
use crate::model::{Message, Role, Session};
use crate::util::{short_id, truncate};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The Cline family of VS Code agents - Cline, Roo Code, and Kilo Code (a Roo
/// fork) - share one on-disk layout, so one parser covers all three. Each is a
/// separate adapter (distinct `scan`/`list` identity) over the same format.
///
/// A task lives under the extension's VS Code globalStorage:
///   <globalStorage>/<extension-id>/tasks/<taskId>/
///     api_conversation_history.json  - the raw Anthropic Messages API array
///                                       (or the legacy name claude_messages.json)
///     ui_messages.json               - the say/ask UI event stream, the only
///                                       place with reliable timestamps (epoch ms)
///
/// Tool calls appear in two shapes that both must be handled: native Anthropic
/// `tool_use` blocks (newer Roo/Kilo) and XML tags embedded in an assistant
/// `text` block (Cline and older forks) - `<write_to_file><path>..</path>..`.
pub struct Cline;
pub struct RooCode;
pub struct KiloCode;

impl Adapter for Cline {
    fn name(&self) -> &'static str {
        "cline"
    }
    fn root(&self) -> Option<PathBuf> {
        primary_root("saoudrizwan.claude-dev")
    }
    fn discover(&self) -> Vec<PathBuf> {
        discover_tasks("saoudrizwan.claude-dev")
    }
    fn parse(&self, path: &Path) -> Result<Session> {
        parse_task(self.name(), path)
    }
}

impl Adapter for RooCode {
    fn name(&self) -> &'static str {
        "roo-code"
    }
    fn root(&self) -> Option<PathBuf> {
        primary_root("rooveterinaryinc.roo-cline")
    }
    fn discover(&self) -> Vec<PathBuf> {
        discover_tasks("rooveterinaryinc.roo-cline")
    }
    fn parse(&self, path: &Path) -> Result<Session> {
        parse_task(self.name(), path)
    }
}

impl Adapter for KiloCode {
    fn name(&self) -> &'static str {
        "kilo-code"
    }
    fn root(&self) -> Option<PathBuf> {
        primary_root("kilocode.kilo-code")
    }
    fn discover(&self) -> Vec<PathBuf> {
        discover_tasks("kilocode.kilo-code")
    }
    fn parse(&self, path: &Path) -> Result<Session> {
        parse_task(self.name(), path)
    }
}

/// VS Code keeps per-extension state in `<config>/<variant>/User/globalStorage`.
/// `dirs::config_dir()` resolves the OS base (~/.config on Linux, ~/Library/
/// Application Support on macOS, %APPDATA% on Windows); we try the editor
/// variants users actually run, plus the remote-server home for SSH/WSL.
const VSCODE_VARIANTS: &[&str] = &["Code", "Code - Insiders", "VSCodium", "Cursor", "Windsurf"];

fn globalstorage_bases() -> Vec<PathBuf> {
    let mut bases = Vec::new();
    if let Some(config) = dirs::config_dir() {
        for v in VSCODE_VARIANTS {
            bases.push(config.join(v).join("User").join("globalStorage"));
        }
    }
    // Remote/WSL/SSH/devcontainer: the server uses a Linux-style home regardless
    // of the client OS.
    if let Some(home) = dirs::home_dir() {
        bases.push(
            home.join(".vscode-server")
                .join("data")
                .join("User")
                .join("globalStorage"),
        );
    }
    bases
}

/// The canonical (VS Code stable) store path, used by `scan` for existence and
/// size. `discover` still scans the other variants.
fn primary_root(ext_id: &str) -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join("Code")
            .join("User")
            .join("globalStorage")
            .join(ext_id),
    )
}

/// The history file for every `tasks/<id>/` across all editor variants. Prefers
/// `api_conversation_history.json`, falling back to the legacy `claude_messages.json`.
fn discover_tasks(ext_id: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for base in globalstorage_bases() {
        let tasks = base.join(ext_id).join("tasks");
        let Ok(entries) = std::fs::read_dir(&tasks) else {
            continue;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let dir = entry.path();
            let primary = dir.join("api_conversation_history.json");
            let legacy = dir.join("claude_messages.json");
            if primary.is_file() {
                out.push(primary);
            } else if legacy.is_file() {
                out.push(legacy);
            }
        }
    }
    out
}

/// File-edit tools across the family. Both the native `tool_use` name and the
/// XML tag name use these strings; each carries the target in a `path` field.
/// Read/list/search/command tools do not establish authorship.
const EDIT_TOOLS: &[&str] = &[
    "write_to_file",
    "replace_in_file",
    "apply_diff",
    "insert_content",
    "search_and_replace",
    "edit_file",
    "new_rule",
    "apply_patch",
];

/// Any tool tag - used only to cut the assistant's prose off before its
/// embedded XML tool call.
const TOOL_TAGS: &[&str] = &[
    "write_to_file",
    "replace_in_file",
    "apply_diff",
    "insert_content",
    "search_and_replace",
    "edit_file",
    "new_rule",
    "apply_patch",
    "read_file",
    "list_files",
    "search_files",
    "list_code_definition_names",
    "execute_command",
    "browser_action",
    "use_mcp_tool",
    "access_mcp_resource",
    "ask_followup_question",
    "attempt_completion",
    "new_task",
    "plan_mode_respond",
];

fn parse_task(tool: &'static str, path: &Path) -> Result<Session> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("open {}", path.display()))?;
    let history: Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    let entries = history.as_array().cloned().unwrap_or_default();

    let task_dir = path.parent();

    let mut messages: Vec<Message> = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    let mut cwd = String::new();

    for entry in &entries {
        let role = match entry.get("role").and_then(Value::as_str) {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            _ => continue,
        };
        let content = entry.get("content");

        if role == Role::User {
            let (text, native_result) = collect_user_text(content);
            if cwd.is_empty() {
                cwd = cwd_from_text(&text);
            }
            // A tool-feedback turn (the result echo + environment dump) is not a
            // human message - drop it, but keep any cwd it carried.
            if native_result || text.contains("] Result:") {
                continue;
            }
            push(&mut messages, Role::User, &strip_wrappers(&text));
            continue;
        }

        // Assistant: text blocks (which may embed XML tool calls) + native
        // tool_use blocks.
        for block in blocks(content) {
            match block.get("type").and_then(Value::as_str) {
                Some("text") | None if block.get("text").is_some() => {
                    let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                    push(&mut messages, Role::Assistant, prose_before_tools(text));
                    for (name, p) in xml_edits(text) {
                        touched.push(p.clone());
                        push(&mut messages, Role::Tool, &format!("{name} {p}"));
                    }
                }
                Some("tool_use") => {
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("?");
                    let input = block.get("input");
                    if EDIT_TOOLS.contains(&name) {
                        if let Some(p) = input
                            .and_then(|i| i.get("path"))
                            .and_then(Value::as_str)
                            .filter(|p| !p.is_empty())
                        {
                            touched.push(p.to_string());
                        }
                    }
                    let arg = input.map(|i| i.to_string()).unwrap_or_default();
                    push(
                        &mut messages,
                        Role::Tool,
                        &format!("{name} {}", truncate(&arg, 300)),
                    );
                }
                _ => {}
            }
        }
    }

    let ui = task_dir
        .map(|d| d.join("ui_messages.json"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<Value>(&s).ok());
    let task_id = task_dir
        .and_then(|d| d.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (started, ended, ui_title) = ui_summary(ui.as_ref(), &task_id);
    let title = ui_title.unwrap_or_else(|| title_from_messages(&messages));

    Ok(Session {
        id: short_id(&path.to_string_lossy()),
        tool,
        path: path.to_path_buf(),
        project: cwd,
        started,
        ended,
        title,
        subagent: false,
        messages,
        touched: dedup_paths(touched),
    })
}

/// `content` is `string | ContentBlock[]`. Normalize to a slice of blocks; a
/// bare string becomes one synthetic text block.
fn blocks(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(s)) => vec![serde_json::json!({"type": "text", "text": s})],
        Some(Value::Array(a)) => a.clone(),
        _ => vec![],
    }
}

/// Gather a user turn's text (joining text blocks) and note whether it carried
/// a native `tool_result` block - both mark it as tool feedback rather than a
/// human turn.
fn collect_user_text(content: Option<&Value>) -> (String, bool) {
    match content {
        Some(Value::String(s)) => (s.clone(), false),
        Some(Value::Array(a)) => {
            let mut text = String::new();
            let mut native_result = false;
            for b in a {
                match b.get("type").and_then(Value::as_str) {
                    Some("tool_result") => native_result = true,
                    _ => {
                        if let Some(t) = b.get("text").and_then(Value::as_str) {
                            text.push_str(t);
                            text.push('\n');
                        }
                    }
                }
            }
            (text, native_result)
        }
        _ => (String::new(), false),
    }
}

/// Prose the assistant wrote before any embedded XML tool call.
fn prose_before_tools(text: &str) -> &str {
    let mut cut = text.len();
    for tag in TOOL_TAGS {
        if let Some(i) = text.find(&format!("<{tag}>")) {
            cut = cut.min(i);
        }
    }
    text[..cut].trim()
}

/// Every `(tool, path)` from XML tool tags - `<write_to_file><path>X</path>`.
fn xml_edits(text: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for &tool in EDIT_TOOLS {
        let open = format!("<{tool}>");
        let close = format!("</{tool}>");
        let mut hay = text;
        while let Some(i) = hay.find(&open) {
            let after = &hay[i + open.len()..];
            let end = after.find(&close).unwrap_or(after.len());
            if let Some(p) = between(&after[..end], "<path>", "</path>") {
                let p = p.trim();
                if !p.is_empty() {
                    out.push((tool, p.to_string()));
                }
            }
            hay = &after[end..];
        }
    }
    out
}

fn between<'a>(s: &'a str, a: &str, b: &str) -> Option<&'a str> {
    let i = s.find(a)? + a.len();
    let j = s[i..].find(b)? + i;
    Some(&s[i..j])
}

/// Strip the `<task>` wrapper and any `<environment_details>` dump from a user
/// turn, leaving the human's actual words.
fn strip_wrappers(text: &str) -> String {
    let mut t = remove_blocks(text, "<environment_details>", "</environment_details>");
    t = t.replace("<task>", "").replace("</task>", "");
    t.trim().to_string()
}

fn remove_blocks(text: &str, open: &str, close: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find(open) {
        out.push_str(&rest[..i]);
        let after = &rest[i + open.len()..];
        match after.find(close) {
            Some(j) => rest = &after[j + close.len()..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Cline appends `<environment_details>` containing a
/// `# Current Working Directory (/abs/path)` line. Best-effort cwd recovery.
fn cwd_from_text(text: &str) -> String {
    let marker = "Current Working Directory (";
    let Some(start) = text.find(marker) else {
        return String::new();
    };
    let rest = &text[start + marker.len()..];
    match rest.find(')') {
        Some(end) => rest[..end].trim().to_string(),
        None => String::new(),
    }
}

/// Timestamps (first/last event) and the title (first `say: "task"`) from the
/// ui_messages event stream; `ts` is epoch milliseconds. Falls back to the
/// taskId when it is itself an epoch-ms value.
fn ui_summary(
    ui: Option<&Value>,
    task_id: &str,
) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>, Option<String>) {
    let events = ui.and_then(Value::as_array);

    let title = events.and_then(|evs| {
        evs.iter()
            .find(|e| e.get("say").and_then(Value::as_str) == Some("task"))
            .and_then(|e| e.get("text").and_then(Value::as_str))
            .filter(|t| !t.trim().is_empty())
            .map(|t| truncate(t, 80))
    });

    let mut stamps: Vec<i64> = events
        .map(|evs| {
            evs.iter()
                .filter_map(|e| e.get("ts").and_then(Value::as_i64))
                .collect()
        })
        .unwrap_or_default();
    stamps.sort_unstable();

    let started = stamps
        .first()
        .copied()
        .or_else(|| task_id.parse::<i64>().ok())
        .and_then(DateTime::from_timestamp_millis);
    let ended = stamps
        .last()
        .copied()
        .and_then(DateTime::from_timestamp_millis);

    (started, ended, title)
}

fn push(messages: &mut Vec<Message>, role: Role, text: &str) {
    let text = text.trim();
    if !text.is_empty() {
        messages.push(Message {
            role,
            text: text.to_string(),
            ts: None,
        });
    }
}
