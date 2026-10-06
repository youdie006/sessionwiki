//! Codex sub-agent rollouts indexed before the adapter read their marker were
//! stored as main sessions. The first open after upgrading reclassifies them
//! from each file's session_meta line, without re-parsing every session.

use rusqlite::params;
use sessionwiki::index;

#[test]
fn opening_an_index_reclassifies_codex_subagent_rows() {
    let files = tempfile::tempdir().unwrap();
    let rollout = |name: &str, meta: &str| {
        let path = files.path().join(name);
        std::fs::write(
            &path,
            format!("{{\"type\":\"session_meta\",\"payload\":{meta}}}\n"),
        )
        .unwrap();
        path
    };
    let sub = rollout(
        "sub.jsonl",
        r#"{"cwd":"/proj","thread_source":"subagent","agent_path":"/root/worker"}"#,
    );
    let main = rollout("main.jsonl", r#"{"cwd":"/proj","source":"cli"}"#);

    let data = tempfile::tempdir().unwrap();
    std::env::set_var("SESSIONWIKI_DATA", data.path());
    let conn = index::open().unwrap();
    for (id, path) in [("sub-1", &sub), ("main-1", &main)] {
        conn.execute(
            "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started, msg_count, kind)
             VALUES (?1, 0, 0, ?2, 'codex', '/proj', '(no user prompt)', '2026-10-07T00:00:00+00:00', 1, 'main')",
            params![path.to_string_lossy(), id],
        )
        .unwrap();
    }
    // As an index written by an older version: never reclassified.
    conn.execute("DELETE FROM meta WHERE key = 'codex_subagent_v1'", [])
        .unwrap();
    drop(conn);

    let conn = index::open().unwrap();
    let kind = |id: &str| -> String {
        conn.query_row("SELECT kind FROM files WHERE session_id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
    };
    assert_eq!(kind("sub-1"), "sub");
    assert_eq!(kind("main-1"), "main");
    let projects = index::projects(&conn).unwrap();
    assert_eq!(
        projects
            .iter()
            .find(|p| p.project == "/proj")
            .map(|p| p.sessions),
        Some(1),
        "a sub-agent thread counted as a project session"
    );
}
