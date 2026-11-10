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
    pub state: (i64, i64, bool),
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
        // Stable path ordering wins if an older version wrote duplicates.
        // INSERT OR REPLACE changes files.rowid, so it cannot order identity owners.
        // Do not delete or merge annotations on those pre-existing rows.
        let rows = {
            let mut stmt =
                conn.prepare("SELECT path, session_id, mtime, size, archived_at IS NOT NULL FROM files WHERE tool = ?1 ORDER BY path")?;
            let rows = stmt.query_map([adapter.name()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    (
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, bool>(4)?,
                    ),
                ))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (key, session_id, state) in rows {
            if let Some((_, identity)) = keys.identity(&key).filter(|(root, _)| root == &keys.root)
            {
                keys.sessions
                    .entry(identity)
                    .and_modify(|group| group.aliases.push(key.clone()))
                    .or_insert_with(|| Match {
                        existing: ExistingKey {
                            key: key.clone(),
                            session_id,
                            state,
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

    fn identity(&mut self, key: &str) -> Option<(PathBuf, PathBuf)> {
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
        Some((
            physical.clone(),
            physical.join(path.strip_prefix(root).ok()?),
        ))
    }

    pub fn resolve(&mut self, key: &str) -> Option<Match> {
        let (_, identity) = self.identity(key)?;
        self.sessions.get(&identity).cloned()
    }

    pub fn protect_other_homes(&self, seen: &mut Vec<String>) {
        seen.extend(self.outside.iter().cloned());
    }
}
