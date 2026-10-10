//! A sessionwiki older than the index must leave it alone.
//!
//! The cache is rebuilt whenever its schema version differs from the binary's.
//! An older binary - typically a `sessionwiki mcp` server started before an
//! upgrade and still running - treated a NEWER index the same way: it dropped
//! the cache and rebuilt it in its own format, then the upgraded CLI rebuilt it
//! back, each time re-parsing every session.

use rusqlite::params;
use sessionwiki::index;

#[test]
fn an_index_written_by_a_newer_version_is_not_rebuilt() {
    let dir = std::env::temp_dir().join("sessionwiki-test-newer-index");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("SESSIONWIKI_DATA", &dir);

    let conn = index::open().unwrap();
    conn.execute(
        "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind)
         VALUES ('/store/a.jsonl', 0, 0, 'a', 'claude-code', '/p', 'a', '2026-10-01T00:00:00+00:00', NULL, 1, 'main')",
        params![],
    )
    .unwrap();
    let newer = index::SCHEMA_VERSION + 1;
    conn.pragma_update(None, "user_version", newer).unwrap();
    drop(conn);

    let opened = index::open();
    let err = opened.expect_err("an older binary must not open a newer index for writing");
    assert!(
        format!("{err:#}").contains("newer"),
        "the error should say the index is newer: {err:#}"
    );

    let raw = rusqlite::Connection::open(dir.join("index.db")).unwrap();
    let version: i64 = raw
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    let rows: i64 = raw
        .query_row("SELECT count(*) FROM files", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, newer, "the newer schema version was overwritten");
    assert_eq!(rows, 1, "the newer index's rows were dropped");
}
