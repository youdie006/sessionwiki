//! A bounded sync uses actual dates independently of change-token units.
use super::*;
use crate::adapters::{Dsh, GrokBuild, KimiCode, Muse};
use serde_json::{json, Value};

const CUTOFF: i64 = 1_800_000_000;

fn write(path: &Path, value: Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("{value}\n")).unwrap();
}

fn fixtures(home: &Path, tool: &str) {
    let mut index = String::new();
    for id in ["old", "recent"] {
        let root = home.join("sessions");
        match tool {
            "kimi-code" => {
                let dir = root.join(id);
                write(&dir.join("state.json"), json!({"title":id}));
                write(
                    &dir.join("agents/main/wire.jsonl"),
                    json!({
                        "type":"turn.prompt", "origin":{"kind":"user"},
                        "input":[{"type":"text","text":"native age fixture"}]
                    }),
                );
                index.push_str(&format!(
                    "{}\n",
                    json!({
                        "sessionId":id,"sessionDir":dir,"workDir":"/work"
                    })
                ));
            }
            "grok-build" => {
                write(
                    &root.join("%2Fwork").join(id).join("chat_history.jsonl"),
                    json!({
                        "type":"user","content":[{"type":"text","text":"native age fixture"}],"prompt_index":0
                    }),
                );
            }
            "muse" => {
                write(
                    &root.join("2026/11/10").join(id).join("session.jsonl"),
                    json!({
                        "schema_version":1,"id":id,"stream":{"kind":"session","id":id},
                        "sequence":1,"recorded_at":1_700_000_000_000_000i64,
                        "record_type":"event","durability":"durable",
                        "payload_type":"runtime.user_intent.accepted","payload_schema_version":1,
                        "payload":{"intent_id":id,"model_messages":[{"role":"user","content":[{"kind":"text","text":"native age fixture"}]}]}
                    }),
                );
            }
            "dsh" => {
                write(
                    &root.join(format!("{id}.json")),
                    json!({
                        "id":id,"cwd":"/work","messages":[{"role":"user","content":"native age fixture"}]
                    }),
                );
                let log = root.join("work").join(id).join("session.v2.jsonl");
                write(
                    &log,
                    json!({
                        "type":"session","version":2,"id":id,"createdAt":1_700_000_000_000i64,
                        "cwd":"/work","isSeeded":false,"delegationDepth":0
                    }),
                );
                use std::io::Write;
                writeln!(
                    std::fs::OpenOptions::new().append(true).open(&log).unwrap(),
                    "{}",
                    json!({
                        "type":"user/message","seq":1,"time":1_700_000_000_001i64,
                        "data":{"content":[{"type":"text","text":"native age fixture"}]}
                    })
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
    }
    if tool == "kimi-code" {
        std::fs::write(home.join("session_index.jsonl"), index).unwrap();
    }
    for entry in walkdir::WalkDir::new(home) {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() {
            continue;
        }
        let is_recent = entry.path().components().any(|c| c.as_os_str() == "recent")
            || entry.file_name() == "recent.json"
            // A shared index append must not make every old Kimi session recent.
            || entry.file_name() == "session_index.jsonl";
        let seconds = if is_recent {
            CUTOFF + 60
        } else {
            CUTOFF - 86_400
        };
        std::fs::File::open(entry.path())
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds as u64),
            ))
            .unwrap();
    }
}

