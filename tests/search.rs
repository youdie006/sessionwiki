//! Search semantics and tokenizer configuration against a disposable index.

use rusqlite::{params, Connection};
use sessionwiki::adapters::{Adapter, Discovered};
use sessionwiki::index;
use sessionwiki::model::{Message, Role, Session};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

static LOCK: Mutex<()> = Mutex::new(());

fn fresh_index() -> (Connection, PathBuf) {
    let dir = std::env::temp_dir().join(format!("sessionwiki-test-search-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("SESSIONWIKI_DATA", &dir);
    let conn = index::open().unwrap();
    (conn, dir)
}

fn seed_message(conn: &Connection, id: &str, text: &str) {
    seed_role_message(conn, id, Role::User, text);
}

fn seed_role_message(conn: &Connection, id: &str, role: Role, text: &str) {
    conn.execute(
        "INSERT OR REPLACE INTO files
         (path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind)
         VALUES (?1, 0, 0, ?2, 'codex', '/search', ?2, '2026-06-10T10:00:00+00:00',
                 '2026-06-10T10:00:00+00:00', 1, 'main')",
        params![format!("/fake/{id}.jsonl"), id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages(session_id, role, text) VALUES (?1, ?2, ?3)",
        params![id, role.label(), text],
    )
    .unwrap();
    let message_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO msgs(rowid, text) VALUES (?1, ?2)",
        params![message_id, text],
    )
    .unwrap();
}

#[test]
fn role_filter_excludes_tool_only_matches_and_leaves_default_search_unchanged() {
    let _guard = LOCK.lock().unwrap();
    let (conn, _) = fresh_index();
    seed_role_message(
        &conn,
        "tool-only",
        Role::Tool,
        "unique needle from command output",
    );

    let hits = index::search(&conn, "unique needle", 10, None, None).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].role, "tool");

    let conversation = [Role::User, Role::Assistant];
    assert!(
        index::search_with_roles(&conn, "unique needle", 10, None, None, Some(&conversation),)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn role_filter_returns_the_best_allowed_message_for_each_session() {
    let _guard = LOCK.lock().unwrap();
    let (conn, _) = fresh_index();
    seed_role_message(&conn, "mixed-roles", Role::Tool, "beacon");
    seed_role_message(&conn, "mixed-roles", Role::Assistant, "assistant beacon");

    let unfiltered = index::search(&conn, "beacon", 10, None, None).unwrap();
    assert_eq!(unfiltered[0].role, "tool");

    let conversation = [Role::User, Role::Assistant];
    let hits =
        index::search_with_roles(&conn, "beacon", 10, None, None, Some(&conversation)).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].row.session_id, "mixed-roles");
    assert_eq!(hits[0].role, "assistant");
    assert!(
        hits[0].snippet.contains("assistant"),
        "{:?}",
        hits[0].snippet
    );
}

#[test]
fn role_filter_runs_before_the_fts_candidate_cap() {
    let _guard = LOCK.lock().unwrap();
    let (mut conn, _) = fresh_index();
    let tx = conn.transaction().unwrap();
    let mut insert_file = tx
        .prepare_cached(
            "INSERT INTO files
             (path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind)
             VALUES (?1, 0, 0, ?2, 'codex', '/search', ?2, '2026-06-10T10:00:00+00:00',
                     '2026-06-10T10:00:00+00:00', 1, 'main')",
        )
        .unwrap();
    let mut insert_message = tx
        .prepare_cached(
            "INSERT INTO messages(session_id, role, text) VALUES (?1, 'tool', 'rankneedle')",
        )
        .unwrap();
    let mut insert_fts = tx
        .prepare_cached("INSERT INTO msgs(rowid, text) VALUES (?1, 'rankneedle')")
        .unwrap();
    for n in 0..4_001 {
        let id = format!("tool-overflow-{n:04}");
        insert_file
            .execute(params![format!("/fake/{id}.jsonl"), id])
            .unwrap();
        insert_message.execute(params![id]).unwrap();
        insert_fts.execute(params![tx.last_insert_rowid()]).unwrap();
    }
    drop(insert_fts);
    drop(insert_message);
    drop(insert_file);
    tx.commit().unwrap();

    let long_context = format!("{}rankneedle", "background ".repeat(500));
    seed_role_message(&conn, "assistant-after-cap", Role::Assistant, &long_context);

    let unfiltered = index::search(&conn, "rankneedle", 10, None, None).unwrap();
    assert!(unfiltered
        .iter()
        .all(|hit| hit.row.session_id != "assistant-after-cap"));

    let assistant = [Role::Assistant];
    let filtered =
        index::search_with_roles(&conn, "rankneedle", 10, None, None, Some(&assistant)).unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].row.session_id, "assistant-after-cap");
}

