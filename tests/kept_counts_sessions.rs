//! "Kept after the tool deleted them" counts sessions, like the number it sits
//! beside. Both `stats` and `doctor` printed it over every archived row,
//! subagent transcripts included, under a session total that counts only main
//! sessions - on a real store "3959 sessions, 2050 kept" where 611 were.

use rusqlite::params;
use sessionwiki::{doctor, index};

#[test]
fn kept_counts_main_sessions_like_the_total_beside_it() {
    let dir = std::env::temp_dir().join("sessionwiki-test-kept-counts");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("SESSIONWIKI_DATA", &dir);
    let conn = index::open().unwrap();
    for (path, kind, archived) in [
        ("/s/main-live.jsonl", "main", false),
        ("/s/main-gone.jsonl", "main", true),
        ("/s/sub-gone-1.jsonl", "sub", true),
        ("/s/sub-gone-2.jsonl", "sub", true),
    ] {
        conn.execute(
            "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind, archived_at)
             VALUES (?1, 0, 0, ?1, 'claude-code', '/p', 't', '2026-10-01T00:00:00+00:00', NULL, 1, ?2, ?3)",
            params![path, kind, archived.then_some("2026-10-02T00:00:00+00:00")],
        )
        .unwrap();
    }

    let stats = index::stats(&conn).unwrap();
    assert_eq!(stats.total_sessions, 2);
    assert_eq!(
        stats.archived, 1,
        "stats counted subagent rows as kept sessions"
    );

    let line = doctor::index_checks(&conn, index::SCHEMA_VERSION)
        .into_iter()
        .find(|c| c.name == "indexed sessions")
        .map(|c| c.detail)
        .expect("an indexed-sessions check");
    assert_eq!(line, "2 (1 kept after the tool deleted them)");
}