#[test]
fn native_stores_bound_initial_sync_by_date_and_report_real_time_spans() {
    let directory = tempfile::tempdir().unwrap();
    for tool in ["kimi-code", "grok-build", "muse", "dsh"] {
        let home = directory.path().join(tool);
        fixtures(&home, tool);
        let adapter: Box<dyn Adapter> = match tool {
            "kimi-code" => Box::new(KimiCode::in_home(&home)),
            "grok-build" => Box::new(GrokBuild::in_home(&home)),
            "muse" => Box::new(Muse::in_sessions_root(home.join("sessions"))),
            "dsh" => Box::new(Dsh::in_home(&home)),
            _ => unreachable!(),
        };
        let report = crate::adapters::report(adapter.as_ref()).unwrap();
        assert_eq!(
            report.oldest.unwrap().timestamp(),
            CUTOFF - 86_400,
            "{tool}"
        );
        assert_eq!(report.newest.unwrap().timestamp(), CUTOFF + 60, "{tool}");
        let mut connection = Connection::open_in_memory().unwrap();
        create_cache_schema(&connection).unwrap();
        let adapters = vec![adapter];
        sync_with(&mut connection, &adapters, Some(CUTOFF)).unwrap();
        let indexed = |connection: &Connection| -> i64 {
            connection
                .query_row("SELECT count(*) FROM files", [], |row| row.get(0))
                .unwrap()
        };
        let recent_count = if tool == "dsh" { 2 } else { 1 };
        assert_eq!(
            indexed(&connection),
            recent_count,
            "{tool}: recent sync must skip old histories"
        );
        let hits = search(&connection, "fixture", 10, Some(tool), None).unwrap();
        assert_eq!(
            hits.len() as i64,
            recent_count,
            "{tool}: indexed conversation is searchable"
        );
        assert!(
            hits.iter().all(|hit| Path::new(&hit.row.path)
                .components()
                .any(|component| component.as_os_str() == "recent"
                    || component.as_os_str() == "recent.json")),
            "{tool}: bounded sync must select recent paths"
        );
        sync_with(&mut connection, &adapters, None).unwrap();
        assert_eq!(
            indexed(&connection),
            recent_count * 2,
            "{tool}: full sync still indexes old histories"
        );
    }
}

/// Mj's old Grok/Muse readers wrote keys using the configured profile path,
/// even when it was a symlink. New readers must update those same rows and
/// reconcile deletions under that spelling rather than create canonical twins.
#[cfg(unix)]
#[test]
fn aliased_profile_key_policies_reuse_existing_rows_and_reconcile_deletions() {
    let directory = tempfile::tempdir().unwrap();
    for (tool, configured) in [
        ("grok-build", false),
        ("grok-build", true),
        ("muse", false),
        ("muse", true),
    ] {
        let home = directory.path().join(format!("{tool}-{configured}"));
        fixtures(&home, tool);
        let alias = directory.path().join(format!("{tool}-{configured}-alias"));
        std::os::unix::fs::symlink(&home, &alias).unwrap();
        let root = alias.join("sessions");
        let adapter: Box<dyn Adapter> = if tool == "muse" {
            let reader = Muse::in_sessions_root(&root);
            Box::new(if configured {
                reader.keep_configured_paths()
            } else {
                reader
            })
        } else {
            let reader = GrokBuild::in_home(&alias);
            Box::new(if configured {
                reader.keep_configured_paths()
            } else {
                reader
            })
        };
        let legacy_keys: Vec<_> = ["old", "recent"]
            .into_iter()
            .map(|id| {
                let key = if tool == "muse" {
                    root.join("2026/11/10").join(id).join("session.jsonl")
                } else {
                    root.join("%2Fwork").join(id)
                };
                if configured {
                    key
                } else {
                    key.canonicalize().unwrap()
                }
            })
            .collect();
        let mut connection = Connection::open_in_memory().unwrap();
        create_cache_schema(&connection).unwrap();
        // Seed either the older embedding keys or the canonical SW 0.34.24 keys.
        let transaction = connection.transaction().unwrap();
        for key in &legacy_keys {
            let key_text = key.to_string_lossy();
            let mut session = adapter.parse_key(&key_text).unwrap();
            session.path = key.clone();
            session.id = crate::util::short_id(&key_text);
            index_one(
                &transaction,
                &session,
                &key_text,
                0,
                0,
                ToolOutputMode::Summary,
            )
            .unwrap();
        }
        transaction.commit().unwrap();
        let adapters = vec![adapter];
        sync_with(&mut connection, &adapters, None).unwrap();
        let hits = search(&connection, "fixture", 10, Some(tool), None).unwrap();
        assert_eq!(
            hits.len(),
            2,
            "{tool}: migration must not duplicate legacy rows"
        );
        for hit in &hits {
            assert!(legacy_keys
                .iter()
                .any(|key| key.to_string_lossy() == hit.row.path));
            assert_eq!(hit.row.session_id, crate::util::short_id(&hit.row.path));
            let parsed = adapters[0].parse_key(&hit.row.path).unwrap();
            assert_eq!(parsed.path.to_string_lossy(), hit.row.path);
            assert_eq!(parsed.id, hit.row.session_id);
            let default_reader = crate::adapters::by_name(tool).unwrap();
            let reparsed = default_reader.parse_key(&hit.row.path).unwrap();
            assert_eq!(reparsed.path.to_string_lossy(), hit.row.path);
            assert_eq!(reparsed.id, hit.row.session_id);
        }
        if legacy_keys[0].is_dir() {
            std::fs::remove_dir_all(&legacy_keys[0]).unwrap();
        } else {
            std::fs::remove_file(&legacy_keys[0]).unwrap();
        }
        sync_with(&mut connection, &adapters, None).unwrap();
        let hits = search(&connection, "fixture", 10, Some(tool), None).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits.iter().filter(|hit| hit.row.archived).count(), 1);
        assert!(hits
            .iter()
            .any(|hit| hit.row.path == legacy_keys[0].to_string_lossy() && hit.row.archived));
        assert!(hits
            .iter()
            .any(|hit| hit.row.path == legacy_keys[1].to_string_lossy() && !hit.row.archived));
    }
}

