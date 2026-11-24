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
    let conn = index::open().unwrap(); // fresh DB is already at the latest version
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

/// The index layout shipped as schema 9: the file-provenance tables (`touched`,
/// `edits`) and an `archive` table whose `touched` column is `NOT NULL`.
const V9_SCHEMA: &str = "
    CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE files(
        path TEXT PRIMARY KEY, mtime INTEGER NOT NULL, size INTEGER NOT NULL,
        session_id TEXT NOT NULL, tool TEXT NOT NULL, project TEXT NOT NULL DEFAULT '',
        title TEXT NOT NULL DEFAULT '', started TEXT, ended TEXT,
        msg_count INTEGER NOT NULL DEFAULT 0, kind TEXT NOT NULL DEFAULT 'main',
        archived_at TEXT, reader INTEGER NOT NULL DEFAULT 0);
    CREATE INDEX idx_files_session ON files(session_id);
    CREATE TABLE messages(
        id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, role TEXT NOT NULL,
        text TEXT NOT NULL, full_text TEXT);
    CREATE INDEX idx_messages_session ON messages(session_id);
    CREATE TABLE summaries(session_id TEXT PRIMARY KEY, summary TEXT NOT NULL, created TEXT NOT NULL);
    CREATE TABLE tags(session_id TEXT NOT NULL, tag TEXT NOT NULL, PRIMARY KEY (session_id, tag));
    CREATE INDEX idx_tags_tag ON tags(tag);
    CREATE TABLE notes(session_id TEXT PRIMARY KEY, note TEXT NOT NULL, updated TEXT NOT NULL);
    CREATE TABLE archive(
        session_id TEXT PRIMARY KEY, path TEXT NOT NULL, mtime INTEGER NOT NULL,
        size INTEGER NOT NULL, tool TEXT NOT NULL, project TEXT NOT NULL DEFAULT '',
        title TEXT NOT NULL DEFAULT '', started TEXT, ended TEXT,
        msg_count INTEGER NOT NULL DEFAULT 0, kind TEXT NOT NULL DEFAULT 'main',
        transcript TEXT NOT NULL, touched TEXT NOT NULL, archived_at TEXT NOT NULL);
    CREATE TABLE touched(session_id TEXT NOT NULL, path TEXT NOT NULL, PRIMARY KEY (session_id, path));
    CREATE INDEX idx_touched_path ON touched(path);
    CREATE TABLE edits(
        session_id TEXT NOT NULL, path TEXT NOT NULL, kind TEXT NOT NULL,
        ts TEXT, snippet TEXT NOT NULL);
    CREATE INDEX idx_edits_path ON edits(path);
    CREATE INDEX idx_edits_session ON edits(session_id);
    CREATE VIRTUAL TABLE msgs USING fts5(
        text, content='messages', content_rowid='id', tokenize='trigram');
";

const OLD_TRANSCRIPT: &str =
    r#"[["user","why does the retry loop spin"],["assistant","the backoff was never reset"]]"#;

