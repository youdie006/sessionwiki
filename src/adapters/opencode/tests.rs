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
    let store = store_in(vec![db.clone()]).unwrap();
    assert!(!store.had_error);
    assert_eq!(store.keys, [(make_key(&db, "ses_v1"), 2000)]);
    let session = OpenCode.parse_key(&store.keys[0].0).unwrap();
    assert_eq!(session.title, "Legacy title");
    assert_eq!(session.messages[0].text, "legacy search marker");
}

#[test]
fn v2_fixture_has_structured_tools_results_and_edit_evidence() {
    let (_dir, db) = database(V2);
    let store = store_in(vec![db.clone()]).unwrap();
    assert!(!store.had_error);
    assert_eq!(store.keys, [(make_key(&db, SID), 1718630500000)]);
    let session = OpenCode.parse_key(&store.keys[0].0).unwrap();
    let roles: Vec<_> = session.messages.iter().map(|m| m.role.label()).collect();
    assert_eq!(roles, ["user", "assistant", "tool", "tool", "assistant"]);
    assert_eq!(session.touched, ["src/auth.ts"]);
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
    let store = store_in(vec![db.clone()]).unwrap();
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
        OpenCode.parse_key(&make_key(&db, SID)).unwrap().title,
        "Migrate authentication to JWT"
    );
    assert_eq!(
        OpenCode.parse_key(&make_key(&db, "ses_v1")).unwrap().title,
        "Legacy title"
    );
    // An empty v2 table must still permit a genuinely v1-only session.
    conn.execute("DELETE FROM session_v2", []).unwrap();
    assert_eq!(
        OpenCode.parse_key(&make_key(&db, SID)).unwrap().title,
        "Stale legacy title"
    );
}

#[test]
fn native_ids_in_different_databases_remain_distinct() {
    let (_a, first) = database(V2);
    let (_b, second) = database(V2);
    let store = store_in(vec![first.clone(), second.clone()]).unwrap();
    assert!(!store.had_error);
    assert_eq!(store.keys.len(), 2);
    assert_ne!(store.keys[0].0, store.keys[1].0);
    assert_ne!(
        OpenCode.parse_key(&store.keys[0].0).unwrap().id,
        OpenCode.parse_key(&store.keys[1].0).unwrap().id
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
        let error = OpenCode.parse_key(&make_key(&db, SID)).unwrap_err();
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
        let error = OpenCode.parse_key(&make_key(&db, SID)).unwrap_err();
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
            let store = store_in(vec![db.clone()]).unwrap();
            assert!(store.had_error, "{broken}/{fault}");
            assert_eq!(store.keys, [(make_key(&db, "good"), 7)], "{broken}/{fault}");
        }
    }
}

#[test]
fn unknown_and_corrupt_databases_are_partial_stores_not_json_fallbacks() {
    let (_dir, db) = database("CREATE TABLE unrelated(id TEXT)");
    assert!(store_in(vec![db.clone()]).unwrap().had_error);
    std::fs::write(&db, b"not a SQLite database").unwrap();
    assert!(store_in(vec![db.clone()]).unwrap().had_error);
    assert!(OpenCode.parse_key(&make_key(&db, SID)).is_err());
    std::fs::remove_file(&db).unwrap();
    assert!(store_in(vec![db]).is_none());
}

#[test]
fn v2_failed_tools_keep_the_error_without_claiming_an_edit() {
    let (_dir, db) = database(V2);
    let conn = Connection::open(&db).unwrap();
    let content = json!({"content":[
        {"type":"tool","name":"write","id":"ok","state":{"status":"completed","input":{"path":"ok.ts","content":"ok"},"content":[{"type":"text","text":"written"}]}},
        {"type":"tool","name":"edit","id":"bad","state":{"status":"error","input":{"path":"bad.ts","newString":"bad"},"error":"permission denied"}},
        {"type":"tool","name":"write","id":"failed","state":{"status":"failed","input":{"path":"failed.ts","content":"bad"}}}
    ]});
    conn.execute(
        "UPDATE session_message SET data=?1 WHERE type='assistant'",
        [content.to_string()],
    )
    .unwrap();
    let session = OpenCode.parse_key(&make_key(&db, SID)).unwrap();
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
        "INSERT INTO session_message VALUES('last-id',?1,'user',0,1999999999999,?2)",
        [SID, r#"{"text":"First by sequence"}"#],
    )
    .unwrap();
    let session = OpenCode.parse_key(&make_key(&db, SID)).unwrap();
    assert!(session.subagent);
    assert_eq!(session.title, "First by sequence");
    assert_eq!(session.messages[0].text, "First by sequence");
}
