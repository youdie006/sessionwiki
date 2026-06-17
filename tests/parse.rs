//! Golden-file tests for the session adapters.
//!
//! These guard the most fragile part of the project: parsing three different
//! JSONL/JSON schemas that drift between tool versions. Each fixture under
//! `tests/fixtures/<tool>/` is a small, realistic session including the edge
//! cases the parsers are supposed to handle (boilerplate dropping, subagent
//! detection, malformed lines, multi-block content, skipped roles).
//!
//! When a tool changes its format, a fixture from the new version plus an
//! assertion here is the cleanest possible bug report and regression test.

use sessionwiki::adapters;
use sessionwiki::model::Session;
use std::path::PathBuf;

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel)
}

fn parse(tool: &str, rel: &str) -> Session {
    let adapter = adapters::by_name(tool).expect("adapter exists");
    adapter.parse(&fixture(rel)).expect("parse succeeds")
}

fn roles(s: &Session) -> Vec<&'static str> {
    s.messages.iter().map(|m| m.role.label()).collect()
}

#[test]
fn claude_main_session() {
    let s = parse(
        "claude-code",
        "claude-code/proj-a/0a000000-0000-4000-8000-000000000001.jsonl",
    );

    // Title comes from the summary line, not the first user message.
    assert_eq!(s.title, "Fix CORS preflight failing on /auth routes");
    assert_eq!(s.tool, "claude-code");
    assert_eq!(s.project, "/home/dev/proj-a");
    assert!(!s.subagent);

    // A malformed line and the isMeta:true boilerplate user turn are dropped;
    // tool_use (Bash, two Edits, a Write) and tool_result all become Tool
    // messages.
    assert_eq!(
        roles(&s),
        [
            "user",
            "assistant",
            "tool",
            "tool",
            "tool",
            "tool",
            "tool",
            "assistant"
        ]
    );

    // The dropped boilerplate must not leak into any message.
    assert!(s
        .messages
        .iter()
        .all(|m| !m.text.contains("harness boilerplate")));

    // First user prompt and final assistant message survive intact.
    assert!(s.messages[0].text.contains("Preflight requests"));
    assert!(s.messages.iter().any(|m| m.text.contains("14 tests pass")));

    // Provenance: Edit/Write file_path is extracted, the repeated edit to
    // mod.rs is de-duplicated, and the read-only Bash call contributes nothing.
    assert_eq!(
        s.touched,
        [
            "/home/dev/proj-a/src/middleware/mod.rs",
            "/home/dev/proj-a/tests/cors_preflight.rs"
        ]
    );

    assert_eq!(
        s.started.map(|t| t.to_rfc3339()),
        Some("2026-06-09T14:01:00+00:00".to_string())
    );
}

#[test]
fn claude_subagent_is_flagged() {
    let s = parse(
        "claude-code",
        "claude-code/proj-a/0a000000-0000-4000-8000-000000000001/subagents/agent-deadbeef.jsonl",
    );
    assert!(s.subagent, "files under /subagents/ must be flagged");
    assert_eq!(roles(&s), ["user", "assistant"]);
    assert_eq!(s.title, "Search the codebase for all CORS references.");
}

#[test]
fn codex_session_with_schema_variants() {
    let s = parse(
        "codex",
        "codex/rollout-2026-06-11T13-00-00-019eb9b2-1466-7e93-8b85-5b596295e96b.jsonl",
    );

    assert_eq!(s.tool, "codex");
    assert_eq!(s.project, "/home/dev/api-server");
    assert_eq!(s.title, "Write property-based tests for the rate limiter.");

    // environment_context boilerplate dropped; both function_calls (the shell
    // test run and the apply_patch) kept as Tool; function_call_output and
    // reasoning dropped; both response_item and event_msg message shapes parse.
    assert_eq!(
        roles(&s),
        ["user", "tool", "tool", "assistant", "user", "assistant"]
    );
    assert!(s
        .messages
        .iter()
        .all(|m| !m.text.contains("environment_context")));
    assert!(s.messages.iter().all(|m| !m.text.contains("cases passed"))); // function_call_output excluded
    assert!(s.messages.iter().any(|m| m.text.contains("2.1M ops/sec")));

    // Provenance: apply_patch file headers (Update File / Add File) are pulled
    // from the call arguments even though the patch is double-JSON-escaped.
    assert_eq!(
        s.touched,
        ["src/rate_limiter.rs", "tests/rate_limiter_props.rs"]
    );
}

#[test]
fn gemini_session() {
    let s = parse(
        "gemini",
        "gemini/myproject/chats/session-2026-06-08T10-00-abcd1234.json",
    );

    assert_eq!(s.tool, "gemini");
    assert_eq!(s.project, "myproject");
    // String content and array-of-blocks content both parse; unknown role
    // ("system") is skipped.
    assert_eq!(roles(&s), ["user", "assistant"]);
    assert!(s.messages[0].text.contains("iframe"));
    assert!(s.messages[1].text.contains("1급 스코프")); // CJK survives intact
    assert_eq!(
        s.ended.map(|t| t.to_rfc3339()),
        Some("2026-06-08T10:00:30+00:00".to_string())
    );
}

#[test]
fn opencode_multi_file_session() {
    // OpenCode splits a session across session/message/part JSON files; the
    // adapter is handed the session file and joins the rest from the store.
    let s = parse("opencode", "opencode/storage/session/proj1/ses_aaa.json");

    assert_eq!(s.tool, "opencode");
    assert_eq!(s.title, "Add retry to the HTTP client"); // from session.title
    assert_eq!(s.project, "/home/dev/myapp"); // session.directory
    assert!(!s.subagent);

    // reasoning parts are dropped; text parts and the edit tool become messages
    // in id order; the malformed part file is skipped without panicking; the
    // patch part contributes no message (only provenance).
    assert_eq!(roles(&s), ["user", "assistant", "tool"]);
    assert!(s.messages[0]
        .text
        .contains("retry with exponential backoff"));
    assert!(s
        .messages
        .iter()
        .any(|m| m.text.contains("3-attempt retry")));
    assert!(s
        .messages
        .iter()
        .all(|m| !m.text.contains("thinking about"))); // reasoning dropped

    // Provenance: edit tool's state.input.filePath + the patch's files list.
    assert_eq!(
        s.touched,
        [
            "/home/dev/myapp/src/http/client.ts",
            "/home/dev/myapp/src/util.ts"
        ]
    );

    // epoch-millis timestamp is parsed (not an RFC3339 string).
    assert_eq!(
        s.started,
        chrono::DateTime::from_timestamp_millis(1718630400000)
    );
}

#[test]
fn missing_file_errors_without_panicking() {
    let adapter = adapters::by_name("codex").unwrap();
    assert!(adapter
        .parse(&fixture("codex/does-not-exist.jsonl"))
        .is_err());
}

#[test]
fn every_adapter_is_addressable_by_name() {
    for tool in ["claude-code", "codex", "gemini", "opencode"] {
        assert!(adapters::by_name(tool).is_some(), "{tool} should resolve");
    }
    assert!(adapters::by_name("nonexistent").is_none());
}
