use super::*;
use serde_json::json;

const SID: &str = "ses_v2_sanitized_001";
const V1: &str = include_str!("../../../tests/fixtures/opencode/v1.sql");
const V2: &str = include_str!("../../../tests/fixtures/opencode/v2_1_1.sql");

fn database(sql: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("opencode.db");
    Connection::open(&path).unwrap().execute_batch(sql).unwrap();
    (dir, path)
}

#[test]
fn v1_discovery_and_decoding_remain_supported() {
    let (_dir, db) = database(V1);
    let store = OpenCode::default().store_in(vec![db.clone()]).unwrap();
    assert!(!store.had_error);
    assert_eq!(store.keys, [(make_key(&db, "ses_v1"), 2000)]);
    let session = OpenCode::default().parse_key(&store.keys[0].0).unwrap();
    assert_eq!(session.title, "Legacy title");
    assert_eq!(session.messages[0].text, "legacy search marker");
}

#[test]
fn v2_fixture_has_structured_tools_results_and_edit_evidence() {
    let (_dir, db) = database(V2);
    let store = OpenCode::default().store_in(vec![db.clone()]).unwrap();
    assert!(!store.had_error);
    assert_eq!(
        store.keys,
        [(
            make_key(&db, SID),
            v2_token(
                Some(1718630400000),
                Some(1718630500000),
                Some(1718630440000)
            )
        )]
    );
    let session = OpenCode::default().parse_key(&store.keys[0].0).unwrap();
    let roles: Vec<_> = session.messages.iter().map(|m| m.role.label()).collect();
    assert_eq!(
        roles,
        [
            "user",
            "assistant",
            "tool",
            "tool",
            "assistant",
            "tool",
            "tool",
            "tool",
            "tool"
        ]
    );
    assert_eq!(
        session.touched,
        ["src/auth.ts", "src/routes.ts", "scripts/rebuild.sh"]
    );
    assert_eq!(session.edits.len(), 1);
    let summaries =
        crate::adapters::parse_session_key(&OpenCode::default(), &make_key(&db, SID)).unwrap();
    for name in ["apply_patch", "patch"] {
        assert!(summaries.messages.iter().any(|message| matches!(
            &message.tool,
            Some(crate::model::ToolEvent::Summary(summary))
                if summary.name == name && summary.output.as_deref() == Some("Patch applied")
        )));
    }
    assert_eq!(
        session.edits[0].snippet,
        "export function verifyToken(token: string) { return true; }"
    );
}

#[test]
fn mixed_inventory_prefers_v2_and_keeps_v1_only_sessions() {
    let (_dir, db) = database(V2);
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(V1).unwrap();
    conn.execute(
        "INSERT INTO session VALUES(?1,NULL,'p1','/old','Stale legacy title',1000,9999)",
        [SID],
    )
    .unwrap();
    let store = OpenCode::default().store_in(vec![db.clone()]).unwrap();
    assert!(!store.had_error);
    assert_eq!(store.keys.len(), 2);
    assert_eq!(
        store
            .keys
            .iter()
            .filter(|(key, _)| key == &make_key(&db, SID))
            .count(),
        1
    );
    assert_eq!(
        OpenCode::default()
            .parse_key(&make_key(&db, SID))
            .unwrap()
            .title,
        "Migrate authentication to JWT"
    );
    assert_eq!(
        OpenCode::default()
            .parse_key(&make_key(&db, "ses_v1"))
            .unwrap()
            .title,
        "Legacy title"
    );
    // An empty v2 table must still permit a genuinely v1-only session.
    conn.execute("DELETE FROM session_v2", []).unwrap();
    assert_eq!(
        OpenCode::default()
            .parse_key(&make_key(&db, SID))
            .unwrap()
            .title,
        "Stale legacy title"
    );
}

