//! A reader fix reaches sessions already indexed only if their rows are parsed
//! again, and a session that never changes is never parsed again. 0.34.9 read
//! Codex history calls declined at approval as failures and applied Gemini
//! `/rewind` records; the first open after upgrading marks exactly the rows
//! those readers produce - Codex history threads and Gemini chats - until
//! this version has parsed them.

use rusqlite::{params, Connection};
use sessionwiki::index;

fn mtime(conn: &Connection, path: &str) -> i64 {
    conn.query_row("SELECT mtime FROM files WHERE path = ?1", [path], |r| {
        r.get(0)
    })
    .unwrap()
}

#[test]
fn rows_of_a_fixed_reader_are_reparsed_once_after_upgrading() {
    let dir = std::env::temp_dir().join("sessionwiki-test-reader-fix-reparse");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("SESSIONWIKI_DATA", &dir);

    let conn = index::open().unwrap();
    let history = "/h/thread_history_1.sqlite\u{1f}019a0000";
    for (path, tool, archived) in [
        (history, "codex", false),
        ("/s/rollout.jsonl", "codex", false),
        ("/g/chats/session-a.jsonl", "gemini", false),
        ("/g/chats/session-gone.json", "gemini", true),
        ("/c/claude.jsonl", "claude-code", false),
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
        mtime(&conn, history),
        -1,
        "a Codex history thread is re-read"
    );
    assert_eq!(
        mtime(&conn, "/g/chats/session-a.jsonl"),
        -1,
        "a Gemini chat is re-read"
    );
    assert_eq!(
        mtime(&conn, "/s/rollout.jsonl"),
        5,
        "rollouts are not touched"
    );
    assert_eq!(
        mtime(&conn, "/g/chats/session-gone.json"),
        5,
        "archived rows stay"
    );
    assert_eq!(mtime(&conn, "/c/claude.jsonl"), 5);

    conn.execute(
        "UPDATE files SET mtime = 7, reader = ?2 WHERE path IN (?1, '/g/chats/session-a.jsonl')",
        params![history, index::READER],
    )
    .unwrap();
    drop(conn);
    let conn = index::open().unwrap();
    assert_eq!(mtime(&conn, history), 7, "only once");
    assert_eq!(mtime(&conn, "/g/chats/session-a.jsonl"), 7, "only once");
}