#[cfg(unix)]
#[test]
fn standalone_and_embedded_writers_share_alias_identity_without_churn_or_lost_notes() {
    fn rows(conn: &Connection) -> Vec<(String, String)> {
        conn.prepare("SELECT path, session_id FROM files ORDER BY path")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }
    for tool in ["grok-build", "muse"] {
        for first_configured in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let home = directory.path().join("home");
            let other = directory.path().join("other");
            fixtures(&home, tool);
            fixtures(&other, tool); // same native IDs, different physical home
            let other_physical = crate::util::canonicalize_as_recorded(&other).unwrap();
            let alias = directory.path().join("alias");
            std::os::unix::fs::symlink(&home, &alias).unwrap();
            let mut conn = Connection::open_in_memory().unwrap();
            create_cache_schema(&conn).unwrap();
            // Cover both writer orders, including a shared sync call whose
            // known-file snapshot initially contains neither spelling.
            sync_with(
                &mut conn,
                &[
                    alias_reader(tool, &alias, first_configured),
                    alias_reader(tool, &alias, !first_configured),
                    alias_reader(tool, &other, false),
                ],
                None,
            )
            .unwrap();
            let original = rows(&conn);
            assert_eq!(original.len(), 4, "{tool}: aliases must not add rows");
            let owned = |path: &str| !Path::new(path).starts_with(&other_physical);
            let recent = original
                .iter()
                .find(|(path, _)| owned(path) && path.contains("recent"))
                .unwrap()
                .clone();
            conn.execute(
                "INSERT INTO notes VALUES(?1, 'keep this note', 'now')",
                [&recent.1],
            )
            .unwrap();
            conn.execute("INSERT INTO tags VALUES(?1, 'keep-tag')", [&recent.1])
                .unwrap();
            let changes = conn.total_changes();
            for mode in [!first_configured, first_configured, !first_configured] {
                sync_with(&mut conn, &[alias_reader(tool, &alias, mode)], None).unwrap();
            }
            assert_eq!(rows(&conn), original);
            assert_eq!(
                conn.total_changes(),
                changes,
                "{tool}: switching writer must not reparse/archive unchanged rows"
            );
            let source = if tool == "muse" {
                home.join("sessions/2026/11/10/recent/session.jsonl")
            } else {
                home.join("sessions/%2Fwork/recent/chat_history.jsonl")
            };
            let reply = if tool == "muse" {
                json!({"schema_version":1,"id":"reply","stream":{"kind":"session","id":"recent"},
                    "sequence":2,"recorded_at":1_700_000_000_000_001i64,"record_type":"event","durability":"durable",
                    "payload_type":"runtime.session","payload_schema_version":1,
                    "payload":{"kind":"run","event":{"kind":"assistant_message_committed","message_id":"reply","text":"shared alias update"}}})
            } else {
                json!({"type":"assistant","content":"shared alias update"})
            };
            use std::io::Write;
            writeln!(
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&source)
                    .unwrap(),
                "{reply}"
            )
            .unwrap();
            sync_with(
                &mut conn,
                &[alias_reader(tool, &alias, !first_configured)],
                None,
            )
            .unwrap();
            let hits = search(&conn, "shared alias update", 10, Some(tool), None).unwrap();
            assert_eq!(hits.len(), 1);
            assert_eq!(
                (&hits[0].row.path, &hits[0].row.session_id),
                (&recent.0, &recent.1)
            );
            assert_eq!(
                note_for(&conn, &recent.1).unwrap().as_deref(),
                Some("keep this note")
            );
            assert_eq!(
                conn.query_row(
                    "SELECT tag FROM tags WHERE session_id=?1",
                    [&recent.1],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
                "keep-tag"
            );
            assert_eq!(rows(&conn), original);
            let old_source = if tool == "muse" {
                home.join("sessions/2026/11/10/old/session.jsonl")
            } else {
                home.join("sessions/%2Fwork/old/chat_history.jsonl")
            };
            let old_body = std::fs::read(&old_source).unwrap();
            if tool == "muse" {
                std::fs::remove_file(&old_source).unwrap();
            } else {
                std::fs::remove_dir_all(old_source.parent().unwrap()).unwrap();
            }
            sync_with(
                &mut conn,
                &[alias_reader(tool, &alias, !first_configured)],
                None,
            )
            .unwrap();
            let hits = search(&conn, "fixture", 10, Some(tool), None).unwrap();
            assert_eq!(hits.len(), 4);
            assert_eq!(hits.iter().filter(|hit| hit.row.archived).count(), 1);
            assert!(hits
                .iter()
                .all(|hit| !Path::new(&hit.row.path).starts_with(&other_physical)
                    || !hit.row.archived));
            std::fs::create_dir_all(old_source.parent().unwrap()).unwrap();
            std::fs::write(&old_source, old_body).unwrap();
            sync_with(
                &mut conn,
                &[alias_reader(tool, &alias, first_configured)],
                None,
            )
            .unwrap();
            assert_eq!(rows(&conn), original);
            assert!(search(&conn, "fixture", 10, Some(tool), None)
                .unwrap()
                .iter()
                .all(|hit| !hit.row.archived));
        }
    }
}

