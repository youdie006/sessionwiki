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
         DELETE FROM notes; DELETE FROM touched; DELETE FROM archive;",
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

// The crux of archive mode: the durable `archive` table must survive a schema
// bump (which drops the disposable cache) and rehydrate the cache, or a version
// bump would silently destroy exactly the sessions that cannot be re-derived
// from disk. This test forces a bump and asserts an archived session comes back
// fully - listable, traceable, searchable, readable.
#[test]
fn archive_survives_schema_bump_rehydration() {
    let _g = LOCK.lock().unwrap();
    let conn = fresh_index();
    conn.execute(
        "INSERT INTO archive
         (session_id, path, mtime, size, tool, project, title, started, ended,
          msg_count, kind, transcript, touched, archived_at)
         VALUES ('arc1','/gone/s.jsonl',0,0,'codex','/proj/api','rate limiter fix',
                 '2026-06-10T10:00:00+00:00','2026-06-10T10:05:00+00:00',2,'main',
                 ?1, ?2, '2026-06-12T00:00:00Z')",
        params![
            r#"[["user","why does the rate limiter go negative"],["assistant","fixed the off-by-one in rate_limiter.rs"]]"#,
            r#"["/proj/api/src/rate_limiter.rs"]"#
        ],
    )
    .unwrap();
    drop(conn);

    // Pretend the on-disk schema is stale, forcing a drop-and-rebuild on reopen.
    {
        let conn = index::open().unwrap();
        conn.pragma_update(None, "user_version", 0i64).unwrap();
    }
    let conn = index::open().unwrap();

    // Back in the live cache, flagged archived.
    let rows = index::resolve(&conn, "arc1").unwrap();
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].archived,
        "rehydrated session must be flagged archived"
    );
    assert_eq!(rows[0].title, "rate limiter fix");

    // Provenance survived: trace still finds it.
    assert_eq!(
        index::files_for(&conn, "arc1").unwrap(),
        ["/proj/api/src/rate_limiter.rs"]
    );
    let hits = index::sessions_for_file(&conn, "src/rate_limiter.rs", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0.session_id, "arc1");

    // Transcript survived: search still finds it, and it reads back.
    let s = index::search(&conn, "negative", 10, None, None).unwrap();
    assert!(
        s.iter().any(|h| h.row.session_id == "arc1"),
        "archived transcript must stay searchable after a rebuild"
    );
    let sess = index::session_from_index(&conn, &rows[0]).unwrap();
    assert_eq!(sess.messages.len(), 2);
    assert_eq!(sess.tool, "codex");
}

// End-to-end: a real session on disk is indexed, the tool deletes it, the next
// sync archives it (does not lose it), it stays traceable/searchable/readable
// from the index, and `forget` finally removes it.
#[test]
fn archive_on_prune_serves_and_forgets() {
    let _g = LOCK.lock().unwrap();
    let home = std::env::temp_dir().join("sessionwiki-test-arc-home");
    let _ = std::fs::remove_dir_all(&home);
    let proj = home.join(".claude/projects/myproj");
    std::fs::create_dir_all(&proj).unwrap();
    let sess = proj.join("aaaaaaaa-0000-4000-8000-000000000abc.jsonl");
    std::fs::write(
        &sess,
        concat!(
            r#"{"type":"summary","summary":"Fix the login bug","leafUuid":"x"}"#,
            "\n",
            r#"{"type":"user","cwd":"/home/dev/app","timestamp":"2026-06-10T10:00:00.000Z","message":{"role":"user","content":"login throws on empty password"}}"#,
            "\n",
            r#"{"type":"assistant","cwd":"/home/dev/app","timestamp":"2026-06-10T10:00:05.000Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/home/dev/app/src/login.rs"}}]}}"#,
            "\n",
            r#"{"type":"assistant","cwd":"/home/dev/app","timestamp":"2026-06-10T10:00:10.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Guarded the empty-password case."}]}}"#,
            "\n",
        ),
    )
    .unwrap();

    std::env::set_var("HOME", &home);
    let data = std::env::temp_dir().join("sessionwiki-test-arc-data");
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data).unwrap();
    std::env::set_var("SESSIONWIKI_DATA", &data);

    let mut conn = index::open().unwrap();
    index::sync(&mut conn, Some("claude-code")).unwrap();

    let rows = index::recent(&conn, 50, None, None, None, false).unwrap();
    let sid = rows
        .iter()
        .find(|r| r.title.contains("login"))
        .map(|r| r.session_id.clone())
        .expect("session indexed");
    assert!(!rows.iter().find(|r| r.session_id == sid).unwrap().archived);
    assert_eq!(
        index::files_for(&conn, &sid).unwrap(),
        ["/home/dev/app/src/login.rs"]
    );

    // The tool prunes the session.
    std::fs::remove_file(&sess).unwrap();
    index::sync(&mut conn, Some("claude-code")).unwrap();

    // Kept, flagged archived; trace and search still work.
    let r = index::resolve(&conn, &sid)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert!(r.archived, "pruned session must be archived, not deleted");
    assert!(
        index::sessions_for_file(&conn, "src/login.rs", 10)
            .unwrap()
            .iter()
            .any(|(s, _)| s.session_id == sid),
        "trace must survive the prune"
    );
    assert!(
        index::search(&conn, "empty password", 10, None, None)
            .unwrap()
            .iter()
            .any(|h| h.row.session_id == sid),
        "search must survive the prune"
    );

    // Readable from the index even though the file is gone.
    let obj = index::session_from_index(&conn, &r).unwrap();
    assert!(obj
        .messages
        .iter()
        .any(|m| m.text.contains("empty-password")));

    // forget removes it for good.
    index::forget(&mut conn, &sid).unwrap();
    assert!(index::resolve(&conn, &sid).unwrap().is_empty());

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&data);
}
