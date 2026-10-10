//! Claude Code keeps a workflow's bookkeeping beside its agents' transcripts,
//! at `<session>/subagents/workflows/wf_*/journal.jsonl`: one `started` and one
//! `result` record per agent, no conversation. Every `.jsonl` under the store
//! was indexed as a session, so each journal became an empty subagent session
//! whose project was the `wf_*` folder name - 257 of them on one machine - and
//! when one was deleted it was archived, although archiving is documented as
//! skipping a session with nothing indexed.

use std::fs;
use std::path::Path;

use sessionwiki::index;

fn rows(conn: &rusqlite::Connection, like: &str) -> Vec<bool> {
    let mut s = conn
        .prepare("SELECT archived_at IS NOT NULL FROM files WHERE path LIKE ?1")
        .unwrap();
    s.query_map([format!("%{like}%")], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn a_workflow_journal_is_not_a_session_and_an_empty_one_is_not_archived() {
    let home = std::env::temp_dir().join("sessionwiki-test-workflow-journal");
    let _ = fs::remove_dir_all(&home);
    let proj = home.join(".claude/projects/proj-a");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude-code/proj-a/0a000000-0000-4000-8000-000000000001.jsonl");
    let workflow = proj.join("0a000000-0000-4000-8000-000000000001/subagents/workflows/wf_1");
    fs::create_dir_all(&workflow).unwrap();
    fs::copy(
        &fixture,
        proj.join("0a000000-0000-4000-8000-000000000001.jsonl"),
    )
    .unwrap();
    fs::copy(&fixture, workflow.join("agent-a1.jsonl")).unwrap();
    fs::write(
        workflow.join("journal.jsonl"),
        "{\"type\":\"started\",\"key\":\"k\",\"agentId\":\"a1\"}\n\
         {\"type\":\"result\",\"key\":\"k\",\"agentId\":\"a1\",\"result\":\"done\"}\n",
    )
    .unwrap();
    // A transcript with nothing in it to index.
    let empty = proj.join("0b000000-0000-4000-8000-000000000002.jsonl");
    fs::write(&empty, "{\"type\":\"file-history-snapshot\"}\n").unwrap();

    std::env::set_var("HOME", &home);
    std::env::set_var("SESSIONWIKI_DATA", home.join("data"));
    let mut conn = index::open().unwrap();
    index::sync(&mut conn, Some("claude-code")).unwrap();

    assert_eq!(
        rows(&conn, "agent-a1"),
        vec![false],
        "the agent transcript is indexed"
    );
    assert!(
        rows(&conn, "journal.jsonl").is_empty(),
        "the workflow journal was indexed as a session"
    );
    assert_eq!(rows(&conn, "0b000000"), vec![false]);

    fs::remove_file(&empty).unwrap();
    index::sync(&mut conn, Some("claude-code")).unwrap();
    assert!(
        rows(&conn, "0b000000").is_empty(),
        "a gone session with nothing indexed was kept as archived"
    );
}