#[cfg(unix)]
fn alias_reader(tool: &str, home: &Path, configured: bool) -> Box<dyn Adapter> {
    if tool == "muse" {
        let reader = Muse::in_sessions_root(home.join("sessions"));
        Box::new(if configured {
            reader.keep_configured_paths()
        } else {
            reader
        })
    } else {
        let reader = GrokBuild::in_home(home);
        Box::new(if configured {
            reader.keep_configured_paths()
        } else {
            reader
        })
    }
}

#[cfg(unix)]
#[test]
fn aliases_do_not_reconcile_a_different_store_nested_under_the_same_directory() {
    for tool in ["grok-build", "muse"] {
        let directory = tempfile::tempdir().unwrap();
        let outer = directory.path().join("outer");
        let nested = outer.join("sessions/nested/profile");
        fixtures(&outer, tool);
        fixtures(&nested, tool);
        let alias = directory.path().join("nested-alias");
        std::os::unix::fs::symlink(&nested, &alias).unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        create_cache_schema(&conn).unwrap();
        sync_with(&mut conn, &[alias_reader(tool, &alias, true)], None).unwrap();
        sync_with(&mut conn, &[alias_reader(tool, &outer, false)], None).unwrap();
        let hits = search(&conn, "fixture", 10, Some(tool), None).unwrap();
        assert_eq!(hits.len(), 4);
        assert!(
            hits.iter().all(|hit| !hit.row.archived),
            "{tool}: outer sync must not archive a nested store"
        );
    }
}

