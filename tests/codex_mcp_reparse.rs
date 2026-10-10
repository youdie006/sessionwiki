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
            "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind, archived_at)
             VALUES (?1, 5, 5, ?1, ?2, '/p', 't', '2026-10-01T00:00:00+00:00', NULL, 1, 'main', ?3)",
            params![path, tool, archived.then_some("2026-10-02T00:00:00+00:00")],
        )
        .unwrap();
    }
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
}
