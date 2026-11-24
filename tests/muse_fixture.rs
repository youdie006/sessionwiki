//! Muse Code 1.4.24 (Linux) native session, sanitized: see
//! `tests/fixtures/muse/README.md`. The fixture is the session as Muse wrote
//! it, so these tests fail if a Muse release changes the shapes the reader
//! depends on.

use sessionwiki::adapters::{self, Adapter, Muse};
use sessionwiki::index;
use sessionwiki::model::{EditKind, Role, Session, ToolEvent};
use std::path::PathBuf;

const SESSION: &str = "01a20eeb-92fc-75b2-b53e-252dab96148e";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/muse/muse-1.4.24-linux")
}

fn session_path() -> PathBuf {
    root()
        .join("2026/11/24")
        .join(SESSION)
        .join("session.jsonl")
}

fn parse() -> Session {
    let adapter = Muse::in_sessions_root(root());
    adapters::parse_session(&adapter, &session_path()).expect("fixture parses")
}

#[test]
fn discovers_only_the_session_log() {
    let adapter = Muse::in_sessions_root(root());
    let found = adapter.discover();
    assert!(!found.had_error);
    assert_eq!(found.files, vec![session_path().canonicalize().unwrap()]);
}

#[test]
fn reads_the_conversation_tools_and_metadata() {
    let s = parse();
    assert_eq!(s.tool, "muse");
    assert!(!s.subagent);
    // Muse's own generated name wins over the first prompt.
    assert_eq!(s.title, "mild-columba");
    assert_eq!(s.project, "/work/example");
    assert_eq!(
        s.started.unwrap().to_rfc3339(),
        "2026-11-24T12:00:01.208143+00:00"
    );
    assert_eq!(
        s.ended.unwrap().to_rfc3339(),
        "2026-11-24T12:00:12.642677+00:00"
    );

    let line = |role: Role| -> Vec<&str> {
        s.messages
            .iter()
            .filter(|m| m.role == role)
            .map(|m| m.text.as_str())
            .collect()
    };
    assert_eq!(
        line(Role::User),
        ["Read sample.txt, replace beta with gamma in sample.txt, and reply Done."]
    );
    // Muse commits commentary and the final answer as separate messages.
    assert_eq!(
        line(Role::Assistant),
        ["Replacing beta with gamma — locating the file.", "Done"]
    );
    let tools: Vec<&str> = s
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| m.text.as_str())
        .collect();
    assert_eq!(tools.len(), 2, "one folded line per tool call: {tools:?}");
    assert_eq!(
        tools,
        [
            "→ read_file(sample.txt) ⇒ ok · 3 lines",
            "→ edit_file(sample.txt) ⇒ ok · 7 lines"
        ]
    );

    // The shared pass folds the structured call/result pairs; before folding
    // the adapter must have emitted both halves with Muse's call ids.
    let adapter = Muse::in_sessions_root(root());
    let raw = adapter.parse(&session_path()).unwrap();
    let ids = |want_call: bool| -> Vec<Option<String>> {
        raw.messages
            .iter()
            .filter_map(|m| match &m.tool {
                Some(ToolEvent::Part(sessionwiki::model::ToolPart::Call(c))) if want_call => {
                    Some(c.id.clone())
                }
                Some(ToolEvent::Part(sessionwiki::model::ToolPart::Result(r))) if !want_call => {
                    Some(r.call_id.clone())
                }
                _ => None,
            })
            .collect()
    };
    assert_eq!(ids(true).len(), 2);
    assert_eq!(ids(true), ids(false));
}

#[test]
fn records_the_edit_against_the_workspace_file() {
    let s = parse();
    assert_eq!(s.touched, ["/work/example/sample.txt"]);
    assert_eq!(s.edits.len(), 1);
    let edit = &s.edits[0];
    assert_eq!(edit.path, "/work/example/sample.txt");
    assert_eq!(edit.kind, EditKind::Edit);
    assert_eq!(edit.snippet, "gamma");
    assert!(edit.ts.is_some());
}

#[test]
fn indexes_searches_and_links_files_in_an_isolated_index() {
    let data = tempfile::tempdir().unwrap();
    let adapters: Vec<Box<dyn Adapter>> = vec![Box::new(Muse::in_sessions_root(root()))];
    // `index::open` reads this process-global variable and this binary has no
    // other test that opens an index, so the real index is never touched.
    std::env::set_var("SESSIONWIKI_DATA", data.path());
    let mut conn = index::open().unwrap();
    index::sync_with(&mut conn, &adapters, None).unwrap();

    let hits = index::search(&conn, "gamma", 10, Some("muse"), None).unwrap();
    assert_eq!(hits.len(), 1);
    let row = &hits[0].row;
    assert_eq!(row.tool, "muse");
    assert_eq!(row.title, "mild-columba");
    assert_eq!(row.project, "/work/example");

    assert_eq!(
        index::files_for(&conn, &row.session_id).unwrap(),
        ["/work/example/sample.txt"]
    );
    let edits = index::edits_for(&conn, "sample.txt", 10).unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].session_id, row.session_id);
    assert_eq!(edits[0].kind, "edit");
    assert_eq!(edits[0].snippet, "gamma");
    assert_eq!(edits[0].matched_path, "/work/example/sample.txt");

    // A second sync of an unchanged store changes nothing.
    index::sync_with(&mut conn, &adapters, None).unwrap();
    assert_eq!(
        index::search(&conn, "gamma", 10, Some("muse"), None)
            .unwrap()
            .len(),
        1
    );
}
