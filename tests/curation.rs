//! Tests for the curation / session-engineering layer: tags, notes, related
//! sessions, and the project/stats rollups.
//!
//! These build a small index in a throwaway data directory (no real session
//! stores involved) by inserting rows the same way `sync` would, then exercise
//! the public query API.

use rusqlite::{params, Connection};
use sessionwiki::index;
use std::sync::Mutex;

// `index::open()` reads the SESSIONWIKI_DATA env var, which is process-global,
// so these tests share one index and run under a single lock.
static LOCK: Mutex<()> = Mutex::new(());

fn seed(conn: &Connection, id: &str, tool: &str, project: &str, title: &str, started: &str) {
    conn.execute(
        "INSERT OR REPLACE INTO files
         (path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind)
         VALUES (?1, 0, 0, ?2, ?3, ?4, ?5, ?6, ?6, 2, 'main')",
        params![
            format!("/fake/{id}.jsonl"),
            id,
            tool,
            project,
            title,
            started
        ],
    )
    .unwrap();
    // one assistant message so message-derived columns have something to read
    conn.execute(
        "INSERT INTO messages(session_id, role, text) VALUES (?1, 'assistant', ?2)",
        params![id, format!("outcome for {title}")],
    )
    .unwrap();
}

fn fresh_index() -> Connection {
    let dir = std::env::temp_dir().join("sessionwiki-test-curation");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("SESSIONWIKI_DATA", &dir);
    let conn = index::open().unwrap();
    conn.execute_batch(
        "DELETE FROM files; DELETE FROM messages; DELETE FROM tags;
         DELETE FROM notes; DELETE FROM touched;",
    )
    .unwrap();
    conn
}

fn touch(conn: &Connection, id: &str, path: &str) {
    conn.execute(
        "INSERT OR IGNORE INTO touched(session_id, path) VALUES (?1, ?2)",
        params![id, path],
    )
    .unwrap();
}

#[test]
fn tags_round_trip_and_counts() {
    let _g = LOCK.lock().unwrap();
    let conn = fresh_index();
    seed(
        &conn,
        "a1",
        "codex",
        "/proj/api",
        "fix auth",
        "2026-06-10T10:00:00+00:00",
    );

    index::add_tag(&conn, "a1", "Auth").unwrap(); // case-folded on write
    index::add_tag(&conn, "a1", "bug").unwrap();
    index::add_tag(&conn, "a1", "auth").unwrap(); // dup ignored

    let counts = index::tag_counts(&conn).unwrap();
    assert!(counts.iter().any(|(t, n)| t == "auth" && *n == 1));
    assert!(counts.iter().any(|(t, n)| t == "bug" && *n == 1));

    assert_eq!(index::remove_tag(&conn, "a1", "bug").unwrap(), 1);
    let counts = index::tag_counts(&conn).unwrap();
    assert!(!counts.iter().any(|(t, _)| t == "bug"));
}

#[test]
fn notes_round_trip() {
    let _g = LOCK.lock().unwrap();
    let conn = fresh_index();
    seed(
        &conn,
        "n1",
        "codex",
        "/proj/api",
        "task",
        "2026-06-10T10:00:00+00:00",
    );
    assert!(index::note_for(&conn, "n1").unwrap().is_none());
    index::set_note(&conn, "n1", "revisit the retry logic").unwrap();
    assert_eq!(
        index::note_for(&conn, "n1").unwrap().as_deref(),
        Some("revisit the retry logic")
    );
    index::set_note(&conn, "n1", "done").unwrap(); // replace
    assert_eq!(
        index::note_for(&conn, "n1").unwrap().as_deref(),
        Some("done")
    );
}

#[test]
fn related_prefers_same_project_then_shared_tags() {
    let _g = LOCK.lock().unwrap();
    let conn = fresh_index();
    seed(
        &conn,
        "p1",
        "codex",
        "/proj/api",
        "cors fix",
        "2026-06-10T10:00:00+00:00",
    );
    seed(
        &conn,
        "p2",
        "codex",
        "/proj/api",
        "rate limiter",
        "2026-06-09T10:00:00+00:00",
    );
    seed(
        &conn,
        "p3",
        "claude-code",
        "/proj/web",
        "ui work",
        "2026-06-08T10:00:00+00:00",
    );
    seed(
        &conn,
        "p4",
        "gemini",
        "/proj/other",
        "unrelated",
        "2026-06-07T10:00:00+00:00",
    );

    // same project => p2 is related to p1; p3/p4 are not (no shared project/tag)
    let rel = index::related(&conn, "p1", 10).unwrap();
    let ids: Vec<&str> = rel.iter().map(|r| r.session_id.as_str()).collect();
    assert!(
        ids.contains(&"p2"),
        "same-project session should be related"
    );
    assert!(!ids.contains(&"p4"), "unrelated project should not appear");

    // shared tag links across projects: tag p1 and p3 the same
    index::add_tag(&conn, "p1", "spike").unwrap();
    index::add_tag(&conn, "p3", "spike").unwrap();
    let rel = index::related(&conn, "p1", 10).unwrap();
    let ids: Vec<&str> = rel.iter().map(|r| r.session_id.as_str()).collect();
    assert!(
        ids.contains(&"p3"),
        "shared-tag session should now be related"
    );
    assert!(!ids.contains(&"p1"), "a session is never related to itself");
}

