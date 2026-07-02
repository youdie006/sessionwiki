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
    // A fresh DB is stamped at baseline (1), then the registered migrations
    // run immediately (v2 tag normalization, a no-op on empty tables).
    assert_eq!(durable_version(&conn), "2", "fresh DB lands on latest");
    drop(conn);
    let conn = index::open().unwrap();
    assert_eq!(durable_version(&conn), "2", "re-open is a no-op");
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
        "2",
        "pre-feature index adopts baseline, then runs forward migrations"
    );
    assert_durables_intact(&conn);
}

#[test]
fn migration_v2_heals_legacy_nfd_tags() {
    let _g = LOCK.lock().unwrap();
    fresh_dir("sessionwiki-test-schema-tagmig");
    let conn = index::open().unwrap(); // fresh DB is already at v2
    conn.execute_batch("DELETE FROM files; DELETE FROM tags;")
        .unwrap();
    conn.execute(
        "INSERT INTO files(path,mtime,size,session_id,tool,project,title,started,ended,msg_count,kind)
         VALUES('/s/t.jsonl',0,0,'tg1','claude-code','/p','t','2026-06-10T10:00:00+00:00','2026-06-10T10:00:00+00:00',1,'main')",
        [],
    ).unwrap();
    // Simulate rows a pre-fix binary wrote: raw NFD bytes, under durable v1.
    conn.execute(
        "INSERT INTO tags(session_id, tag) VALUES ('tg1', ?1)",
        ["cafe\u{301}"],
    )
    .unwrap();
    conn.execute(
        "UPDATE meta SET value = '1' WHERE key = 'durable_version'",
        [],
    )
    .unwrap();
    drop(conn);

    // Upgrade: reopen runs v2, which re-normalizes the stored rows (and takes
    // the pre-migration backup).
    let conn = index::open().unwrap();
    assert_eq!(durable_version(&conn), "2");
    let stored: String = conn
        .query_row("SELECT tag FROM tags WHERE session_id='tg1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(stored, "caf\u{e9}", "legacy NFD row converged on NFC");
    // ...and it is reachable again through the normalized lookups.
    let rows = index::recent(&conn, 10, None, None, Some("cafe\u{301}"), false).unwrap();
    assert_eq!(rows.len(), 1, "legacy tag filterable after the migration");
    assert_eq!(index::remove_tag(&conn, "tg1", "cafe\u{301}").unwrap(), 1);
    // The one-time backup of the durable data was taken before migrating.
    let dir = std::env::temp_dir().join("sessionwiki-test-schema-tagmig");
    assert!(
        dir.join("index.db.bak-v1").exists(),
        "VACUUM INTO backup before the first pending migration"
    );
}
