//! End-to-end blame wiring: a real committed line, a seeded touching session
//! whose time window contains the commit, attributed Confident through
//! `commands::blame_runs` (isolates index + heuristic from stdout; the git path
//! is covered by tests/blame_git.rs).

use rusqlite::{params, Connection};
use sessionwiki::{blame, commands, index};
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn fresh() -> Connection {
    let dir = std::env::temp_dir().join("sessionwiki-test-blame-cmd");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("SESSIONWIKI_DATA", &dir);
    let conn = index::open().unwrap();
    conn.execute_batch("DELETE FROM files; DELETE FROM touched;")
        .unwrap();
    conn
}

fn git(dir: &Path, args: &[&str], date: Option<&str>) {
    let mut c = Command::new("git");
    c.current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Dev")
        .env("GIT_AUTHOR_EMAIL", "dev@example.com")
        .env("GIT_COMMITTER_NAME", "Dev")
        .env("GIT_COMMITTER_EMAIL", "dev@example.com");
    if let Some(d) = date {
        c.env("GIT_AUTHOR_DATE", d).env("GIT_COMMITTER_DATE", d);
    }
    assert!(c.status().unwrap().success(), "git {args:?}");
}

#[test]
fn blame_runs_attributes_a_committed_line_to_the_touching_session() {
    let _g = LOCK.lock().unwrap();
    let conn = fresh();

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"], None);
    std::fs::write(root.join("a.rs"), "x\n").unwrap();
    git(root, &["add", "a.rs"], None);
    let commit_epoch = 1_000_000_500i64;
    git(
        root,
        &["commit", "-q", "-m", "add"],
        Some(&format!("@{commit_epoch} +0000")),
    );

    // A session that touched a.rs, with a window that contains the commit time.
    let started = chrono::DateTime::from_timestamp(commit_epoch - 300, 0)
        .unwrap()
        .to_rfc3339();
    let ended = chrono::DateTime::from_timestamp(commit_epoch + 300, 0)
        .unwrap()
        .to_rfc3339();
    conn.execute(
        "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind)
         VALUES ('/store/sX.jsonl', 0, 0, 'sX', 'claude-code', ?1, 'fix', ?2, ?3, 1, 'main')",
        params![root.to_string_lossy(), started, ended],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO touched(session_id, path) VALUES ('sX', 'a.rs')",
        [],
    )
    .unwrap();

    let file = root.join("a.rs");
    let repo = blame::repo_root(&file).unwrap();
    let raw = blame::run_git_blame(&repo, &file, None).unwrap();
    let runs = blame::group_runs(&blame::parse_line_porcelain(&raw));
    let results = commands::blame_runs(&conn, "a.rs", &repo.to_string_lossy(), runs).unwrap();

    assert_eq!(results.len(), 1, "one run for the single commit");
    match &results[0].attribution {
        blame::Attribution::Confident(s) => assert_eq!(s.session_id, "sX"),
        other => panic!("expected Confident, got {other:?}"),
    }
}
