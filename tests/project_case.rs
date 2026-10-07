//! On a case-insensitive filesystem (WSL's /mnt/<drive>, macOS by default) a
//! shell can `cd` into the same directory spelled `apo` or `APO`, and each tool
//! records what it was given. One project then split into case-variant rows:
//! on the machine this was found, 14 groups, one of them 23 + 514 sessions.

use sessionwiki::util::project_key;
use std::path::Path;

/// SESSIONWIKI_DATA is process-wide; tests that set it take turns.
static DATA_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Whether `dir`'s filesystem resolves a name in another case.
fn case_insensitive(dir: &Path) -> bool {
    let probe = dir.join("CaseProbe");
    std::fs::create_dir_all(&probe).unwrap();
    dir.join("caseprobe").exists()
}

fn scratch() -> tempfile::TempDir {
    // SESSIONWIKI_CASE_DIR lets a Linux run point this at a drvfs mount.
    match std::env::var_os("SESSIONWIKI_CASE_DIR") {
        Some(dir) => tempfile::tempdir_in(dir).unwrap(),
        None => tempfile::tempdir().unwrap(),
    }
}

#[test]
fn a_case_variant_of_an_existing_directory_takes_its_on_disk_case() {
    let root = scratch();
    let base = std::fs::canonicalize(root.path()).unwrap();
    std::fs::create_dir_all(base.join("MyProject/APO")).unwrap();
    if !case_insensitive(&base) {
        eprintln!("skipped: {} is case-sensitive", base.display());
        return;
    }
    let variant = format!("{}/myproject/apo", base.display());
    assert_eq!(
        project_key(&variant),
        format!("{}/MyProject/APO", base.display())
    );
}

#[test]
fn a_path_spelled_as_on_disk_or_missing_is_left_alone() {
    let root = scratch();
    let base = std::fs::canonicalize(root.path()).unwrap();
    std::fs::create_dir_all(base.join("Exact")).unwrap();
    let exact = format!("{}/Exact", base.display());
    assert_eq!(project_key(&exact), exact);
    // Gone from disk: nothing to compare against, so it stays as recorded.
    let gone = format!("{}/Deleted/Project", base.display());
    assert_eq!(project_key(&gone), gone);
    // Not a filesystem path at all.
    assert_eq!(project_key("relative/dir"), "relative/dir");
    assert_eq!(project_key(""), "");
}

#[test]
fn two_directories_that_differ_only_in_case_are_not_merged() {
    let root = scratch();
    let base = std::fs::canonicalize(root.path()).unwrap();
    if case_insensitive(&base) {
        eprintln!("skipped: {} cannot hold both spellings", base.display());
        return;
    }
    std::fs::create_dir_all(base.join("Proj")).unwrap();
    std::fs::create_dir_all(base.join("proj")).unwrap();
    for name in ["Proj", "proj"] {
        let p = format!("{}/{name}", base.display());
        assert_eq!(project_key(&p), p);
    }
    // A spelling that does not exist here is another (perhaps deleted)
    // project, not this one: it must not be folded into `Only`.
    std::fs::create_dir_all(base.join("Only")).unwrap();
    let other = format!("{}/only", base.display());
    assert_eq!(project_key(&other), other);
}

/// Rows indexed before this fix keep their variant spelling until something
/// respells them; the first open after upgrading does, once.
#[test]
fn opening_an_index_respells_variants_recorded_before_the_fix() {
    let _data = DATA_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = scratch();
    let base = std::fs::canonicalize(root.path()).unwrap();
    std::fs::create_dir_all(base.join("MyProject/APO")).unwrap();
    if !case_insensitive(&base) {
        eprintln!("skipped: {} is case-sensitive", base.display());
        return;
    }
    let data = tempfile::tempdir().unwrap();
    std::env::set_var("SESSIONWIKI_DATA", data.path());
    let conn = sessionwiki::index::open().unwrap();
    let real = format!("{}/MyProject/APO", base.display());
    let variant = format!("{}/myproject/apo", base.display());
    for (id, project) in [("a", &real), ("b", &variant), ("c", &variant)] {
        conn.execute(
            "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started, msg_count, kind)
             VALUES (?1, 0, 0, ?2, 'codex', ?3, ?2, '2026-09-01T00:00:00+00:00', 1, 'main')",
            rusqlite::params![format!("/store/{id}.jsonl"), id, project],
        )
        .unwrap();
    }
    // As an index written by an older version: never respelled.
    conn.execute("DELETE FROM meta WHERE key = 'project_case_upto'", [])
        .unwrap();
    drop(conn);

    let conn = sessionwiki::index::open().unwrap();
    let projects = sessionwiki::index::projects(&conn).unwrap();
    let names: Vec<(&str, i64)> = projects
        .iter()
        .map(|p| (p.project.as_str(), p.sessions))
        .collect();
    assert_eq!(names, [(real.as_str(), 3)], "{names:?}");
}

/// The same late-writer gap for project spelling: a row an older binary writes
/// after the first open must still be respelled on the next one.
#[test]
fn a_variant_written_after_the_pass_is_respelled_on_the_next_open() {
    let _data = DATA_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = scratch();
    let base = std::fs::canonicalize(root.path()).unwrap();
    std::fs::create_dir_all(base.join("MyProject/APO")).unwrap();
    if !case_insensitive(&base) {
        eprintln!("skipped: {} is case-sensitive", base.display());
        return;
    }
    let data = tempfile::tempdir().unwrap();
    std::env::set_var("SESSIONWIKI_DATA", data.path());
    // The first open checks an existing row and records how far it got.
    let conn = sessionwiki::index::open().unwrap();
    conn.execute(
        "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started, msg_count, kind)
         VALUES ('/store/earlier.jsonl', 0, 0, 'earlier', 'codex', '/other', 'e', '2026-09-01T00:00:00+00:00', 1, 'main')",
        [],
    )
    .unwrap();
    drop(conn);
    drop(sessionwiki::index::open().unwrap());
    let real = format!("{}/MyProject/APO", base.display());
    let variant = format!("{}/myproject/apo", base.display());
    let conn = sessionwiki::index::open().unwrap();
    conn.execute(
        "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started, msg_count, kind)
         VALUES ('/store/late.jsonl', 0, 0, 'late', 'codex', ?1, 'late', '2026-09-01T00:00:00+00:00', 1, 'main')",
        rusqlite::params![variant],
    )
    .unwrap();
    drop(conn);

    let conn = sessionwiki::index::open().unwrap();
    let project: String = conn
        .query_row(
            "SELECT project FROM files WHERE session_id = 'late'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        project, real,
        "a late write from an older binary was never respelled"
    );
}