#[test]
fn role_filter_applies_to_the_short_term_like_path() {
    let _guard = LOCK.lock().unwrap();
    let (conn, _) = fresh_index();
    seed_role_message(
        &conn,
        "short-term",
        Role::Assistant,
        "xy in the assistant reply",
    );
    seed_role_message(&conn, "short-term", Role::Tool, "xy in newer tool output");

    let assistant = [Role::Assistant];
    let hits = index::search_with_roles(&conn, "xy", 10, None, None, Some(&assistant)).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].role, "assistant");
    assert!(hits[0].snippet.contains("assistant reply"));
}

#[test]
fn cli_rejects_roles_the_index_does_not_store() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_sessionwiki"))
        .args(["search", "needle", "--role", "system", "--no-sync"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("role must be user, assistant, or tool"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_accepts_comma_separated_search_roles() {
    let data = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_sessionwiki"))
        .args(["search", "needle", "--role", "user,assistant", "--no-sync"])
        .env("SESSIONWIKI_DATA", data.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn new_index_defaults_to_trigram_and_creates_external_content_fts() {
    let _guard = LOCK.lock().unwrap();
    let (conn, _) = fresh_index();

    assert_eq!(index::tokenizer_spec(&conn).unwrap(), "trigram");
    let ddl: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'msgs'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(ddl.contains("content='messages'"), "{ddl}");
    assert!(ddl.contains("content_rowid='id'"), "{ddl}");
    assert!(ddl.contains("tokenize='trigram'"), "{ddl}");
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, index::SCHEMA_VERSION);
}

#[test]
fn opening_current_index_succeeds_while_another_connection_holds_write_lock() {
    let _guard = LOCK.lock().unwrap();
    let (conn, _) = fresh_index();
    seed_message(&conn, "writer-lock", "searchable result");

    let blocker = Connection::open(index::db_path().unwrap()).unwrap();
    blocker
        .busy_timeout(std::time::Duration::from_millis(5_000))
        .unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();

    let open_and_search = (|| -> anyhow::Result<Vec<index::Hit>> {
        let reopened = index::open()?;
        index::search(&reopened, "searchable result", 10, None, None)
    })();
    blocker.execute_batch("ROLLBACK").unwrap();

    let hits = open_and_search.expect("current index open and search should not need a write lock");
    assert!(hits.iter().any(|hit| hit.row.session_id == "writer-lock"));
}

#[test]
fn unquoted_terms_match_when_separated_in_one_message() {
    let _guard = LOCK.lock().unwrap();
    let (conn, _) = fresh_index();
    seed_message(
        &conn,
        "separated",
        "alpha appears early; the matching word comes much later: beta.",
    );

    let hits = index::search(&conn, "alpha beta", 10, None, None).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].row.session_id, "separated");
}

#[test]
fn trigram_phrases_preserve_whitespace_and_require_adjacency() {
    let _guard = LOCK.lock().unwrap();
    let (conn, _) = fresh_index();
    seed_message(&conn, "wrapped", "before alpha\n  beta after");
    seed_message(&conn, "adjacent", "before alpha beta after");
    seed_message(&conn, "apart", "before alpha elsewhere beta after");

    let hits = index::search(&conn, "\"alpha beta\"", 10, None, None).unwrap();
    let ids: Vec<_> = hits.iter().map(|hit| hit.row.session_id.as_str()).collect();
    assert_eq!(ids, ["adjacent"]);

    let hits = index::search(&conn, "\"alpha beta", 10, None, None).unwrap();
    let ids: Vec<_> = hits.iter().map(|hit| hit.row.session_id.as_str()).collect();
    assert_eq!(
        ids,
        ["adjacent"],
        "an unmatched quote opens a phrase to end"
    );

    let hits = index::search(&conn, "\"alpha\n  beta\"", 10, None, None).unwrap();
    let ids: Vec<_> = hits.iter().map(|hit| hit.row.session_id.as_str()).collect();
    assert_eq!(ids, ["wrapped"]);
}

#[test]
fn terms_in_different_messages_of_one_session_do_not_match() {
    let _guard = LOCK.lock().unwrap();
    let (conn, _) = fresh_index();
    seed_message(&conn, "split", "alpha is in this message");
    seed_message(&conn, "split", "beta is in another message");

    assert!(index::search(&conn, "alpha beta", 10, None, None)
        .unwrap()
        .is_empty());
}

#[test]
fn mixed_short_long_and_all_short_terms_are_anded() {
    let _guard = LOCK.lock().unwrap();
    let (conn, _) = fresh_index();
    seed_message(&conn, "mixed", "the DB reconnect can retry");
    let hits = index::search(&conn, "DB retry", 10, None, None).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].row.session_id, "mixed");

    seed_message(&conn, "all-short", "use db with id");
    seed_message(&conn, "split-short", "db in one message");
    seed_message(&conn, "split-short", "id in another message");
    let hits = index::search(&conn, "db id", 10, None, None).unwrap();
    let ids: Vec<_> = hits.iter().map(|hit| hit.row.session_id.as_str()).collect();
    assert_eq!(ids, ["all-short"]);
}

