//! Codex now keeps most of its history in `thread_history_1.sqlite`, and the
//! rollout files it migrated from disappear. On one reporter's machine 2,987
//! threads had history but only 128 were indexed, all from the rollouts left
//! in `sessions/`; 47 more rollouts sat in `archived_sessions/`, never scanned.

use super::*;
use crate::adapters::{Adapter, Codex, CodexHistory};
use std::path::{Path, PathBuf};

const GONE: &str = "019a0000-0000-7000-8000-000000000001";
const LIVE: &str = "019a0000-0000-7000-8000-000000000002";

fn adapters(home: &Path) -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(Codex::in_home(home)),
        Box::new(Codex::archived_in(home)),
        Box::new(CodexHistory::in_home(home)),
    ]
}

fn rollout(home: &Path, dir: &str, id: &str, text: &str) -> PathBuf {
    let path = home
        .join(dir)
        .join("2026/10/01")
        .join(format!("rollout-2026-10-01T10-00-00-{id}.jsonl"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        format!(
            "{{\"timestamp\":\"2026-10-01T10:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"/repo\"}}}}\n\
             {{\"timestamp\":\"2026-10-01T10:00:01Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"{text}\"}}}}\n"
        ),
    )
    .unwrap();
    path
}

/// Write a Codex history db and its thread registry with one row per thread.
fn history(home: &Path, threads: &[(&str, &str, &[serde_json::Value])]) {
    let db = Connection::open(home.join("thread_history_1.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TABLE thread_items(thread_id TEXT NOT NULL, turn_id TEXT NOT NULL,
            item_id TEXT NOT NULL, rollout_ordinal INTEGER NOT NULL,
            created_at_ms INTEGER NOT NULL, item_json TEXT NOT NULL,
            item_type TEXT NOT NULL DEFAULT '', updated_at_ordinal INTEGER NOT NULL DEFAULT 0,
            started_at_ms INTEGER, completed_at_ms INTEGER,
            PRIMARY KEY (thread_id, turn_id, item_id));",
    )
    .unwrap();
    let state = Connection::open(home.join("state_5.sqlite")).unwrap();
    state
        .execute_batch(
            "CREATE TABLE threads(id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL,
                cwd TEXT NOT NULL, title TEXT NOT NULL, source TEXT NOT NULL,
                thread_source TEXT, history_mode TEXT NOT NULL DEFAULT 'paginated');",
        )
        .unwrap();
    for (id, source, items) in threads {
        state
            .execute(
                "INSERT INTO threads(id, rollout_path, cwd, title, source, thread_source)
                 VALUES (?1, ?2, '/repo', 'registry title', ?3, ?4)",
                params![
                    id,
                    home.join(format!("sessions/gone-{id}.jsonl"))
                        .to_string_lossy(),
                    if *source == "subagent" {
                        r#"{"subagent":{"thread_spawn":{"depth":1}}}"#
                    } else {
                        "cli"
                    },
                    source,
                ],
            )
            .unwrap();
        for (n, item) in items.iter().enumerate() {
            db.execute(
                "INSERT INTO thread_items(thread_id, turn_id, item_id, rollout_ordinal,
                    created_at_ms, item_json, item_type)
                 VALUES (?1, 't1', ?2, ?3, ?4, ?5, ?6)",
                params![
                    id,
                    format!("i{n}"),
                    n as i64,
                    1_790_000_000_000i64 + n as i64 * 1000,
                    item.to_string(),
                    item["type"].as_str().unwrap(),
                ],
            )
            .unwrap();
        }
    }
}

fn user(text: &str) -> serde_json::Value {
    serde_json::json!({"type":"userMessage","id":"u","content":[{"type":"text","text":text}]})
}

fn agent(text: &str) -> serde_json::Value {
    serde_json::json!({"type":"agentMessage","id":"a","text":text})
}

fn index() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    create_cache_schema(&c).unwrap();
    c
}

