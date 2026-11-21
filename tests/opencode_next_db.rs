//! `opencode-next.db` is the database of OpenCode's v2 pre-release. OpenCode
//! 2.1.1 imports it into `opencode.db` during its v1-to-v2 migration and leaves
//! the file in place, so it must not be read as a v1 channel database (#133).

use rusqlite::Connection;
use std::path::Path;
use std::process::Output;

const V1_SCHEMA: &str = "
    CREATE TABLE project(id TEXT PRIMARY KEY, worktree TEXT);
    CREATE TABLE session(id TEXT PRIMARY KEY, parent_id TEXT, project_id TEXT,
        directory TEXT, title TEXT, time_created INTEGER, time_updated INTEGER);
    CREATE TABLE message(id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
    CREATE TABLE part(id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT,
        time_created INTEGER, data TEXT);";

fn v1_db(path: &Path, session: &str, text: &str) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(V1_SCHEMA).unwrap();
    conn.execute(
        "INSERT INTO session VALUES (?1, NULL, 'p1', '/home/dev/app', 'Add retry',
             1718630400000, 1718630500000)",
        [session],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO message VALUES ('m1', ?1, 1718630400000, '{\"role\":\"user\"}')",
        [session],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO part VALUES ('pa', 'm1', ?1, 1718630400000, ?2)",
        [
            session,
            &serde_json::json!({"type": "text", "text": text}).to_string(),
        ],
    )
    .unwrap();
}

/// The layout OpenCode's migration recognises as a v2 pre-release database:
/// `project`, `session` and `session_message`, and no v1 `message` table.
fn next_db(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE project(id TEXT PRIMARY KEY, worktree TEXT);
         CREATE TABLE session(id TEXT PRIMARY KEY, project_id TEXT, directory TEXT,
             title TEXT, time_created INTEGER, time_updated INTEGER);
         CREATE TABLE session_message(id TEXT PRIMARY KEY, session_id TEXT, type TEXT,
             time_created INTEGER, time_updated INTEGER, data TEXT);
         INSERT INTO project VALUES ('p1', '/home/dev/app');
         INSERT INTO session VALUES ('ses_next', 'p1', '/home/dev/app', 'Imported',
             1718630400000, 1718630500000);
         INSERT INTO session_message VALUES ('sm1', 'ses_next', 'user',
             1718630400000, 1718630400000, '{}');",
    )
    .unwrap();
}

fn sync(home: &Path, data: &Path) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_sessionwiki"))
        .args(["sync", "--tool", "opencode"])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join("share"))
        .env("SESSIONWIKI_DATA", data)
        .output()
        .unwrap()
}

#[test]
fn the_v2_prerelease_database_is_not_read_as_a_v1_channel() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let dir = home.join("share").join("opencode");
    std::fs::create_dir_all(&dir).unwrap();
    v1_db(
        &dir.join("opencode.db"),
        "ses_main",
        "add retry to the client",
    );
    // OpenCode 2.1.1 adds the v2 tables to `opencode.db` next to the v1 ones,
    // so the layout alone does not tell the two files apart.
    Connection::open(dir.join("opencode.db"))
        .unwrap()
        .execute_batch(
            "CREATE TABLE session_message(id TEXT PRIMARY KEY, session_id TEXT, type TEXT,
                 time_created INTEGER, time_updated INTEGER, data TEXT);",
        )
        .unwrap();
    next_db(&dir.join("opencode-next.db"));

    let out = sync(&home, &t.path().join("data"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "sync failed: {stderr}");
    assert!(!stderr.contains("failed to parse"), "{stderr}");
    assert!(stderr.contains("[opencode] indexed 1/1"), "{stderr}");
}

/// Only a file with the pre-release layout is skipped, as OpenCode itself only
/// imports one with that layout. A v1 database under the same name is read.
#[test]
fn a_v1_database_named_opencode_next_is_still_read() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let dir = home.join("share").join("opencode");
    std::fs::create_dir_all(&dir).unwrap();
    v1_db(
        &dir.join("opencode.db"),
        "ses_main",
        "add retry to the client",
    );
    v1_db(
        &dir.join("opencode-next.db"),
        "ses_channel",
        "rename the flag",
    );

    let out = sync(&home, &t.path().join("data"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "sync failed: {stderr}");
    assert!(stderr.contains("[opencode] indexed 2/2"), "{stderr}");
}
