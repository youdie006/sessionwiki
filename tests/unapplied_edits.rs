//! A file an edit was attempted on but never changed is not a file the session
//! edited. On one real machine 271 of 6695 session-file links came only from
//! Edit/Write calls the user rejected or that failed ("File has not been read
//! yet", a subagent told to return findings as text), so `trace` and `blame`
//! named sessions that never wrote those files.

use serde_json::json;
use sessionwiki::adapters;

fn line(v: serde_json::Value) -> String {
    v.to_string() + "\n"
}

fn tool_use(id: &str, name: &str, path: &str) -> String {
    line(
        json!({"type":"assistant","timestamp":"2026-10-25T00:00:00Z","message":{"role":"assistant","content":[
        {"type":"tool_use","id":id,"name":name,"input":{"file_path":path,"old_string":"a","new_string":"b","content":"x"}}]}}),
    )
}

fn result(id: &str, error: bool, text: &str) -> String {
    line(
        json!({"type":"user","timestamp":"2026-10-25T00:00:01Z","message":{"role":"user","content":[
        {"type":"tool_result","tool_use_id":id,"is_error":error,"content":text}]}}),
    )
}

#[test]
fn rejected_and_failed_edits_do_not_count_as_touched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut s = line(
        json!({"type":"user","timestamp":"2026-10-25T00:00:00Z","cwd":"/p","message":{"role":"user","content":"fix it"}}),
    );
    s += &tool_use("t1", "Edit", "/p/applied.rs");
    s += &result("t1", false, "The file /p/applied.rs has been updated.");
    s += &tool_use("t2", "Write", "/p/rejected.md");
    s += &result(
        "t2",
        true,
        "The user doesn't want to proceed with this tool use. The tool use was rejected",
    );
    s += &tool_use("t3", "Edit", "/p/unread.rs");
    s += &result("t3", true, "<tool_use_error>File has not been read yet. Read it first before writing to it.</tool_use_error>");
    // Failed once, then applied: the file was edited.
    s += &tool_use("t4", "Edit", "/p/retried.rs");
    s += &result(
        "t4",
        true,
        "<tool_use_error>String to replace not found in file.</tool_use_error>",
    );
    s += &tool_use("t5", "Edit", "/p/retried.rs");
    s += &result("t5", false, "The file /p/retried.rs has been updated.");
    // A call whose result never arrived (session cut off) still counts.
    s += &tool_use("t6", "Write", "/p/cut-off.rs");
    std::fs::write(&path, s).unwrap();

    let a = adapters::by_name("claude-code").unwrap();
    let session = adapters::parse_session(a.as_ref(), &path).unwrap();
    let mut touched = session.touched.clone();
    touched.sort();
    assert_eq!(touched, ["/p/applied.rs", "/p/cut-off.rs", "/p/retried.rs"]);
    let mut edited: Vec<_> = session.edits.iter().map(|e| e.path.as_str()).collect();
    edited.sort();
    edited.dedup();
    assert_eq!(edited, ["/p/applied.rs", "/p/cut-off.rs", "/p/retried.rs"]);
}
