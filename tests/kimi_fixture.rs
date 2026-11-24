//! A sanitized capture of a real Kimi Code 2.3.2 session on Linux (see
//! `tests/fixtures/kimi/README.md`) parses, indexes and answers file queries.

use chrono::{TimeZone, Utc};
use sessionwiki::adapters::{Adapter, KimiCode};
use sessionwiki::index;
use sessionwiki::model::{EditKind, Role, ToolEvent, ToolPart};
use std::fs;
use std::path::Path;

const FIXTURE_HOME: &str = "tests/fixtures/kimi/kimi-2.3.2-linux";
const CAPTURED_HOME: &str = "/home/example/.kimi-code";
const SESSION: &str = "sessions/wd_work_0123456789ab/session_00000000-0000-4000-8000-000000000001";
const SAMPLE: &str = "/work/example/sample.txt";

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// The capture records absolute session directories in `session_index.jsonl`.
/// Copy it somewhere writable and point those entries at the copy.
fn installed_home(root: &Path) -> std::path::PathBuf {
    let home = root.join(".kimi-code");
    copy_tree(Path::new(FIXTURE_HOME), &home);
    let home = home.canonicalize().unwrap();
    let index = home.join("session_index.jsonl");
    let text = fs::read_to_string(&index).unwrap();
    fs::write(&index, text.replace(CAPTURED_HOME, &home.to_string_lossy())).unwrap();
    home
}

#[test]
fn kimi_2_3_2_capture_parses_indexes_and_answers_file_queries() {
    let root = tempfile::tempdir().unwrap();
    let home = installed_home(root.path());
    let session_dir = home.join(SESSION);
    let wire = session_dir.join("agents/main/wire.jsonl");
    // Copying stamped every file with the current time. Give the session's
    // files the dates of the capture, with the wire the latest.
    let set_mtime = |path: &Path, time: chrono::DateTime<Utc>| {
        fs::File::open(path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(time.into()))
            .unwrap();
    };
    let captured = Utc.timestamp_opt(1_790_000_050, 0).unwrap();
    let modified = Utc.timestamp_opt(1_790_000_100, 0).unwrap();
    set_mtime(&session_dir.join("state.json"), captured);
    set_mtime(&wire, modified);

    let adapter = KimiCode::in_home(&home);
    let discovered = adapter.discover();
    assert!(!discovered.had_error);
    assert_eq!(discovered.files, vec![session_dir.clone()]);
    assert_eq!(
        adapter.modified_at(&session_dir.to_string_lossy()),
        Some(modified)
    );

    let session = adapter.parse(&session_dir).unwrap();
    assert_eq!(session.tool, "kimi-code");
    assert_eq!(session.project, "/work/example");
    // The capture has no title in state.json, so it comes from the prompt.
    assert_eq!(
        session.title,
        "Read sample.txt, replace beta with gamma in sample.txt, and reply Done."
    );
    assert_eq!(
        session.started.unwrap().timestamp_millis(),
        1_790_000_000_000
    );
    assert_eq!(session.ended.unwrap().timestamp_millis(), 1_790_000_014_035);
    assert!(!session.subagent);

    // One prompt and one reply; the wire's repeated message copies, system
    // reminders and reasoning are not conversation.
    let prose: Vec<_> = session
        .messages
        .iter()
        .filter(|m| m.tool.is_none())
        .map(|m| (m.role, m.text.as_str()))
        .collect();
    assert_eq!(
        prose,
        vec![
            (
                Role::User,
                "Read sample.txt, replace beta with gamma in sample.txt, and reply Done."
            ),
            (Role::Assistant, "Done"),
        ]
    );

    let calls: Vec<_> = session
        .messages
        .iter()
        .filter_map(|m| match &m.tool {
            Some(ToolEvent::Part(ToolPart::Call(call))) => Some(call),
            _ => None,
        })
        .collect();
    let results: Vec<_> = session
        .messages
        .iter()
        .filter_map(|m| match &m.tool {
            Some(ToolEvent::Part(ToolPart::Result(result))) => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "Read");
    assert_eq!(calls[0].args["path"], "sample.txt");
    assert_eq!(calls[1].name, "Edit");
    assert_eq!(calls[1].args["old_string"], "beta");
    assert_eq!(calls[1].args["new_string"], "gamma");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].call_id, calls[0].id);
    assert!(results[0].text.contains("2\tbeta"));
    assert!(!results[1].is_error);
    assert_eq!(results[1].call_id, calls[1].id);
    assert_eq!(results[1].text, "Replaced 1 occurrence in sample.txt");

    // The wire records `sample.txt`; the resolved path is in the call's display.
    assert_eq!(session.touched, vec![SAMPLE]);
    assert_eq!(session.edits.len(), 1);
    assert_eq!(session.edits[0].path, SAMPLE);
    assert_eq!(session.edits[0].kind, EditKind::Edit);
    assert_eq!(session.edits[0].snippet, "gamma");
    assert_eq!(
        session.edits[0].ts.unwrap().timestamp_millis(),
        1_790_000_010_983
    );

    // Index it under an isolated data directory.
    let data = root.path().join("data");
    std::env::set_var("SESSIONWIKI_DATA", &data);
    let mut conn = index::open().unwrap();
    let adapters: Vec<Box<dyn Adapter>> = vec![Box::new(KimiCode::in_home(&home))];
    index::sync_with(&mut conn, &adapters, None).unwrap();

    let hits = index::search(&conn, "gamma", 10, Some("kimi-code"), None).unwrap();
    assert!(!hits.is_empty());
    let id = hits[0].row.session_id.clone();
    assert!(hits.iter().all(|hit| hit.row.session_id == id));
    assert_eq!(hits[0].row.project, "/work/example");
    assert_eq!(index::files_for(&conn, &id).unwrap(), vec![SAMPLE]);
    let edits = index::edits_for(&conn, "sample.txt", 10).unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].session_id, id);
    assert_eq!(edits[0].kind, "edit");
    assert_eq!(edits[0].snippet, "gamma");
    assert_eq!(edits[0].matched_path, SAMPLE);
}
