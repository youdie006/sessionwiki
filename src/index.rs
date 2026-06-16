use crate::adapters::{self, Adapter};
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

/// The index lives outside the session stores and never touches them.
/// Default: ~/.local/share/sessionwiki/index.db (platform equivalent).
pub fn db_path() -> Result<PathBuf> {
    let dir = std::env::var_os("SESSIONWIKI_DATA")
        .map(PathBuf::from)
        .or_else(|| dirs::data_dir().map(|d| d.join("sessionwiki")))
        .context("cannot determine a data directory")?;
    // One-time migration from earlier names, newest first. This carries over
    // the existing index AND the curated tags/notes/summaries, which are not
    // rebuildable. The project was session-atlas, then sessiondex.
    if !dir.exists() {
        if let Some(parent) = dirs::data_dir() {
            for old_name in ["sessiondex", "session-atlas"] {
                let old = parent.join(old_name);
                if old.exists() {
                    let _ = std::fs::rename(&old, &dir);
                    break;
                }
            }
        }
    }
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
        // The summaries table deliberately survives this: rebuilding the
        // index is cheap, re-running an LLM over every session is not.
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
        );
        CREATE TABLE IF NOT EXISTS summaries(
            session_id TEXT PRIMARY KEY,
            summary    TEXT NOT NULL,
            created    TEXT NOT NULL
        );
        -- Curation layer (the editable 'wiki' part). Like summaries, these
        -- are user-authored and survive index rebuilds: only files/messages/
        -- msgs are dropped on a schema bump, never these.
        CREATE TABLE IF NOT EXISTS tags(
            session_id TEXT NOT NULL,
            tag        TEXT NOT NULL,
            PRIMARY KEY (session_id, tag)
        );
        CREATE INDEX IF NOT EXISTS idx_tags_tag ON tags(tag);
        CREATE TABLE IF NOT EXISTS notes(
            session_id TEXT PRIMARY KEY,
            note       TEXT NOT NULL,
            updated    TEXT NOT NULL
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
            Ok((
                r.get::<_, String>(0)?,
                (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?),
            ))
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

            let Ok(session) = adapter.parse(&path) else {
                continue;
            };
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
            let mut ins_row = tx
                .prepare_cached("INSERT INTO messages(session_id, role, text) VALUES (?1,?2,?3)")?;
            let mut ins_fts = tx.prepare_cached("INSERT INTO msgs(rowid, text) VALUES (?1,?2)")?;
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
    conn.execute(
        "DELETE FROM messages WHERE session_id = ?1",
        params![session_id],
    )?;
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
    /// Cached LLM synopsis, if `summarize` has been run for this session.
    pub summary: Option<String>,
    /// Comma-joined user tags, if any.
    pub tags: Option<String>,
}

/// Correlated subquery for the preview column; messages.id preserves
/// insertion order, which is message order.
const PREVIEW_SQL: &str = "(SELECT substr(m2.text, 1, 280) FROM messages m2
    WHERE m2.session_id = f.session_id AND m2.role = 'assistant'
    ORDER BY m2.id DESC LIMIT 1)";

const SUMMARY_SQL: &str = "(SELECT s.summary FROM summaries s WHERE s.session_id = f.session_id)";

const TAGS_SQL: &str =
    "(SELECT group_concat(t.tag, ',') FROM tags t WHERE t.session_id = f.session_id)";

