//! A bounded sync uses Kimi sessions' actual dates, not their change tokens.
use super::*;
use crate::adapters::KimiCode;
use serde_json::{json, Value};

const CUTOFF: i64 = 1_800_000_000;

fn write(path: &Path, value: Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("{value}\n")).unwrap();
}

fn set_mtime(path: &Path, seconds: i64) {
    std::fs::File::open(path)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new().set_modified(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds as u64),
            ),
        )
        .unwrap();
}

fn fixtures(home: &Path) {
    let mut index = String::new();
    for id in ["old", "recent"] {
        let dir = home.join("sessions").join(id);
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
            json!({"sessionId":id,"sessionDir":dir,"workDir":"/work"})
        ));
    }
    std::fs::write(home.join("session_index.jsonl"), index).unwrap();
    for entry in walkdir::WalkDir::new(home) {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() {
            continue;
        }
        // A shared index append must not make every old session recent.
        let is_recent = entry.path().components().any(|c| c.as_os_str() == "recent")
            || entry.file_name() == "session_index.jsonl";
        set_mtime(
            entry.path(),
            if is_recent {
                CUTOFF + 60
            } else {
                CUTOFF - 86_400
            },
        );
    }
}

#[test]
fn kimi_bounds_initial_sync_by_date_and_reports_real_time_spans() {
    let home = tempfile::tempdir().unwrap();
    fixtures(home.path());
    let adapter: Box<dyn Adapter> = Box::new(KimiCode::in_home(home.path()));
    let report = crate::adapters::report(adapter.as_ref()).unwrap();
    assert_eq!(report.oldest.unwrap().timestamp(), CUTOFF - 86_400);
    assert_eq!(report.newest.unwrap().timestamp(), CUTOFF + 60);
    let mut connection = Connection::open_in_memory().unwrap();
    create_cache_schema(&connection).unwrap();
    let adapters = vec![adapter];
    sync_with(&mut connection, &adapters, Some(CUTOFF)).unwrap();
    let indexed = |connection: &Connection| -> i64 {
        connection
            .query_row("SELECT count(*) FROM files", [], |row| row.get(0))
            .unwrap()
    };
    assert_eq!(
        indexed(&connection),
        1,
        "a recent sync must skip old histories"
    );
    let hits = search(&connection, "fixture", 10, Some("kimi-code"), None).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(Path::new(&hits[0].row.path)
        .components()
        .any(|component| component.as_os_str() == "recent"));
    sync_with(&mut connection, &adapters, None).unwrap();
    assert_eq!(indexed(&connection), 2, "a full sync indexes old histories");
}