#[cfg(unix)]
#[test]
fn existing_duplicate_alias_owner_does_not_change_when_files_rows_are_replaced() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    fixtures(&home, "grok-build");
    let alias = directory.path().join("alias");
    std::os::unix::fs::symlink(&home, &alias).unwrap();
    let canonical = home.join("sessions/%2Fwork/recent").canonicalize().unwrap();
    let lexical = alias.join("sessions/%2Fwork/recent");
    let mut keys = [canonical, lexical];
    keys.sort();
    let adapter = alias_reader("grok-build", &alias, false);
    let mut conn = Connection::open_in_memory().unwrap();
    create_cache_schema(&conn).unwrap();
    let tx = conn.transaction().unwrap();
    // Insertion order deliberately differs from stable path ordering.
    for key in keys.iter().rev() {
        let session = adapter.parse_key(&key.to_string_lossy()).unwrap();
        index_one(
            &tx,
            &session,
            &key.to_string_lossy(),
            0,
            0,
            ToolOutputMode::Summary,
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let owner = crate::util::short_id(&keys[0].to_string_lossy());
    conn.execute(
        "INSERT INTO notes VALUES(?1, 'owner-note', 'now')",
        [&owner],
    )
    .unwrap();
    for (round, configured) in [true, false, true].into_iter().enumerate() {
        use std::io::Write;
        writeln!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(keys[0].join("chat_history.jsonl"))
                .unwrap(),
            "\n{}",
            json!({"type":"assistant", "content":format!("stable alias owner round{round}")})
        )
        .unwrap();
        sync_with(
            &mut conn,
            &[alias_reader("grok-build", &alias, configured)],
            None,
        )
        .unwrap();
        let hits = search(
            &conn,
            &format!("round{round}"),
            10,
            Some("grok-build"),
            None,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].row.session_id, owner);
        assert_eq!(
            note_for(&conn, &owner).unwrap().as_deref(),
            Some("owner-note")
        );
    }
}

