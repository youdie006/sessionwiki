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
fn aliased_grok_and_muse_homes_reuse_legacy_rows_and_reconcile_deletions() {
    let directory = tempfile::tempdir().unwrap();
    for tool in ["grok-build", "muse"] {
        let home = directory.path().join(tool);
        fixtures(&home, tool);
        let alias = directory.path().join(format!("{tool}-alias"));
        std::os::unix::fs::symlink(&home, &alias).unwrap();
        let root = alias.join("sessions");
        let adapter: Box<dyn Adapter> = if tool == "muse" {
            Box::new(Muse::in_sessions_root(&root))
        } else {
            Box::new(GrokBuild::in_home(&alias))
        };
        let legacy_keys: Vec<_> = ["old", "recent"]
            .into_iter()
            .map(|id| {
                if tool == "muse" {
                    root.join("2026/11/10").join(id).join("session.jsonl")
                } else {
                    root.join("%2Fwork").join(id)
                }
            })
            .collect();
        let mut connection = Connection::open_in_memory().unwrap();
        create_cache_schema(&connection).unwrap();
        // Seed the exact key and ID derivation used before the extraction.
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
