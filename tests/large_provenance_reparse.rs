//! Sessions over the byte cap were indexed without the files they edited
//! between their head and tail, and a session that has ended never changes, so
//! it is never parsed again. The first open after upgrading marks the live
//! windowed rows for one re-parse.

use rusqlite::{params, Connection};
use sessionwiki::index;

fn mtime(conn: &Connection, path: &str) -> i64 {
    conn.query_row("SELECT mtime FROM files WHERE path = ?1", [path], |r| {
        r.get(0)
    })
    .unwrap()
}

#[test]
fn live_windowed_rows_are_reparsed_once_after_upgrading() {
    let dir = std::env::temp_dir().join("sessionwiki-test-large-provenance-reparse");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("SESSIONWIKI_DATA", &dir);

    let conn = index::open().unwrap();
    conn.execute(
        "DELETE FROM meta WHERE key = 'large_provenance_reparsed'",
        [],
    )
    .unwrap();
    for (path, tool, title, archived) in [
        (
            "/s/claude-large.jsonl",
            "claude-code",
            "[large] big work",
            false,
        ),
        (
            "/s/codex-large.jsonl",
            "codex",
            "[large] big rollout",
            false,
        ),
        (
            "/s/claude-gone.jsonl",
            "claude-code",
            "[large] deleted",
            true,
        ),
        ("/s/claude-small.jsonl", "claude-code", "small work", false),
    ] {
        conn.execute(
            "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind, archived_at)
             VALUES (?1, 5, 5, ?1, ?2, '/p', ?3, '2026-10-01T00:00:00+00:00', NULL, 1, 'main', ?4)",
            params![path, tool, title, archived.then_some("2026-10-02T00:00:00+00:00")],
        )
        .unwrap();
    }
    drop(conn);

    let conn = index::open().unwrap();
    assert_eq!(mtime(&conn, "/s/claude-large.jsonl"), -1);
    assert_eq!(mtime(&conn, "/s/codex-large.jsonl"), -1);
    assert_eq!(
        mtime(&conn, "/s/claude-gone.jsonl"),
        5,
        "no file to re-parse"
    );
    assert_eq!(
        mtime(&conn, "/s/claude-small.jsonl"),
        5,
        "read in full already"
    );

    conn.execute(
        "UPDATE files SET mtime = 7 WHERE path = '/s/claude-large.jsonl'",
        [],
    )
    .unwrap();
    drop(conn);
    let conn = index::open().unwrap();
    assert_eq!(mtime(&conn, "/s/claude-large.jsonl"), 7, "only once");
}
