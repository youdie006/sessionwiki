use super::{
    bounded_redacted_output, dedup_paths, ok_or_flag, parse_ts, title_from_messages, Adapter,
    Discovered,
};
use crate::model::{Message, Role, Session, ToolCall, ToolResult};
use crate::util::short_id;
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Codex CLI stores one JSONL rollout per session under
/// `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`.
/// Lines carry a `type` plus a `payload`; the schema has shifted across
/// versions, so both `response_item` and `event_msg` shapes are handled.
///
/// `Codex::default()` reads the stock `~/.codex` install. An embedder that
/// runs several Codex installs in different homes builds one adapter per
/// install with [`Codex::in_home`].
#[derive(Default)]
pub struct Codex {
    /// Explicit sessions directory, or `None` for the stock location.
    root: Option<PathBuf>,
}

impl Codex {
    /// An adapter for the Codex install rooted at `home` (e.g. `~/.codex2`).
    /// The sessions sub-directory layout is the adapter's business, not the
    /// caller's.
    pub fn in_home(home: impl Into<PathBuf>) -> Self {
        Codex {
            root: Some(home.into().join("sessions")),
        }
    }
}

impl Adapter for Codex {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn root(&self) -> Option<PathBuf> {
        match &self.root {
            Some(root) => Some(root.clone()),
            None => Some(dirs::home_dir()?.join(".codex").join("sessions")),
        }
    }

    /// Every install speaks only for the rows under the root it scans.
    fn reconcile_scope(&self) -> Option<String> {
        super::root_scope(self.root().as_deref())
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
            .into_iter()
            .filter_map(|e| ok_or_flag(e, &mut had_error))
            .filter(|e| e.file_type().is_file())
            .filter(|e| {
                let name = e.file_name().to_string_lossy();
                name.starts_with("rollout-") && name.ends_with(".jsonl")
            })
            .map(|e| e.into_path())
            .collect();
        Discovered { files, had_error }
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        // Over the byte cap: window (head+tail) instead of dropping the session.
        let (lines, windowed) = crate::util::session_lines(path)?;

        let mut messages: Vec<Message> = Vec::new();
        let mut touched: Vec<String> = Vec::new();
        let mut cwd: Option<String> = None;
        // Sub-agent threads say so in session_meta; their task arrives encrypted.
        let mut subagent = false;
        let mut started = None;
        let mut ended = None;
        // Current rollouts carry each user prompt TWICE (event_msg AND
        // response_item). Counted multisets cancel each pair exactly once, so
        // a genuinely repeated prompt ("continue" twice) still indexes twice.
        let mut response_user_texts: HashMap<String, u32> = HashMap::new();
        let mut event_user_texts: HashMap<String, u32> = HashMap::new();

        for line in &lines {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };

            let ts = v
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_ts);
            if let Some(t) = ts {
                if started.is_none() {
                    started = Some(t);
                }
                ended = Some(t);
            }

