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

const V2_TOKEN_TAG: i64 = 3 << 61;

fn v2_token(created: Option<i64>, updated: Option<i64>, message_updated: Option<i64>) -> i64 {
    // FNV-1a over explicitly encoded optional timestamps keeps the persisted
    // token stable. Changes to either watermark matter even below their max.
    let mut hash = 0xcbf29ce484222325u64;
    for time in [created, updated, message_updated] {
        let present = u8::from(time.is_some());
        for byte in [present].into_iter().chain(time.unwrap_or(0).to_le_bytes()) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    // Reserve the high bits to distinguish this token from v1 timestamps
    // and from the earlier v2 timestamp-based token.
    (hash as i64 & ((1 << 61) - 1)) | V2_TOKEN_TAG
}

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
        let in_v2 = uses_v2(&tx, sid).ok()?;
        let sql = if in_v2 {
            "SELECT time_updated, time_created FROM session_v2 WHERE id=?1"
        } else {
            "SELECT time_updated, time_created FROM session WHERE id=?1"
        };
        let (updated, created): (Option<i64>, Option<i64>) = tx
            .query_row(sql, [sid], |row| Ok((row.get(0)?, row.get(1)?)))
            .ok()?;
        let message_updated = if in_v2 {
            v2::latest_message_update(&tx, sid).ok()?
        } else {
            None
        };
        v2::activity_millis(created, updated, message_updated)
            .filter(|time| *time > 0)
            .and_then(DateTime::from_timestamp_millis)
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        v1::parse_json(self.name(), path)
    }
}

// A partial SQL scan must never authorize deletion reconciliation. Keep
// row errors as well as preparation/query errors, and prefer v2 for duplicate
// native IDs while retaining unmigrated v1-only sessions in a mixed database.
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
            if is_next_prerelease(db, &tx) {
                continue;
            }
            let legacy = match LegacyPolicy::read(&tx) {
                Ok(policy) => policy,
                Err(_) => {
                    had_error = true;
                    LegacyPolicy::None
                }
            };
            let mut seen = std::collections::HashSet::new();
            let mut has_schema = false;
            for table in ["session_v2", "session"] {
                match table_exists(&tx, table) {
                    Ok(false) => continue,
                    Ok(true) => has_schema = true,
                    Err(_) => {
                        had_error = true;
                        continue;
                    }
                }
                let in_v2 = table == "session_v2";
                let stmt = if in_v2 {
                    v2::inventory_sql(&tx).and_then(|sql| tx.prepare(sql))
                } else {
                    tx.prepare("SELECT id, time_updated, time_created, NULL FROM session")
                };
                let mut stmt = match stmt {
                    Ok(stmt) => stmt,
                    Err(_) => {
                        had_error = true;
                        continue;
                    }
                };
                let rows = match stmt.query_map([], |row| {
                    let id: String = row.get(0)?;
                    // A v2 row owns its ID even when its timestamps are unreadable.
                    if in_v2 && !seen.insert(id.clone()) {
                        return Ok(None);
                    }
                    let updated: Option<i64> = row.get(1)?;
                    let created: Option<i64> = row.get(2)?;
                    let message_updated: Option<i64> = row.get(3)?;
                    if !in_v2 && (!legacy.allows(&id) || !seen.insert(id.clone())) {
                        return Ok(None);
                    }
                    let time = v2::activity_millis(created, updated, message_updated);
                    let token = if in_v2 {
                        v2_token(created, updated, message_updated)
                    } else {
                        time.unwrap_or(0)
                    };
                    Ok(Some((id, time, token)))
                }) {
                    Ok(rows) => rows,
                    Err(_) => {
                        had_error = true;
                        continue;
                    }
                };
                for row in rows {
                    match row {
                        Ok(Some((id, time, token))) => {
                            let key = make_key(db, &id);
                            activity.insert(
                                key.clone(),
                                time.filter(|time| *time > 0)
                                    .and_then(DateTime::from_timestamp_millis),
                            );
                            keys.push((key, token));
                        }
                        Ok(None) => {}
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

// OpenCode migrates in descending ID order and advances its cursor in the
// same transaction as each copy. Processed legacy rows remain in the source
// table, including after a user deletes their corresponding v2 sessions.
enum LegacyPolicy {
    All,
    Before(String),
    None,
}

impl LegacyPolicy {
    fn allows(&self, id: &str) -> bool {
        match self {
            Self::All => true,
            Self::Before(cursor) => id < cursor.as_str(),
            Self::None => false,
        }
    }

    fn read(conn: &Connection) -> Result<Self> {
        if !table_exists(conn, "kv")? {
            return Ok(Self::All);
        }
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM kv WHERE key='migration.v1-v2'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let Some(value) = value else {
            return Ok(Self::All);
        };
        let state: Value = serde_json::from_str(&value).context("read OpenCode migration state")?;
        anyhow::ensure!(
            table_exists(conn, "session_v2")?,
            "OpenCode migration state without session_v2"
        );
        match state.get("phase").and_then(Value::as_str) {
            Some("completed") => Ok(Self::None),
            Some("sessions") => match state.get("cursor") {
                None => Ok(Self::All),
                Some(Value::String(cursor)) => Ok(Self::Before(cursor.clone())),
                _ => anyhow::bail!("invalid OpenCode migration cursor"),
            },
            _ => anyhow::bail!("unknown OpenCode migration phase"),
        }
    }
}

fn uses_v2(conn: &Connection, sid: &str) -> Result<bool> {
    let in_v2 = table_exists(conn, "session_v2")?
        && conn
            .query_row("SELECT 1 FROM session_v2 WHERE id=?1", [sid], |_| Ok(()))
            .optional()
            .context("check session_v2")?
            .is_some();
    if !in_v2 {
        anyhow::ensure!(
            LegacyPolicy::read(conn)?.allows(sid),
            "OpenCode session {sid} is absent from v2 after migration; refusing its stale v1 copy"
        );
    }
    Ok(in_v2)
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

/// `opencode-next.db` in the v2 pre-release layout. OpenCode 2.1.1 imports it
/// into `opencode.db` during its v1-to-v2 migration and leaves the file behind,
/// so it is not a v1 channel database. The layout test is OpenCode's own
/// (`isNextDatabase`); a v1 database under this name is still read.
fn is_next_prerelease(db: &Path, conn: &Connection) -> bool {
    let has = |table: &str| {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |_| Ok(()),
        )
        .is_ok()
    };
    db.file_name().is_some_and(|n| n == "opencode-next.db")
        && has("project")
        && has("session")
        && has("session_message")
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
