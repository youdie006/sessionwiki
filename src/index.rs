use crate::adapters::{self, Adapter};
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

/// The index lives outside the session stores and never touches them.
/// Default: ~/.local/share/session-atlas/index.db (platform equivalent).
pub fn db_path() -> Result<PathBuf> {
    let dir = std::env::var_os("SESSION_ATLAS_DATA")
        .map(PathBuf::from)
        .or_else(|| dirs::data_dir().map(|d| d.join("session-atlas")))
        .context("cannot determine a data directory")?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("index.db"))
}

/// Bump when the schema changes. The index is a disposable cache, so a
/// mismatch simply drops and rebuilds it instead of migrating.
const SCHEMA_VERSION: i64 = 2;

pub fn open() -> Result<Connection> {
    let conn = Connection::open(db_path()?)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version != SCHEMA_VERSION {
        conn.execute_batch(
            "DROP TABLE IF EXISTS msgs;
             DROP TABLE IF EXISTS messages;
             DROP TABLE IF EXISTS files;",
        )?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS files(
            path       TEXT PRIMARY KEY,
            mtime      INTEGER NOT NULL,
            size       INTEGER NOT NULL,
            session_id TEXT NOT NULL,
            tool       TEXT NOT NULL,
            project    TEXT NOT NULL DEFAULT '',
            title      TEXT NOT NULL DEFAULT '',
            started    TEXT,
            ended      TEXT,
            msg_count  INTEGER NOT NULL DEFAULT 0,
            kind       TEXT NOT NULL DEFAULT 'main'
        );
        CREATE INDEX IF NOT EXISTS idx_files_session ON files(session_id);
        -- Plain rows + external-content FTS. Deleting a session is an
        -- indexed lookup here; with session_id stored UNINDEXED inside the
        -- FTS table it was a full scan per file, which made re-index runs
        -- quadratic in practice.
        CREATE TABLE IF NOT EXISTS messages(
            id         INTEGER PRIMARY KEY,
            session_id TEXT NOT NULL,
            role       TEXT NOT NULL,
            text       TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);
        CREATE VIRTUAL TABLE IF NOT EXISTS msgs USING fts5(
            text,
            content='messages',
            content_rowid='id',
            tokenize='trigram'
        );",
    )?;
    Ok(conn)
}

/// Bring the index up to date with what is on disk. Only files whose
/// (mtime, size) changed since the last run are re-parsed.
pub fn sync(conn: &mut Connection, only_tool: Option<&str>) -> Result<()> {
    let adapters: Vec<Box<dyn Adapter>> = match only_tool {
        Some(t) => adapters::by_name(t).into_iter().collect(),
        None => adapters::all(),
    };

    let mut known: HashMap<String, (i64, i64)> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT path, mtime, size FROM files")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))
        })?;
        for row in rows {
            let (path, ms) = row?;
            known.insert(path, ms);
        }
    }

    for adapter in &adapters {
        let files = adapter.discover();
        let mut seen: Vec<String> = Vec::with_capacity(files.len());
        let mut pending: Vec<(PathBuf, i64, i64)> = Vec::new();

        for f in files {
            let Ok(meta) = f.metadata() else { continue };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let size = meta.len() as i64;
            let key = f.to_string_lossy().into_owned();
            if known.get(&key) != Some(&(mtime, size)) {
                pending.push((f, mtime, size));
            }
            seen.push(key);
        }

        // Drop index rows for files that disappeared from this store.
        let tool = adapter.name();
        prune_deleted(conn, tool, &seen)?;

        if pending.is_empty() {
            continue;
        }
        let total = pending.len();
        let mut done = 0usize;
        let tx = conn.transaction()?;
        for (path, mtime, size) in pending {
            done += 1;
            eprint!("\r[{tool}] indexing {done}/{total}");
            std::io::stderr().flush().ok();

            let Ok(session) = adapter.parse(&path) else { continue };
            let key = path.to_string_lossy();
            delete_session_msgs(&tx, &session.id)?;
            tx.execute(
                "INSERT OR REPLACE INTO files
                 (path, mtime, size, session_id, tool, project, title, started, ended, msg_count, kind)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    key,
                    mtime,
                    size,
                    session.id,
                    session.tool,
                    session.project,
                    session.title,
                    session.started.map(|t| t.to_rfc3339()),
                    session.ended.map(|t| t.to_rfc3339()),
                    session.messages.len() as i64,
                    if session.subagent { "sub" } else { "main" },
                ],
            )?;
            let mut ins_row = tx.prepare_cached(
                "INSERT INTO messages(session_id, role, text) VALUES (?1,?2,?3)",
            )?;
            let mut ins_fts =
                tx.prepare_cached("INSERT INTO msgs(rowid, text) VALUES (?1,?2)")?;
            for m in &session.messages {
                ins_row.execute(params![session.id, m.role.label(), m.text])?;
                ins_fts.execute(params![tx.last_insert_rowid(), m.text])?;
            }
        }
        tx.commit()?;
        eprintln!("\r[{tool}] indexed {done}/{total}    ");
    }
    Ok(())
}

/// External-content FTS5 requires handing back the old rows on delete.
fn delete_session_msgs(conn: &Connection, session_id: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO msgs(msgs, rowid, text)
         SELECT 'delete', id, text FROM messages WHERE session_id = ?1",
        params![session_id],
    )?;
    conn.execute("DELETE FROM messages WHERE session_id = ?1", params![session_id])?;
    Ok(())
}