#[test]
fn native_ids_in_different_databases_remain_distinct() {
    let (_a, first) = database(V2);
    let (_b, second) = database(V2);
    let store = OpenCode::default()
        .store_in(vec![first.clone(), second.clone()])
        .unwrap();
    assert!(!store.had_error);
    assert_eq!(store.keys.len(), 2);
    assert_ne!(store.keys[0].0, store.keys[1].0);
    assert_ne!(
        OpenCode::default().parse_key(&store.keys[0].0).unwrap().id,
        OpenCode::default().parse_key(&store.keys[1].0).unwrap().id
    );
}

#[test]
fn v2_json_and_row_errors_do_not_fall_back_to_a_valid_v1_copy() {
    for data in ["'not valid JSON'", "CAST(x'ff' AS BLOB)"] {
        let (_dir, db) = database(V2);
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(V1).unwrap();
        conn.execute("UPDATE session SET id=?1", [SID]).unwrap();
        conn.execute_batch(&format!(
            "UPDATE session_message SET data={data} WHERE type='user'"
        ))
        .unwrap();
        let error = OpenCode::default()
            .parse_key(&make_key(&db, SID))
            .unwrap_err();
        let detail = format!("{error:#}");
        assert!(
            detail.contains("malformed message json") || detail.contains("data"),
            "{detail}"
        );
    }
}

#[test]
fn v2_schema_and_message_query_errors_do_not_trigger_v1_fallback() {
    for ddl in [
        "ALTER TABLE session_v2 RENAME COLUMN id TO missing_id",
        "DROP TABLE session_message",
    ] {
        let (_dir, db) = database(V2);
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(V1).unwrap();
        conn.execute("UPDATE session SET id=?1", [SID]).unwrap();
        conn.execute_batch(ddl).unwrap();
        let error = OpenCode::default()
            .parse_key(&make_key(&db, SID))
            .unwrap_err();
        let detail = format!("{error:#}");
        assert!(
            detail.contains("session_v2") || detail.contains("session_message"),
            "{detail}"
        );
    }
}

#[test]
fn both_schema_scans_keep_sql_and_row_failures_visible() {
    for broken in ["session_v2", "session"] {
        let good = if broken == "session" {
            "session_v2"
        } else {
            "session"
        };
        for fault in ["row", "sql"] {
            let (_dir, db) = database("");
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(&format!(
                "CREATE TABLE {broken}(id TEXT, time_updated INTEGER, time_created INTEGER);
                 CREATE TABLE {good}(id TEXT, time_updated INTEGER, time_created INTEGER);
                 INSERT INTO {good} VALUES('good',7,NULL);
                 INSERT INTO {broken} VALUES('bad','not a timestamp',NULL);"
            ))
            .unwrap();
            if fault == "sql" {
                conn.execute_batch(&format!("ALTER TABLE {broken} DROP COLUMN time_updated"))
                    .unwrap();
            }
            let store = OpenCode::default().store_in(vec![db.clone()]).unwrap();
            assert!(store.had_error, "{broken}/{fault}");
            assert_eq!(
                store.keys,
                [(
                    make_key(&db, "good"),
                    if good == "session_v2" {
                        v2_token(None, Some(7), None)
                    } else {
                        7
                    }
                )],
                "{broken}/{fault}"
            );
        }
    }
}

#[test]
fn unreadable_v2_timestamps_reserve_the_id_without_hiding_healthy_v1_rows() {
    let (_dir, db) = database(V2);
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(V1).unwrap();
    conn.execute("INSERT INTO session SELECT * FROM session_v2", [])
        .unwrap();
    conn.execute("UPDATE session_v2 SET time_updated='bad timestamp'", [])
        .unwrap();

    let reader = OpenCode::default();
    let store = reader.store_in(vec![db.clone()]).unwrap();
    assert!(store.had_error);
    assert_eq!(store.keys, [(make_key(&db, "ses_v1"), 2000)]);
}

