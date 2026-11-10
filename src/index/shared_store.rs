//! Shared-store indexing. Filesystem aliases select an existing owner in
//! short database transactions; enumeration and parsing never hold that lock.
use super::{
    archive_or_prune, index_one, path_keys, report_indexed, restore_unchanged, ToolOutputMode,
};
use crate::adapters::{self, Adapter, Store};
use anyhow::Result;
use rusqlite::{Connection, TransactionBehavior};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

#[derive(Clone, Copy)]
pub(super) struct Options {
    pub since: Option<i64>,
    pub output_mode: ToolOutputMode,
    pub progress: bool,
}

pub(super) fn sync(
    conn: &mut Connection,
    adapter: &dyn Adapter,
    known: &mut HashMap<String, (i64, i64, bool)>,
    options: Options,
) -> Result<Option<usize>> {
    // Snapshot before discovery, without a writer lock. This also resolves
    // filesystem roots once per store, rather than once per session.
    let aliases = path_keys::PathKeys::new(conn, adapter)?;
    let Some(store) = adapter.store() else {
        return Ok(None);
    };
    if let Some(aliases) = aliases {
        sync_aliased(conn, adapter, &store, aliases, known, options).map(Some)
    } else {
        sync_store(conn, adapter, &store, known, options).map(Some)
    }
}

fn needs_parse(
    adapter: &dyn Adapter,
    key: &str,
    token: i64,
    previous: Option<(i64, i64, bool)>,
    since: Option<i64>,
) -> bool {
    if previous.is_some_and(|row| row.0 == token && row.1 == 0) {
        return false;
    }
    previous.is_some_and(|row| row.2 || row.0 < 0 || row.1 < 0)
        || since.is_none_or(|s| adapter.modified_at(key).map_or(token, |t| t.timestamp()) >= s)
}

fn warn_incomplete(tool: &str) {
    eprintln!(
        "[{tool}] the store could not be read in full; skipping deletion reconciliation this run"
    );
}

fn sync_aliased(
    conn: &mut Connection,
    adapter: &dyn Adapter,
    store: &Store,
    mut aliases: path_keys::PathKeys,
    known: &mut HashMap<String, (i64, i64, bool)>,
    options: Options,
) -> Result<usize> {
    let tool = adapter.name();
    let store_present = adapter.root().is_some_and(|root| root.exists());
    let sources: Vec<_> = store
        .keys
        .iter()
        .map(|(key, token)| {
            let previous = aliases.observe(key).map(|row| row.state);
            (
                key,
                *token,
                needs_parse(adapter, key, *token, previous, options.since),
            )
        })
        .collect();
    let total = sources.iter().filter(|(_, _, parse)| *parse).count();
    let mut attempted = 0;
    let mut failed = 0;
    for (key, token, parse) in sources {
        let mut parsed = if parse {
            attempted += 1;
            if options.progress {
                eprint!("\r[{tool}] indexing {attempted}/{total}");
                std::io::stderr().flush().ok();
            }
            match adapters::parse_session_key(adapter, key) {
                Ok(session) => Some(session),
                Err(error) => {
                    failed += 1;
                    eprintln!("\r[{tool}] failed to parse {key}: {error:#}");
                    continue;
                }
            }
        } else {
            None
        };
        // Only database operations occur under the writer lock. A competing
        // writer may have published a different path spelling while we parsed.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = aliases.claim(&tx, key)?;
        if let Some(existing) = current
            .as_ref()
            .filter(|row| row.state.0 == token && row.state.1 == 0)
        {
            if existing.state.2 {
                restore_unchanged(&tx, std::slice::from_ref(&existing.key))?;
            }
        } else if let Some(session) = parsed.as_mut().filter(|_| {
            current
                .as_ref()
                .is_none_or(|row| aliases.unchanged_since_snapshot(key, row))
        }) {
            let indexed_key = if let Some(existing) = &current {
                session.id.clone_from(&existing.session_id);
                session.path = PathBuf::from(&existing.key);
                &existing.key
            } else {
                key
            };
            index_one(&tx, session, indexed_key, token, 0, options.output_mode)?;
            known.insert(indexed_key.clone(), (token, 0, false));
        }
        tx.commit()?;
    }
    if total > 0 {
        report_indexed(tool, total, failed);
    }
    if store.had_error {
        warn_incomplete(tool);
        return Ok(0);
    }
    // Only rows unchanged since the pre-discovery snapshot can disappear.
    // A concurrent writer's new/updated rows survive an older inventory.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let archived = aliases.reconcile(&tx, store_present)?;
    tx.commit()?;
    Ok(archived)
}

fn sync_store(
    conn: &Connection,
    adapter: &dyn Adapter,
    store: &Store,
    known: &mut HashMap<String, (i64, i64, bool)>,
    options: Options,
) -> Result<usize> {
    let Options {
        since,
        output_mode,
        progress,
    } = options;
    let tool = adapter.name();
    let store_present = adapter.root().is_some_and(|root| root.exists());
    let mut seen = Vec::with_capacity(store.keys.len());
    let mut pending = Vec::new();
    let mut restore = Vec::new();
    for (key, token) in &store.keys {
        let previous = known.get(key).copied();
        if previous == Some((*token, 0, true)) {
            restore.push(key.clone());
        } else if needs_parse(adapter, key, *token, previous, since) {
            pending.push((key, *token));
        }
        seen.push(key.clone());
    }
    restore_unchanged(conn, &restore)?;
    let archived = if store.had_error {
        warn_incomplete(tool);
        0
    } else {
        archive_or_prune(
            conn,
            tool,
            &seen,
            store_present,
            adapter.reconcile_scope().as_deref(),
        )?
    };
    if !pending.is_empty() {
        let total = pending.len();
        let mut failed = 0;
        let tx = conn.unchecked_transaction()?;
        for (i, (key, token)) in pending.iter().enumerate() {
            if progress {
                eprint!("\r[{tool}] indexing {}/{total}", i + 1);
                std::io::stderr().flush().ok();
            }
            let session = match adapters::parse_session_key(adapter, key) {
                Ok(session) => session,
                Err(error) => {
                    failed += 1;
                    eprintln!("\r[{tool}] failed to parse {key}: {error:#}");
                    continue;
                }
            };
            index_one(&tx, &session, key, *token, 0, output_mode)?;
            known.insert((*key).clone(), (*token, 0, false));
        }
        tx.commit()?;
        report_indexed(tool, total, failed);
    }
    Ok(archived)
}
