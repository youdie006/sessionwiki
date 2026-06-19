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