/// (path, archived) of every Codex row, in path order.
fn codex_rows(c: &Connection) -> Vec<(String, bool)> {
    let mut stmt = c
        .prepare(
            "SELECT path, archived_at IS NOT NULL FROM files WHERE tool = 'codex' ORDER BY path",
        )
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn messages_matching(c: &Connection, text: &str) -> i64 {
    c.query_row(
        "SELECT count(*) FROM messages WHERE text = ?1",
        [text],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn a_thread_whose_rollout_is_gone_is_indexed_from_thread_history() {
    let home = tempfile::tempdir().unwrap();
    rollout(home.path(), "sessions", LIVE, "kept in a rollout");
    history(
        home.path(),
        &[
            (
                GONE,
                "user",
                &[
                    user("prune restic snapshots"),
                    agent("pruned three snapshots"),
                ],
            ),
            (LIVE, "user", &[user("kept in a rollout")]),
        ],
    );
    let mut c = index();
    sync_with(&mut c, &adapters(home.path()), None).unwrap();

    let rows = codex_rows(&c);
    assert_eq!(rows.len(), 2, "one row per thread: {rows:?}");
    let gone = rows
        .iter()
        .find(|(p, _)| native_id_of(p).as_deref() == Some(GONE))
        .expect("the thread without a rollout is indexed");
    assert!(!gone.1, "it is live, not archived");
    assert_eq!(messages_matching(&c, "prune restic snapshots"), 1);
    assert_eq!(messages_matching(&c, "pruned three snapshots"), 1);
    assert_eq!(
        messages_matching(&c, "kept in a rollout"),
        1,
        "a thread that still has its rollout is read from the rollout alone"
    );
    let project: String = c
        .query_row(
            "SELECT project FROM files WHERE path = ?1",
            [&gone.0],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(project, crate::util::project_key("/repo"));
}

#[test]
fn a_subagent_thread_from_history_is_a_subagent() {
    let home = tempfile::tempdir().unwrap();
    history(
        home.path(),
        &[(GONE, "subagent", &[agent("worker output")])],
    );
    let mut c = index();
    sync_with(&mut c, &adapters(home.path()), None).unwrap();
    let kind: String = c
        .query_row("SELECT kind FROM files WHERE tool = 'codex'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(kind, "sub");
}

#[test]
fn history_tool_calls_keep_their_outcome_and_edited_files() {
    let home = tempfile::tempdir().unwrap();
    history(
        home.path(),
        &[(
            GONE,
            "user",
            &[
                serde_json::json!({"type":"commandExecution","id":"c1","command":"cargo test",
                    "cwd":"/repo","status":"failed","aggregatedOutput":"1 failed","exitCode":101}),
                serde_json::json!({"type":"fileChange","id":"f1","status":"completed",
                    "changes":[{"path":"/repo/src/lib.rs","kind":{"type":"update","move_path":null},"diff":"@@ -1 +1 @@\n-a\n+b\n"}]}),
                serde_json::json!({"type":"mcpToolCall","id":"m1","server":"playwright",
                    "tool":"browser_click","status":"failed","arguments":{"ref":"e1"},
                    "result":null,"error":{"message":"element not found"}}),
                serde_json::json!({"type":"mcpToolCall","id":"m2","server":"playwright",
                    "tool":"browser_tabs","status":"completed","arguments":{},
                    "result":{"content":[{"type":"text","text":"0: about:blank"}]},"error":null}),
            ],
        )],
    );
    let adapter = CodexHistory::in_home(home.path());
    let store = adapter.store().expect("the history db is a store");
    assert_eq!(store.keys.len(), 1);
    let session = adapter.parse_key(&store.keys[0].0).unwrap();
    assert_eq!(session.touched, vec!["/repo/src/lib.rs".to_string()]);
    let results: Vec<(String, bool)> = session
        .messages
        .iter()
        .filter_map(|m| match &m.tool {
            Some(crate::model::ToolEvent::Part(crate::model::ToolPart::Result(r))) => {
                Some((r.text.clone(), r.is_error))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        vec![
            ("1 failed".to_string(), true),
            (String::new(), false),
            ("element not found".to_string(), true),
            ("0: about:blank".to_string(), false),
        ]
    );
    let calls: Vec<String> = session
        .messages
        .iter()
        .filter_map(|m| match &m.tool {
            Some(crate::model::ToolEvent::Part(crate::model::ToolPart::Call(c))) => {
                Some(c.name.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        calls,
        [
            "exec_command",
            "apply_patch",
            "browser_click",
            "browser_tabs"
        ]
    );
}

#[test]
fn archived_sessions_rollouts_are_indexed() {
    let home = tempfile::tempdir().unwrap();
    rollout(home.path(), "archived_sessions", GONE, "an archived thread");
    let mut c = index();
    sync_with(&mut c, &adapters(home.path()), None).unwrap();
    assert_eq!(messages_matching(&c, "an archived thread"), 1);
    assert_eq!(codex_rows(&c).len(), 1);
}

/// Codex archives a thread by moving its rollout into `archived_sessions/`.
/// The old path is gone, so its row is kept as archived; the new path must not
/// add a second listing of the same conversation beside it.
#[test]
fn a_rollout_moved_to_archived_sessions_is_listed_once_and_keeps_its_tags() {
    let home = tempfile::tempdir().unwrap();
    let before = rollout(home.path(), "sessions", GONE, "moved later");
    let mut c = index();
    sync_with(&mut c, &adapters(home.path()), None).unwrap();
    let old_sid: String = c
        .query_row(
            "SELECT session_id FROM files WHERE tool = 'codex'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    add_tag(&c, &old_sid, "release").unwrap();

    let after = home
        .path()
        .join("archived_sessions")
        .join(before.file_name().unwrap());
    std::fs::create_dir_all(after.parent().unwrap()).unwrap();
    std::fs::rename(&before, &after).unwrap();
    sync_with(&mut c, &adapters(home.path()), None).unwrap();

    assert_eq!(
        codex_rows(&c),
        vec![(after.to_string_lossy().into_owned(), false)]
    );
    assert_eq!(messages_matching(&c, "moved later"), 1);
    let tagged: i64 = c
        .query_row(
            "SELECT count(*) FROM tags t JOIN files f ON f.session_id = t.session_id
             WHERE t.tag = 'release' AND f.path = ?1",
            [after.to_string_lossy()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tagged, 1, "the tag follows the session to its new path");
    assert_eq!(
        c.query_row("SELECT count(*) FROM archive", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

/// After the migration Codex can drop a rollout whose thread lives on in the
/// history db. The conversation moves from the rollout row to the history row.
#[test]
fn a_rollout_deleted_after_migration_continues_as_its_history_row() {
    let home = tempfile::tempdir().unwrap();
    let file = rollout(home.path(), "sessions", GONE, "migrated thread");
    history(home.path(), &[(GONE, "user", &[user("migrated thread")])]);
    let mut c = index();
    sync_with(&mut c, &adapters(home.path()), None).unwrap();
    assert_eq!(
        codex_rows(&c),
        vec![(file.to_string_lossy().into_owned(), false)],
        "while the rollout exists it is the only source"
    );

    std::fs::remove_file(&file).unwrap();
    sync_with(&mut c, &adapters(home.path()), None).unwrap();
    let rows = codex_rows(&c);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(!rows[0].1);
    assert_eq!(native_id_of(&rows[0].0).as_deref(), Some(GONE));
    assert_eq!(messages_matching(&c, "migrated thread"), 1);
}

/// A rollout that held more than the history row (a partial projection) must
/// not be replaced by it: the archived copy stays.
#[test]
fn a_shorter_history_row_does_not_replace_a_kept_rollout() {
    let home = tempfile::tempdir().unwrap();
    let file = rollout(home.path(), "sessions", GONE, "first prompt");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .unwrap()
        .write_all(b"{\"timestamp\":\"2026-10-01T10:00:02Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"second prompt\"}}\n")
        .unwrap();
    history(home.path(), &[(GONE, "user", &[user("first prompt")])]);
    let mut c = index();
    sync_with(&mut c, &adapters(home.path()), None).unwrap();
    std::fs::remove_file(&file).unwrap();
    sync_with(&mut c, &adapters(home.path()), None).unwrap();
    assert_eq!(messages_matching(&c, "second prompt"), 1);
    assert_eq!(codex_rows(&c).len(), 2);
}

#[test]
fn every_codex_store_syncs_under_the_codex_tool_name() {
    let names: Vec<&str> = crate::adapters::named("codex")
        .iter()
        .map(|a| a.name())
        .collect();
    assert_eq!(names, ["codex", "codex", "codex"]);
}

fn compress(path: &Path) -> PathBuf {
    let zst = path.with_extension("jsonl.zst");
    let raw = std::fs::read(path).unwrap();
    std::fs::write(
        &zst,
        ruzstd::encoding::compress_to_vec(
            raw.as_slice(),
            ruzstd::encoding::CompressionLevel::Fastest,
        ),
    )
    .unwrap();
    std::fs::remove_file(path).unwrap();
    zst
}

/// Codex compresses older rollouts in place. On one machine 678 of 684
/// rollouts were `.jsonl.zst`; none were indexed, and the 667 already indexed
/// were kept as deleted sessions once their `.jsonl` disappeared.
#[test]
fn a_rollout_compressed_in_place_is_read_and_listed_once() {
    let home = tempfile::tempdir().unwrap();
    let plain = rollout(home.path(), "sessions", GONE, "compressed later");
    let mut c = index();
    sync_with(&mut c, &adapters(home.path()), None).unwrap();
    let old_sid: String = c
        .query_row(
            "SELECT session_id FROM files WHERE tool = 'codex'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    add_tag(&c, &old_sid, "keep").unwrap();

    let zst = compress(&plain);
    sync_with(&mut c, &adapters(home.path()), None).unwrap();
    assert_eq!(
        codex_rows(&c),
        vec![(zst.to_string_lossy().into_owned(), false)]
    );
    assert_eq!(messages_matching(&c, "compressed later"), 1);
    let tagged: i64 = c
        .query_row(
            "SELECT count(*) FROM tags t JOIN files f ON f.session_id = t.session_id
             WHERE t.tag = 'keep' AND f.path = ?1",
            [zst.to_string_lossy()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tagged, 1);
}

#[test]
fn a_thread_with_a_compressed_rollout_is_not_read_from_history() {
    let home = tempfile::tempdir().unwrap();
    compress(&rollout(home.path(), "sessions", GONE, "only once"));
    history(home.path(), &[(GONE, "user", &[user("only once")])]);
    let mut c = index();
    sync_with(&mut c, &adapters(home.path()), None).unwrap();
    assert_eq!(codex_rows(&c).len(), 1, "{:?}", codex_rows(&c));
    assert_eq!(messages_matching(&c, "only once"), 1);
}
