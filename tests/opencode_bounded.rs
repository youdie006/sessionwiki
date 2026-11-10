//! Schema-tagged fingerprints must not make old sessions look recent.
use sessionwiki::index;

#[test]
fn bounded_sync_uses_activity_dates_instead_of_schema_tagged_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let db = data.join("opencode/opencode.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let source = rusqlite::Connection::open(&db).unwrap();
    source
        .execute_batch(include_str!("fixtures/opencode/v2_1_1.sql"))
        .unwrap();
    source.execute_batch("INSERT INTO session_v2 VALUES('recent',NULL,'p','/work','Recent',1800000060000,1800000060000)").unwrap();
    // This integration binary has one test. Every source and the index are
    // under the disposable directory; no live index or source is opened.
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