/// Write a schema-9 index holding one archived session (its original file is
/// gone) plus the curation rows for it.
fn write_v9_index(dir: &std::path::Path) {
    let conn = Connection::open(dir.join("index.db")).unwrap();
    conn.execute_batch(V9_SCHEMA).unwrap();
    conn.execute_batch(
        "INSERT INTO meta VALUES('durable_version','2');
         INSERT INTO files(path,mtime,size,session_id,tool,project,title,started,ended,
                           msg_count,kind,archived_at,reader)
           VALUES('/gone/old.jsonl',0,0,'old1','codex','/proj/api','retry loop',
                  '2026-06-10T10:00:00+00:00','2026-06-10T10:05:00+00:00',2,'main',
                  '2026-06-12 00:00:00',3);
         INSERT INTO messages(id,session_id,role,text) VALUES
           (1,'old1','user','why does the retry loop spin'),
           (2,'old1','assistant','the backoff was never reset');
         INSERT INTO msgs(rowid,text) VALUES
           (1,'why does the retry loop spin'),(2,'the backoff was never reset');
         INSERT INTO files(path,mtime,size,session_id,tool,project,title,msg_count,kind,reader)
           VALUES('/gone/live.jsonl',0,0,'live1','claude-code','/proj/api','live session',1,'main',4);
         INSERT INTO messages(id,session_id,role,text) VALUES(3,'live1','user','unarchived question');
         INSERT INTO msgs(rowid,text) VALUES(3,'unarchived question');
         INSERT INTO files(path,mtime,size,session_id,tool,project,title,msg_count,kind,reader)
           VALUES('/gone/large.jsonl',123,456,'large1','claude-code','/proj/api',
                  '[large] legacy conversation',1,'main',4);
         INSERT INTO touched VALUES('old1','/proj/api/src/retry.rs');
         INSERT INTO edits VALUES('old1','/proj/api/src/retry.rs','edit',NULL,'reset()');
         INSERT INTO summaries VALUES('old1','Fixed the retry backoff','2026-06-12');
         INSERT INTO tags VALUES('old1','spike');
         INSERT INTO notes VALUES('old1','keep this one','2026-06-12');",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO archive VALUES('old1','/gone/old.jsonl',0,0,'codex','/proj/api','retry loop',
                '2026-06-10T10:00:00+00:00','2026-06-10T10:05:00+00:00',2,'main',?1,
                '[\"/proj/api/src/retry.rs\"]','2026-06-12 00:00:00')",
        [OLD_TRANSCRIPT],
    )
    .unwrap();
    conn.pragma_update(None, "user_version", 9i64).unwrap();
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |r| r.get(0),
    )
    .unwrap()
}

