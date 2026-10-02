//! `projects` LAST is when a project was last worked in, the same clock the
//! session list sorts by (`COALESCE(ended, started)`), not when its newest
//! session happened to begin.

use rusqlite::{params, Connection};
use sessionwiki::index;

fn fresh() -> Connection {
    let dir = std::env::temp_dir().join("sessionwiki-test-projects-last-active");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("SESSIONWIKI_DATA", &dir);
    let conn = index::open().unwrap();
    conn.execute_batch("DELETE FROM files;").unwrap();
    conn
}

fn seed(conn: &Connection, id: &str, project: &str, started: &str, ended: Option<&str>) {
    conn.execute(
        "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind)
         VALUES (?1, 0, 0, ?2, 'claude-code', ?3, ?2, ?4, ?5, 1, 'main')",
        params![format!("/store/{id}.jsonl"), id, project, started, ended],
    )
    .unwrap();
}

#[test]
fn a_long_session_still_running_makes_its_project_the_latest() {
    let conn = fresh();
    // /proj/long: one session begun in May and worked in until yesterday.
    seed(
        &conn,
        "long",
        "/proj/long",
        "2026-05-01T09:00:00+00:00",
        Some("2026-10-01T18:00:00+00:00"),
    );
    // /proj/short: one short session in September.
    seed(
        &conn,
        "short",
        "/proj/short",
        "2026-09-20T09:00:00+00:00",
        Some("2026-09-20T10:00:00+00:00"),
    );
    // A session with no recorded end still counts by its start.
    seed(
        &conn,
        "open",
        "/proj/open",
        "2026-09-25T09:00:00+00:00",
        None,
    );

    let projects = index::projects(&conn).unwrap();
    let long = projects.iter().find(|p| p.project == "/proj/long").unwrap();
    assert_eq!(
        long.newest.as_deref(),
        Some("2026-10-01T18:00:00+00:00"),
        "LAST is when the project was last worked in"
    );
    let open = projects.iter().find(|p| p.project == "/proj/open").unwrap();
    assert_eq!(open.newest.as_deref(), Some("2026-09-25T09:00:00+00:00"));
    let order: Vec<&str> = projects.iter().map(|p| p.project.as_str()).collect();
    assert_eq!(
        order,
        ["/proj/long", "/proj/open", "/proj/short"],
        "equally busy projects are ordered by last activity"
    );
}