#[test]
fn unknown_and_corrupt_databases_are_partial_stores_not_json_fallbacks() {
    let (_dir, db) = database("CREATE TABLE unrelated(id TEXT)");
    assert!(
        OpenCode::default()
            .store_in(vec![db.clone()])
            .unwrap()
            .had_error
    );
    std::fs::write(&db, b"not a SQLite database").unwrap();
    assert!(
        OpenCode::default()
            .store_in(vec![db.clone()])
            .unwrap()
            .had_error
    );
    assert!(OpenCode::default().parse_key(&make_key(&db, SID)).is_err());
    std::fs::remove_file(&db).unwrap();
    assert!(OpenCode::default().store_in(vec![db]).is_none());
}

#[test]
fn v2_failed_tools_keep_the_error_without_claiming_an_edit() {
    let (_dir, db) = database(V2);
    let conn = Connection::open(&db).unwrap();
    let content = json!({"content":[
        {"type":"tool","name":"write","id":"ok","state":{"status":"completed","input":{"path":"ok.ts","content":"ok"},"content":[{"type":"text","text":"written"}]}},
        {"type":"tool","name":"edit","id":"bad","state":{"status":"error","input":{"path":"bad.ts","newString":"bad"},"content":[],"error":{"message":"permission denied"}}},
        {"type":"tool","name":"write","id":"failed","state":{"status":"failed","input":{"path":"failed.ts","content":"bad"}}},
        {"type":"tool","name":"write","id":"pending","state":{"status":"pending","input":{"path":"pending.ts","content":"bad"}}},
        {"type":"tool","name":"edit","id":"running","state":{"status":"running","input":{"path":"running.ts","newString":"bad"}}}
    ]});
    conn.execute(
        "DELETE FROM session_message WHERE id IN ('msg_003', 'msg_004')",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE session_message SET data=?1 WHERE id='msg_002'",
        [content.to_string()],
    )
    .unwrap();
    let session = OpenCode::default().parse_key(&make_key(&db, SID)).unwrap();
    assert_eq!(session.touched, ["ok.ts"]);
    assert_eq!(session.edits.len(), 1);
    assert!(session.messages.iter().any(|message| matches!(
        &message.tool,
        Some(crate::model::ToolEvent::Part(crate::model::ToolPart::Result(result)))
            if result.is_error && result.text.contains("permission denied")
    )));
}

