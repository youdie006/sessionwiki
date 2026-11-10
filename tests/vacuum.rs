use sessionwiki::{commands, index};
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn fresh_index() -> (tempfile::TempDir, rusqlite::Connection) {
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("SESSIONWIKI_DATA", dir.path());
    let conn = index::open().unwrap();
    (dir, conn)
}

#[test]
fn vacuum_preserves_data_and_search_results() {
    let _guard = LOCK.lock().unwrap();
    let (_dir, conn) = fresh_index();

    // Insert sample metadata, files, messages, and FTS entries
    conn.execute(
        "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started, msg_count, kind)
         VALUES ('/path/to/test.json', 1000, 500, 'session-v1', 'claude-code', 'proj', 'Refactor auth', '2026-10-24T12:00:00Z', 2, 'main')",
        [],
    ).unwrap();

    conn.execute(
        "INSERT INTO messages(session_id, role, text)
         VALUES ('session-v1', 'user', 'Please implement OAuth2 token verification')",
        [],
    ).unwrap();

    conn.execute(
        "INSERT INTO messages(session_id, role, text)
         VALUES ('session-v1', 'assistant', 'Implemented OAuth2 verification in auth.rs')",
        [],
    ).unwrap();

    conn.execute(
        "INSERT INTO msgs(rowid, text) VALUES (1, 'Please implement OAuth2 token verification')",
        [],
    ).unwrap();

    conn.execute(
        "INSERT INTO msgs(rowid, text) VALUES (2, 'Implemented OAuth2 verification in auth.rs')",
        [],
    ).unwrap();

    drop(conn);

    // Run vacuum command
    commands::vacuum().expect("vacuum should succeed");

    // Reopen and verify data and FTS search results are intact
    let conn = index::open().unwrap();

    let count: i64 = conn
        .query_row("SELECT count(*) FROM files WHERE session_id = 'session-v1'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);

    let msg_count: i64 = conn
        .query_row("SELECT count(*) FROM messages WHERE session_id = 'session-v1'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(msg_count, 2);

    // Verify FTS search works on preserved tables
    let search_hits: i64 = conn
        .query_row("SELECT count(*) FROM msgs WHERE msgs MATCH 'OAuth2'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(search_hits, 2);
}

#[test]
fn vacuum_insufficient_space_preflights() {
    let _guard = LOCK.lock().unwrap();
    let (_dir, conn) = fresh_index();

    conn.execute(
        "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started, msg_count, kind)
         VALUES ('/path/to/test.json', 1000, 500, 'session-space', 'claude-code', 'proj', 'Space test', '2026-10-24T12:00:00Z', 1, 'main')",
        [],
    ).unwrap();
    drop(conn);

    // Simulate insufficient free space (1 byte)
    std::env::set_var("SESSIONWIKI_SIMULATE_FREE_SPACE", "1");
    let err = commands::vacuum().expect_err("vacuum must fail when disk space is insufficient");
    std::env::remove_var("SESSIONWIKI_SIMULATE_FREE_SPACE");

    let msg = err.to_string();
    assert!(
        msg.contains("insufficient disk space"),
        "error message should indicate insufficient disk space, got: {msg}"
    );
}

#[test]
fn vacuum_lock_contention_reports_busy() {
    let _guard = LOCK.lock().unwrap();
    let (_dir, conn1) = fresh_index();

    // Insert data on connection 1
    conn1.execute(
        "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started, msg_count, kind)
         VALUES ('/path/to/test.json', 1000, 500, 'session-lock', 'claude-code', 'proj', 'Lock test', '2026-10-24T12:00:00Z', 1, 'main')",
        [],
    ).unwrap();

    // Start a read transaction on connection 1 (holding a read lock on WAL)
    let tx1 = conn1.unchecked_transaction().unwrap();
    let _val: i64 = tx1.query_row("SELECT count(*) FROM files", [], |r| r.get(0)).unwrap();

    // Connection 2 attempts WAL checkpoint with truncate while connection 1 is actively reading
    let conn2 = index::open().unwrap();

    // We also make a write to WAL so there is an active WAL page to checkpoint
    conn2.execute(
        "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started, msg_count, kind)
         VALUES ('/path/to/test2.json', 1000, 500, 'session-lock2', 'claude-code', 'proj', 'Lock test 2', '2026-10-24T12:00:00Z', 1, 'main')",
        [],
    ).unwrap();

    let (busy, _log, _checkpointed): (i32, i32, i32) = conn2
        .query_row("PRAGMA wal_checkpoint(TRUNCATE);", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();

    // Connection 1 held an open transaction reading the database, so the checkpoint cannot truncate
    assert_eq!(busy, 1, "WAL checkpoint should report busy == 1 under reader lock contention");

    drop(tx1);
    drop(conn1);
    drop(conn2);
}
