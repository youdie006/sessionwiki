//! The hook subcommand must ALWAYS exit 0 with empty stdout on a broken index,
//! so it can never pollute the agent's context or block session start.

use std::io::Write;

#[test]
fn hook_exits_0_and_empty_on_broken_index() {
    let dir = std::env::temp_dir().join("sw-hook-broken");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("index.db"), b"not a sqlite database").unwrap();

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_sessionwiki"))
        .args(["hook", "session-start"])
        .env("SESSIONWIKI_DATA", &dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"cwd":"/tmp","source":"startup","session_id":"x"}"#)
        .unwrap();
    let out = child.wait_with_output().unwrap();

    assert!(
        out.status.success(),
        "hook must exit 0 even on a broken index"
    );
    assert!(out.stdout.is_empty(), "broken index -> empty brief");
}

/// Claude Code hands the hook its folder as it records it (`C:\...` on
/// Windows), and the brief lists that project's past sessions with each file
/// relative to it; a file outside the project shows only its name.
#[test]
fn hook_briefs_the_project_it_was_started_in() {
    let root = tempfile::tempdir().unwrap();
    let canon = std::fs::canonicalize(root.path()).unwrap();
    let canon = canon.to_string_lossy();
    let base = std::path::PathBuf::from(canon.strip_prefix(r"\\?\").unwrap_or(&canon));
    let proj = base.join("app");
    std::fs::create_dir_all(&proj).unwrap();
    let data = base.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let proj_str = proj.to_string_lossy().into_owned();

    std::env::set_var("SESSIONWIKI_DATA", &data);
    let conn = sessionwiki::index::open().unwrap();
    conn.execute(
        "INSERT INTO files(path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind)
         VALUES ('s.jsonl', 0, 0, 'past', 'claude-code', ?1, 'fixed the login form',
                 '2026-10-01T10:00:00+00:00', '2026-10-01T10:00:00+00:00', 1, 'main')",
        [&proj_str],
    )
    .unwrap();
    for file in [
        proj.join("src").join("login.rs"),
        base.join("app-old").join("notes.md"),
    ] {
        conn.execute(
            "INSERT INTO touched(session_id, path) VALUES ('past', ?1)",
            [file.to_string_lossy()],
        )
        .unwrap();
    }
    drop(conn);

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_sessionwiki"))
        .args(["hook", "session-start"])
        .env("SESSIONWIKI_DATA", &data)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let input = serde_json::json!({"cwd": proj_str, "source": "startup", "session_id": "now"});
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let brief = String::from_utf8_lossy(&out.stdout);

    assert!(brief.contains("fixed the login form"), "brief: {brief:?}");
    let login = std::path::Path::new("src").join("login.rs");
    assert!(
        brief.contains(&format!("touched {}, notes.md", login.display())),
        "brief: {brief:?}"
    );
    assert!(
        !brief.contains(&*base.to_string_lossy()),
        "brief: {brief:?}"
    );
}
