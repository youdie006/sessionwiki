//! Store-root aliases coordinate filesystem identities without keeping a
//! database writer lock while walking or parsing native histories.
use crate::adapters::Adapter;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Row};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct ExistingKey {
    pub key: String,
    pub session_id: String,
    pub state: (i64, i64, bool),
    rowid: i64,
}

fn read_row(row: &Row<'_>) -> rusqlite::Result<ExistingKey> {
    Ok(ExistingKey {
        key: row.get(0)?,
        session_id: row.get(1)?,
        state: (row.get(2)?, row.get(3)?, row.get(4)?),
        rowid: row.get(5)?,
    })
}

struct Snapshot {
    row: ExistingKey,
    identity: PathBuf,
}

pub(super) struct PathKeys {
    depth: usize,
    root: PathBuf,
    tool: &'static str,
    registry_key: String,
    roots: HashMap<PathBuf, Option<PathBuf>>,
    aliases: BTreeSet<String>,
    invalid_registered: HashSet<String>,
    initial: HashMap<PathBuf, ExistingKey>,
    snapshots: HashMap<String, Snapshot>,
    seen: HashSet<PathBuf>,
    relative: HashMap<String, PathBuf>,
}

impl PathKeys {
    /// Capture row versions before discovery. Reconciliation never removes a
    /// row another writer inserted or updated after this snapshot.
    pub fn new(conn: &Connection, adapter: &dyn Adapter) -> Result<Option<Self>> {
        let Some(depth) = adapter.path_key_depth().filter(|depth| *depth > 0) else {
            return Ok(None);
        };
        let Some(raw_root) = adapter.root() else {
            return Ok(None);
        };
        let Ok(root) = crate::util::canonicalize_as_recorded(&raw_root) else {
            return Ok(None);
        };
        let registry_key = format!("path-roots:{}:{}", adapter.name(), root.to_string_lossy());
        let mut keys = Self {
            depth,
            root,
            tool: adapter.name(),
            registry_key,
            roots: HashMap::new(),
            aliases: BTreeSet::new(),
            invalid_registered: HashSet::new(),
            initial: HashMap::new(),
            snapshots: HashMap::new(),
            seen: HashSet::new(),
            relative: HashMap::new(),
        };
        keys.include_root(&raw_root);
        let root = keys.root.clone();
        keys.include_root(&root);
        if let Some(scope) = adapter.reconcile_scope() {
            keys.include_root(Path::new(&scope));
        }
        for alias in keys.registered(conn)? {
            if !keys.include_root(Path::new(&alias)) {
                keys.invalid_registered.insert(alias);
            }
        }
        let rows = Self::rows(conn, keys.tool)?;
        for row in rows {
            if let Some((identity, _)) = keys.identity(&row.key) {
                // rows are ordered by path, independently of INSERT OR REPLACE.
                keys.initial
                    .entry(identity.clone())
                    .or_insert_with(|| row.clone());
                keys.snapshots
                    .insert(row.key.clone(), Snapshot { row, identity });
            }
        }
        Ok(Some(keys))
    }

    fn rows(conn: &Connection, tool: &str) -> Result<Vec<ExistingKey>> {
        let mut stmt = conn.prepare(
            "SELECT path, session_id, mtime, size, archived_at IS NOT NULL, rowid
             FROM files WHERE tool = ?1 ORDER BY path",
        )?;
        let rows = stmt.query_map([tool], read_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn registered(&self, conn: &Connection) -> Result<BTreeSet<String>> {
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key=?1",
                [&self.registry_key],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_default())
    }

    fn include_root(&mut self, root: &Path) -> bool {
        if !root.is_absolute() {
            return false;
        }
        let physical = self
            .roots
            .entry(root.to_path_buf())
            .or_insert_with(|| crate::util::canonicalize_as_recorded(root).ok());
        if physical.as_ref() == Some(&self.root) {
            self.aliases.insert(root.to_string_lossy().into_owned());
            true
        } else {
            false
        }
    }

    fn identity(&mut self, key: &str) -> Option<(PathBuf, PathBuf)> {
        let path = Path::new(key);
        let root = path.ancestors().nth(self.depth)?;
        if !self.include_root(root) {
            return None;
        }
        let relative = path.strip_prefix(root).ok()?.to_path_buf();
        Some((self.root.join(&relative), relative))
    }

    pub fn observe(&mut self, key: &str) -> Option<ExistingKey> {
        let (identity, relative) = self.identity(key)?;
        self.seen.insert(identity.clone());
        self.relative.insert(key.to_owned(), relative);
        self.initial.get(&identity).cloned()
    }

    /// A short writer transaction merges root spellings, then uses indexed
    /// path lookups to find the current owner. Concurrent writers register
    /// their root before inserting, so the second writer sees the first ID.
    /// No filesystem access or source parsing occurs in this method.
    pub fn claim(&self, conn: &Connection, key: &str) -> Result<Option<ExistingKey>> {
        let Some(relative) = self.relative.get(key) else {
            return Ok(None);
        };
        let mut roots = self.registered(conn)?;
        roots.retain(|root| !self.invalid_registered.contains(root));
        roots.extend(self.aliases.iter().cloned());
        let value = serde_json::to_string(&roots)?;
        conn.execute(
            "INSERT INTO meta(key,value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value WHERE value<>excluded.value",
            [&self.registry_key, &value],
        )?;
        let candidates: BTreeSet<_> = roots
            .iter()
            .map(|root| {
                Path::new(root)
                    .join(relative)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        let mut stmt = conn.prepare_cached(
            "SELECT path, session_id, mtime, size, archived_at IS NOT NULL, rowid
             FROM files WHERE path=?1 AND tool=?2",
        )?;
        for candidate in candidates {
            if let Some(row) = stmt
                .query_row([candidate.as_str(), self.tool], read_row)
                .optional()?
            {
                return Ok(Some(row));
            }
        }
        Ok(None)
    }

    /// Do not overwrite content published by another writer while we parsed.
    /// Archive-marker-only changes do not change rowid or content metadata.
    pub fn unchanged_since_snapshot(&self, key: &str, current: &ExistingKey) -> bool {
        let Some(relative) = self.relative.get(key) else {
            return false;
        };
        self.initial
            .get(&self.root.join(relative))
            .is_some_and(|old| {
                old.key == current.key
                    && old.session_id == current.session_id
                    && old.rowid == current.rowid
                    && old.state.0 == current.state.0
                    && old.state.1 == current.state.1
            })
    }

    pub fn reconcile(&self, conn: &Connection, store_present: bool) -> Result<usize> {
        let mut protected = Vec::new();
        for current in Self::rows(conn, self.tool)? {
            let missing = self.snapshots.get(&current.key).is_some_and(|snapshot| {
                snapshot.row == current && !self.seen.contains(&snapshot.identity)
            });
            if !missing {
                protected.push(current.key);
            }
        }
        super::archive_or_prune(conn, self.tool, &protected, store_present, None)
    }
}
