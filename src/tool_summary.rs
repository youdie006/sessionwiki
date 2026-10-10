//! Shared compact rendering and pairing for structured tool calls/results.

use crate::model::{Message, Session, ToolCall, ToolEvent, ToolPart, ToolResult, ToolSummary};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};

const PRIMARY_ARG_MAX: usize = 120;

/// Recognize compact lines after they have been reloaded from the text-only
/// index, where the in-memory `ToolSummary` is intentionally not persisted.
pub fn is_compact_summary_message(message: &Message) -> bool {
    message.role == crate::model::Role::Tool && is_compact_summary_line(&message.text)
}

pub fn is_compact_summary_line(text: &str) -> bool {
    text.starts_with("→ ") && text.contains(" ⇒ ") && !text.contains('\n')
}

/// Fold adapter-emitted tool parts into one compact, searchable Tool message.
/// Already-folded summaries and legacy plain-text Tool messages pass through.
pub fn fold_tool_parts(session: &mut Session) {
    let count = session.messages.len();
    let mut calls_by_id: HashMap<&str, VecDeque<usize>> = HashMap::new();
    for (index, message) in session.messages.iter().enumerate() {
        if let Some(ToolEvent::Part(ToolPart::Call(call))) = &message.tool {
            if let Some(id) = call.id.as_deref().filter(|id| !id.is_empty()) {
                calls_by_id.entry(id).or_default().push_back(index);
            }
        }
    }

    let mut call_results = vec![None; count];
    let mut result_calls = vec![None; count];
    for (result_index, message) in session.messages.iter().enumerate() {
        let Some(ToolEvent::Part(ToolPart::Result(result))) = &message.tool else {
            continue;
        };
        let Some(id) = result.call_id.as_deref().filter(|id| !id.is_empty()) else {
            continue;
        };
        let Some(queue) = calls_by_id.get_mut(id) else {
            continue;
        };
        while let Some(call_index) = queue.pop_front() {
            if call_results[call_index].is_none() {
                call_results[call_index] = Some(result_index);
                result_calls[result_index] = Some(call_index);
                break;
            }
        }
    }

    let mut preceding_calls = Vec::new();
    for (index, message) in session.messages.iter().enumerate() {
        match &message.tool {
            Some(ToolEvent::Part(ToolPart::Call(_))) if call_results[index].is_none() => {
                preceding_calls.push(index);
            }
            Some(ToolEvent::Part(ToolPart::Result(result)))
                if result_calls[index].is_none()
                    && result.call_id.as_deref().is_none_or(str::is_empty) =>
            {
                while let Some(call_index) = preceding_calls.pop() {
                    if call_results[call_index].is_none() {
                        call_results[call_index] = Some(index);
                        result_calls[index] = Some(call_index);
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    let result_values: Vec<Option<ToolResult>> = session
        .messages
        .iter()
        .map(|message| match &message.tool {
            Some(ToolEvent::Part(ToolPart::Result(result))) => Some(result.clone()),
            _ => None,
        })
        .collect();
    let old_messages = std::mem::take(&mut session.messages);
    let mut folded = Vec::with_capacity(old_messages.len());
    for (index, mut message) in old_messages.into_iter().enumerate() {
        match message.tool.take() {
            Some(ToolEvent::Part(ToolPart::Call(call))) => {
                let result = call_results[index]
                    .and_then(|result_index| result_values[result_index].clone());
                let summary = summary_for_call(call, result);
                message.text = render_summary(&summary);
                message.tool = Some(ToolEvent::Summary(summary));
                folded.push(message);
            }
            Some(ToolEvent::Part(ToolPart::Result(_))) if result_calls[index].is_some() => {}
            Some(ToolEvent::Part(ToolPart::Result(result))) => {
                let summary = summary_for_orphan(result);
                message.text = render_summary(&summary);
                message.tool = Some(ToolEvent::Summary(summary));
                folded.push(message);
            }
            Some(ToolEvent::Summary(summary)) => {
                message.tool = Some(ToolEvent::Summary(summary));
                folded.push(message);
            }
            None => folded.push(message),
        }
    }
    session.messages = folded;
}

fn summary_for_call(call: ToolCall, result: Option<ToolResult>) -> ToolSummary {
    match result {
        Some(result) => ToolSummary {
            name: call.name,
            args: call.args,
            lines: Some(line_count(&result.text)),
            output: Some(result.text),
            is_error: Some(result.is_error),
        },
        None => ToolSummary {
            name: call.name,
            args: call.args,
            output: None,
            is_error: None,
            lines: None,
        },
    }
}

fn summary_for_orphan(result: ToolResult) -> ToolSummary {
    ToolSummary {
        name: "tool".into(),
        args: json!({}),
        lines: Some(line_count(&result.text)),
        output: Some(result.text),
        is_error: Some(result.is_error),
    }
}

fn line_count(text: &str) -> usize {
    text.lines().count()
}

/// Render one structured summary in the shared compact format.
pub fn render_summary(summary: &ToolSummary) -> String {
    let name = one_line(&summary.name, PRIMARY_ARG_MAX);
    let arg = primary_arg(&summary.name, &summary.args);
    let head = format!("→ {name}({arg})");
    let Some(is_error) = summary.is_error else {
        return format!("{head} ⇒ pending");
    };
    let lines = summary.lines.unwrap_or_else(|| {
        summary
            .output
            .as_deref()
            .map(line_count)
            .unwrap_or_default()
    });
    let count = format!("{lines} {}", if lines == 1 { "line" } else { "lines" });
    if !is_error {
        return format!("{head} ⇒ ok · {count}");
    }
    let first = summary
        .output
        .as_deref()
        .unwrap_or_default()
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| one_line(line, PRIMARY_ARG_MAX))
        .unwrap_or_default();
    if first.is_empty() {
        format!("{head} ⇒ error · {count}")
    } else {
        format!("{head} ⇒ error · {count} — {first}")
    }
}

fn primary_arg(name: &str, args: &Value) -> String {
    let Some(object) = args.as_object() else {
        return if args.is_null() {
            "{}".into()
        } else {
            one_line(&args.to_string(), PRIMARY_ARG_MAX)
        };
    };
    if object.is_empty() {
        return "{}".into();
    }

    let normalized_name = name.to_ascii_lowercase();
    if normalized_name == "grep" || normalized_name == "glob" {
        let pattern = object.get("pattern").and_then(value_text);
        let path = ["path", "paths", "include"]
            .iter()
            .find_map(|key| object.get(*key).and_then(value_text));
        return match (pattern, path) {
            (Some(pattern), Some(path)) => {
                one_line(&format!("{pattern} @ {path}"), PRIMARY_ARG_MAX)
            }
            (Some(pattern), None) => one_line(&pattern, PRIMARY_ARG_MAX),
            (None, Some(path)) => one_line(&path, PRIMARY_ARG_MAX),
            (None, None) => primary_arg_fallback(object, args),
        };
    }

    if normalized_name == "apply_patch" {
        if let Some(paths) = patch_paths(args) {
            return one_line(&paths, PRIMARY_ARG_MAX);
        }
    }

    if matches!(normalized_name.as_str(), "shell" | "exec_command" | "bash") {
        for key in ["command", "cmd"] {
            if let Some(command) = object
                .get(key)
                .and_then(|value| value_text_with_sep(value, " "))
            {
                return one_line(&command, PRIMARY_ARG_MAX);
            }
        }
    }

    for key in [
        "path",
        "file_path",
        "filePath",
        "filepath",
        "command",
        "cmd",
        "pattern",
        "url",
        "query",
        "prompt",
        "description",
        "assignment",
        "note",
        "message",
        "op",
        "name",
        "id",
    ] {
        if let Some(value) = object.get(key).and_then(value_text) {
            return one_line(&value, PRIMARY_ARG_MAX);
        }
    }
    primary_arg_fallback(object, args)
}

fn primary_arg_fallback(object: &serde_json::Map<String, Value>, args: &Value) -> String {
    if let Some(value) = object
        .values()
        .find_map(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        return one_line(value, PRIMARY_ARG_MAX);
    }
    one_line(&args.to_string(), PRIMARY_ARG_MAX)
}

fn value_text(value: &Value) -> Option<String> {
    value_text_with_sep(value, ", ")
}

fn value_text_with_sep(value: &Value, separator: &str) -> Option<String> {
    if let Some(s) = value.as_str() {
        return (!s.trim().is_empty()).then(|| s.to_string());
    }
    let values = value.as_array()?;
    if values.is_empty() || !values.iter().all(Value::is_string) {
        return None;
    }
    let joined = values
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(separator);
    (!joined.trim().is_empty()).then_some(joined)
}

fn patch_paths(args: &Value) -> Option<String> {
    let patch = if let Some(text) = args.as_str() {
        Some(text)
    } else {
        ["patch", "input", "body", "diff"]
            .iter()
            .find_map(|key| args.get(*key).and_then(Value::as_str))
            .or_else(|| {
                args.as_object()?
                    .values()
                    .find_map(|value| value.as_str().filter(|s| s.contains("*** ")))
            })
    }?;
    let paths: Vec<&str> = patch
        .lines()
        .filter_map(|line| {
            ["*** Add File: ", "*** Update File: ", "*** Delete File: "]
                .iter()
                .find_map(|prefix| line.strip_prefix(prefix))
        })
        .collect();
    (!paths.is_empty()).then(|| paths.join(", "))
}

fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = flat.chars().count();
    if count <= max {
        return flat;
    }
    let keep = max.saturating_sub(1);
    format!("{}…", flat.chars().take(keep).collect::<String>())
}

/// Return non-empty tool output inside an adaptive Markdown fence, capped by
/// raw-output bytes and lines. No output returns `None`, so readers can omit an
/// empty expansion entirely.
pub fn render_output(summary: &ToolSummary, max_bytes: usize, max_lines: usize) -> Option<String> {
    let output = summary.output.as_deref().filter(|text| !text.is_empty())?;
    let mut body = cap_lines(output, max_lines.max(1));
    body = cap_bytes(&body, max_bytes);
    if body.is_empty() {
        return None;
    }
    let longest = longest_backtick_run(&body);
    let fence = "`".repeat(3.max(longest + 1));
    Some(format!("{fence}text\n{body}\n{fence}"))
}

fn cap_lines(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.len() <= max_lines {
        return text.to_string();
    }
    if max_lines == 1 {
        return format!("{}…", lines[0]);
    }
    let kept = max_lines - 1;
    let head = kept.div_ceil(2);
    let tail = kept - head;
    let omitted = lines.len() - head - tail;
    let mut rendered = lines[..head].join("\n");
    rendered.push_str(&format!("\n[… {omitted} lines omitted …]"));
    if tail > 0 {
        rendered.push('\n');
        rendered.push_str(&lines[lines.len() - tail..].join("\n"));
    }
    rendered
}

fn cap_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    if max_bytes == 0 {
        return String::new();
    }
    const MARKER: &str = "… output bytes omitted …";
    if max_bytes <= MARKER.len() {
        return prefix_at_boundary(text, max_bytes).to_string();
    }
    let content_budget = max_bytes - MARKER.len();
    let head_budget = content_budget.div_ceil(2);
    let tail_budget = content_budget - head_budget;
    let head_end = floor_char_boundary(text, head_budget);
    let tail_start = ceil_char_boundary(text, text.len().saturating_sub(tail_budget));
    format!("{}{MARKER}{}", &text[..head_end], &text[tail_start..])
}

fn prefix_at_boundary(text: &str, max_bytes: usize) -> &str {
    &text[..floor_char_boundary(text, max_bytes)]
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn longest_backtick_run(text: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for ch in text.chars() {
        if ch == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

#[cfg(test)]
mod tests {
    /// Output that ends in a newline - most command output - was counted one
    /// line too many: "a\nb\n" read as 3 lines.
    #[test]
    fn a_trailing_newline_is_not_a_line() {
        assert_eq!(super::line_count("a\nb\n"), 2);
        assert_eq!(super::line_count("a\nb"), 2);
        assert_eq!(super::line_count("a\r\nb\r\n"), 2);
        assert_eq!(super::line_count(""), 0);
    }

    use super::*;

    fn summary(
        name: &str,
        args: Value,
        output: Option<&str>,
        is_error: Option<bool>,
    ) -> ToolSummary {
        ToolSummary {
            name: name.into(),
            args,
            lines: output.map(line_count),
            output: output.map(str::to_owned),
            is_error,
        }
    }

    fn session(messages: Vec<Message>) -> Session {
        Session {
            id: "s1".into(),
            tool: "test",
            path: "test.jsonl".into(),
            project: "test".into(),
            started: None,
            ended: None,
            title: "test".into(),
            subagent: false,
            messages,
            touched: vec![],
            edits: vec![],
        }
    }

    #[test]
    fn renders_ok_error_pending_and_orphan_lines() {
        assert_eq!(
            render_summary(&summary(
                "Read",
                json!({"path":"src/a.rs"}),
                Some("ok"),
                Some(false)
            )),
            "→ Read(src/a.rs) ⇒ ok · 1 line"
        );
        assert_eq!(
            render_summary(&summary(
                "Bash",
                json!({"command":"cargo test"}),
                Some("\nfailed\nmore"),
                Some(true)
            )),
            "→ Bash(cargo test) ⇒ error · 3 lines — failed"
        );
        assert_eq!(
            render_summary(&summary("Read", json!({}), None, None)),
            "→ Read({}) ⇒ pending"
        );
        assert_eq!(
            render_summary(&summary("tool", json!({}), Some(""), Some(false))),
            "→ tool({}) ⇒ ok · 0 lines"
        );
        assert_eq!(
            render_summary(&summary("tool", json!({}), Some("a\nb"), Some(false))),
            "→ tool({}) ⇒ ok · 2 lines"
        );
        assert_eq!(
            render_summary(&summary("tool", json!({}), Some("\n"), Some(true))),
            "→ tool({}) ⇒ error · 1 line"
        );
    }

    #[test]
    fn primary_args_follow_tool_specific_and_generic_rules() {
        let cases = [
            ("grep", json!({"pattern":"TODO","path":"src"}), "TODO @ src"),
            (
                "GLOB",
                json!({"pattern":"*.rs","include":["src","tests"]}),
                "*.rs @ src, tests",
            ),
            ("grep", json!({"pattern":"TODO"}), "TODO"),
            ("glob", json!({"paths":["a","b"]}), "a, b"),
            (
                "apply_patch",
                json!({"patch":"*** Begin Patch\n*** Update File: src/a.rs\n*** Add File: src/b.rs\n*** End Patch"}),
                "src/a.rs, src/b.rs",
            ),
            (
                "shell",
                json!({"command":["cargo","test","-p","x"]}),
                "cargo test -p x",
            ),
            (
                "exec_command",
                json!({"cmd":["git","status","--short"]}),
                "git status --short",
            ),
            ("bash", json!({"command":"pwd"}), "pwd"),
            (
                "read",
                json!({"path":"src/a.rs","prompt":"later"}),
                "src/a.rs",
            ),
            ("read", json!({"file_path":"src/b.rs"}), "src/b.rs"),
            ("read", json!({"filePath":"src/c.rs"}), "src/c.rs"),
            ("other", json!({"command":"make check"}), "make check"),
            ("other", json!({"cmd":"cargo clippy"}), "cargo clippy"),
            ("other", json!({"pattern":"needle"}), "needle"),
            (
                "other",
                json!({"url":"https://example.test"}),
                "https://example.test",
            ),
            ("other", json!({"unexpected":7,"query":"where"}), "where"),
            ("other", json!({"prompt":"fix it"}), "fix it"),
            (
                "other",
                json!({"description":"inspect status"}),
                "inspect status",
            ),
            ("other", json!({"assignment":"add tests"}), "add tests"),
            ("other", json!({"note":"carefully"}), "carefully"),
            ("other", json!({"message":"hello"}), "hello"),
            ("other", json!({"op":"replace"}), "replace"),
            ("other", json!({"name":"read_file"}), "read_file"),
            ("other", json!({"id":"call-1"}), "call-1"),
            ("other", json!({"n":7}), "{\"n\":7}"),
            ("other", json!({}), "{}"),
        ];
        for (name, args, expected) in cases {
            assert_eq!(primary_arg(name, &args), expected, "{name} {args}");
        }
    }

    #[test]
    fn primary_args_collapse_whitespace_and_truncate_to_120_chars() {
        assert_eq!(one_line("  alpha\n\t beta  ", 120), "alpha beta");
        let out = one_line(&"x".repeat(125), 120);
        assert_eq!(out.chars().count(), 120);
        assert!(out.ends_with('…'));
        let summary = summary("tool", json!({}), Some(&"x".repeat(130)), Some(true));
        assert_eq!(
            render_summary(&summary).chars().count(),
            "→ tool({}) ⇒ error · 1 line — ".chars().count() + 120
        );
    }

    #[test]
    fn pairs_by_id_out_of_order_and_by_nearest_preceding_call() {
        let mut s = session(vec![
            Message::tool_call(
                None,
                ToolCall {
                    id: Some("a".into()),
                    name: "A".into(),
                    args: json!({}),
                },
            ),
            Message::tool_call(
                None,
                ToolCall {
                    id: Some("b".into()),
                    name: "B".into(),
                    args: json!({}),
                },
            ),
            Message::tool_result(
                None,
                ToolResult {
                    call_id: Some("b".into()),
                    text: "bee".into(),
                    is_error: false,
                },
            ),
            Message::tool_result(
                None,
                ToolResult {
                    call_id: Some("a".into()),
                    text: "aye".into(),
                    is_error: false,
                },
            ),
            Message::tool_call(
                None,
                ToolCall {
                    id: None,
                    name: "C".into(),
                    args: json!({}),
                },
            ),
            Message::tool_call(
                None,
                ToolCall {
                    id: None,
                    name: "D".into(),
                    args: json!({}),
                },
            ),
            Message::tool_result(
                None,
                ToolResult {
                    call_id: None,
                    text: "dee".into(),
                    is_error: false,
                },
            ),
            Message::tool_result(
                None,
                ToolResult {
                    call_id: None,
                    text: "cee".into(),
                    is_error: false,
                },
            ),
        ]);
        fold_tool_parts(&mut s);
        assert_eq!(s.messages.len(), 4);
        assert_eq!(s.messages[0].text, "→ A({}) ⇒ ok · 1 line");
        assert_eq!(s.messages[1].text, "→ B({}) ⇒ ok · 1 line");
        assert_eq!(s.messages[2].text, "→ C({}) ⇒ ok · 1 line");
        assert_eq!(s.messages[3].text, "→ D({}) ⇒ ok · 1 line");
        assert!(matches!(s.messages[0].tool, Some(ToolEvent::Summary(_))));
    }

    #[test]
    fn orphan_parts_and_pending_calls_fold_and_pass_is_idempotent() {
        let mut s = session(vec![
            Message::tool_call(
                None,
                ToolCall {
                    id: None,
                    name: "Open".into(),
                    args: json!({"path":"x"}),
                },
            ),
            Message::tool_call(
                None,
                ToolCall {
                    id: None,
                    name: "Later".into(),
                    args: json!({}),
                },
            ),
            Message::tool_result(
                None,
                ToolResult {
                    call_id: Some("missing".into()),
                    text: "problem".into(),
                    is_error: true,
                },
            ),
        ]);
        fold_tool_parts(&mut s);
        assert_eq!(s.messages.len(), 3);
        assert_eq!(s.messages[0].text, "→ Open(x) ⇒ pending");
        assert_eq!(s.messages[1].text, "→ Later({}) ⇒ pending");
        assert_eq!(s.messages[2].text, "→ tool({}) ⇒ error · 1 line — problem");
        let before: Vec<String> = s
            .messages
            .iter()
            .map(|message| message.text.clone())
            .collect();
        fold_tool_parts(&mut s);
        assert_eq!(
            s.messages
                .iter()
                .map(|message| message.text.clone())
                .collect::<Vec<_>>(),
            before
        );
    }

    #[test]
    fn render_output_fences_and_bounds_bytes_and_lines() {
        let s = summary(
            "Read",
            json!({}),
            Some("one\ntwo\nthree\nfour"),
            Some(false),
        );
        let rendered = render_output(&s, 100, 3).unwrap();
        assert!(rendered.starts_with("```text\n"));
        assert!(rendered.ends_with("\n```"));
        assert!(rendered.contains("lines omitted"));
        let capped = render_output(&s, 12, 80).unwrap();
        let body = capped
            .strip_prefix("```text\n")
            .unwrap()
            .strip_suffix("\n```")
            .unwrap();
        assert!(body.len() <= 12);
        assert!(
            render_output(&summary("Read", json!({}), Some(""), Some(false)), 100, 80).is_none()
        );
    }

    #[test]
    fn message_json_omits_absent_tool_data() {
        let plain = Message {
            role: crate::model::Role::User,
            text: "hello".into(),
            ts: None,
            tool: None,
        };
        let value = serde_json::to_value(plain).unwrap();
        assert!(value.get("tool").is_none());
        let call = Message::tool_call(
            None,
            ToolCall {
                id: None,
                name: "Read".into(),
                args: json!({"path":"a.rs"}),
            },
        );
        assert!(serde_json::to_value(call).unwrap().get("tool").is_some());
    }
}
