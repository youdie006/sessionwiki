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

/// HOME and SESSIONWIKI_DATA are process-wide; tests that set them take turns.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

/// An older version archived every gone session, the empty journals included,
/// and #58 only stopped new ones: those already archived stayed, each listed
/// with its `wf_*` folder as a project - 81 on one machine. Opening the index
/// drops an archived session with no transcript, unless the user tagged,
/// noted or summarized it.
#[test]
fn an_empty_session_an_older_version_archived_is_dropped_on_open() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let data = tempfile::tempdir().unwrap();
    std::env::set_var("SESSIONWIKI_DATA", data.path());
    let conn = index::open().unwrap();
    for (sid, transcript) in [
        ("journal", "[]"),
        ("kept", "[[\"user\",\"hi\"]]"),
        ("noted", "[]"),
        ("summarized", "[]"),
        ("tagged", "[]"),
    ] {
        conn.execute(
            "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started,
                               msg_count, kind, archived_at)
             VALUES (?1, 0, 0, ?1, 'claude-code', 'wf_1', 't', '2026-10-01T10:00:00+00:00',
                     0, 'sub', '2026-10-02 10:00:00')",
            [sid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO archive(session_id, path, mtime, size, tool, project, title, started,
                                 msg_count, kind, transcript, touched, archived_at)
             VALUES (?1, ?1, 0, 0, 'claude-code', 'wf_1', 't', '2026-10-01T10:00:00+00:00',
                     0, 'sub', ?2, '[]', '2026-10-02 10:00:00')",
            [sid, transcript],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO messages(session_id, role, text) VALUES ('kept', 'user', 'hi')",
        [],
    )
    .unwrap();
    for sql in [
        "INSERT INTO tags(session_id, tag) VALUES ('tagged', 'keep')",
        "INSERT INTO notes(session_id, note, updated) VALUES ('noted', 'n', '2026-10-03')",
        "INSERT INTO summaries(session_id, summary, created) VALUES ('summarized', 's', '2026-10-03')",
    ] {
        conn.execute(sql, []).unwrap();
    }
    drop(conn);

    let conn = index::open().unwrap();
    let left = |table: &str| -> Vec<String> {
        let mut s = conn
            .prepare(&format!(
                "SELECT session_id FROM {table} ORDER BY session_id"
            ))
            .unwrap();
        s.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let (files, archive) = (left("files"), left("archive"));
    std::env::remove_var("SESSIONWIKI_DATA");
    let want = ["kept", "noted", "summarized", "tagged"];
    assert_eq!(files, want);
    assert_eq!(archive, want);
}
