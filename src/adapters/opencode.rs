use super::{ok_or_flag, Adapter, Discovered, Store};
use crate::model::{Message, Role, Session};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use walkdir::WalkDir;

mod v1;
mod v2;

const V2_TOKEN_TAG: i64 = 1 << 62;

/// OpenCode (sst/opencode) keeps its data under `$XDG_DATA_HOME/opencode`
/// (default `~/.local/share/opencode`, the same on macOS - it uses
/// xdg-basedir, not the platform data dir). The supported storage layouts are:
///
///   - **v1 SQLite (v1.2.0+):** `opencode.db` (and
///     `opencode-<channel>.db`) with `session` / `message` / `part` tables.
///     This is the source of truth on any recent install, so when a db is
///     present we index it as a shared [`Store`] and ignore the JSON below.
///   - **v2 SQLite:** `session_v2` / `session_message` in the same databases.
///     Mixed stores retain both generations and preserve the same session IDs.
///   - **Legacy JSON:** `storage/session|message|part/**.json`, one file each.
///     Still read on pre-1.2.0 installs that never migrated.
#[derive(Default)]
pub struct OpenCode {
    // Replaced after every inventory, so time queries for that batch need no
    // further database opens. Keeping it on the reader bounds its lifetime.
    activity: Mutex<HashMap<String, Option<DateTime<Utc>>>>,
}

impl Adapter for OpenCode {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn root(&self) -> Option<PathBuf> {
        // The opencode data dir - exists for both the SQLite and JSON layouts,
        // so `scan` presence and deletion reconciliation work either way.
        data_dir()
    }

    fn discover(&self) -> Discovered {
        // Legacy JSON only. When a db exists, `store()` is Some and the indexer
        // never calls this (the JSON files are stale post-migration leftovers).
        let Some(dir) = data_dir() else {
            return Vec::new().into();
        };
        let sessions = dir.join("storage").join("session");
        if !sessions.exists() {
            return Vec::new().into(); // no legacy layout - normal
        }
        let mut had_error = false;
        let files = WalkDir::new(sessions)
            .min_depth(2)
            .max_depth(2)
            .into_iter()
            .filter_map(|e| ok_or_flag(e, &mut had_error))
            .filter(|e| e.file_type().is_file())
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .map(|e| e.into_path())
            .collect();
        Discovered { files, had_error }
    }

    fn store(&self) -> Option<Store> {
        self.store_in(db_paths())
    }

    fn parse_key(&self, key: &str) -> Result<Session> {
        let (db, sid) = key.split_once(KEY_SEP).context("malformed opencode key")?;
        let conn = open_ro(Path::new(db)).with_context(|| format!("open {db}"))?;
        // Schema selection and decoding must see the same source snapshot.
        let tx = conn.unchecked_transaction()?;
        let in_v2 = uses_v2(&tx, sid).with_context(|| format!("check OpenCode schema in {db}"))?;
        // Only absence permits v1 fallback. An existing v2 record's SQL/JSON
        // errors must remain visible, even when a legacy copy also exists.
        let session = if in_v2 {
            v2::parse(self.name(), &tx, sid, key)?
        } else {
            v1::parse_sql(self.name(), &tx, sid, key)?
        };
        tx.commit()?;
        Ok(session)
    }

    fn modified_at(&self, key: &str) -> Option<DateTime<Utc>> {
        if let Some(time) = self
            .activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
        {
            return *time;
        }
        // A direct key query outside the current inventory reads only metadata.
        // The normal sync/scan path uses the snapshot cached above.
        let (db, sid) = key.split_once(KEY_SEP)?;
        let conn = open_ro(Path::new(db)).ok()?;
        let tx = conn.unchecked_transaction().ok()?;
        let sql = if uses_v2(&tx, sid).ok()? {
            "SELECT time_updated, time_created FROM session_v2 WHERE id=?1"
        } else {
            "SELECT time_updated, time_created FROM session WHERE id=?1"
        };
        let (updated, created): (Option<i64>, Option<i64>) = tx
            .query_row(sql, [sid], |row| Ok((row.get(0)?, row.get(1)?)))
            .ok()?;
        updated
            .or(created)
            .filter(|time| *time > 0)
            .and_then(DateTime::from_timestamp_millis)
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        v1::parse_json(self.name(), path)
    }
}

