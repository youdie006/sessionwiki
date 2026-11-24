//! A bounded sync uses actual dates independently of change-token units.
use super::*;
use crate::adapters::Muse;
use serde_json::json;

const CUTOFF: i64 = 1_800_000_000;

fn write(path: &Path, value: serde_json::Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("{value}\n")).unwrap();
}

#[test]
fn muse_bounds_initial_sync_by_date_and_reports_real_time_spans() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("sessions");
    for id in ["old", "recent"] {
        let path = root.join("2026/11/10").join(id).join("session.jsonl");
        write(
            &path,
            json!({
                "schema_version":1,"id":id,"stream":{"kind":"session","id":id},
                "sequence":1,"recorded_at":1_700_000_000_000_000i64,
                "record_type":"event","durability":"durable",
                "payload_type":"runtime.user_intent.accepted","payload_schema_version":1,
                "payload":{"intent_id":id,"model_messages":[{"role":"user","content":[{"kind":"text","text":"native age fixture"}]}]}
            }),
        );
        let seconds = if id == "recent" {
            CUTOFF + 60
        } else {
            CUTOFF - 86_400
        };
        std::fs::File::open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds as u64),
            ))
            .unwrap();
    }
    let adapter: Box<dyn Adapter> = Box::new(Muse::in_sessions_root(&root));
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
        "recent sync must skip old histories"
    );
    let hits = search(&connection, "fixture", 10, Some("muse"), None).unwrap();
    assert_eq!(hits.len(), 1, "indexed conversation is searchable");
    assert!(
        hits.iter().all(|hit| Path::new(&hit.row.path)
            .components()
            .any(|component| component.as_os_str() == "recent")),
        "bounded sync must select recent paths"
    );
    sync_with(&mut connection, &adapters, None).unwrap();
    assert_eq!(
        indexed(&connection),
        2,
        "full sync still indexes old histories"
    );
}