fn prune_deleted(conn: &Connection, tool: &str, seen: &[String]) -> Result<()> {
    let mut stmt = conn.prepare("SELECT path, session_id FROM files WHERE tool = ?1")?;
    let rows = stmt.query_map(params![tool], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let seen: std::collections::HashSet<&str> = seen.iter().map(String::as_str).collect();
    let mut gone: Vec<(String, String)> = Vec::new();
    for row in rows {
        let (path, sid) = row?;
        if !seen.contains(path.as_str()) {
            gone.push((path, sid));
        }
    }
    for (path, sid) in gone {
        conn.execute("DELETE FROM files WHERE path = ?1", params![path])?;
        delete_session_msgs(conn, &sid)?;
    }
    Ok(())
}

pub struct SessionRow {
    pub session_id: String,
    pub tool: String,
    pub path: String,
    pub project: String,
    pub title: String,
    pub started: Option<String>,
    pub msg_count: i64,
    pub kind: String,
    /// Tail of the conversation (last assistant message), so a list can show
    /// how the session ended without opening it.
    pub preview: Option<String>,
}

/// Correlated subquery for the preview column; messages.id preserves
/// insertion order, which is message order.
const PREVIEW_SQL: &str = "(SELECT substr(m2.text, 1, 280) FROM messages m2
    WHERE m2.session_id = f.session_id AND m2.role = 'assistant'
    ORDER BY m2.id DESC LIMIT 1)";

pub fn recent(
    conn: &Connection,
    limit: usize,
    tool: Option<&str>,
    project: Option<&str>,
    include_subagents: bool,
) -> Result<Vec<SessionRow>> {
    let mut sql = format!(
        "SELECT session_id, tool, path, project, title, started, msg_count, kind, {PREVIEW_SQL}
         FROM files f WHERE 1=1",
    );
    let mut args: Vec<String> = Vec::new();
    if !include_subagents {
        sql.push_str(" AND kind = 'main'");
    }
    if let Some(t) = tool {
        sql.push_str(" AND tool = ?");
        args.push(t.to_string());
    }
    if let Some(p) = project {
        sql.push_str(" AND project LIKE ?");
        args.push(format!("%{p}%"));
    }
    sql.push_str(&format!(" ORDER BY started DESC LIMIT {limit}"));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| {
        Ok(SessionRow {
            session_id: r.get(0)?,
            tool: r.get(1)?,
            path: r.get(2)?,
            project: r.get(3)?,
            title: r.get(4)?,
            started: r.get(5)?,
            msg_count: r.get(6)?,
            kind: r.get(7)?,
            preview: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub struct Hit {
    pub row: SessionRow,
    pub role: String,
    pub snippet: String,
}

/// Full-text search, best match per session. The trigram tokenizer gives
/// substring matching, which also makes CJK text searchable.
pub fn search(
    conn: &Connection,
    query: &str,
    limit: usize,
    tool: Option<&str>,
    project: Option<&str>,
) -> Result<Vec<Hit>> {
    // A plain quoted string disables FTS5 operator parsing: users type
    // text, not query syntax.
    let fts_query = format!("\"{}\"", query.replace('"', "\"\""));

    // snippet()/rank only work in a plain FTS5 query context, not under
    // joins or GROUP BY, so match in a subquery and attach metadata outside.
    let mut sql = String::from(
        "SELECT f.session_id, f.tool, f.path, f.project, f.title, f.started, f.msg_count, f.kind,
                m.role, x.snip, min(x.rank) AS best
         FROM (SELECT rowid AS mid,
                      snippet(msgs, 0, char(2), char(3), char(8230), 18) AS snip,
                      rank
               FROM msgs WHERE msgs MATCH ? ORDER BY rank LIMIT 1000) x
         JOIN messages m ON m.id = x.mid
         JOIN files f ON f.session_id = m.session_id
         WHERE 1=1",
    );
    let mut args: Vec<String> = vec![fts_query];
    if let Some(t) = tool {
        sql.push_str(" AND f.tool = ?");
        args.push(t.to_string());
    }
    if let Some(p) = project {
        sql.push_str(" AND f.project LIKE ?");
        args.push(format!("%{p}%"));
    }
    sql.push_str(&format!(" GROUP BY f.session_id ORDER BY best LIMIT {limit}"));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| {
        Ok(Hit {
            row: SessionRow {
                session_id: r.get(0)?,
                tool: r.get(1)?,
                path: r.get(2)?,
                project: r.get(3)?,
                title: r.get(4)?,
                started: r.get(5)?,
                msg_count: r.get(6)?,
                kind: r.get(7)?,
                preview: None,
            },
            role: r.get(8)?,
            snippet: r.get(9)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Resolve a (possibly abbreviated) session id to its file row.
pub fn resolve(conn: &Connection, id_prefix: &str) -> Result<Vec<SessionRow>> {
    let mut stmt = conn.prepare(
        "SELECT session_id, tool, path, project, title, started, msg_count, kind
         FROM files WHERE session_id LIKE ?1 LIMIT 10",
    )?;
    let rows = stmt.query_map(params![format!("{id_prefix}%")], |r| {
        Ok(SessionRow {
            session_id: r.get(0)?,
            tool: r.get(1)?,
            path: r.get(2)?,
            project: r.get(3)?,
            title: r.get(4)?,
            started: r.get(5)?,
            msg_count: r.get(6)?,
            kind: r.get(7)?,
            preview: None,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}