            match v.get("type").and_then(Value::as_str) {
                Some("session_meta") => {
                    subagent |= is_subagent_meta(&v);
                    if cwd.is_none() {
                        cwd = v
                            .pointer("/payload/cwd")
                            .and_then(Value::as_str)
                            .map(String::from);
                    }
                }
                Some("response_item") => {
                    match v.pointer("/payload/type").and_then(Value::as_str) {
                        Some("message") => {
                            let role = match v.pointer("/payload/role").and_then(Value::as_str) {
                                Some("user") => Role::User,
                                Some("assistant") => Role::Assistant,
                                _ => continue,
                            };
                            let Some(Value::Array(blocks)) = v.pointer("/payload/content") else {
                                continue;
                            };
                            for b in blocks {
                                let Some(text) = b.get("text").and_then(Value::as_str) else {
                                    continue;
                                };
                                if role == Role::User && is_boilerplate(text) {
                                    continue;
                                }
                                if role == Role::User {
                                    if let Some(n) =
                                        event_user_texts.get_mut(text).filter(|n| **n > 0)
                                    {
                                        *n -= 1; // pairs with an already-pushed event_msg
                                        continue;
                                    }
                                    *response_user_texts.entry(text.to_string()).or_insert(0) += 1;
                                }
                                push(&mut messages, role, text, ts);
                            }
                        }
                        Some("function_call") => {
                            let payload = v.get("payload").unwrap_or(&Value::Null);
                            let raw_args = payload.get("arguments").and_then(Value::as_str);
                            // Inspect the original JSON string so escaped patch
                            // newlines continue to contribute to provenance.
                            collect_patched_paths(raw_args.unwrap_or_default(), &mut touched);
                            let mut args = if let Some(raw_args) = raw_args {
                                serde_json::from_str(raw_args)
                                    .unwrap_or_else(|_| json!({"arguments": raw_args}))
                            } else {
                                json!({})
                            };
                            redact_json_strings(&mut args);
                            messages.push(Message::tool_call(
                                ts,
                                ToolCall {
                                    id: call_id(payload),
                                    name: payload
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or("?")
                                        .to_owned(),
                                    args,
                                },
                            ));
                        }
                        Some("custom_tool_call") => {
                            let payload = v.get("payload").unwrap_or(&Value::Null);
                            let input = payload.get("input").and_then(Value::as_str).unwrap_or("");
                            let is_patch = input.contains("*** Begin Patch");
                            let (name, mut args) = if is_patch {
                                let patch = input
                                    .split_once("*** Begin Patch")
                                    .map(|(_, patch)| format!("*** Begin Patch{patch}"))
                                    .unwrap_or_else(|| input.to_owned())
                                    .replace("\\n", "\n");
                                collect_patched_paths(&patch, &mut touched);
                                ("apply_patch", json!({"patch": patch}))
                            } else if let Some(cmd) = exec_command_cmd(input) {
                                // The `exec` tool runs a JS snippet; the common shape is
                                // `await tools.exec_command({cmd:"..."})`. Surface the
                                // command itself, not the wrapper around it.
                                ("exec_command", json!({"cmd": cmd}))
                            } else {
                                ("exec", json!({"input": input}))
                            };
                            redact_json_strings(&mut args);
                            messages.push(Message::tool_call(
                                ts,
                                ToolCall {
                                    id: call_id(payload),
                                    name: if name != "exec" {
                                        name.to_owned()
                                    } else {
                                        payload
                                            .get("name")
                                            .and_then(Value::as_str)
                                            .unwrap_or(name)
                                            .to_owned()
                                    },
                                    args,
                                },
                            ));
                        }
                        Some("function_call_output" | "custom_tool_call_output") => {
                            let payload = v.get("payload").unwrap_or(&Value::Null);
                            let (output, is_error) = payload
                                .get("output")
                                .map(unwrap_output_value)
                                .unwrap_or_default();
                            messages.push(Message::tool_result(
                                ts,
                                ToolResult {
                                    call_id: call_id(payload),
                                    text: bounded_redacted_output(&output),
                                    is_error,
                                },
                            ));
                        }
                        // Reasoning and other rollout bookkeeping are not user
                        // or tool content and remain excluded from the index.
                        _ => {}
                    }
                }
                Some("event_msg") => match v.pointer("/payload/type").and_then(Value::as_str) {
                    Some("user_message") => {
                        if let Some(t) = v.pointer("/payload/message").and_then(Value::as_str) {
                            if !is_boilerplate(t) {
                                if let Some(n) = response_user_texts.get_mut(t).filter(|n| **n > 0)
                                {
                                    *n -= 1; // pairs with an already-pushed response_item
                                } else {
                                    *event_user_texts.entry(t.to_string()).or_insert(0) += 1;
                                    push(&mut messages, Role::User, t, ts);
                                }
                            }
                        }
                    }
                    Some("agent_message") => {
                        if let Some(t) = v.pointer("/payload/message").and_then(Value::as_str) {
                            push(&mut messages, Role::Assistant, t, ts);
                        }
                    }
                    _ => {}
                },
                Some("message") => {
                    let role = match v.get("role").and_then(Value::as_str) {
                        Some("user") => Role::User,
                        Some("assistant") => Role::Assistant,
                        _ => continue,
                    };
                    let Some(Value::Array(blocks)) = v.get("content") else {
                        continue;
                    };
                    for b in blocks {
                        let Some(text) = b.get("text").and_then(Value::as_str) else {
                            continue;
                        };
                        if role == Role::User && is_boilerplate(text) {
                            continue;
                        }
                        push(&mut messages, role, text, ts);
                    }
                }
                _ => {}
            }
        }

        let project = cwd.unwrap_or_default();
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
            subagent,
            messages,
            touched: dedup_paths(touched),
            edits: Vec::new(),
        })
    }
}

