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

/// A file name is not a file. The candidates are found by path suffix, so a
/// session that edited `a.rs` in another repository matched too, and the
/// project tie-break cannot tell them apart when both sessions ran from a
/// common parent directory. On a real machine 126 lines of one README were
/// "ambiguous" between the session that wrote them and one that edited another
/// repository's README; with that other session alone, it was "confident".
/// A candidate whose matching paths all name another existing file did not
/// edit this one. A path that no longer exists may still have been this file.
#[test]
fn blame_ignores_a_session_that_edited_another_repositorys_file() {
    let _g = LOCK.lock().unwrap();

    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().canonicalize().unwrap();
    let repo = parent.join("repo");
    let other = parent.join("other");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("a.rs"), "y\n").unwrap();
    git(&repo, &["init", "-q"], None);
    std::fs::write(repo.join("a.rs"), "x\n").unwrap();
    git(&repo, &["add", "a.rs"], None);
    let commit_epoch = 1_000_000_500i64;
    git(
        &repo,
        &["commit", "-q", "-m", "add"],
        Some(&format!("@{commit_epoch} +0000")),
    );
    let file = repo.join("a.rs");
    let raw = blame::run_git_blame(&repo, &file, None).unwrap();
    let runs = || blame::group_runs(&blame::parse_line_porcelain(&raw));

    let started = chrono::DateTime::from_timestamp(commit_epoch - 300, 0)
        .unwrap()
        .to_rfc3339();
    let ended = chrono::DateTime::from_timestamp(commit_epoch + 300, 0)
        .unwrap()
        .to_rfc3339();
    let seed = |conn: &Connection, id: &str, project: &Path, path: &str| {
        conn.execute(
            "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind)
             VALUES (?1, 0, 0, ?2, 'codex', ?3, ?2, ?4, ?5, 1, 'main')",
            params![format!("/store/{id}.jsonl"), id, project.to_string_lossy(), started, ended],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO touched(session_id, path) VALUES (?1, ?2)",
            params![id, path],
        )
        .unwrap();
    };
    let attribute = |conn: &Connection| {
        let results = commands::blame_runs(conn, "a.rs", &repo.to_string_lossy(), runs()).unwrap();
        assert_eq!(results.len(), 1);
        results[0].attribution.clone()
    };
    let abs = |p: &Path| p.join("a.rs").to_string_lossy().into_owned();

    // Both ran from the common parent; only one edited this repository's file.
    let conn = fresh();
    seed(&conn, "here", &parent, &abs(&repo));
    seed(&conn, "there", &parent, &abs(&other));
    match attribute(&conn) {
        blame::Attribution::Confident(s) => assert_eq!(s.session_id, "here"),
        other => panic!("expected the session that edited this file, got {other:?}"),
    }

    // Alone, the other repository's session is not this file's author.
    let conn = fresh();
    seed(&conn, "there", &parent, &abs(&other));
    seed(&conn, "there-rel", &other, "a.rs");
    assert_eq!(attribute(&conn), blame::Attribution::Unattributed);

    // A relative path is read against the session's own directory, and a path
    // that no longer exists is not evidence against the session.
    let conn = fresh();
    seed(&conn, "here-rel", &repo, "a.rs");
    match attribute(&conn) {
        blame::Attribution::Confident(s) => assert_eq!(s.session_id, "here-rel"),
        other => panic!("expected the relative-path session, got {other:?}"),
    }
    // A worktree or clone of this repository commits into its history, so a
    // session that edited the file there is still this file's author. A
    // repository of its own is not.
    git(&repo, &["worktree", "add", "-q", "--detach", "../wt"], None);
    git(&parent, &["clone", "-q", "repo", "clone"], None);
    git(&other, &["init", "-q"], None);
    git(&other, &["add", "a.rs"], None);
    git(
        &other,
        &["commit", "-q", "-m", "other"],
        Some("@1000000000 +0000"),
    );
    for (id, dir) in [("worktree", "wt"), ("clone", "clone")] {
        let conn = fresh();
        seed(&conn, id, &parent, &abs(&parent.join(dir)));
        match attribute(&conn) {
            blame::Attribution::Confident(s) => assert_eq!(s.session_id, id),
            other => panic!("a {id} of this repository is this file, got {other:?}"),
        }
    }
    let conn = fresh();
    seed(&conn, "there", &parent, &abs(&other));
    assert_eq!(attribute(&conn), blame::Attribution::Unattributed);

    let conn = fresh();
    seed(&conn, "gone", &parent, &abs(&parent.join("moved")));
    match attribute(&conn) {
        blame::Attribution::Confident(s) => assert_eq!(s.session_id, "gone"),
        other => panic!("a vanished path must not drop its session, got {other:?}"),
    }
}
