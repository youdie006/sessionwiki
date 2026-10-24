//! A `sessionwiki mcp` started before an upgrade keeps parsing with its old
//! reader, so a row it rewrites after the upgrade's re-parse mark is wrong
//! again; the mark follows each row's reader, not a once-only flag.

use rusqlite::{params, Connection};
use sessionwiki::adapters::{Adapter, Discovered};
use sessionwiki::index;
use sessionwiki::model::{Message, Role, Session};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// SESSIONWIKI_DATA is process-global; each test holds this for its whole run.
static INDEX_ENV: Mutex<()> = Mutex::new(());

fn mtime(conn: &Connection, path: &str) -> i64 {
    conn.query_row("SELECT mtime FROM files WHERE path = ?1", [path], |r| {
        r.get(0)
    })
    .unwrap()
}

#[test]
fn a_row_an_older_running_binary_rewrites_is_marked_again() {
    let _env = INDEX_ENV.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join("sessionwiki-test-reader-fix-old-writer");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("SESSIONWIKI_DATA", &dir);

    let chat = "/g/chats/session-b.jsonl";
    let conn = index::open().unwrap();
    // A `sessionwiki mcp` started before the upgrade re-parses the marked row
    // with its old reader; it knows nothing of the reader column.
    conn.execute(
        "INSERT OR REPLACE INTO files(path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind)
         VALUES (?1, 9, 9, ?1, 'gemini', '/p', 't', NULL, NULL, 1, 'main')",
        [chat],
    )
    .unwrap();
    drop(conn);
    let conn = index::open().unwrap();
    assert_eq!(mtime(&conn, chat), -1, "the old reader's result is re-read");

    // This version's own parse is final.
    conn.execute(
        "UPDATE files SET mtime = 9, reader = ?2 WHERE path = ?1",
        params![chat, index::READER],
    )
    .unwrap();
    drop(conn);
    let conn = index::open().unwrap();
    assert_eq!(mtime(&conn, chat), 9);
}

struct OneChat(PathBuf);

impl Adapter for OneChat {
    fn name(&self) -> &'static str {
        "gemini"
    }
    fn root(&self) -> Option<PathBuf> {
        self.0.parent().map(Path::to_path_buf)
    }
    fn discover(&self) -> Discovered {
        vec![self.0.clone()].into()
    }
    fn parse(&self, path: &Path) -> anyhow::Result<Session> {
        Ok(Session {
            id: "gemini-chat".into(),
            tool: "gemini",
            path: path.into(),
            project: "/p".into(),
            started: None,
            ended: None,
            title: "chat".into(),
            subagent: false,
            messages: vec![Message {
                role: Role::User,
                text: "hello".into(),
                ts: None,
                tool: None,
            }],
            touched: vec![],
            edits: vec![],
        })
    }
}

#[test]
fn a_row_this_version_parsed_is_not_marked_again() {
    let _env = INDEX_ENV.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join("sessionwiki-test-reader-fix-own-parse");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("chats")).unwrap();
    let chat = dir.join("chats").join("session-c.jsonl");
    std::fs::write(&chat, "{}").unwrap();

    std::env::set_var("SESSIONWIKI_DATA", dir.join("data"));
    let mut conn = index::open().unwrap();
    let adapters: Vec<Box<dyn Adapter>> = vec![Box::new(OneChat(chat.clone()))];
    index::sync_with(&mut conn, &adapters, None).unwrap();
    drop(conn);
    let conn = index::open().unwrap();
    assert_ne!(mtime(&conn, &chat.to_string_lossy()), -1);
}
