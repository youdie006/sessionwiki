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

#[test]
fn degraded_fields_preserve_metadata_without_blocking_transcript_updates_or_restore() {
    let _guard = INDEX_ENV.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("gemini");
    let p = home.join("antigravity-cli/brain/s/transcript.jsonl");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let capture = include_str!("fixtures/antigravity/agy-1.0.14-linux.jsonl");
    std::fs::write(&p, capture).unwrap();
    let db = home.join("antigravity-cli/conversation_summaries.db");
    let native = rusqlite::Connection::open(&db).unwrap();
    native.execute_batch("CREATE TABLE conversation_summaries(conversation_id TEXT,title TEXT,workspace_uris TEXT,parent_conversation_id TEXT); INSERT INTO conversation_summaries VALUES('s','Initial','[\"file:///original\"]','parent')").unwrap();
    std::env::set_var("SESSIONWIKI_DATA", dir.path().join("index"));
    let mut conn = index::open().unwrap();
    let adapters: Vec<Box<dyn Adapter>> = vec![Box::new(Antigravity::cli_in_home(&home))];
    index::sync_with(&mut conn, &adapters, None).unwrap();
    for bad in ["{broken", "[1]", "{\"key\":\"value\"}"] {
        native
            .execute(
                "UPDATE conversation_summaries SET workspace_uris=?1,title='Malformed workspace'",
                [bad],
            )
            .unwrap();
        index::sync_with(&mut conn, &adapters, None).unwrap();
        let (title, project): (String, String) = conn
            .query_row("SELECT title,project FROM files", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(title, "Malformed workspace");
        assert_eq!(project, "/original");
    }
    native.execute_batch("ALTER TABLE conversation_summaries DROP COLUMN workspace_uris; UPDATE conversation_summaries SET title='New title'").unwrap();
    let appended = format!(
        "{capture}\n{}\n{}\n{}\n",
        serde_json::json!({"step_index":9,"type":"USER_INPUT","content":"Unique appended message"}),
        serde_json::json!({"step_index":10,"type":"PLANNER_RESPONSE","tool_calls":[{"name":"replace_file_content","args":{"TargetFile":"/work/second.txt","TargetContent":"old","ReplacementContent":"new"}}]}),
        serde_json::json!({"step_index":11,"type":"CODE_ACTION","status":"DONE","content":"success"})
    );
    std::fs::write(&p, &appended).unwrap();
    index::sync_with(&mut conn, &adapters, None).unwrap();
    let metadata = |c: &rusqlite::Connection| {
        c.query_row(
            "SELECT title,project,kind='sub' FROM files LIMIT 1",
            [],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, bool>(2)?,
                ))
            },
        )
        .unwrap()
    };
    assert_eq!(
        metadata(&conn),
        ("New title".into(), "/original".into(), true)
    );
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM messages WHERE text LIKE '%Unique appended%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(count > 0);
    let edits: i64 = conn
        .query_row(
            "SELECT count(*) FROM edits WHERE path='/work/second.txt'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(edits > 0);
    native.close().unwrap();
    for bytes in [b"".as_slice(), b"corrupt".as_slice()] {
        std::fs::write(&db, bytes).unwrap();
        // Append a distinct edit while metadata is wholly unreadable.
        let extra = appended
            .replace("second.txt", "third.txt")
            .replace("Unique appended", "Unique corrupt");
        std::fs::write(&p, extra).unwrap();
        conn.execute_batch("UPDATE files SET archived_at='2026-01-01'")
            .unwrap();
        index::sync_with(&mut conn, &adapters, None).unwrap();
        assert_eq!(
            metadata(&conn),
            ("New title".into(), "/original".into(), true)
        );
        let live: i64 = conn
            .query_row(
                "SELECT count(*) FROM files WHERE archived_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(live, 1);
        // Indexed JSON retains the changed transcript, including new edit evidence.
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM messages WHERE text LIKE '%Unique corrupt%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(count > 0);
        let edits: i64 = conn
            .query_row(
                "SELECT count(*) FROM edits WHERE path='/work/third.txt'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(edits > 0);
    }
}

#[test]
fn first_bounded_sync_does_not_admit_old_unindexed_rows_from_db_freshness() {
    let _guard = INDEX_ENV.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("gemini");
    let p = home.join("antigravity-cli/brain/old/transcript.jsonl");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(
        &p,
        include_str!("fixtures/antigravity/agy-1.0.14-linux.jsonl"),
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
    native.execute_batch("CREATE TABLE conversation_summaries(conversation_id TEXT,title TEXT,workspace_uris TEXT,parent_conversation_id TEXT); INSERT INTO conversation_summaries VALUES('old','Old','[]','')").unwrap();
    std::env::set_var("SESSIONWIKI_DATA", dir.path().join("index"));
    let mut conn = index::open().unwrap();
    let adapters: Vec<Box<dyn Adapter>> = vec![Box::new(Antigravity::cli_in_home(&home))];
    index::sync_with(&mut conn, &adapters, Some(1700000000)).unwrap();
    let count: i64 = conn
        .query_row("SELECT count(*) FROM files", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    index::sync_with(&mut conn, &adapters, None).unwrap();
    native
        .execute("UPDATE conversation_summaries SET title='Changed'", [])
        .unwrap();
    index::sync_with(&mut conn, &adapters, Some(1700000000)).unwrap();
    let title: String = conn
        .query_row("SELECT title FROM files", [], |r| r.get(0))
        .unwrap();
    assert_eq!(title, "Changed");
}
