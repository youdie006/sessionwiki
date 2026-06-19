//! `index::project_brief` must match the launch project EXACTLY (not substring),
//! so a SessionStart brief never bleeds in a sibling/child project's history.

use rusqlite::{params, Connection};
use sessionwiki::index;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn fresh() -> Connection {
    let dir = std::env::temp_dir().join("sessionwiki-test-hook-index");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("SESSIONWIKI_DATA", &dir);
    let conn = index::open().unwrap();
    conn.execute_batch("DELETE FROM files; DELETE FROM touched;")
        .unwrap();
    conn
}

fn seed(conn: &Connection, id: &str, project: &str, started: &str) {
    conn.execute(
        "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind)
         VALUES (?1, 0, 0, ?2, 'claude-code', ?3, ?2, ?4, ?4, 1, 'main')",
        params![format!("/store/{id}.jsonl"), id, project, started],
    )
    .unwrap();
}

#[test]
fn project_brief_is_exact_match_newest_first() {
    let _g = LOCK.lock().unwrap();
    let conn = fresh();
    seed(&conn, "old", "/home/me/app", "2026-06-08T10:00:00+00:00");
    seed(&conn, "new", "/home/me/app", "2026-06-10T10:00:00+00:00");
    seed(
        &conn,
        "sibling",
        "/home/me/app-staging",
        "2026-06-11T10:00:00+00:00",
    ); // substring trap
    seed(
        &conn,
        "child",
        "/home/me/app/sub",
        "2026-06-11T10:00:00+00:00",
    ); // parent-path trap

    let rows = index::project_brief(&conn, "/home/me/app", 5).unwrap();
    let ids: Vec<&str> = rows.iter().map(|r| r.session_id.as_str()).collect();
    assert_eq!(ids, vec!["new", "old"], "exact match only, newest first");

    assert!(index::project_brief(&conn, "/home/me/other", 5)
        .unwrap()
        .is_empty());
    // trailing slash normalizes to the same project
    assert_eq!(
        index::project_brief(&conn, "/home/me/app/", 5)
            .unwrap()
            .len(),
        2
    );
}