#[test]
fn quotes_and_fts_operator_words_are_searched_as_text() {
    let _guard = LOCK.lock().unwrap();
    let (conn, _) = fresh_index();
    seed_message(
        &conn,
        "operators",
        "literal AND OR NEAR * marker; say \"hello\" here",
    );

    let hits = index::search(&conn, "AND \"OR\" NEAR *", 10, None, None).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].row.session_id, "operators");

    let hits = index::search(&conn, "\"say \"\"hello\"\"\"", 10, None, None).unwrap();
    assert_eq!(hits.len(), 1, "escaped quotes remain searchable text");
    assert_eq!(hits[0].row.session_id, "operators");
}

#[test]
fn snippets_highlight_all_short_terms_on_the_like_path() {
    let _guard = LOCK.lock().unwrap();
    let (conn, _) = fresh_index();
    seed_message(
        &conn,
        "snippet",
        "first words db among records and id near the end",
    );

    let hits = index::search(&conn, "db id", 10, None, None).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].snippet.contains("\u{2}db\u{3}"));
    assert!(hits[0].snippet.contains("\u{2}id\u{3}"));
}

#[test]
fn fts_snippets_highlight_terms_and_exact_phrase_whitespace() {
    let _guard = LOCK.lock().unwrap();
    let (conn, _) = fresh_index();
    seed_message(&conn, "snippet-fts", "first alpha\n  beta");

    let hits = index::search(&conn, "first \"alpha\n  beta\"", 10, None, None).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].snippet.contains("\u{2}first\u{3}"));
    assert!(hits[0].snippet.contains("\u{2}alpha\n  beta\u{3}"));
}

struct SearchAdapter {
    source: PathBuf,
    parses: Arc<AtomicUsize>,
}

impl Adapter for SearchAdapter {
    fn name(&self) -> &'static str {
        "search-fixture"
    }

    fn root(&self) -> Option<PathBuf> {
        self.source.parent().map(Path::to_path_buf)
    }

    fn discover(&self) -> Discovered {
        vec![self.source.clone()].into()
    }

    fn parse(&self, path: &Path) -> anyhow::Result<Session> {
        self.parses.fetch_add(1, Ordering::SeqCst);
        Ok(Session {
            id: "reindexed-session".into(),
            tool: self.name(),
            path: path.into(),
            project: "/search".into(),
            started: None,
            ended: None,
            title: "reindex fixture".into(),
            subagent: false,
            messages: vec![Message {
                role: Role::User,
                text: std::fs::read_to_string(path)?,
                ts: None,
                tool: None,
            }],
            touched: vec![],
            edits: vec![],
        })
    }
}

fn adapter(source: PathBuf, parses: Arc<AtomicUsize>) -> Vec<Box<dyn Adapter>> {
    vec![Box::new(SearchAdapter { source, parses })]
}

#[test]
fn unicode61_persists_rebuilds_fts_without_parsing_and_matches_wrapped_phrase() {
    let _guard = LOCK.lock().unwrap();
    let (mut conn, dir) = fresh_index();
    let source = dir.join("session.txt");
    std::fs::write(&source, "before alpha\n  beta after").unwrap();
    let parses = Arc::new(AtomicUsize::new(0));
    let adapters = adapter(source.clone(), parses.clone());

    index::sync_with(&mut conn, &adapters, None).unwrap();
    let parse_count = parses.load(Ordering::SeqCst);
    assert_eq!(parse_count, 1);
    assert!(index::set_tokenizer_spec(&conn, "unicode61").unwrap());
    assert!(!index::set_tokenizer_spec(&conn, "unicode61").unwrap());
    assert_eq!(parses.load(Ordering::SeqCst), parse_count);

    let raw: String = conn
        .query_row(
            "SELECT text FROM messages WHERE session_id = 'reindexed-session'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(raw, "before alpha\n  beta after");
    drop(conn);

    let conn = index::open().unwrap();
    assert_eq!(index::tokenizer_spec(&conn).unwrap(), "unicode61");
    let hits = index::search(&conn, "\"alpha beta\"", 10, None, None).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].row.session_id, "reindexed-session");
}

