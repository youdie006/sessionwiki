use sessionwiki::{
    adapters::{Adapter, Antigravity},
    index,
};
static INDEX_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[test]
fn bounded_sync_indexes_new_summary_metadata_on_an_old_transcript() {
    let _guard = INDEX_ENV.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("gemini");
    let p = home.join("antigravity-cli/brain/s/.system_generated/logs/transcript.jsonl");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(
        &p,
        include_str!("fixtures/antigravity/agy-1.5.7-linux.jsonl"),
    )
    .unwrap();
    std::fs::File::open(&p)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1600000000)),
        )
        .unwrap();
    let native =
        rusqlite::Connection::open(home.join("antigravity-cli/conversation_summaries.db")).unwrap();
    native.execute_batch("CREATE TABLE conversation_summaries(conversation_id TEXT,title TEXT,workspace_uris TEXT,parent_conversation_id TEXT,last_modified_time TEXT); INSERT INTO conversation_summaries VALUES('s','Old title','[]','','2020-01-01T00:00:00Z')").unwrap();
    native.pragma_update(None, "journal_mode", "WAL").unwrap();
    std::env::set_var("SESSIONWIKI_DATA", dir.path().join("index"));
    let mut conn = index::open().unwrap();
    let adapters: Vec<Box<dyn Adapter>> = vec![Box::new(Antigravity::cli_in_home(&home))];
    index::sync_with(&mut conn, &adapters, None).unwrap();
    native.execute("UPDATE conversation_summaries SET title='Updated summary title',last_modified_time='2027-02-01 00:00:00+00:00'",[]).unwrap();
    index::sync_with(&mut conn, &adapters, Some(1700000000)).unwrap();
    let title: String = conn
        .query_row(
            "SELECT title FROM files WHERE tool='antigravity'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(title, "Updated summary title");
    native
        .execute_batch(
            "ALTER TABLE conversation_summaries DROP COLUMN last_modified_time;
        UPDATE conversation_summaries SET title='Legacy summary schema update'",
        )
        .unwrap();
    index::sync_with(&mut conn, &adapters, Some(1700000000)).unwrap();
    let title: String = conn
        .query_row(
            "SELECT title FROM files WHERE tool='antigravity'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(title, "Legacy summary schema update");
    // A temporarily unreadable optional database must not erase the last known
    // metadata or archive its sessions. A later healthy scan retries normally.
    native
        .execute_batch("ALTER TABLE conversation_summaries RENAME TO unavailable_summaries")
        .unwrap();
    let failed = adapters[0].store().unwrap();
    assert!(!failed.had_error);
    assert_eq!(failed.keys.len(), 1);
    index::sync_with(&mut conn, &adapters, None).unwrap();
    let (title, archived): (String, bool) = conn
        .query_row(
            "SELECT title,archived_at IS NOT NULL FROM files WHERE tool='antigravity'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(title, "Legacy summary schema update");
    assert!(!archived);
    native
        .execute_batch(
            "ALTER TABLE unavailable_summaries RENAME TO conversation_summaries;
        UPDATE conversation_summaries SET title='Recovered native summary'",
        )
        .unwrap();
    index::sync_with(&mut conn, &adapters, None).unwrap();
    let title: String = conn
        .query_row(
            "SELECT title FROM files WHERE tool='antigravity'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(title, "Recovered native summary");
    native
        .execute("UPDATE conversation_summaries SET title=x'0102'", [])
        .unwrap();
    assert!(adapters[0].store().is_some());
    index::sync_with(&mut conn, &adapters, None).unwrap();
    let title: String = conn
        .query_row(
            "SELECT title FROM files WHERE tool='antigravity'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(title, "Recovered native summary");
    native
        .execute_batch(
            "ALTER TABLE conversation_summaries ADD COLUMN last_modified_time TEXT;
        UPDATE conversation_summaries SET title='Changed with stale row timestamp',
        last_modified_time='2020-01-01T00:00:00Z'",
        )
        .unwrap();
    index::sync_with(&mut conn, &adapters, Some(1700000000)).unwrap();
    let title: String = conn
        .query_row(
            "SELECT title FROM files WHERE tool='antigravity'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(title, "Changed with stale row timestamp");
    native
        .execute("DELETE FROM conversation_summaries", [])
        .unwrap();
    index::sync_with(&mut conn, &adapters, Some(1700000000)).unwrap();
    let title: String = conn
        .query_row(
            "SELECT title FROM files WHERE tool='antigravity'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(title, "Inspect the example project");
}

#[test]
fn degraded_databases_allow_new_sessions_and_root_errors_preserve_live_rows() {
    let _guard = INDEX_ENV.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("gemini");
    let brain = home.join("antigravity-cli/brain");
    let write = |id: &str| {
        let p = brain.join(id).join("transcript.jsonl");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            p,
            include_str!("fixtures/antigravity/agy-1.0.14-linux.jsonl"),
        )
        .unwrap();
    };
    write("old");
    let db = home.join("antigravity-cli/conversation_summaries.db");
    let native = rusqlite::Connection::open(&db).unwrap();
    native.execute_batch("CREATE TABLE conversation_summaries(conversation_id TEXT,title TEXT,workspace_uris TEXT,parent_conversation_id TEXT); INSERT INTO conversation_summaries VALUES('old','Preserved','[]','parent')").unwrap();
    std::env::set_var("SESSIONWIKI_DATA", dir.path().join("index"));
    let mut conn = index::open().unwrap();
    let adapters: Vec<Box<dyn Adapter>> = vec![Box::new(Antigravity::cli_in_home(&home))];
    index::sync_with(&mut conn, &adapters, None).unwrap();
    write("sibling");
    native.execute_batch("INSERT INTO conversation_summaries VALUES('sibling','Good sibling','[]',''); UPDATE conversation_summaries SET title=x'0102' WHERE conversation_id='old'").unwrap();
    index::sync_with(&mut conn, &adapters, None).unwrap();
    let title: String = conn
        .query_row(
            "SELECT title FROM files WHERE path LIKE '%/sibling'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(title, "Good sibling");
    native
        .execute_batch(
            "UPDATE conversation_summaries SET title='Preserved' WHERE conversation_id='old'",
        )
        .unwrap();
    index::sync_with(&mut conn, &adapters, None).unwrap();
    native.close().unwrap();
    for bytes in [b"".as_slice(), b"not sqlite".as_slice()] {
        std::fs::write(&db, bytes).unwrap();
        write("new");
        index::sync_with(&mut conn, &adapters, None).unwrap();
        let title: String = conn
            .query_row("SELECT title FROM files WHERE path LIKE '%/old'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(title, "Preserved");
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM files WHERE archived_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 3);
    }
    std::fs::rename(&brain, brain.with_extension("saved")).unwrap();
    std::fs::write(&brain, "not a directory").unwrap();
    assert!(adapters[0].store().unwrap().had_error);
    index::sync_with(&mut conn, &adapters, None).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM files WHERE archived_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 3);
}