pub fn recent(
    conn: &Connection,
    limit: usize,
    tool: Option<&str>,
    project: Option<&str>,
    tag: Option<&str>,
    include_subagents: bool,
) -> Result<Vec<SessionRow>> {
    let mut sql = format!(
        "SELECT session_id, tool, path, project, title, started, msg_count, kind, {PREVIEW_SQL}, {SUMMARY_SQL}, {TAGS_SQL}
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
    if let Some(t) = tag {
        sql.push_str(
            " AND EXISTS (SELECT 1 FROM tags g WHERE g.session_id = f.session_id AND g.tag = ?)",
        );
        args.push(t.to_string());
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
            summary: r.get(9)?,
            tags: r.get(10)?,
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
    sql.push_str(&format!(
        " GROUP BY f.session_id ORDER BY best LIMIT {limit}"
    ));

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
                summary: None,
                tags: None,
            },
            role: r.get(8)?,
            snippet: r.get(9)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Resolve a (possibly abbreviated) session id to its file row.
pub fn resolve(conn: &Connection, id_prefix: &str) -> Result<Vec<SessionRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT session_id, tool, path, project, title, started, msg_count, kind, {SUMMARY_SQL}, {TAGS_SQL}
         FROM files f WHERE session_id LIKE ?1 LIMIT 10",
    ))?;
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
            summary: r.get(8)?,
            tags: r.get(9)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Store (or replace) the cached synopsis for a session.
pub fn set_summary(conn: &Connection, session_id: &str, summary: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO summaries(session_id, summary, created)
         VALUES (?1, ?2, datetime('now'))",
        params![session_id, summary],
    )?;
    Ok(())
}

/// Most recent main sessions that have no cached summary yet.
pub fn unsummarized(
    conn: &Connection,
    limit: usize,
    tool: Option<&str>,
) -> Result<Vec<SessionRow>> {
    let mut sql = format!(
        "SELECT session_id, tool, path, project, title, started, msg_count, kind, {PREVIEW_SQL}, {SUMMARY_SQL}, {TAGS_SQL}
         FROM files f
         WHERE kind = 'main' AND NOT EXISTS
               (SELECT 1 FROM summaries s WHERE s.session_id = f.session_id)",
    );
    let mut args: Vec<String> = Vec::new();
    if let Some(t) = tool {
        sql.push_str(" AND tool = ?");
        args.push(t.to_string());
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
            summary: r.get(9)?,
            tags: r.get(10)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// --- curation (the editable wiki layer) ---

pub fn add_tag(conn: &Connection, session_id: &str, tag: &str) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO tags(session_id, tag) VALUES (?1, ?2)",
        params![session_id, tag.trim().to_lowercase()],
    )?;
    Ok(())
}

pub fn remove_tag(conn: &Connection, session_id: &str, tag: &str) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM tags WHERE session_id = ?1 AND tag = ?2",
        params![session_id, tag.trim().to_lowercase()],
    )?)
}

pub fn set_note(conn: &Connection, session_id: &str, note: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO notes(session_id, note, updated)
         VALUES (?1, ?2, datetime('now'))",
        params![session_id, note],
    )?;
    Ok(())
}

pub fn note_for(conn: &Connection, session_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT note FROM notes WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )
        .ok())
}

