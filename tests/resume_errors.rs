//! resume's error paths must diagnose correctly: a tool without headless
//! resume gets the "cannot be resumed" message even though its stored path is
//! a shared-store key (not a real file), while a genuinely deleted file for a
//! resumable tool still reports "file is gone".

use rusqlite::{params, Connection};
use sessionwiki::{commands, index};
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn fresh() -> Connection {
    let dir = std::env::temp_dir().join("sessionwiki-test-resume-errors");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("SESSIONWIKI_DATA", &dir);
    let conn = index::open().unwrap();
    conn.execute_batch("DELETE FROM files;").unwrap();
    conn
}

fn seed(conn: &Connection, id: &str, tool: &str, path: &str) {
    conn.execute(
        "INSERT OR REPLACE INTO files
         (path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind)
         VALUES (?1, 0, 0, ?2, ?3, '/proj/api', 't',
                 '2026-06-10T10:00:00+00:00', '2026-06-10T10:00:00+00:00', 1, 'main')",
        params![path, id, tool],
    )
    .unwrap();
}

#[test]
fn unsupported_tool_reports_headless_not_deleted_file() {
    let _g = LOCK.lock().unwrap();
    let conn = fresh();
    // An aider row's stored path is "<file>\u{1f}<run>" - a key, not a file, so
    // exists() is false. The error must say "no headless resume", not lie that
    // the tool deleted the session file.
    seed(
        &conn,
        "aid1",
        "aider",
        "/proj/api/.aider.chat.history.md\u{1f}0",
    );
    drop(conn);
    let err = commands::resume_cmd("aid1", true, true)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("cannot be resumed headlessly"),
        "wrong diagnosis: {err}"
    );
    assert!(!err.contains("session file is gone"), "misdiagnosed: {err}");
}

#[test]
fn deleted_file_for_resumable_tool_still_reports_file_gone() {
    let _g = LOCK.lock().unwrap();
    let conn = fresh();
    seed(
        &conn,
        "cc1",
        "claude-code",
        "/nonexistent/11111111-1111-1111-1111-111111111111.jsonl",
    );
    drop(conn);
    let err = commands::resume_cmd("cc1", true, true)
        .unwrap_err()
        .to_string();
    assert!(err.contains("session file is gone"), "wrong error: {err}");
}