#[test]
fn unicode61_finds_short_words_through_fts_not_substring_like() {
    let _guard = LOCK.lock().unwrap();
    let (conn, _) = fresh_index();
    seed_message(&conn, "word", "the id is here");
    seed_message(&conn, "substring", "identity is here");
    index::set_tokenizer_spec(&conn, "unicode61").unwrap();

    let hits = index::search(&conn, "id", 10, None, None).unwrap();
    let ids: Vec<_> = hits.iter().map(|hit| hit.row.session_id.as_str()).collect();
    assert_eq!(ids, ["word"]);
}

#[test]
fn unicode61_tokenless_punctuation_term_returns_no_hits_without_sql_error() {
    let _guard = LOCK.lock().unwrap();
    let (conn, _) = fresh_index();
    seed_message(&conn, "punctuation", "ordinary searchable words");
    index::set_tokenizer_spec(&conn, "unicode61").unwrap();

    assert!(index::search(&conn, "***", 10, None, None)
        .unwrap()
        .is_empty());
}

#[test]
fn invalid_tokenizer_leaves_existing_index_and_setting_unchanged() {
    let _guard = LOCK.lock().unwrap();
    let (conn, _) = fresh_index();
    seed_message(&conn, "preserved", "searchable words remain");
    let ddl: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'msgs'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert!(index::set_tokenizer_spec(&conn, "unicode61 unknown_option").is_err());
    assert_eq!(index::tokenizer_spec(&conn).unwrap(), "trigram");
    let after: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'msgs'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after, ddl);
    assert_eq!(
        index::search(&conn, "searchable words", 10, None, None)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn tokenizer_spec_with_quotes_cannot_inject_sql() {
    let _guard = LOCK.lock().unwrap();
    let (conn, _) = fresh_index();
    seed_message(&conn, "preserved", "searchable words remain");

    let malicious = "trigram'); DROP TABLE messages; --";
    assert!(index::set_tokenizer_spec(&conn, malicious).is_err());
    assert_eq!(index::tokenizer_spec(&conn).unwrap(), "trigram");
    let messages: i64 = conn
        .query_row("SELECT count(*) FROM messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(messages, 1);

    let quoted = "unicode61 tokenchars '._-'";
    assert!(index::set_tokenizer_spec(&conn, quoted).unwrap());
    assert_eq!(index::tokenizer_spec(&conn).unwrap(), quoted);
    let messages: i64 = conn
        .query_row("SELECT count(*) FROM messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(messages, 1);
}

#[test]
fn cache_rebuild_uses_the_stored_tokenizer() {
    let _guard = LOCK.lock().unwrap();
    let (conn, _) = fresh_index();
    index::set_tokenizer_spec(&conn, "unicode61").unwrap();
    conn.pragma_update(None, "user_version", 0i64).unwrap();
    drop(conn);

    let conn = index::open().unwrap();
    assert_eq!(index::tokenizer_spec(&conn).unwrap(), "unicode61");
    let ddl: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'msgs'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(ddl.contains("tokenize='unicode61'"), "{ddl}");
}

#[test]
fn mcp_short_query_guard_depends_on_tokenizer() {
    let _guard = LOCK.lock().unwrap();
    let (conn, _) = fresh_index();
    assert!(index::mcp_search_guard(&conn, "xy").unwrap().is_some());
    assert!(index::mcp_search_guard(&conn, "xyz").unwrap().is_none());
    assert!(index::mcp_search_guard(&conn, "   ").unwrap().is_some());

    index::set_tokenizer_spec(&conn, "unicode61").unwrap();
    assert!(index::mcp_search_guard(&conn, "xy").unwrap().is_none());
    assert!(index::mcp_search_guard(&conn, "   ").unwrap().is_some());
}

#[test]
fn reindexing_a_session_removes_its_old_external_content_rows() {
    let _guard = LOCK.lock().unwrap();
    let (mut conn, dir) = fresh_index();
    let source = dir.join("session.txt");
    std::fs::write(&source, "old material phrase").unwrap();
    let adapters = adapter(source.clone(), Arc::new(AtomicUsize::new(0)));

    index::sync_with(&mut conn, &adapters, None).unwrap();
    assert_eq!(
        index::search(&conn, "old phrase", 10, None, None)
            .unwrap()
            .len(),
        1
    );
    std::fs::write(&source, "new material now").unwrap();
    index::sync_with(&mut conn, &adapters, None).unwrap();

    assert!(index::search(&conn, "old phrase", 10, None, None)
        .unwrap()
        .is_empty());
    assert_eq!(
        index::search(&conn, "new material", 10, None, None)
            .unwrap()
            .len(),
        1
    );
}
