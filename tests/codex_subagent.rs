//! Codex sub-agent rollouts indexed before the adapter read their marker were
//! stored as main sessions. The first open after upgrading reclassifies them
//! from each file's session_meta line, without re-parsing every session.

use rusqlite::params;
use sessionwiki::index;

/// SESSIONWIKI_DATA is process-wide; tests that set it take turns.
static DATA_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn opening_an_index_reclassifies_codex_subagent_rows() {
    let _data = DATA_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let files = tempfile::tempdir().unwrap();
    let rollout = |name: &str, meta: &str| {
        let path = files.path().join(name);
        std::fs::write(
            &path,
            format!("{{\"type\":\"session_meta\",\"payload\":{meta}}}\n"),
        )
        .unwrap();
        path
    };
    let sub = rollout(
        "sub.jsonl",
        r#"{"cwd":"/proj","thread_source":"subagent","agent_path":"/root/worker"}"#,
    );
    let main = rollout("main.jsonl", r#"{"cwd":"/proj","source":"cli"}"#);

    let data = tempfile::tempdir().unwrap();
    std::env::set_var("SESSIONWIKI_DATA", data.path());
    let conn = index::open().unwrap();
    for (id, path) in [("sub-1", &sub), ("main-1", &main)] {
        conn.execute(
            "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started, msg_count, kind)
             VALUES (?1, 0, 0, ?2, 'codex', '/proj', '(no user prompt)', '2026-10-07T00:00:00+00:00', 1, 'main')",
            params![path.to_string_lossy(), id],
        )
        .unwrap();
    }
    // As an index written by an older version: never reclassified.
    conn.execute("DELETE FROM meta WHERE key = 'codex_subagent_upto'", [])
        .unwrap();
    drop(conn);

    let conn = index::open().unwrap();
    let kind = |id: &str| -> String {
        conn.query_row("SELECT kind FROM files WHERE session_id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
    };
    assert_eq!(kind("sub-1"), "sub");
    assert_eq!(kind("main-1"), "main");
    let projects = index::projects(&conn).unwrap();
    assert_eq!(
        projects
            .iter()
            .find(|p| p.project == "/proj")
            .map(|p| p.sessions),
        Some(1),
        "a sub-agent thread counted as a project session"
    );
}

/// A `sessionwiki mcp` started before the upgrade keeps running the old
/// adapter for weeks, writing new sub-agent rollouts as main AFTER the
/// one-off pass. A once-only flag never looked at those rows again.
#[test]
fn a_row_written_after_the_pass_by_an_older_binary_is_reclassified() {
    let _data = DATA_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let files = tempfile::tempdir().unwrap();
    let sub = files.path().join("late-sub.jsonl");
    std::fs::write(
        &sub,
        "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/proj\",\"thread_source\":\"subagent\"}}\n",
    )
    .unwrap();
    let data = tempfile::tempdir().unwrap();
    std::env::set_var("SESSIONWIKI_DATA", data.path());
    // The upgrade's first open: the pass runs and records how far it checked.
    let conn = index::open().unwrap();
    conn.execute(
        "INSERT INTO files(path, mtime, size, session_id, tool, project, title, msg_count, kind)
         VALUES ('/store/earlier.jsonl', 0, 0, 'earlier', 'claude-code', '/proj', 't', 1, 'main')",
        [],
    )
    .unwrap();
    drop(conn);
    drop(index::open().unwrap());

    // An old binary still running indexes a new sub-agent rollout as main.
    let conn = index::open().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO files(path, mtime, size, session_id, tool, project, title, started, msg_count, kind)
         VALUES (?1, 0, 0, 'late-1', 'codex', '/proj', '(no user prompt)', '2026-10-07T00:00:00+00:00', 1, 'main')",
        params![sub.to_string_lossy()],
    )
    .unwrap();
    drop(conn);

    let conn = index::open().unwrap();
    let kind: String = conn
        .query_row(
            "SELECT kind FROM files WHERE session_id = 'late-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        kind, "sub",
        "a late write from an older binary was never revisited"
    );
}

/// A schema bump rebuilds `files`, and rowids start over at 1. A mark left
/// from before the rebuild would sit above every new row and skip them.
#[test]
fn a_cache_rebuild_restarts_the_check() {
    let _data = DATA_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let files = tempfile::tempdir().unwrap();
    let sub = files.path().join("after-rebuild.jsonl");
    std::fs::write(
        &sub,
        "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/proj\",\"thread_source\":\"subagent\"}}\n",
    )
    .unwrap();
    let data = tempfile::tempdir().unwrap();
    std::env::set_var("SESSIONWIKI_DATA", data.path());
    let conn = index::open().unwrap();
    for n in 0..5 {
        conn.execute(
            "INSERT INTO files(path, mtime, size, session_id, tool, project, title, msg_count, kind)
             VALUES (?1, 0, 0, ?1, 'claude-code', '/proj', 't', 1, 'main')",
            params![format!("/store/filler-{n}.jsonl")],
        )
        .unwrap();
    }
    drop(conn);
    // The pass marks rowid 5 as checked; then a schema bump drops the cache.
    drop(index::open().unwrap());
    let conn = index::open().unwrap();
    conn.pragma_update(None, "user_version", 0).unwrap();
    drop(conn);
    let conn = index::open().unwrap();
    conn.execute(
        "INSERT INTO files(path, mtime, size, session_id, tool, project, title, msg_count, kind)
         VALUES (?1, 0, 0, 'rebuilt-1', 'codex', '/proj', '(no user prompt)', 1, 'main')",
        params![sub.to_string_lossy()],
    )
    .unwrap();
    drop(conn);

    let conn = index::open().unwrap();
    let kind: String = conn
        .query_row(
            "SELECT kind FROM files WHERE session_id = 'rebuilt-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kind, "sub", "a row written after a rebuild was skipped");
}