/// All tags in use, with how many sessions carry each.
pub fn tag_counts(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut stmt =
        conn.prepare("SELECT tag, count(*) FROM tags GROUP BY tag ORDER BY count(*) DESC, tag")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// --- related sessions (backlinks) ---

/// Sessions related to `session_id`. A session is most usefully "related" to
/// the others about the same codebase, so same-project sessions are the spine;
/// sessions sharing a user tag are layered on as explicit links. Both are
/// indexed lookups, so this is instant even over a large store - the earlier
/// full-text-on-title approach was both slow and noisy (generic title words
/// like "session" matched everything).
pub fn related(conn: &Connection, session_id: &str, limit: usize) -> Result<Vec<SessionRow>> {
    let Some(target) = resolve(conn, session_id)?.into_iter().next() else {
        return Ok(vec![]);
    };
    let target_tags: Vec<String> = target
        .tags
        .as_deref()
        .map(|t| t.split(',').map(String::from).collect())
        .unwrap_or_default();

    let mut out: Vec<SessionRow> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    seen.insert(target.session_id.clone());

    // 1. same project (exact), most recent first - the same-context spine.
    if !target.project.is_empty() {
        let sql = format!(
            "SELECT session_id, tool, path, project, title, started, msg_count, kind,
                    {PREVIEW_SQL}, {SUMMARY_SQL}, {TAGS_SQL}
             FROM files f
             WHERE kind = 'main' AND project = ?1 AND session_id != ?2
             ORDER BY started DESC LIMIT ?3"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![target.project, target.session_id, limit as i64 + 1],
            map_row,
        )?;
        for row in rows {
            let row = row?;
            if seen.insert(row.session_id.clone()) {
                out.push(row);
            }
        }
    }

    // 2. sessions that share a tag with the target (explicit wiki links).
    if out.len() < limit && !target_tags.is_empty() {
        let placeholders = target_tags
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT DISTINCT f.session_id, f.tool, f.path, f.project, f.title, f.started,
                    f.msg_count, f.kind, {PREVIEW_SQL}, {SUMMARY_SQL}, {TAGS_SQL}
             FROM files f JOIN tags t ON t.session_id = f.session_id
             WHERE f.kind = 'main' AND t.tag IN ({placeholders})
             ORDER BY f.started DESC LIMIT 50"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&target_tags), map_row)?;
        for row in rows {
            let row = row?;
            if seen.insert(row.session_id.clone()) {
                out.push(row);
                if out.len() >= limit {
                    break;
                }
            }
        }
    }

    out.truncate(limit);
    Ok(out)
}

/// Row mapper for the full session-list column set.
fn map_row(r: &rusqlite::Row) -> rusqlite::Result<SessionRow> {
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
        summary: r.get(9)?,
        tags: r.get(10)?,
    })
}

// --- session engineering: management views ---

pub struct ProjectRow {
    pub project: String,
    pub sessions: i64,
    pub messages: i64,
    pub oldest: Option<String>,
    pub newest: Option<String>,
}

/// One row per project (a wiki "category" page), busiest first.
pub fn projects(conn: &Connection) -> Result<Vec<ProjectRow>> {
    let mut stmt = conn.prepare(
        "SELECT project, count(*), coalesce(sum(msg_count), 0), min(started), max(started)
         FROM files WHERE kind = 'main' AND project != ''
         GROUP BY project ORDER BY count(*) DESC, max(started) DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ProjectRow {
            project: r.get(0)?,
            sessions: r.get(1)?,
            messages: r.get(2)?,
            oldest: r.get(3)?,
            newest: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub struct Stats {
    pub per_tool: Vec<(String, i64, i64)>, // tool, sessions, messages
    pub per_month: Vec<(String, i64)>,     // YYYY-MM, sessions
    pub total_sessions: i64,
    pub total_messages: i64,
    pub projects: i64,
    pub tags: i64,
    pub summarized: i64,
}

pub fn stats(conn: &Connection) -> Result<Stats> {
    let mut per_tool_stmt = conn.prepare(
        "SELECT tool, count(*), coalesce(sum(msg_count),0) FROM files WHERE kind='main'
         GROUP BY tool ORDER BY count(*) DESC",
    )?;
    let per_tool = per_tool_stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut per_month_stmt = conn.prepare(
        "SELECT substr(started,1,7) AS ym, count(*) FROM files
         WHERE kind='main' AND started IS NOT NULL
         GROUP BY ym ORDER BY ym DESC LIMIT 12",
    )?;
    let per_month = per_month_stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let one = |sql: &str| -> Result<i64> { Ok(conn.query_row(sql, [], |r| r.get(0))?) };
    Ok(Stats {
        per_tool,
        per_month,
        total_sessions: one("SELECT count(*) FROM files WHERE kind='main'")?,
        total_messages: one("SELECT coalesce(sum(msg_count),0) FROM files WHERE kind='main'")?,
        projects: one(
            "SELECT count(DISTINCT project) FROM files WHERE kind='main' AND project!=''",
        )?,
        tags: one("SELECT count(DISTINCT tag) FROM tags")?,
        summarized: one("SELECT count(*) FROM summaries")?,
    })
}