#[cfg(unix)]
#[test]
fn concurrent_alias_writers_choose_one_identity_without_locking_filesystem_reads() {
    use crate::adapters::{Discovered, Store};
    use crate::model::Session;
    use std::sync::{Arc, Barrier};
    struct UnlockedStore {
        inner: Box<dyn Adapter>,
        database: PathBuf,
    }
    impl Adapter for UnlockedStore {
        fn name(&self) -> &'static str {
            self.inner.name()
        }
        fn root(&self) -> Option<PathBuf> {
            self.inner.root()
        }
        fn discover(&self) -> Discovered {
            self.inner.discover()
        }
        fn parse(&self, path: &Path) -> Result<Session> {
            self.inner.parse(path)
        }
        fn parse_key(&self, key: &str) -> Result<Session> {
            let mut probe = Connection::open(&self.database).unwrap();
            probe
                .busy_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            probe
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .expect("parsing must not hold a writer lock")
                .commit()
                .unwrap();
            self.inner.parse_key(key)
        }
        fn modified_at(&self, key: &str) -> Option<chrono::DateTime<chrono::Utc>> {
            self.inner.modified_at(key)
        }
        fn reconcile_scope(&self) -> Option<String> {
            self.inner.reconcile_scope()
        }
        fn path_key_depth(&self) -> Option<usize> {
            self.inner.path_key_depth()
        }
        fn store(&self) -> Option<Store> {
            let mut probe = Connection::open(&self.database).unwrap();
            probe
                .busy_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            probe
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .expect("inventory must not hold a writer lock")
                .commit()
                .unwrap();
            self.inner.store()
        }
    }
    for tool in ["grok-build", "muse"] {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        fixtures(&home, tool);
        let alias = directory.path().join("alias");
        std::os::unix::fs::symlink(&home, &alias).unwrap();
        let database = directory.path().join("index.db");
        let conn = Connection::open(&database).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL").unwrap();
        create_cache_schema(&conn).unwrap();
        drop(conn);
        let barrier = Arc::new(Barrier::new(2));
        let threads: Vec<_> = [false, true]
            .into_iter()
            .map(|configured| {
                let database = database.clone();
                let alias = alias.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let mut conn = Connection::open(&database).unwrap();
                    conn.busy_timeout(std::time::Duration::from_secs(10))
                        .unwrap();
                    let reader = UnlockedStore {
                        inner: alias_reader(tool, &alias, configured),
                        database,
                    };
                    barrier.wait();
                    sync_with(&mut conn, &[Box::new(reader)], None).unwrap();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let conn = Connection::open(&database).unwrap();
        let hits = search(&conn, "fixture", 10, Some(tool), None).unwrap();
        assert_eq!(
            hits.len(),
            2,
            "{tool}: concurrent policies must share one row per session"
        );
        assert!(hits.iter().all(|hit| !hit.row.archived));
    }
}

#[cfg(unix)]
#[test]
fn stale_alias_inventory_preserves_concurrent_inserts_and_updates() {
    use std::io::Write;
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    fixtures(&home, "grok-build");
    let alias = directory.path().join("alias");
    std::os::unix::fs::symlink(&home, &alias).unwrap();
    let mut conn = Connection::open_in_memory().unwrap();
    create_cache_schema(&conn).unwrap();
    sync_with(&mut conn, &[alias_reader("grok-build", &alias, true)], None).unwrap();
    let reader = alias_reader("grok-build", &alias, true);
    // Two old inventories: one missed 'old', another already parsed it.
    let mut deletion = path_keys::PathKeys::new(&conn, reader.as_ref())
        .unwrap()
        .unwrap();
    let mut parsing = path_keys::PathKeys::new(&conn, reader.as_ref())
        .unwrap()
        .unwrap();
    let store = reader.store().unwrap();
    let old = store
        .keys
        .iter()
        .find(|(key, _)| key.ends_with("/old"))
        .unwrap()
        .0
        .clone();
    let recent = store
        .keys
        .iter()
        .find(|(key, _)| key.ends_with("/recent"))
        .unwrap()
        .0
        .clone();
    deletion.observe(&recent);
    parsing.observe(&old);
    let source = home.join("sessions/%2Fwork/old/chat_history.jsonl");
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(&source)
            .unwrap(),
        "{}",
        json!({"type":"assistant", "content":"concurrent fresh update"})
    )
    .unwrap();
    let new = home.join("sessions/%2Fwork/new/chat_history.jsonl");
    std::fs::create_dir_all(new.parent().unwrap()).unwrap();
    std::fs::copy(&source, new).unwrap();
    // The other spelling publishes an update and a new row after the snapshots.
    sync_with(
        &mut conn,
        &[alias_reader("grok-build", &alias, false)],
        None,
    )
    .unwrap();
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let current = parsing.claim(&tx, &old).unwrap().unwrap();
    assert!(
        !parsing.unchanged_since_snapshot(&old, &current),
        "a stale parse must not overwrite the competing writer's content"
    );
    assert_eq!(
        deletion.reconcile(&tx, true).unwrap(),
        0,
        "an older inventory must not archive concurrent inserts or updates"
    );
    tx.commit().unwrap();
    let hits = search(&conn, "fixture", 10, Some("grok-build"), None).unwrap();
    assert_eq!(hits.len(), 3);
    assert!(hits.iter().all(|hit| !hit.row.archived));
    assert_eq!(
        search(
            &conn,
            "concurrent fresh update",
            10,
            Some("grok-build"),
            None
        )
        .unwrap()
        .len(),
        2
    );
}
