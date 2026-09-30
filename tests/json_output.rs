//! The agent-facing --json contract: stable snake_case field names, no ANSI /
//! control codes in the JSON, the absolute path hidden, and CLI JSON == web JSON
//! for a session row (both go through SessionRow's Serialize derive).

use rusqlite::{params, Connection};
use sessionwiki::{commands, index};
use std::path::Path;
use std::process::Output;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn fresh() -> Connection {
    let dir = std::env::temp_dir().join("sessionwiki-test-json");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("SESSIONWIKI_DATA", &dir);
    let conn = index::open().unwrap();
    conn.execute_batch(
        "DELETE FROM files; DELETE FROM messages; DELETE FROM tags;
         DELETE FROM notes; DELETE FROM touched; DELETE FROM archive;",
    )
    .unwrap();
    conn
}

fn seed(conn: &Connection, id: &str) {
    conn.execute(
        "INSERT OR REPLACE INTO files
         (path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind)
         VALUES (?1,0,0,?2,'codex','/proj/api','fix auth bug',
                 '2026-06-10T10:00:00+00:00','2026-06-10T10:00:00+00:00',2,'main')",
        params![format!("/secret/abs/path/{id}.jsonl"), id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages(session_id, role, text) VALUES (?1,'assistant','done fixing auth')",
        params![id],
    )
    .unwrap();
}

#[test]
fn clean_snippet_strips_markers_and_controls() {
    let raw = "the \u{2}auth\u{3} bug\nand a \u{0}nul";
    let (plain, marked) = commands::clean_snippet(raw);
    assert_eq!(plain, "the auth bug and a nul"); // markers gone, newline->space, NUL dropped
    assert_eq!(marked, "the [[auth]] bug and a nul"); // stable ASCII match delimiters
    assert!(!plain.contains('\u{2}') && !plain.contains('\u{3}'));
    assert!(!plain.contains('\u{0}'));
}

#[test]
fn strip_snippet_controls_drops_esc_keeps_fts_markers() {
    // The terminal search-snippet path must drop ESC/DEL/C1 (an untrusted body
    // could inject ANSI/OSC) while keeping the \x02/\x03 FTS markers the caller
    // swaps to ANSI.
    let raw = "ok\u{1b}[31mred\u{7f}\u{9b}\u{2}hit\u{3}";
    let out = commands::strip_snippet_controls(raw);
    assert!(!out.contains('\u{1b}'), "ESC stripped");
    assert!(
        !out.contains('\u{7f}') && !out.contains('\u{9b}'),
        "DEL/C1 stripped"
    );
    assert_eq!(out, "ok[31mred\u{2}hit\u{3}", "markers kept, controls gone");
}

#[test]
fn session_row_json_has_pinned_fields() {
    let _g = LOCK.lock().unwrap();
    let conn = fresh();
    seed(&conn, "s1");
    index::add_tag(&conn, "s1", "auth").unwrap();

    let rows = index::recent(&conn, 10, None, None, None, false).unwrap();
    let v = serde_json::to_value(&rows[0]).unwrap();
    let obj = v.as_object().unwrap();

    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "account",
            "archived",
            "id",
            "kind",
            // Added for `recent`: the list is ordered by it (last activity).
            "last_active",
            "msgs",
            "native_id",
            "preview",
            "project",
            "started",
            "summary",
            "tags",
            "title",
            "tool"
        ]
    );
    assert!(
        !obj.contains_key("path"),
        "absolute path must not leak into JSON"
    );
    // The native id (codex rollout / claude transcript UUID) is exposed for the
    // tower-row join; here the seeded filename carries no UUID, so it is null -
    // a missing badge, never a guess. The full path is still never serialized.
    assert!(
        obj.contains_key("native_id") && obj["native_id"].is_null(),
        "native_id present, null when the filename has no UUID"
    );
    assert!(!obj.contains_key("session_id"), "renamed to id");
    assert!(!obj.contains_key("msg_count"), "renamed to msgs");

    assert_eq!(obj["id"], "s1");
    assert_eq!(obj["tool"], "codex");
    assert_eq!(obj["msgs"], 2);
    assert_eq!(obj["started"], "2026-06-10T10:00:00+00:00");
    assert_eq!(obj["archived"], false);
    assert_eq!(obj["tags"], serde_json::json!(["auth"])); // array, not "auth"
    assert!(obj["preview"].as_str().unwrap().contains("done fixing"));
}

