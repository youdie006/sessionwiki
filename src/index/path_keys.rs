//! Shared filesystem stores can be read through different home aliases by
//! the CLI and an embedder. Preserve the first indexed key and its annotations;
//! identity is a canonical store root plus the relative native session path.
use crate::adapters::Adapter;
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub(super) struct ExistingKey {
    pub key: String,
    pub session_id: String,
}

#[derive(Clone)]
pub(super) struct Match {
    pub existing: ExistingKey,
    pub aliases: Vec<String>,
}

pub(super) struct PathKeys {
    depth: usize,
    root: PathBuf,
    roots: HashMap<PathBuf, Option<PathBuf>>,
    sessions: HashMap<PathBuf, Match>,
    outside: Vec<String>,
}

impl PathKeys {
    pub fn new(conn: &Connection, adapter: &dyn Adapter) -> Result<Option<Self>> {
        let Some(depth) = adapter.path_key_depth().filter(|depth| *depth > 0) else {
            return Ok(None);
        };
        let Some(root) = adapter
            .root()
            .and_then(|p| crate::util::canonicalize_as_recorded(p).ok())
        else {
            return Ok(None);
        };
        let mut keys = Self {
            depth,
            root,
            roots: HashMap::new(),
            sessions: HashMap::new(),
            outside: Vec::new(),
        };
        // Oldest identity wins if an older version already wrote duplicates.
        // Do not delete or merge annotations on those pre-existing rows.
        // Finish the SQLite read before resolving filesystem roots, which
        // may live on a different volume. No DB read lock spans that I/O.
        let rows = {
            let mut stmt =
                conn.prepare("SELECT path, session_id FROM files WHERE tool = ?1 ORDER BY rowid")?;
            let rows = stmt.query_map([adapter.name()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (key, session_id) in rows {
            if let Some(identity) = keys
                .identity(&key)
                .filter(|identity| identity.starts_with(&keys.root))
            {
                keys.sessions
                    .entry(identity)
                    .and_modify(|group| group.aliases.push(key.clone()))
                    .or_insert_with(|| Match {
                        existing: ExistingKey {
                            key: key.clone(),
                            session_id,
                        },
                        aliases: vec![key],
                    });
            } else {
                // Reconciliation is scoped by physical root, not by spelling.
                // All other homes (including unreadable ones) stay protected.
                keys.outside.push(key);
            }
        }
        Ok(Some(keys))
    }

    fn identity(&mut self, key: &str) -> Option<PathBuf> {
        let path = Path::new(key);
        let root = path.ancestors().nth(self.depth)?;
        if !root.is_absolute() {
            return None;
        }
        let physical = self
            .roots
            .entry(root.to_path_buf())
            .or_insert_with(|| crate::util::canonicalize_as_recorded(root).ok())
            .as_ref()?;
        Some(physical.join(path.strip_prefix(root).ok()?))
    }

    pub fn resolve(&mut self, key: &str) -> Option<Match> {
        let identity = self.identity(key)?;
        self.sessions.get(&identity).cloned()
    }

    pub fn protect_other_homes(&self, seen: &mut Vec<String>) {
        seen.extend(self.outside.iter().cloned());
    }
}