fn archive_columns(conn: &Connection) -> Vec<String> {
    conn.prepare("SELECT name FROM pragma_table_info('archive')")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Index one Claude Code session from a scratch HOME, delete its file, and sync
/// again so the index archives it. Panics if the archive write fails.
fn archive_a_new_session(conn: &mut Connection, tag: &str) -> String {
    let home = std::env::temp_dir().join(format!("sessionwiki-test-schema-home-{tag}"));
    let _ = std::fs::remove_dir_all(&home);
    let project = home.join(".claude/projects/newproj");
    std::fs::create_dir_all(&project).unwrap();
    let file = project.join("bbbbbbbb-0000-4000-8000-000000000def.jsonl");
    std::fs::write(
        &file,
        concat!(
            r#"{"type":"user","cwd":"/home/dev/app","timestamp":"2026-06-10T10:00:00.000Z","message":{"role":"user","content":"cache invalidation question"}}"#,
            "\n",
            r#"{"type":"assistant","cwd":"/home/dev/app","timestamp":"2026-06-10T10:00:10.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Clear it on write."}]}}"#,
            "\n",
        ),
    )
    .unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("USERPROFILE", &home);
    index::sync(conn, Some("claude-code")).unwrap();
    let sid = index::recent(conn, 50, None, None, None, false)
        .unwrap()
        .into_iter()
        .find(|r| r.title.contains("cache invalidation"))
        .expect("new session indexed")
        .session_id;
    std::fs::remove_file(&file).unwrap();
    index::sync(conn, Some("claude-code")).unwrap();
    let archived: i64 = conn
        .query_row(
            "SELECT count(*) FROM archive WHERE session_id = ?1",
            [&sid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(archived, 1, "the pruned session was archived");
    let touched: String = conn
        .query_row(
            "SELECT touched FROM archive WHERE session_id = ?1",
            [&sid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(touched, "[]");
    sid
}

#[test]
fn a_fresh_index_has_no_provenance_tables_and_archives_new_sessions() {
    let _g = LOCK.lock().unwrap();
    fresh_dir("sessionwiki-test-schema-fresh-v10");
    let mut conn = index::open().unwrap();
    assert_eq!(index::SCHEMA_VERSION, 10);
    assert!(!table_exists(&conn, "touched"));
    assert!(!table_exists(&conn, "edits"));
    assert!(archive_columns(&conn).contains(&"touched".to_string()));
    archive_a_new_session(&mut conn, "fresh");
}

#[test]
fn a_v9_index_upgrades_to_v10_and_keeps_every_durable_row() {
    let _g = LOCK.lock().unwrap();
    fresh_dir("sessionwiki-test-schema-v9-upgrade");
    let dir = std::path::PathBuf::from(std::env::var_os("SESSIONWIKI_DATA").unwrap());
    write_v9_index(&dir);

    let mut conn = index::open().unwrap();

    // Cache upgraded in place; the provenance tables are gone.
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, index::SCHEMA_VERSION);
    assert!(!table_exists(&conn, "touched"), "touched dropped");
    assert!(!table_exists(&conn, "edits"), "edits dropped");
    assert_eq!(durable_version(&conn), "2");
    assert!(!dir.join("index.db.bak-v2").exists());
    let touched: String = conn
        .query_row(
            "SELECT touched FROM archive WHERE session_id = 'old1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(touched, r#"["/proj/api/src/retry.rs"]"#);

    let live = index::resolve(&conn, "live1").unwrap();
    assert_eq!(live.len(), 1);
    assert!(!live[0].archived);
    assert_eq!(
        index::session_from_index(&conn, &live[0])
            .unwrap()
            .messages
            .len(),
        1
    );
    let live_mtime_size: (i64, i64) = conn
        .query_row(
            "SELECT mtime, size FROM files WHERE session_id = 'live1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        live_mtime_size,
        (0, 0),
        "normal live session is not marked stale"
    );
    assert!(index::search(&conn, "unarchived", 10, None, None)
        .unwrap()
        .iter()
        .any(|h| h.row.session_id == "live1"));

    let large_mtime_size: (i64, i64) = conn
        .query_row(
            "SELECT mtime, size FROM files WHERE session_id = 'large1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        large_mtime_size,
        (-1, -1),
        "legacy large session is reparsed"
    );
    let large_marker: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'large_provenance_reparsed'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(large_marker, "3");

    // The archived session survives byte for byte.
    let (title, project, transcript, archived_at, msg_count): (
        String,
        String,
        String,
        String,
        i64,
    ) = conn
        .query_row(
            "SELECT title, project, transcript, archived_at, msg_count
             FROM archive WHERE session_id = 'old1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(title, "retry loop");
    assert_eq!(project, "/proj/api");
    assert_eq!(transcript, OLD_TRANSCRIPT);
    assert_eq!(archived_at, "2026-06-12 00:00:00");
    assert_eq!(msg_count, 2);

    // ...is back in the live cache, flagged archived, searchable and readable.
    let rows = index::resolve(&conn, "old1").unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].archived);
    let hits = index::search(&conn, "backoff", 10, None, None).unwrap();
    assert!(hits.iter().any(|h| h.row.session_id == "old1"));
    assert_eq!(
        index::session_from_index(&conn, &rows[0])
            .unwrap()
            .messages
            .len(),
        2
    );

    // Curation rows are untouched.
    assert_eq!(
        index::note_for(&conn, "old1").unwrap().as_deref(),
        Some("keep this one")
    );
    assert!(index::tag_counts(&conn)
        .unwrap()
        .iter()
        .any(|(t, _)| t == "spike"));
    let summary: String = conn
        .query_row(
            "SELECT summary FROM summaries WHERE session_id = 'old1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(summary, "Fixed the retry backoff");

    // New archives fill the deprecated NOT NULL column with an empty list.
    let new_sid = archive_a_new_session(&mut conn, "upgraded");
    let total: i64 = conn
        .query_row("SELECT count(*) FROM archive", [], |r| r.get(0))
        .unwrap();
    assert_eq!(total, 2, "old and new archives side by side");

    // Reopening is a no-op, and both archives survive another cache rebuild.
    drop(conn);
    let conn = index::open().unwrap();
    conn.pragma_update(None, "user_version", 0i64).unwrap();
    drop(conn);
    let conn = index::open().unwrap();
    for sid in ["old1", new_sid.as_str()] {
        let rows = index::resolve(&conn, sid).unwrap();
        assert_eq!(rows.len(), 1, "{sid} rehydrated");
        assert!(rows[0].archived, "{sid}");
    }
}
