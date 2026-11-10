//! Real CLI sync must preserve cached sessions after partial SQL discovery.
use rusqlite::Connection;
use std::path::Path;

fn sync(home: &Path, db: &Path, cache: &Path) {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_sessionwiki"))
        .args(["sync", "--tool", "opencode"])
        .env("OPENCODE_DB", db)
        .env("XDG_DATA_HOME", home)
        .env("SESSIONWIKI_DATA", cache)
        .env_remove("SESSIONWIKI_NO_ARCHIVE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn rows(cache: &Path) -> Vec<(String, bool)> {
    let conn = Connection::open(cache.join("index.db")).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT title, archived_at IS NOT NULL FROM files WHERE tool='opencode' ORDER BY title",
        )
        .unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn partial_sql_discovery_preserves_live_sessions_and_clean_deletion_still_archives() {
    for fault in ["row", "sql"] {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("data");
        let db = home.join("opencode/opencode.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let cache = dir.path().join("cache");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(include_str!("fixtures/opencode/v2_1_1.sql"))
            .unwrap();
        conn.execute_batch(include_str!("fixtures/opencode/v1.sql"))
            .unwrap();
        sync(&home, &db, &cache);
        let initial = rows(&cache);
        assert_eq!(initial.len(), 2);
        assert!(initial.iter().all(|(_, archived)| !archived));
        // One source disappears while the remaining schema cannot be scanned.
        conn.execute("DELETE FROM session_v2", []).unwrap();
        let (break_sql, repair_sql) = if fault == "row" {
            (
                "UPDATE session SET time_updated='bad timestamp'",
                "UPDATE session SET time_updated=2000",
            )
        } else {
            (
                "ALTER TABLE session RENAME COLUMN time_updated TO broken",
                "ALTER TABLE session RENAME COLUMN broken TO time_updated",
            )
        };
        conn.execute_batch(break_sql).unwrap();
        sync(&home, &db, &cache);
        assert_eq!(
            rows(&cache),
            initial,
            "{fault}: partial inventory must preserve both cached sessions"
        );
        conn.execute_batch(repair_sql).unwrap();
        sync(&home, &db, &cache);
        assert_eq!(
            rows(&cache),
            [
                ("Legacy title".into(), false),
                ("Migrate authentication to JWT".into(), true),
            ],
            "{fault}: real deletion is archived after a clean scan"
        );
    }
}
