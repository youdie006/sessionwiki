//! Alias-aware stores serialize enumeration, identity selection and writes.
//! This prevents concurrent CLI/embedded writers from selecting different IDs
//! or reconciling against an inventory older than another writer's commit.
use super::{
    archive_or_prune, index_one, path_keys, report_indexed, restore_unchanged, ToolOutputMode,
};
use crate::adapters::{self, Adapter, Store};
use anyhow::Result;
use rusqlite::{Connection, Transaction, TransactionBehavior};
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
    if adapter.path_key_depth().is_some() {
        // Acquire before discovery: a second index writer must not publish a
        // session that our earlier inventory would then mistake for deletion.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = if let Some(store) = adapter.store() {
            Some(sync_store(&tx, Some(&tx), adapter, &store, known, options)?)
        } else {
            None
        };
        tx.commit()?;
        Ok(result)
    } else if let Some(store) = adapter.store() {
        sync_store(conn, None, adapter, &store, known, options).map(Some)
    } else {
        Ok(None)
    }
}

fn sync_store(
    conn: &Connection,
    enclosing_tx: Option<&Transaction<'_>>,
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
    let mut archived_total = 0;
    let mut aliases = path_keys::PathKeys::new(conn, adapter)?;
    let mut resolved = HashMap::new();
    let mut seen: Vec<String> = Vec::with_capacity(store.keys.len());
    let mut pending: Vec<String> = Vec::new();
    let mut restore: Vec<String> = Vec::new();
    for (key, token) in &store.keys {
        let matched = aliases.as_mut().and_then(|aliases| aliases.resolve(key));
        let indexed_key = matched
            .as_ref()
            .map_or(key.as_str(), |group| group.existing.key.as_str());
        let previous = if aliases.is_some() {
            matched.as_ref().map(|group| group.existing.state)
        } else {
            known.get(indexed_key).copied()
        };
        match previous {
            Some((mtime, 0, true)) if mtime == *token => restore.push(indexed_key.to_owned()),
            Some((mtime, 0, false)) if mtime == *token => {}
            old if old.is_some_and(|row| row.2 || row.0 < 0 || row.1 < 0)
                || since.is_none_or(|s| {
                    adapter
                        .modified_at(key)
                        .map_or(*token, |time| time.timestamp())
                        >= s
                }) =>
            {
                pending.push(key.clone());
            }
            _ => {}
        }
        if let Some(group) = matched {
            seen.extend(group.aliases);
            resolved.insert(key.as_str(), group.existing);
        } else {
            seen.push(key.clone());
        }
    }
    restore_unchanged(conn, &restore)?;
    // Reconcile deletions only when the whole store was read this run.
    // If a backing db was present but unreadable (locked, half-written),
    // `seen` is partial - pruning off it would archive the whole corpus
    // on a transient hiccup, so skip reconciliation until a clean read.
    if store.had_error {
        // The discover path says this out loud; this one did not. It is
        // the path aider takes, whose walk is capped at two seconds over
        // the whole home - so on a large home the flag can be set every
        // run, reconciliation never happens, and nothing says why a
        // session the tool deleted is still listed.
        eprintln!(
            "[{tool}] the store could not be read in full; \
             skipping deletion reconciliation this run"
        );
    } else {
        let scope = adapter.reconcile_scope();
        if let Some(aliases) = &aliases {
            aliases.protect_other_homes(&mut seen);
        }
        archived_total += archive_or_prune(
            conn,
            tool,
            &seen,
            store_present,
            if aliases.is_some() {
                None
            } else {
                scope.as_deref()
            },
        )?;
    }

    if !pending.is_empty() {
        let token_of: HashMap<&str, i64> =
            store.keys.iter().map(|(k, t)| (k.as_str(), *t)).collect();
        let total = pending.len();
        let mut failed = 0usize;
        let local_tx = if enclosing_tx.is_none() {
            Some(conn.unchecked_transaction()?)
        } else {
            None
        };
        let tx = enclosing_tx
            .or(local_tx.as_ref())
            .expect("store write transaction");
        for (i, key) in pending.iter().enumerate() {
            if progress {
                eprint!("\r[{tool}] indexing {}/{total}", i + 1);
                std::io::stderr().flush().ok();
            }
            // A failed parse is warned, not silently dropped: the user
            // must know a session is missing from the corpus.
            let mut session = match adapters::parse_session_key(adapter, key) {
                Ok(s) => s,
                Err(e) => {
                    failed += 1;
                    eprintln!("\r[{tool}] failed to parse {key}: {e:#}");
                    continue;
                }
            };
            let indexed_key = if let Some(existing) = resolved.get(key.as_str()) {
                session.id.clone_from(&existing.session_id);
                session.path = PathBuf::from(&existing.key);
                &existing.key
            } else {
                key
            };
            let token = token_of.get(key.as_str()).copied().unwrap_or(0);
            index_one(tx, &session, indexed_key, token, 0, output_mode)?;
            known.insert(indexed_key.clone(), (token, 0, false));
        }
        if let Some(tx) = local_tx {
            tx.commit()?;
        }
        report_indexed(tool, total, failed);
    }

    Ok(archived_total)
}