#[test]
fn provenance_files_trace_and_shared_file_relatedness() {
    let _g = LOCK.lock().unwrap();
    let conn = fresh_index();
    // Two sessions in different projects both edit the same file (e.g. a
    // shared lib); a third edits something else.
    seed(
        &conn,
        "f1",
        "claude-code",
        "/proj/api",
        "add auth guard",
        "2026-06-10T10:00:00+00:00",
    );
    seed(
        &conn,
        "f2",
        "codex",
        "/proj/web",
        "reuse auth helper",
        "2026-06-11T10:00:00+00:00",
    );
    seed(
        &conn,
        "f3",
        "codex",
        "/proj/web",
        "unrelated",
        "2026-06-09T10:00:00+00:00",
    );
    touch(&conn, "f1", "/home/me/proj/src/auth.rs");
    touch(&conn, "f1", "/home/me/proj/src/lib.rs");
    touch(&conn, "f2", "/home/me/proj/src/auth.rs");
    touch(&conn, "f3", "/home/me/proj/src/other.rs");

    // files_for: a session's own edits, order preserved.
    assert_eq!(
        index::files_for(&conn, "f1").unwrap(),
        ["/home/me/proj/src/auth.rs", "/home/me/proj/src/lib.rs"]
    );

    // trace by suffix: a relative path finds the absolute stored path, both
    // sessions that touched it, newest first (f2 then f1).
    let hits = index::sessions_for_file(&conn, "src/auth.rs", 10).unwrap();
    let ids: Vec<&str> = hits.iter().map(|(r, _)| r.session_id.as_str()).collect();
    assert_eq!(ids, ["f2", "f1"]);
    assert_eq!(hits[0].1, "/home/me/proj/src/auth.rs"); // matched path reported
                                                        // a file only one session touched
    let one = index::sessions_for_file(&conn, "src/other.rs", 10).unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].0.session_id, "f3");

    // related now links across projects via the shared file: f2 is related to
    // f1 even though they are in different projects and share no tag.
    let rel = index::related(&conn, "f1", 10).unwrap();
    let ids: Vec<&str> = rel.iter().map(|r| r.session_id.as_str()).collect();
    assert!(
        ids.contains(&"f2"),
        "session editing the same file should be related across projects"
    );
    assert!(
        !ids.contains(&"f3"),
        "session touching only other files should not be related"
    );

    // stats counts distinct linked files (auth.rs, lib.rs, other.rs = 3).
    assert_eq!(index::stats(&conn).unwrap().files, 3);
}

#[test]
fn projects_and_stats_rollups() {
    let _g = LOCK.lock().unwrap();
    let conn = fresh_index();
    seed(
        &conn,
        "s1",
        "codex",
        "/proj/api",
        "a",
        "2026-06-10T10:00:00+00:00",
    );
    seed(
        &conn,
        "s2",
        "codex",
        "/proj/api",
        "b",
        "2026-05-10T10:00:00+00:00",
    );
    seed(
        &conn,
        "s3",
        "gemini",
        "/proj/web",
        "c",
        "2026-06-10T10:00:00+00:00",
    );

    let projects = index::projects(&conn).unwrap();
    let api = projects.iter().find(|p| p.project == "/proj/api").unwrap();
    assert_eq!(api.sessions, 2);
    assert_eq!(projects[0].project, "/proj/api"); // busiest first

    let st = index::stats(&conn).unwrap();
    assert_eq!(st.total_sessions, 3);
    assert_eq!(st.projects, 2);
    assert!(st.per_tool.iter().any(|(t, n, _)| t == "codex" && *n == 2));
    assert!(st.per_month.iter().any(|(ym, _)| ym == "2026-06"));
}
