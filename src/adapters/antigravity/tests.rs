use super::*;
const LIVE: &str = include_str!("../../../tests/fixtures/antigravity/agy-1.5.7-linux.jsonl");
fn transcript(home: &Path, variant: &str, id: &str, name: &str, content: &str) -> PathBuf {
    let p = home
        .join(variant)
        .join("brain")
        .join(id)
        .join(".system_generated/logs")
        .join(name);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, content).unwrap();
    p
}
fn edit_log(status: &str) -> String {
    [json!({"step_index":0,"type":"USER_INPUT","created_at":"2026-11-13T12:00:00Z","content":"<USER_REQUEST>Update auth</USER_REQUEST>","cwd":"/work/example"}),
    json!({"step_index":1,"type":"PLANNER_RESPONSE","tool_calls":[{"name":"edit_file","args":{"Path":"src/auth.rs","Replacement":"token = secret","api_key":"sk-test_abcdefghijklmnopqrstuvwxyz0123456789"}}]}),
    json!({"step_index":2,"type":"GENERIC","status":status,"content":"Edit result"})].iter().map(|v|v.to_string()+"\n").collect()
}
#[test]
fn native_capture_parses_messages_and_paired_tools() {
    let home = tempfile::tempdir().unwrap();
    let path = transcript(
        home.path(),
        "antigravity-cli",
        "cli-session",
        "transcript.jsonl",
        LIVE,
    );
    let a = Antigravity::cli_in_home(home.path());
    let store = a.store().unwrap();
    assert_eq!(store.keys.len(), 1);
    assert!(!store.had_error);
    let s = crate::adapters::parse_session_key(&a, &store.keys[0].0).unwrap();
    assert_eq!(s.title, "Inspect the example project");
    assert_eq!(
        s.messages.iter().filter(|m| m.role == Role::User).count(),
        1
    );
    assert_eq!(
        s.messages.iter().filter(|m| m.role == Role::Tool).count(),
        3
    );
    assert!(s.edits.is_empty());
    assert_eq!(a.parse(&path).unwrap().id, s.id);
    assert!(a.modified_at(&store.keys[0].0).is_some());
}
#[test]
fn roots_are_independent_and_ids_do_not_collide() {
    let h = tempfile::tempdir().unwrap();
    for v in ["antigravity", "antigravity-cli"] {
        transcript(h.path(), v, "same-id", "transcript.jsonl", LIVE);
    }
    let ide = Antigravity::in_home(h.path());
    let cli = Antigravity::cli_in_home(h.path());
    let a = ide.store().unwrap();
    let b = cli.store().unwrap();
    assert_eq!(a.keys.len(), 1);
    assert_eq!(b.keys.len(), 1);
    assert_ne!(
        ide.parse_key(&a.keys[0].0).unwrap().id,
        cli.parse_key(&b.keys[0].0).unwrap().id
    );
    assert_ne!(ide.reconcile_scope(), cli.reconcile_scope());
    assert!(a.keys[0].0.starts_with(
        ide.reconcile_scope()
            .unwrap()
            .trim_end_matches(std::path::MAIN_SEPARATOR)
    ));
}
#[test]
fn full_and_compact_share_a_key_and_metadata_fingerprint() {
    let h = tempfile::tempdir().unwrap();
    let compact = transcript(h.path(), "antigravity-cli", "s", "transcript.jsonl", LIVE);
    let a = Antigravity::cli_in_home(h.path());
    let old = a.store().unwrap();
    let id = a.parse_key(&old.keys[0].0).unwrap().id;
    let full = transcript(
        h.path(),
        "antigravity-cli",
        "s",
        "transcript_full.jsonl",
        &LIVE.replace(
            "Inspecting the example project.",
            "Full reply without truncation.",
        ),
    );
    let new = a.store().unwrap();
    assert_eq!(old.keys[0].0, new.keys[0].0);
    assert_ne!(old.keys[0].1, new.keys[0].1);
    let s = a.parse_key(&new.keys[0].0).unwrap();
    assert_eq!(s.id, id);
    assert!(s
        .messages
        .iter()
        .any(|m| m.text == "Full reply without truncation."));
    std::fs::remove_file(full).unwrap();
    assert_eq!(a.parse(&compact).unwrap().id, id);
}
#[test]
fn a_stale_full_log_does_not_hide_new_compact_turns() {
    let h = tempfile::tempdir().unwrap();
    let compact = transcript(h.path(), "antigravity-cli", "s", "transcript.jsonl", LIVE);
    let full = transcript(
        h.path(),
        "antigravity-cli",
        "s",
        "transcript_full.jsonl",
        LIVE,
    );
    let a = Antigravity::cli_in_home(h.path());
    let before = a.store().unwrap();
    let newer = format!(
        "{LIVE}{}\n",
        json!({"step_index":7,"type":"USER_INPUT","content":"A new live turn"})
    );
    std::fs::write(&compact, &newer).unwrap();
    let after = a.store().unwrap();
    assert_ne!(before.keys[0].1, after.keys[0].1);
    let s = a.parse_key(&after.keys[0].0).unwrap();
    assert!(s.messages.iter().any(|m| m.text == "A new live turn"));
    let id = s.id;
    // Once the full log catches up, its richer content wins at equal coverage.
    std::fs::write(
        &full,
        newer.replace("A new live turn", "The full untruncated turn"),
    )
    .unwrap();
    let s = a.parse_key(&after.keys[0].0).unwrap();
    assert_eq!(s.id, id);
    assert!(s
        .messages
        .iter()
        .any(|m| m.text == "The full untruncated turn"));
}
#[test]
fn only_completed_edits_are_evidence_and_arguments_are_redacted() {
    let h = tempfile::tempdir().unwrap();
    let a = Antigravity::cli_in_home(h.path());
    for status in ["DONE", "ERROR", "RUNNING"] {
        let p = transcript(
            h.path(),
            "antigravity-cli",
            "s",
            "transcript.jsonl",
            &edit_log(status),
        );
        let s = crate::adapters::parse_session(&a, &p).unwrap();
        assert_eq!(s.edits.len(), usize::from(status == "DONE"));
        assert!(
            !format!("{:?}", s.messages).contains("sk-test_abcdefghijklmnopqrstuvwxyz0123456789")
        );
    }
}
#[test]
fn windowed_transcripts_keep_middle_edit_provenance() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            crate::util::TEST_WINDOW.with(|w| w.set(None));
        }
    }
    let h = tempfile::tempdir().unwrap();
    let mut text = edit_log("DONE");
    text.push_str(&format!(
        "{}\n",
        json!({"step_index":3,"type":"GENERIC","content":"x".repeat(8000)})
    ));
    let p = transcript(h.path(), "antigravity-cli", "s", "transcript.jsonl", &text);
    let a = Antigravity::cli_in_home(h.path());
    let whole = a.parse(&p).unwrap();
    crate::util::TEST_WINDOW.with(|w| w.set(Some((400, 100, 120))));
    let _reset = Reset;
    let window = a.parse(&p).unwrap();
    assert_eq!(whole.edits.len(), window.edits.len());
    assert_eq!(window.edits[0].path, "src/auth.rs");
}
#[test]
fn malformed_lines_and_empty_files_are_tolerated() {
    let h = tempfile::tempdir().unwrap();
    let a = Antigravity::in_home(h.path());
    let p = transcript(
        h.path(),
        "antigravity",
        "s",
        "transcript.jsonl",
        &(LIVE.to_owned() + "\n{bad json"),
    );
    assert!(a.parse(&p).is_ok());
    std::fs::write(&p, "").unwrap();
    assert!(a
        .parse_key(
            p.parent()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .to_str()
                .unwrap()
        )
        .is_err());
}
#[test]
fn native_summary_supplies_workspace_title_and_parent() {
    let h = tempfile::tempdir().unwrap();
    transcript(h.path(), "antigravity-cli", "s", "transcript.jsonl", LIVE);
    let db = h.path().join("antigravity-cli/conversation_summaries.db");
    let c = rusqlite::Connection::open(&db).unwrap();
    c.execute_batch("CREATE TABLE conversation_summaries(conversation_id TEXT,title TEXT,workspace_uris TEXT,parent_conversation_id TEXT); INSERT INTO conversation_summaries VALUES('s','Native title','[\"file:///work/my%20project\"]','parent')").unwrap();
    let a = Antigravity::cli_in_home(h.path());
    let store = a.store().unwrap();
    let s = a.parse_key(&store.keys[0].0).unwrap();
    assert_eq!(s.project, "/work/my project");
    assert_eq!(s.title, "Native title");
    assert!(s.subagent);
}
#[test]
fn uri_decoding_handles_unicode_and_windows() {
    assert_eq!(
        file_uri("file:///C:/work/a%20b"),
        Some("C:/work/a b".into())
    );
    assert_eq!(
        file_uri("file:///work/λ%20test"),
        Some("/work/λ test".into())
    );
    assert_eq!(file_uri("https://example.test/path"), None);
}
#[test]
fn malformed_percent_escapes_do_not_panic() {
    assert_eq!(file_uri("file:///work/%€"), Some("/work/%€".into()));
}
#[cfg(unix)]
#[test]
fn symlinked_transcripts_cannot_escape_the_conversation() {
    let h = tempfile::tempdir().unwrap();
    let p = transcript(h.path(), "antigravity-cli", "s", "transcript.jsonl", LIVE);
    let outside = h.path().join("outside.jsonl");
    std::fs::write(&outside, LIVE).unwrap();
    std::fs::remove_file(&p).unwrap();
    std::os::unix::fs::symlink(outside, &p).unwrap();
    let a = Antigravity::cli_in_home(h.path());
    assert!(a.store().unwrap().had_error);
    assert!(a
        .parse_key(
            p.parent()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .to_str()
                .unwrap()
        )
        .is_err());
}
#[test]
fn nullable_summary_fields_do_not_discard_other_conversations() {
    let h = tempfile::tempdir().unwrap();
    for id in ["empty", "good"] {
        transcript(h.path(), "antigravity-cli", id, "transcript.jsonl", LIVE);
    }
    let c = rusqlite::Connection::open(h.path().join("antigravity-cli/conversation_summaries.db"))
        .unwrap();
    c.execute_batch("CREATE TABLE conversation_summaries(conversation_id TEXT,title TEXT,workspace_uris TEXT,parent_conversation_id TEXT); INSERT INTO conversation_summaries VALUES('empty',NULL,NULL,NULL); INSERT INTO conversation_summaries VALUES('good','Valid native title','[\"file:///work/example\"]',NULL)").unwrap();
    let a = Antigravity::cli_in_home(h.path());
    let before = a.store().unwrap();
    let good = before
        .keys
        .iter()
        .find(|(k, _)| k.ends_with("good"))
        .unwrap();
    assert_eq!(a.parse_key(&good.0).unwrap().title, "Valid native title");
    c.execute(
        "UPDATE conversation_summaries SET title='Other update' WHERE conversation_id='empty'",
        [],
    )
    .unwrap();
    let after = a.store().unwrap();
    assert_eq!(
        good.1,
        after
            .keys
            .iter()
            .find(|(k, _)| k.ends_with("good"))
            .unwrap()
            .1,
        "unrelated metadata must not invalidate this conversation"
    );
}