#[test]
fn native_id_surfaces_in_list_json_for_a_rollout_path() {
    let _g = LOCK.lock().unwrap();
    let conn = fresh();
    // A real Codex rollout path: the native uuid trails the timestamp.
    let path = "/home/u/.codex/sessions/2025/05/13/rollout-2025-05-13T18-19-30-0a000000-0000-4000-8000-000000000001.jsonl";
    conn.execute(
        "INSERT INTO files
         (path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind)
         VALUES (?1,0,0,'sx','codex','/proj','t','2026-06-10T10:00:00+00:00','2026-06-10T10:00:00+00:00',1,'main')",
        params![path],
    )
    .unwrap();
    let rows = index::recent(&conn, 10, None, None, None, false).unwrap();
    let v = serde_json::to_value(&rows[0]).unwrap();
    assert_eq!(v["native_id"], "0a000000-0000-4000-8000-000000000001");
    assert!(v.get("path").is_none(), "path itself never serialized");
}

#[test]
fn null_tags_serialize_as_null_not_empty_array() {
    let _g = LOCK.lock().unwrap();
    let conn = fresh();
    seed(&conn, "s2"); // no tags
    let rows = index::recent(&conn, 10, None, None, None, false).unwrap();
    let v = serde_json::to_value(&rows[0]).unwrap();
    assert!(v["tags"].is_null());
}

#[test]
fn search_hit_serializes_with_snippet_and_role() {
    let _g = LOCK.lock().unwrap();
    let conn = fresh();
    seed(&conn, "s3");
    conn.execute(
        "INSERT INTO messages(session_id, role, text) VALUES ('s3','user','negative offset overflow')",
        [],
    )
    .unwrap();
    let id: i64 = conn
        .query_row(
            "SELECT id FROM messages WHERE text LIKE 'negative%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO msgs(rowid, text) VALUES (?1,'negative offset overflow')",
        params![id],
    )
    .unwrap();

    let hits = index::search(&conn, "negative", 10, None, None).unwrap();
    assert_eq!(hits.len(), 1);

    let mut v = serde_json::to_value(&hits[0].row).unwrap();
    let (plain, marked) = commands::clean_snippet(&hits[0].snippet);
    v["snippet"] = serde_json::json!(plain);
    v["snippet_marked"] = serde_json::json!(marked);
    v["role"] = serde_json::json!(hits[0].role);

    assert_eq!(v["id"], "s3");
    assert_eq!(v["role"], "user");
    assert!(v["snippet"].as_str().unwrap().contains("negative"));
    assert!(!v["snippet"].as_str().unwrap().contains('\u{2}'));
    assert!(v["snippet_marked"].as_str().unwrap().contains("[["));
    // search rows do not load preview/summary/tags
    assert!(v["preview"].is_null());
    assert!(v["summary"].is_null());
    assert!(v["tags"].is_null());
}

#[test]
fn brief_json_object_shape() {
    let _g = LOCK.lock().unwrap();
    let conn = fresh();
    seed(&conn, "s4");
    let row = index::resolve(&conn, "s4")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let session = index::session_from_index(&conn, &row).unwrap();

    let v = serde_json::json!({
        "id": session.id,
        "tool": session.tool,
        "project": session.project,
        "title": session.title,
        "started": session.started.map(|t| t.to_rfc3339()),
        "source": session.path.display().to_string(),
        "markdown": "# Previous session: ...",
    });
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(
        keys,
        ["id", "markdown", "project", "source", "started", "title", "tool"]
    );
    assert_eq!(v["id"], "s4");
    assert_eq!(v["tool"], "codex");
    assert_eq!(v["started"], "2026-06-10T10:00:00+00:00");
}

