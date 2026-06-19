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

fn seed_durables(conn: &Connection) {
    conn.execute(
        "INSERT INTO files(path,mtime,size,session_id,tool,project,title,started,ended,msg_count,kind)
         VALUES('/s/x.jsonl',0,0,'sx','claude-code','/p','t','2026-06-10T10:00:00+00:00','2026-06-10T10:00:00+00:00',1,'main')",
        [],
    ).unwrap();
    index::set_summary(conn, "sx", "did the thing").unwrap();
    index::add_tag(conn, "sx", "spike").unwrap();
    index::set_note(conn, "sx", "remember this").unwrap();
}

fn assert_durables_intact(conn: &Connection) {
    assert_eq!(
        index::note_for(conn, "sx").unwrap().as_deref(),
        Some("remember this")
    );
    assert!(index::tag_counts(conn)
        .unwrap()
        .iter()
        .any(|(t, _)| t == "spike"));
    let s: Option<String> = conn
        .query_row(
            "SELECT summary FROM summaries WHERE session_id='sx'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(s.as_deref(), Some("did the thing"));
}

#[test]
fn durables_survive_a_cache_schema_bump() {
    let _g = LOCK.lock().unwrap();
    fresh_dir("sessionwiki-test-schema-survive");
    let conn = index::open().unwrap();
    conn.execute_batch(
        "DELETE FROM files; DELETE FROM summaries; DELETE FROM tags; DELETE FROM notes;",
    )
    .unwrap();
    seed_durables(&conn);
    conn.pragma_update(None, "user_version", 0i64).unwrap(); // force cache drop+rebuild on reopen
    drop(conn);
    let conn = index::open().unwrap();
    assert_durables_intact(&conn); // cache rebuilt, durables untouched
}

#[test]
fn adopts_baseline_on_a_pre_feature_index() {
    let _g = LOCK.lock().unwrap();
    fresh_dir("sessionwiki-test-schema-adopt");
    let conn = index::open().unwrap();
    conn.execute_batch(
        "DELETE FROM files; DELETE FROM summaries; DELETE FROM tags; DELETE FROM notes;",
    )
    .unwrap();
    seed_durables(&conn);
    // simulate a real v5 index from before this feature: no meta table
    conn.execute_batch("DROP TABLE meta").unwrap();
    conn.pragma_update(None, "user_version", 5i64).unwrap();
    drop(conn);
    let conn = index::open().unwrap();
    assert_eq!(
        durable_version(&conn),
        "1",
        "pre-feature index adopts baseline, no migration re-run"
    );
    assert_durables_intact(&conn);
}
