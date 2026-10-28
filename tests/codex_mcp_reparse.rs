//! Codex sessions indexed before MCP failures were read show every failed MCP
//! call as ok, and a rollout that never changes is never parsed again. The
//! first open after upgrading marks the live Codex rows for one re-parse.

use rusqlite::{params, Connection};
use sessionwiki::index;

fn mtime(conn: &Connection, path: &str) -> i64 {
    conn.query_row("SELECT mtime FROM files WHERE path = ?1", [path], |r| {
        r.get(0)
    })
    .unwrap()
}

#[test]
fn live_codex_rows_are_reparsed_once_after_upgrading() {
    let dir = std::env::temp_dir().join("sessionwiki-test-codex-mcp-reparse");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("SESSIONWIKI_DATA", &dir);

    // An index from before the fix: rows present, the one-time step not done.
    let conn = index::open().unwrap();
    conn.execute("DELETE FROM meta WHERE key = 'codex_mcp_reparsed'", [])
        .unwrap();
    for (path, tool, archived) in [
        ("/s/codex-live.jsonl", "codex", false),
        ("/s/codex-gone.jsonl", "codex", true),
        ("/s/claude.jsonl", "claude-code", false),
    ] {
        conn.execute(
            // Parsed by this reader, so only the fix under test marks them.
            "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind, archived_at, reader)
             VALUES (?1, 5, 5, ?1, ?2, '/p', 't', '2026-10-01T00:00:00+00:00', NULL, 1, 'main', ?3, ?4)",
            params![path, tool, archived.then_some("2026-10-02T00:00:00+00:00"), index::READER],
        )
        .unwrap();
    }
    // An archived session keeps its messages; one without any is dropped on open.
    conn.execute(
        "INSERT INTO messages(session_id, role, text) VALUES ('/s/codex-gone.jsonl', 'user', 'hi')",
        [],
    )
    .unwrap();
    drop(conn);

    let conn = index::open().unwrap();
    assert_eq!(
        mtime(&conn, "/s/codex-live.jsonl"),
        -1,
        "the live Codex row is not marked for re-parse"
    );
    assert_eq!(
        mtime(&conn, "/s/codex-gone.jsonl"),
        5,
        "an archived row has no file to re-parse"
    );
    assert_eq!(
        mtime(&conn, "/s/claude.jsonl"),
        5,
        "other tools are untouched"
    );

    // Once: a re-parsed row is not marked again.
    conn.execute(
        "UPDATE files SET mtime = 7 WHERE path = '/s/codex-live.jsonl'",
        [],
    )
    .unwrap();
    drop(conn);
    let conn = index::open().unwrap();
    assert_eq!(
        mtime(&conn, "/s/codex-live.jsonl"),
        7,
        "the re-parse was requested again"
    );

    // An index re-parsed for an earlier Codex parser change (0.34.3 stored 1)
    // is re-parsed once more for the next one.
    conn.execute(
        "UPDATE meta SET value = '1' WHERE key = 'codex_mcp_reparsed'",
        [],
    )
    .unwrap();
    drop(conn);
    let conn = index::open().unwrap();
    assert_eq!(
        mtime(&conn, "/s/codex-live.jsonl"),
        -1,
        "an older re-parse did not trigger the new one"
    );
}