fn isolated_sessionwiki(
    args: &[&str],
    data: &Path,
    home: &Path,
    aider_roots: &Path,
    prodex_registry: &Path,
    timeline: &Path,
) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_sessionwiki"))
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join("share"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("SESSIONWIKI_DATA", data)
        .env("SESSIONWIKI_AIDER_ROOTS", aider_roots)
        .env("SESSIONWIKI_PRODEX_REGISTRY", prodex_registry)
        .env("SESSIONWIKI_SWAPDEX_TIMELINE", timeline)
        .output()
        .unwrap()
}

#[test]
fn brief_and_summarizer_redact_reparsed_source_while_show_stays_raw() {
    let _g = LOCK.lock().unwrap();
    let t = tempfile::tempdir().unwrap();
    let data = t.path().join("data");
    let home = t.path().join("home");
    let aider = t.path().join("aider");
    std::fs::create_dir_all(&aider).unwrap();
    let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789ABCD";
    let source_dir = t.path().join(secret);
    std::fs::create_dir_all(&source_dir).unwrap();
    let source = source_dir.join("conversation.jsonl");
    std::fs::write(
        &source,
        format!(
            "{{\"role\":\"user\",\"content\":\"rotate {secret}\",\"timestamp\":\"2026-06-10T10:00:00\"}}\n\
             {{\"role\":\"assistant\",\"content\":\"body {secret}\",\"timestamp\":\"2026-06-10T10:01:00\"}}\n"
        ),
    )
    .unwrap();

    std::env::set_var("SESSIONWIKI_DATA", &data);
    let conn = index::open().unwrap();
    conn.execute(
        "INSERT INTO files(path,mtime,size,session_id,tool,project,title,started,ended,msg_count,kind)
         VALUES(?1,0,0,'raw-brief','gptme','already-redacted','already-redacted',
                '2026-06-10T10:00:00+00:00','2026-06-10T10:01:00+00:00',2,'main')",
        params![source.to_string_lossy()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages(session_id,role,text) VALUES('raw-brief','user','[redacted:openai]')",
        [],
    )
    .unwrap();
    drop(conn);
    std::env::remove_var("SESSIONWIKI_DATA");

    let registry = t.path().join("absent-bridges.json");
    let timeline = t.path().join("absent-timeline.jsonl");
    let run =
        |args: &[&str]| isolated_sessionwiki(args, &data, &home, &aider, &registry, &timeline);

    let plain = run(&["brief", "raw-brief", "--no-sync"]);
    assert!(
        plain.status.success(),
        "{}",
        String::from_utf8_lossy(&plain.stderr)
    );
    let plain = String::from_utf8(plain.stdout).unwrap();
    assert!(!plain.contains(secret), "plain brief leaked: {plain}");
    assert!(
        plain.matches("[redacted:openai]").count() >= 4,
        "title, project, source, and body are marked: {plain}"
    );

    let json = run(&["brief", "raw-brief", "--no-sync", "--json"]);
    assert!(
        json.status.success(),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );
    let json_text = String::from_utf8(json.stdout).unwrap();
    assert!(
        !json_text.contains(secret),
        "JSON brief leaked: {json_text}"
    );
    let value: serde_json::Value = serde_json::from_str(&json_text).unwrap();
    for field in ["project", "title", "source", "markdown"] {
        assert!(
            value[field]
                .as_str()
                .is_some_and(|s| s.contains("[redacted:openai]")),
            "{field} was not redacted: {value}"
        );
    }

    let check_stdin =
        format!("if grep -Fq '{secret}'; then exit 9; else printf 'safe summary'; fi");
    let summarized = run(&["summarize", "raw-brief", "--cmd", &check_stdin, "--force"]);
    let summarized_out = String::from_utf8(summarized.stdout).unwrap();
    let summarized_err = String::from_utf8(summarized.stderr).unwrap();
    assert!(summarized.status.success(), "{summarized_err}");
    assert!(
        summarized_out.contains("safe summary") && !summarized_err.contains("summarizer failed"),
        "summarizer received an unredacted export:\nstdout={summarized_out}\nstderr={summarized_err}"
    );

    let shown = run(&["show", "raw-brief", "--no-sync", "--json"]);
    assert!(
        shown.status.success(),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    assert!(
        String::from_utf8(shown.stdout).unwrap().contains(secret),
        "local raw show must retain the source transcript"
    );
}
