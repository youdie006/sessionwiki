//! Schema-tagged fingerprints must not make old sessions look recent.
use rusqlite::Connection;
use sessionwiki::{adapters, index};

static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn bounded_sync_uses_activity_dates_instead_of_schema_tagged_tokens() {
    let _env = ENV.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let db = data.join("opencode/opencode.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let source = rusqlite::Connection::open(&db).unwrap();
    source
        .execute_batch(include_str!("fixtures/opencode/v2_1_1.sql"))
        .unwrap();
    source.execute_batch("INSERT INTO session_v2 VALUES('recent',NULL,'p','/work','Recent',1800000060000,1800000060000)").unwrap();
    std::env::set_var("XDG_DATA_HOME", &data);
    std::env::set_var("OPENCODE_DB", &db);
    std::env::set_var("SESSIONWIKI_DATA", dir.path().join("index"));
    let mut conn = index::open().unwrap();
    index::sync_bounded(&mut conn, Some("opencode"), Some(1800000000)).unwrap();
    let titles: Vec<String> = conn
        .prepare("SELECT title FROM files WHERE tool='opencode'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(titles, ["Recent"]);
}

#[test]
fn full_and_bounded_sync_refresh_shell_completion_from_the_message_watermark() {
    let _env = ENV.lock().unwrap();
    const STARTED: i64 = 1_718_630_510_000;
    const COMPLETED: i64 = 1_718_630_520_000;
    for since in [None, Some(COMPLETED / 1000)] {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let db = data.join("opencode/opencode.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let source = Connection::open(&db).unwrap();
        source
            .execute_batch(include_str!("fixtures/opencode/v2_1_1.sql"))
            .unwrap();
        source
            .execute(
                "INSERT INTO session_message
             VALUES('shell-row','ses_v2_sanitized_001','shell',5,?1,?1,?2)",
                rusqlite::params![
                    STARTED,
                    r#"{"shellID":"shell1","command":"make verify","status":"running"}"#
                ],
            )
            .unwrap();
        std::env::set_var("XDG_DATA_HOME", &data);
        std::env::set_var("OPENCODE_DB", &db);
        std::env::set_var("SESSIONWIKI_DATA", dir.path().join("index"));
        let mut indexed = index::open().unwrap();
        index::set_tool_output_mode(&indexed, index::ToolOutputMode::Full).unwrap();
        index::sync(&mut indexed, Some("opencode")).unwrap();

        let key: String = indexed
            .query_row("SELECT path FROM files", [], |row| row.get(0))
            .unwrap();
        let read_indexed_shell = |conn: &Connection| {
            conn.query_row(
                "SELECT f.ended, m.text, m.full_text FROM files AS f
                 JOIN messages AS m ON m.session_id = f.session_id
                 WHERE m.role='tool' AND m.text LIKE '%make verify%'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .unwrap()
        };
        let started = chrono::DateTime::from_timestamp_millis(STARTED).unwrap();
        let (ended, shell, output) = read_indexed_shell(&indexed);
        assert_eq!(ended, started.to_rfc3339());
        assert!(shell.ends_with("⇒ pending"), "{shell}");
        assert_eq!(output, None);

        let completed_shell = serde_json::json!({
            "shellID": "shell1", "command": "make verify", "status": "exited", "exit": 0,
            "output": {"output": "verification complete"}
        });
        source
            .execute(
                "UPDATE session_message SET time_updated=?1, data=?2 WHERE id='shell-row'",
                rusqlite::params![COMPLETED, completed_shell.to_string()],
            )
            .unwrap();

        let completed = chrono::DateTime::from_timestamp_millis(COMPLETED).unwrap();
        let adapter = adapters::by_name("opencode").unwrap();
        assert_eq!(adapter.modified_at(&key), Some(completed));
        assert_eq!(adapter.parse_key(&key).unwrap().ended, Some(completed));
        index::sync_bounded(&mut indexed, Some("opencode"), since).unwrap();

        let (ended, shell, output) = read_indexed_shell(&indexed);
        assert_eq!(ended, completed.to_rfc3339());
        assert!(shell.ends_with("⇒ ok · 1 line"), "{shell}");
        assert_eq!(output.as_deref(), Some("verification complete"));
    }
}
