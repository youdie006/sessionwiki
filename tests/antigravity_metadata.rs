use sessionwiki::{
    adapters::{Adapter, Antigravity},
    index,
};
#[test]
fn bounded_sync_indexes_new_summary_metadata_on_an_old_transcript() {
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
    assert!(failed.had_error);
    assert!(failed.keys.is_empty());
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
}
