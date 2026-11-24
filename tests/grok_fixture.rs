//! A real Grok Build 1.0.92 session (see `tests/fixtures/grok/README.md`)
//! flows through discovery, parsing and the index.

use sessionwiki::adapters::{self, GrokBuild};
use sessionwiki::index;
use sessionwiki::model::{Role, ToolEvent, ToolPart};
use std::fs;
use std::path::{Path, PathBuf};

const SESSION: &str = "00000000-0000-4000-8000-000000000001";

/// Lay the fixture out as Grok Build writes it under `<home>/sessions`.
fn install(home: &Path) -> PathBuf {
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/grok/grok-1.0.92-linux");
    let dir = home.join("sessions/%2Fwork%2Fexample").join(SESSION);
    fs::create_dir_all(&dir).unwrap();
    for name in ["summary.json", "chat_history.jsonl"] {
        fs::copy(source.join(name), dir.join(name)).unwrap();
    }
    dir.canonicalize().unwrap()
}

#[test]
fn grok_1_0_92_session_parses_and_indexes() {
    let data = tempfile::tempdir().unwrap();
    std::env::set_var("SESSIONWIKI_DATA", data.path());
    let home = tempfile::tempdir().unwrap();
    let dir = install(home.path());
    let adapter = GrokBuild::in_home(home.path());

    let store = adapters::Adapter::store(&adapter)
        .unwrap()
        .keys
        .into_iter()
        .map(|(k, _)| k)
        .collect::<Vec<_>>();
    assert_eq!(store, vec![dir.to_string_lossy().into_owned()]);

    let session = adapters::parse_session_key(&adapter, &store[0]).unwrap();
    assert_eq!(session.tool, "grok-build");
    assert_eq!(session.project, "/work/example");
    assert_eq!(session.title, "Replace beta with gamma in sample.txt");
    assert_eq!(
        session.started.unwrap().to_rfc3339(),
        "2026-11-24T12:00:00+00:00"
    );
    assert_eq!(
        session.ended.unwrap().to_rfc3339(),
        "2026-11-24T12:00:05+00:00"
    );
    assert!(!session.subagent);

    // The environment block and the skill reminders are not the user's words;
    // the prompt loses its <user_query> wrapper.
    let users: Vec<_> = session
        .messages
        .iter()
        .filter(|m| m.role == Role::User)
        .map(|m| m.text.as_str())
        .collect();
    assert_eq!(
        users,
        ["Read sample.txt, replace beta with gamma in sample.txt, and reply Done."]
    );
    let assistants: Vec<_> = session
        .messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .map(|m| m.text.as_str())
        .collect();
    assert_eq!(
        assistants,
        [
            "I'll read `sample.txt`, replace `beta` with `gamma`, and then reply.",
            "Done."
        ]
    );
    assert!(session
        .messages
        .iter()
        .all(|m| !m.text.contains("system prompt") && !m.text.contains("<user_info>")));

    // Two tool calls, each folded with its result by the shared summary pass.
    let tools: Vec<_> = session
        .messages
        .iter()
        .filter_map(|m| match &m.tool {
            Some(ToolEvent::Summary(s)) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].name, "read_file");
    assert_eq!(tools[1].name, "search_replace");
    assert_eq!(tools[1].args["file_path"], "/work/example/sample.txt");
    assert_eq!(tools[1].is_error, Some(false));

    assert_eq!(session.touched, vec!["/work/example/sample.txt"]);
    assert_eq!(session.edits.len(), 1);
    assert_eq!(session.edits[0].path, "/work/example/sample.txt");
    assert_eq!(session.edits[0].kind.as_str(), "edit");
    assert_eq!(session.edits[0].snippet, "gamma");

    // The unfolded parse keeps native call and result ids paired.
    let raw = adapters::Adapter::parse_key(&adapter, &store[0]).unwrap();
    let calls: Vec<_> = raw
        .messages
        .iter()
        .filter_map(|m| match &m.tool {
            Some(ToolEvent::Part(ToolPart::Call(c))) => c.id.clone(),
            _ => None,
        })
        .collect();
    let results: Vec<_> = raw
        .messages
        .iter()
        .filter_map(|m| match &m.tool {
            Some(ToolEvent::Part(ToolPart::Result(r))) => r.call_id.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls, results);

    // modified_at is the newest backing-file time, read without parsing.
    let modified = adapters::Adapter::modified_at(&adapter, &store[0]).unwrap();
    let on_disk = fs::metadata(dir.join("chat_history.jsonl"))
        .unwrap()
        .modified()
        .unwrap();
    assert!(modified >= chrono::DateTime::<chrono::Utc>::from(on_disk));

    // Indexing makes the conversation searchable and records the edit.
    let mut conn = index::open().unwrap();
    let readers: Vec<Box<dyn adapters::Adapter>> = vec![Box::new(GrokBuild::in_home(home.path()))];
    index::sync_with(&mut conn, &readers, None).unwrap();
    let hits = index::search(&conn, "gamma", 10, Some("grok-build"), None).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].row.project, "/work/example");
    assert_eq!(
        index::files_for(&conn, &hits[0].row.session_id).unwrap(),
        vec!["/work/example/sample.txt"]
    );
    let edits = index::edits_for(&conn, "sample.txt", 10).unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].kind, "edit");
    assert_eq!(edits[0].snippet, "gamma");
}