/// Extract the files an apply_patch touched from the raw call arguments. The
/// patch format names each file on a header line - `*** Add File: path`,
/// `*** Update File: path`, `*** Delete File: path`, `*** Move to: path` -
/// regardless of whether the call arrives as a dedicated apply_patch function
/// or a shell command wrapping a heredoc. Newlines may be JSON-escaped (\\n)
/// when the patch is embedded in an arguments string, so handle both.
fn collect_patched_paths(args: &str, out: &mut Vec<String>) {
    const MARKERS: [&str; 4] = [
        "*** Add File: ",
        "*** Update File: ",
        "*** Delete File: ",
        "*** Move to: ",
    ];
    let normalized = args.replace("\\n", "\n");
    for line in normalized.lines() {
        let line = line.trim();
        for m in MARKERS {
            if let Some(rest) = line.strip_prefix(m) {
                let path = rest.trim().trim_matches('"');
                if !path.is_empty() {
                    out.push(path.to_string());
                }
            }
        }
    }
}

fn call_id(payload: &Value) -> Option<String> {
    payload
        .get("call_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

/// Flatten a result payload's `output` to text and an error flag. A string is
/// unwrapped as one chunk. The `exec` tool returns an array: a preamble block
/// (`Script completed\nWall time ...\nOutput:`), then one JSON chunk per
/// `exec_command` the script ran, each with its own `exit_code`, sometimes
/// with other text blocks between. The preamble is dropped, every chunk is
/// unwrapped, and any nonzero exit code or a failed script marks the result.
fn unwrap_output_value(output: &Value) -> (String, bool) {
    match output {
        Value::String(text) => unwrap_output(text),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            let mut is_error = false;
            for text in blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
            {
                if text.starts_with("Script completed\n") || text.starts_with("Script failed") {
                    is_error |= text.starts_with("Script failed");
                    continue;
                }
                let (chunk, chunk_error) = unwrap_output(text);
                is_error |= chunk_error;
                if !chunk.is_empty() {
                    parts.push(chunk);
                }
            }
            (parts.join("\n"), is_error)
        }
        _ => (String::new(), false),
    }
}

/// Unwrap Codex's JSON-encoded command result while preserving ordinary text.
/// Both current top-level exit_code and the older metadata.exit_code shape are
/// used to mark failed calls.
fn unwrap_output(text: &str) -> (String, bool) {
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(text) else {
        return unwrap_text_output(text).unwrap_or_else(|| (text.to_owned(), false));
    };
    let Some(output) = object.get("output").and_then(Value::as_str) else {
        return (text.to_owned(), false);
    };
    let exit_code = object.get("exit_code").and_then(Value::as_i64).or_else(|| {
        object
            .get("metadata")
            .and_then(|metadata| metadata.get("exit_code"))
            .and_then(Value::as_i64)
    });
    (output.to_owned(), exit_code.is_some_and(|code| code != 0))
}

/// Codex also writes command results as plain text: a header with
/// `Exit code: N` or `Process exited with code N`, then `Output:` and the
/// output itself.
fn unwrap_text_output(text: &str) -> Option<(String, bool)> {
    let (header, output) = text
        .split_once("\nOutput:\n")
        .or_else(|| text.strip_suffix("\nOutput:").map(|header| (header, "")))?;
    let exit_code = header.lines().find_map(|line| {
        line.strip_prefix("Exit code: ")
            .or_else(|| line.strip_prefix("Process exited with code "))
            .and_then(|code| code.trim().parse::<i64>().ok())
    })?;
    Some((output.to_owned(), exit_code != 0))
}

fn redact_json_strings(value: &mut Value) {
    match value {
        Value::String(text) => *text = crate::redact::redact(text).into_owned(),
        Value::Array(values) => values.iter_mut().for_each(redact_json_strings),
        Value::Object(values) => values.values_mut().for_each(redact_json_strings),
        _ => {}
    }
}

/// Codex wraps instructions and environment dumps in pseudo-XML tags and
/// repeats them in every session. Indexing them buries real matches.
fn is_boilerplate(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<user_instructions>")
        || t.starts_with("<environment_context>")
        || t.starts_with("<ENVIRONMENT_CONTEXT>")
        || t.starts_with("<turn_context>")
        || t.starts_with("# AGENTS.md instructions")
        || t.starts_with("<INSTRUCTIONS>")
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

/// Whether a rollout's `session_meta` line marks a sub-agent thread. Shared
/// with the index, which reclassifies rows parsed before this was read.
pub(crate) fn is_subagent_meta(v: &Value) -> bool {
    v.pointer("/payload/thread_source").and_then(Value::as_str) == Some("subagent")
        || v.pointer("/payload/source/subagent").is_some()
}

/// Pull the command strings out of an `exec` tool snippet of the form
/// `tools.exec_command({cmd:"..."})`, decoding JS string escapes. A script
/// that runs several commands yields them joined with `; `. Returns None for
/// any other snippet so the caller keeps the raw input.
fn exec_command_cmd(input: &str) -> Option<String> {
    let cmds: Vec<String> = input
        .split("tools.exec_command(")
        .skip(1)
        .filter_map(js_cmd_literal)
        .collect();
    (!cmds.is_empty()).then(|| cmds.join("; "))
}

/// Decode the `cmd` string literal from one `exec_command` argument object.
fn js_cmd_literal(call: &str) -> Option<String> {
    let (_, after_key) = call.split_once("cmd")?;
    let rest = after_key.trim_start().strip_prefix(':')?.trim_start();
    let mut chars = rest.chars();
    let quote = chars.next()?;
    if !matches!(quote, '"' | '\'' | '`') {
        return None;
    }
    let mut cmd = String::new();
    while let Some(c) = chars.next() {
        match c {
            c if c == quote => return (!cmd.trim().is_empty()).then_some(cmd),
            '\\' => match chars.next()? {
                'n' => cmd.push('\n'),
                't' => cmd.push('\t'),
                'r' => cmd.push('\r'),
                other => cmd.push(other),
            },
            c => cmd.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An embedder points one adapter at each Codex install. The adapter must
    /// find that install's rollouts and claim only that install's rows for
    /// deletion reconciliation.
    #[test]
    fn in_home_discovers_that_installs_rollouts_and_scopes_reconciliation() {
        let home = tempfile::tempdir().unwrap();
        let day = home
            .path()
            .join("sessions")
            .join("2026")
            .join("01")
            .join("01");
        std::fs::create_dir_all(&day).unwrap();
        let file = day.join("rollout-2026-01-01T10-00-00-abc.jsonl");
        std::fs::write(
            &file,
            "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/repo\"}}\n",
        )
        .unwrap();

        let adapter = Codex::in_home(home.path());
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
            Codex::default().reconcile_scope(),
            crate::adapters::root_scope(Codex::default().root().as_deref()),
            "the stock adapter speaks only for its own install"
        );
    }

    /// Codex marks a rollout a sub-agent's in its session_meta
    /// (`thread_source: "subagent"`, `source.subagent`). The adapter called
    /// every rollout a main session, so on one machine 2,925 of 4,725
    /// rollouts - sub-agent threads whose task arrives encrypted - filled
    /// `list`, `projects` and the recall hook as "(no user prompt)" rows.
    #[test]
    fn a_subagent_rollout_is_parsed_as_a_subagent() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, meta: &str| {
            let file = dir.path().join(name);
            std::fs::write(
                &file,
                format!(
                    "{{\"timestamp\":\"2026-10-07T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{meta}}}\n\
                     {{\"timestamp\":\"2026-10-07T00:00:01Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"working\"}}]}}}}\n"
                ),
            )
            .unwrap();
            file
        };
        let main = write("main.jsonl", r#"{"cwd":"/repo","source":"cli"}"#);
        let by_thread = write(
            "sub1.jsonl",
            r#"{"cwd":"/repo","thread_source":"subagent","agent_path":"/root/worker"}"#,
        );
        let by_source = write(
            "sub2.jsonl",
            r#"{"cwd":"/repo","source":{"subagent":{"thread_spawn":{"depth":1}}}}"#,
        );
        let codex = Codex::default();
        assert!(!codex.parse(&main).unwrap().subagent);
        assert!(codex.parse(&by_thread).unwrap().subagent);
        assert!(codex.parse(&by_source).unwrap().subagent);
    }
}
