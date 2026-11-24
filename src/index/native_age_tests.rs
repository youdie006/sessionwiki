//! A bounded sync uses a Grok Build session's real file dates, which its
//! opaque change token does not carry.
use super::*;
use crate::adapters::GrokBuild;
use serde_json::json;

const CUTOFF: i64 = 1_800_000_000;

fn write_session(home: &Path, id: &str, seconds: i64) {
    let dir = home.join("sessions/%2Fwork").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let history = dir.join("chat_history.jsonl");
    std::fs::write(
        &history,
        format!(
            "{}\n",
            json!({
                "type":"user","content":[{"type":"text","text":"native age fixture"}],"prompt_index":0
            })
        ),
    )
    .unwrap();
    std::fs::File::open(&history)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new().set_modified(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds as u64),
            ),
        )
        .unwrap();
}

#[test]
fn grok_bounded_sync_uses_file_dates_and_reports_real_time_spans() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    write_session(home, "old", CUTOFF - 86_400);
    write_session(home, "recent", CUTOFF + 60);
    let adapter: Box<dyn Adapter> = Box::new(GrokBuild::in_home(home));
    let report = crate::adapters::report(adapter.as_ref()).unwrap();
    assert_eq!(report.files, 2);
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
    let hits = search(&connection, "fixture", 10, Some("grok-build"), None).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(Path::new(&hits[0].row.path)
        .components()
        .any(|component| component.as_os_str() == "recent"));
    sync_with(&mut connection, &adapters, None).unwrap();
    assert_eq!(
        indexed(&connection),
        2,
        "full sync still indexes old histories"
    );
}