#[test]
fn v2_message_sequence_controls_order_and_fallback_title() {
    let (_dir, db) = database(V2);
    let conn = Connection::open(&db).unwrap();
    conn.execute("UPDATE session_v2 SET title='', parent_id='parent'", [])
        .unwrap();
    conn.execute(
        "INSERT INTO session_message VALUES('last-id',?1,'user',0,1999999999999,NULL,?2)",
        [SID, r#"{"text":"First by sequence"}"#],
    )
    .unwrap();
    let session = OpenCode::default().parse_key(&make_key(&db, SID)).unwrap();
    assert!(session.subagent);
    assert_eq!(session.title, "First by sequence");
    assert_eq!(session.messages[0].text, "First by sequence");
}

#[test]
fn v2_shell_commands_and_paged_output_survive_normalization() {
    let (_dir, db) = database(V2);
    let conn = Connection::open(&db).unwrap();
    let data = json!({"shellID":"shell1", "command":"cargo check", "status":"exited", "exit":1,
        "output":{"output":"missing dependency", "cursor":18,"size":18,"truncated":false}});
    conn.execute(
        "INSERT INTO session_message VALUES('shell-row',?1,'shell',3,1718630500000,NULL,?2)",
        [SID, &data.to_string()],
    )
    .unwrap();
    let session =
        crate::adapters::parse_session_key(&OpenCode::default(), &make_key(&db, SID)).unwrap();
    let shell = session
        .messages
        .iter()
        .find_map(|message| match &message.tool {
            Some(crate::model::ToolEvent::Summary(summary)) if summary.name == "shell" => {
                Some(summary)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(shell.args["command"], "cargo check");
    assert_eq!(shell.output.as_deref(), Some("missing dependency"));
    assert_eq!(shell.is_error, Some(true));
}

#[test]
fn schema_tokens_are_distinct_but_activity_dates_remain_real() {
    struct Source(PathBuf, OpenCode);
    impl Adapter for Source {
        fn name(&self) -> &'static str {
            "opencode"
        }
        fn root(&self) -> Option<PathBuf> {
            self.0.parent().map(Path::to_path_buf)
        }
        fn discover(&self) -> Discovered {
            Vec::new().into()
        }
        fn parse(&self, path: &Path) -> Result<Session> {
            OpenCode::default().parse(path)
        }
        fn store(&self) -> Option<Store> {
            self.1.store_in(vec![self.0.clone()])
        }
        fn modified_at(&self, key: &str) -> Option<DateTime<Utc>> {
            self.1.modified_at(key)
        }
    }
    let (_dir, db) = database(V2);
    let key = make_key(&db, SID);
    let store = OpenCode::default().store_in(vec![db.clone()]).unwrap();
    assert_ne!(store.keys[0].1, 1718630500000);
    assert_eq!(
        OpenCode::default().modified_at(&key),
        DateTime::from_timestamp_millis(1718630500000)
    );
    let report = crate::adapters::report(&Source(db, OpenCode::default())).unwrap();
    assert_eq!(
        report.oldest,
        DateTime::from_timestamp_millis(1718630500000)
    );
    assert_eq!(report.newest, report.oldest);
}

#[test]
fn v2_shell_timeout_is_an_error_even_without_output_or_exit_code() {
    let (_dir, db) = database(V2);
    let conn = Connection::open(&db).unwrap();
    let data = json!({"shellID":"timeout1","command":"long command","status":"timeout"});
    conn.execute(
        "INSERT INTO session_message VALUES('timeout-row',?1,'shell',3,1718630500000,NULL,?2)",
        [SID, &data.to_string()],
    )
    .unwrap();
    let session =
        crate::adapters::parse_session_key(&OpenCode::default(), &make_key(&db, SID)).unwrap();
    assert!(session.messages.iter().any(|message| matches!(&message.tool,
        Some(crate::model::ToolEvent::Summary(summary)) if summary.name == "shell" && summary.is_error == Some(true))));
}

#[test]
fn v2_shell_error_without_output_emits_a_failed_tool_result() {
    let (_dir, db) = database(V2);
    let conn = Connection::open(&db).unwrap();
    let data = json!({"shellID":"shell-error","command":"make verify","status":"error"});
    conn.execute(
        "INSERT INTO session_message VALUES('shell-error',?1,'shell',5,1718630500000,NULL,?2)",
        [SID, &data.to_string()],
    )
    .unwrap();

    let session = OpenCode::default().parse_key(&make_key(&db, SID)).unwrap();
    assert!(session.messages.iter().any(|message| matches!(
        &message.tool,
        Some(crate::model::ToolEvent::Part(crate::model::ToolPart::Result(result)))
            if result.call_id.as_deref() == Some("shell-error") && result.is_error
    )));
}

#[test]
fn activity_queries_reuse_the_inventory_snapshot_and_refresh_on_the_next_scan() {
    let (_dir, db) = database(V2);
    let reader = OpenCode::default();
    let store = reader.store_in(vec![db.clone()]).unwrap();
    let key = &store.keys[0].0;
    // No connection or query per key: all metadata remains available from
    // the inventory even if the source disappears before its dates are read.
    std::fs::remove_file(&db).unwrap();
    assert_eq!(
        reader.modified_at(key),
        DateTime::from_timestamp_millis(1718630500000)
    );
    assert!(reader.store_in(vec![db.clone()]).is_none());
    assert_eq!(reader.modified_at(key), None);
    Connection::open(&db).unwrap().execute_batch(V2).unwrap();
    let conn = Connection::open(&db).unwrap();
    conn.execute("UPDATE session_v2 SET time_updated=1800000000000", [])
        .unwrap();
    reader.store_in(vec![db]).unwrap();
    assert_eq!(
        reader.modified_at(key),
        DateTime::from_timestamp_millis(1800000000000)
    );
}

fn migration_marker(conn: &Connection, state: Value) {
    conn.execute_batch("CREATE TABLE kv(key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .unwrap();
    conn.execute(
        "INSERT INTO kv VALUES('migration.v1-v2',?1)",
        [state.to_string()],
    )
    .unwrap();
}

#[test]
fn completed_migration_does_not_resurrect_deleted_v2_sessions_from_legacy_rows() {
    let (_dir, db) = database(V2);
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(V1).unwrap();
    conn.execute("INSERT INTO session SELECT id,parent_id,project_id,directory,title,time_created,time_updated FROM session_v2", []).unwrap();
    conn.execute("INSERT INTO session_v2 SELECT id,parent_id,project_id,directory,title,time_created,time_updated FROM session WHERE id='ses_v1'", []).unwrap();
    migration_marker(&conn, json!({"phase":"completed"}));
    conn.execute("DELETE FROM session_v2 WHERE id=?1", [SID])
        .unwrap();
    let store = OpenCode::default().store_in(vec![db.clone()]).unwrap();
    assert!(!store.had_error);
    assert_eq!(store.keys.len(), 1);
    assert_eq!(store.keys[0].0, make_key(&db, "ses_v1"));
    assert!(OpenCode::default().parse_key(&make_key(&db, SID)).is_err());
    assert_eq!(
        conn.query_row("SELECT count(*) FROM session WHERE id=?1", [SID], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn migration_cursor_preserves_unprocessed_v1_sessions_only() {
    let (_dir, db) = database(V2);
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(V1).unwrap();
    conn.execute("INSERT INTO session SELECT id,parent_id,project_id,directory,title,time_created,time_updated FROM session_v2", []).unwrap();
    migration_marker(&conn, json!({"phase":"sessions", "cursor":SID}));
    conn.execute("DELETE FROM session_v2", []).unwrap();
    let store = OpenCode::default().store_in(vec![db.clone()]).unwrap();
    assert!(!store.had_error);
    assert_eq!(store.keys, [(make_key(&db, "ses_v1"), 2000)]);
    assert_eq!(
        OpenCode::default()
            .parse_key(&make_key(&db, "ses_v1"))
            .unwrap()
            .title,
        "Legacy title"
    );
    assert!(OpenCode::default().parse_key(&make_key(&db, SID)).is_err());
}

#[test]
fn unstarted_migration_keeps_v1_sessions_live_with_an_existing_kv_table() {
    for (key, value) in [
        ("migration.v1-v2", r#"{"phase":"sessions"}"#),
        ("unrelated", "{}"),
    ] {
        let (_dir, db) = database(V2);
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(V1).unwrap();
        conn.execute_batch("CREATE TABLE kv(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
            .unwrap();
        conn.execute("INSERT INTO kv VALUES(?1,?2)", [key, value])
            .unwrap();
        assert!(matches!(
            LegacyPolicy::read(&conn).unwrap(),
            LegacyPolicy::All
        ));

        let reader = OpenCode::default();
        let store = reader.store_in(vec![db.clone()]).unwrap();
        assert!(!store.had_error, "{key}");
        let legacy = make_key(&db, "ses_v1");
        assert!(store.keys.contains(&(legacy.clone(), 2000)), "{key}");
        assert_eq!(reader.parse_key(&legacy).unwrap().title, "Legacy title");
    }
}

#[test]
fn migration_markers_require_the_v2_session_table() {
    for phase in ["sessions", "completed"] {
        let (_dir, db) = database(V1);
        let conn = Connection::open(&db).unwrap();
        migration_marker(&conn, json!({"phase":phase}));
        assert!(LegacyPolicy::read(&conn).is_err(), "{phase}");

        let reader = OpenCode::default();
        let store = reader.store_in(vec![db.clone()]).unwrap();
        assert!(store.had_error, "{phase}");
        assert!(store.keys.is_empty(), "{phase}");
        let error = reader.parse_key(&make_key(&db, "ses_v1")).unwrap_err();
        assert!(format!("{error:#}").contains("OpenCode migration state without session_v2"));
    }
}

#[test]
fn both_v2_watermarks_change_the_token_even_below_the_latest_activity() {
    let (_dir, db) = database(V2);
    let conn = Connection::open(&db).unwrap();
    let reader = OpenCode::default();
    let key = make_key(&db, SID);
    let scan = || {
        let store = reader.store_in(vec![db.clone()]).unwrap();
        assert!(!store.had_error);
        assert_eq!(store.keys.len(), 1);
        assert_eq!(store.keys[0].0, key);
        (store.keys[0].1, reader.modified_at(&key).unwrap())
    };
    let (first_token, first_activity) = scan();
    assert_ne!(first_token, 0);

    conn.execute(
        "UPDATE session_message SET time_updated=1718630450000 WHERE id='msg_004'",
        [],
    )
    .unwrap();
    let (second_token, second_activity) = scan();
    assert_ne!(first_token, second_token);
    assert_eq!(first_activity, second_activity);

    conn.execute(
        "UPDATE session_message SET time_updated=1718630600000 WHERE id='msg_004'",
        [],
    )
    .unwrap();
    let (third_token, third_activity) = scan();
    assert!(third_activity > second_activity);

    conn.execute("UPDATE session_v2 SET time_updated=1718630550000", [])
        .unwrap();
    let (fourth_token, fourth_activity) = scan();
    assert_ne!(third_token, fourth_token);
    assert_eq!(third_activity, fourth_activity);
}

#[test]
fn v2_activity_handles_empty_messages_null_times_and_older_schemas() {
    for sql in [
        "DELETE FROM session_message",
        "UPDATE session_message SET time_updated=NULL",
        "ALTER TABLE session_message DROP COLUMN time_updated",
    ] {
        let (_dir, db) = database(V2);
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(sql).unwrap();
        conn.execute("UPDATE session_v2 SET time_updated=NULL", [])
            .unwrap();
        let key = make_key(&db, SID);
        let expected = DateTime::from_timestamp_millis(1718630400000);
        let reader = OpenCode::default();
        assert_eq!(reader.modified_at(&key), expected, "{sql}");
        let store = reader.store_in(vec![db]).unwrap();
        assert!(!store.had_error, "{sql}");
        assert_eq!(store.keys.len(), 1, "{sql}");
        assert_eq!(reader.modified_at(&key), expected, "{sql}");
        assert_eq!(reader.parse_key(&key).unwrap().ended, expected, "{sql}");
    }
}

#[test]
fn invalid_migration_state_keeps_discovery_partial_without_losing_valid_v2_rows() {
    for value in [
        "invalid json",
        r#"{"phase":"future"}"#,
        r#"{"phase":"sessions","cursor":42}"#,
    ] {
        let (_dir, db) = database(V2);
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(V1).unwrap();
        conn.execute_batch("CREATE TABLE kv(key TEXT PRIMARY KEY,value TEXT NOT NULL)")
            .unwrap();
        conn.execute("INSERT INTO kv VALUES('migration.v1-v2',?1)", [value])
            .unwrap();
        let store = OpenCode::default().store_in(vec![db.clone()]).unwrap();
        assert!(store.had_error);
        assert_eq!(store.keys.len(), 1);
        assert_eq!(store.keys[0].0, make_key(&db, SID));
        assert!(OpenCode::default()
            .parse_key(&make_key(&db, "ses_v1"))
            .is_err());
    }
}
