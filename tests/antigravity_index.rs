use sessionwiki::{
    adapters::{Adapter, Antigravity},
    index,
};
use std::path::{Path, PathBuf};
fn write(home: &Path, variant: &str, id: &str) -> PathBuf {
    let p = home
        .join(variant)
        .join("brain")
        .join(id)
        .join(".system_generated/logs/transcript.jsonl");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(
        &p,
        include_str!("fixtures/antigravity/agy-1.5.7-linux.jsonl"),
    )
    .unwrap();
    p
}
#[test]
fn real_index_scopes_stores_preserves_notes_and_keeps_live_data_on_read_errors() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("gemini");
    let cli = write(&home, "antigravity-cli", "same-id");
    write(&home, "antigravity", "same-id");
    std::env::set_var("SESSIONWIKI_DATA", dir.path().join("cache"));
    let mut conn = index::open().unwrap();
    let adapters: Vec<Box<dyn Adapter>> = vec![
        Box::new(Antigravity::cli_in_home(&home)),
        Box::new(Antigravity::in_home(&home)),
    ];
    index::sync_with(&mut conn, &adapters, None).unwrap();
    assert_eq!(
        index::search(&conn, "Inspect the example", 10, Some("antigravity"), None)
            .unwrap()
            .len(),
        2
    );
    let sid = Antigravity::cli_in_home(&home).parse(&cli).unwrap().id;
    index::set_note(&conn, &sid, "keep note").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_sessionwiki"))
        .args(["show", &sid])
        .env("SESSIONWIKI_DATA", dir.path().join("cache"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Inspect the example project"));

    // A broken symlink makes a previously readable source unavailable, not a
    // clean deletion. The adapter must retain the inventory error guard.
    #[cfg(unix)]
    {
        let outside = dir.path().join("outside");
        std::fs::write(&outside, "private").unwrap();
        std::fs::remove_file(&cli).unwrap();
        std::os::unix::fs::symlink(&outside, &cli).unwrap();
        std::fs::remove_file(&outside).unwrap();
        index::sync_with(&mut conn, &adapters[..1], None).unwrap();
        let archived: bool = conn
            .query_row(
                "SELECT archived_at IS NOT NULL FROM files WHERE session_id=?1",
                [&sid],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!archived);
        std::fs::remove_file(&cli).unwrap();
        std::fs::write(
            &cli,
            include_str!("fixtures/antigravity/agy-1.5.7-linux.jsonl"),
        )
        .unwrap();
    }
    std::fs::remove_file(&cli).unwrap();
    index::sync_with(&mut conn, &adapters[..1], None).unwrap();
    let hits = index::search(&conn, "Inspect the example", 10, Some("antigravity"), None).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits.iter().filter(|h| h.row.archived).count(), 1);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_sessionwiki"))
        .args(["show", &sid])
        .env("SESSIONWIKI_DATA", dir.path().join("cache"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Inspect the example project"));
    assert_eq!(
        index::note_for(&conn, &sid).unwrap().as_deref(),
        Some("keep note")
    );
}
