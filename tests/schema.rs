//! The durable schema versions independently of the disposable cache, and
//! durable data (summaries, tags, notes, archive) survives every upgrade.

use rusqlite::Connection;
use sessionwiki::index;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn fresh_dir(name: &str) {
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("SESSIONWIKI_DATA", &dir);
}

fn durable_version(conn: &Connection) -> String {
    conn.query_row(
        "SELECT value FROM meta WHERE key='durable_version'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn open_stamps_baseline_durable_version_and_is_idempotent() {
    let _g = LOCK.lock().unwrap();
    fresh_dir("sessionwiki-test-schema-baseline");
    let conn = index::open().unwrap();
    assert_eq!(durable_version(&conn), "1", "fresh DB stamps baseline");
    drop(conn);
    let conn = index::open().unwrap();
    assert_eq!(durable_version(&conn), "1", "re-open is a no-op");
}
