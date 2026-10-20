//! `index::sessions_touching` returns the start/end window blame needs, and a
//! repo-relative query matches both Claude Code's absolute touched paths and
//! Codex's relative ones (the P0 path-namespace join from the design review).

use rusqlite::{params, Connection};
use sessionwiki::index;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn fresh() -> Connection {
    let dir = std::env::temp_dir().join("sessionwiki-test-blame-index");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("SESSIONWIKI_DATA", &dir);
    let conn = index::open().unwrap();
    conn.execute_batch("DELETE FROM files; DELETE FROM touched;")
        .unwrap();
    conn
}

fn seed(conn: &Connection, id: &str, path: &str, started: &str, ended: &str, project: &str) {
    conn.execute(
        "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind)
         VALUES (?1, 0, 0, ?2, 'claude-code', ?3, ?2, ?4, ?5, 1, 'main')",
        params![format!("/store/{id}.jsonl"), id, project, started, ended],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO touched(session_id, path) VALUES (?1, ?2)",
        params![id, path],
    )
    .unwrap();
}

#[test]
fn sessions_touching_matches_abs_and_relative_and_parses_times() {
    let _g = LOCK.lock().unwrap();
    let conn = fresh();
    // s_abs stores an absolute touched path (Claude Code); s_rel a relative one
    // (Codex). A repo-relative query must match both.
    seed(
        &conn,
        "s_abs",
        "/home/dev/proj/src/auth.rs",
        "2026-06-08T10:00:00+00:00",
        "2026-06-08T11:00:00+00:00",
        "/home/dev/proj",
    );
    seed(
        &conn,
        "s_rel",
        "src/auth.rs",
        "2026-06-09T10:00:00+00:00",
        "2026-06-09T11:00:00+00:00",
        "/home/dev/proj",
    );

    let got = index::sessions_touching(&conn, "src/auth.rs").unwrap();
    let ids: Vec<&str> = got.iter().map(|s| s.session_id.as_str()).collect();
    assert!(
        ids.contains(&"s_abs") && ids.contains(&"s_rel"),
        "both tools' rows must match, got {ids:?}"
    );
    let abs = got.iter().find(|s| s.session_id == "s_abs").unwrap();
    assert!(
        abs.started.is_some() && abs.ended.is_some(),
        "times must parse to epoch"
    );
    assert_eq!(abs.project, "/home/dev/proj");
}

/// `_` and `%` in the file name are literal. Unescaped, `_` matched any one
/// character, so blaming `newdata_copy.py` also took the session that edited
/// `newdata copy.py` - a pair that exists side by side on a real machine.
#[test]
fn sessions_touching_treats_like_metacharacters_literally() {
    let _g = LOCK.lock().unwrap();
    let conn = fresh();
    let (s, e) = ("2026-06-08T10:00:00+00:00", "2026-06-08T11:00:00+00:00");
    seed(&conn, "s_under", "/p/KBS/newdata_copy.py", s, e, "/p");
    seed(&conn, "s_space", "/p/KBS/newdata copy.py", s, e, "/p");
    seed(&conn, "s_pct", "/p/KBS/100%.py", s, e, "/p");
    seed(&conn, "s_other", "/p/KBS/1000.py", s, e, "/p");

    let ids = |q: &str| -> Vec<String> {
        index::sessions_touching(&conn, q)
            .unwrap()
            .into_iter()
            .map(|s| s.session_id)
            .collect()
    };
    assert_eq!(ids("KBS/newdata_copy.py"), ["s_under"]);
    assert_eq!(ids("KBS/100%.py"), ["s_pct"]);
}