// A partial SQL scan must never authorize deletion reconciliation. Keep
// row errors as well as preparation/query errors, and prefer v2 for duplicate
// native IDs while retaining v1-only sessions in a mixed database.
impl OpenCode {
    fn store_in(&self, paths: Vec<PathBuf>) -> Option<Store> {
        let dbs: Vec<_> = paths.into_iter().filter(|path| path.is_file()).collect();
        if dbs.is_empty() {
            self.activity
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            return None;
        }
        let mut activity = HashMap::new();
        let mut keys = Vec::new();
        let mut had_error = false;
        for db in &dbs {
            let conn = match open_ro(db) {
                Ok(conn) => conn,
                Err(_) => {
                    had_error = true;
                    continue;
                }
            };
            // One read transaction per physical database prevents an inventory
            // assembled across different stages of a concurrent schema migration.
            let tx = match conn.unchecked_transaction() {
                Ok(tx) => tx,
                Err(_) => {
                    had_error = true;
                    continue;
                }
            };
            let mut seen = std::collections::HashSet::new();
            let mut has_schema = false;
            for (table, sql, schema_tag) in [
                (
                    "session_v2",
                    "SELECT id, time_updated, time_created FROM session_v2",
                    V2_TOKEN_TAG,
                ),
                (
                    "session",
                    "SELECT id, time_updated, time_created FROM session",
                    0,
                ),
            ] {
                match table_exists(&tx, table) {
                    Ok(false) => continue,
                    Ok(true) => has_schema = true,
                    Err(_) => {
                        had_error = true;
                        continue;
                    }
                }
                let mut stmt = match tx.prepare(sql) {
                    Ok(stmt) => stmt,
                    Err(_) => {
                        had_error = true;
                        continue;
                    }
                };
                let rows = match stmt.query_map([], |row| {
                    let id: String = row.get(0)?;
                    let updated: Option<i64> = row.get(1)?;
                    let created: Option<i64> = row.get(2)?;
                    Ok((id, updated.or(created).unwrap_or(0)))
                }) {
                    Ok(rows) => rows,
                    Err(_) => {
                        had_error = true;
                        continue;
                    }
                };
                for row in rows {
                    match row {
                        Ok((id, token)) if seen.insert(id.clone()) => {
                            let key = make_key(db, &id);
                            activity.insert(
                                key.clone(),
                                (token > 0)
                                    .then(|| DateTime::from_timestamp_millis(token))
                                    .flatten(),
                            );
                            keys.push((key, token ^ schema_tag))
                        }
                        Ok(_) => {}
                        Err(_) => had_error = true,
                    }
                }
            }
            had_error |= !has_schema;
            if tx.commit().is_err() {
                had_error = true;
            }
        }
        *self.activity.lock().unwrap_or_else(|e| e.into_inner()) = activity;
        Some(Store {
            keys,
            files: dbs,
            had_error,
        })
    }
}

fn uses_v2(conn: &Connection, sid: &str) -> Result<bool> {
    Ok(table_exists(conn, "session_v2")?
        && conn
            .query_row("SELECT 1 FROM session_v2 WHERE id=?1", [sid], |_| Ok(()))
            .optional()
            .context("check session_v2")?
            .is_some())
}

fn table_exists(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |_| Ok(()),
    )
    .optional()
    .map(|row| row.is_some())
}

/// Key encoding for a shared-store session: `<db path><US><session id>`. The
/// unit separator never appears in a path or a uuid.
const KEY_SEP: char = '\u{1f}';
fn make_key(db: &Path, session_id: &str) -> String {
    format!("{}{KEY_SEP}{session_id}", db.display())
}

/// A human-facing form of a shared-store key for `Session.path` (shown in
/// `show`/`brief`/web). The raw key carries a U+001F separator that must not
/// leak into output or an LLM briefing; render it as `<db>#<id>`.
fn display_path(key: &str) -> String {
    key.replace(KEY_SEP, "#")
}

/// `$XDG_DATA_HOME/opencode`, or `~/.local/share/opencode`.
fn data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("share")))?;
    Some(base.join("opencode"))
}

/// The session databases: `opencode.db` plus any `opencode-<channel>.db`, or a
/// single path from `$OPENCODE_DB` (absolute, or relative to the data dir).
fn db_paths() -> Vec<PathBuf> {
    if let Some(over) = std::env::var_os("OPENCODE_DB") {
        let p = PathBuf::from(&over);
        if !p.as_os_str().is_empty() && p.as_path() != Path::new(":memory:") {
            return vec![if p.is_absolute() {
                p
            } else {
                data_dir().map(|d| d.join(&p)).unwrap_or(p)
            }];
        }
    }
    let Some(dir) = data_dir() else {
        return vec![];
    };
    let mut out = vec![dir.join("opencode.db")];
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with("opencode-") && name.ends_with(".db") {
                out.push(p);
            }
        }
    }
    out
}

/// Open another tool's database strictly read-only - never creating, writing,
/// checkpointing, or recovering it. A plain read-only handle can fail on a WAL
/// db when the shared-memory file is missing/locked; the fallback retries with
/// an `immutable=1` URI, which tells SQLite to assume the file cannot change and
/// skip all locking and shared-memory, so it still cannot write. (Trade-off:
/// changes a running OpenCode makes mid-read may be missed - fine for indexing.)
fn open_ro(db: &Path) -> Result<Connection> {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .or_else(|_| {
            let uri = format!("file:{}?immutable=1", db.display());
            Connection::open_with_flags(
                &uri,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
            )
        })
        .map_err(Into::into)
}

fn redact_json_strings(value: &mut Value) {
    match value {
        Value::String(text) => *text = crate::redact::redact(text).into_owned(),
        Value::Array(values) => values.iter_mut().for_each(redact_json_strings),
        Value::Object(values) => values.values_mut().for_each(redact_json_strings),
        _ => {}
    }
}

fn push(messages: &mut Vec<Message>, role: Role, text: &str, ts: Option<DateTime<Utc>>) {
    let text = text.trim();
    if !text.is_empty() {
        messages.push(Message {
            role,
            text: text.to_string(),
            ts,
            tool: None,
        });
    }
}

#[cfg(test)]
mod tests;
