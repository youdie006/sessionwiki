use super::{is_boilerplate, push, redact_json_strings};
use crate::adapters::{
    bounded_redacted_output, dedup_paths, title_from_messages, Adapter, Discovered, Store,
};
use crate::model::{Message, Role, Session, ToolCall, ToolResult};
use crate::util::short_id;
use anyhow::{Context, Result};
use chrono::DateTime;
use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Codex keeps paginated thread history in `thread_history_<n>.sqlite`
/// (`thread_items`, one JSON item per row) and the thread registry - cwd and
/// source - in `state_<n>.sqlite`. After migrating a thread it may drop the
/// rollout, leaving the database as the only copy. This store lists the
/// threads that have no rollout in `sessions/` or `archived_sessions/`;
/// threads that still have one are read from the rollout by [`super::Codex`].
#[derive(Default)]
pub struct CodexHistory {
    /// Explicit Codex home, or `None` for `~/.codex`.
    home: Option<PathBuf>,
}

impl CodexHistory {
    /// The history of the Codex install rooted at `home`.
    pub fn in_home(home: impl Into<PathBuf>) -> Self {
        CodexHistory {
            home: Some(home.into()),
        }
    }

    fn home(&self) -> Option<PathBuf> {
        match &self.home {
            Some(home) => Some(home.clone()),
            None => Some(dirs::home_dir()?.join(".codex")),
        }
    }

    fn db(&self) -> Option<PathBuf> {
        let home = self.home()?;
        Some(
            newest(&home, "thread_history_")
                .unwrap_or_else(|| home.join("thread_history_1.sqlite")),
        )
    }

    /// Native ids of every rollout on disk, from the file names alone.
    fn rollout_ids(&self, home: &Path, had_error: &mut bool) -> HashSet<String> {
        let mut ids = HashSet::new();
        for dir in ["sessions", "archived_sessions"] {
            let dir = home.join(dir);
            if !dir.exists() {
                continue;
            }
            for entry in WalkDir::new(dir) {
                let Some(entry) = crate::adapters::ok_or_flag(entry, had_error) else {
                    continue;
                };
                let name = entry.file_name().to_string_lossy();
                if entry.file_type().is_file() && name.starts_with("rollout-") {
                    if let Some(id) = crate::index::native_id_of(&name) {
                        ids.insert(id);
                    }
                }
            }
        }
        ids
    }
}

/// The highest-numbered `<prefix><n>.sqlite` in `home`.
fn newest(home: &Path, prefix: &str) -> Option<PathBuf> {
    std::fs::read_dir(home)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let n: u32 = name
                .strip_prefix(prefix)?
                .strip_suffix(".sqlite")?
                .parse()
                .ok()?;
            Some((n, e.path()))
        })
        .max_by_key(|(n, _)| *n)
        .map(|(_, p)| p)
}

const KEY_SEP: char = '\u{1f}';

fn open_ro(db: &Path) -> Result<Connection> {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(Into::into)
}

impl Adapter for CodexHistory {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn root(&self) -> Option<PathBuf> {
        self.db()
    }

    /// Only the keys of this database; the rollout adapters own the rest.
    fn reconcile_scope(&self) -> Option<String> {
        Some(format!("{}{KEY_SEP}", self.db()?.display()))
    }

    fn discover(&self) -> Discovered {
        Vec::new().into()
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        anyhow::bail!("{} is not a Codex rollout", path.display())
    }

    fn store(&self) -> Option<Store> {
        let home = self.home()?;
        let db = self.db().filter(|db| db.is_file())?;
        let mut had_error = false;
        let on_disk = self.rollout_ids(&home, &mut had_error);
        let mut keys = Vec::new();
        let read = open_ro(&db).and_then(|conn| {
            let mut stmt = conn.prepare(
                "SELECT thread_id, MAX(MAX(created_at_ms, COALESCE(completed_at_ms, 0)))
                 FROM thread_items GROUP BY thread_id",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            for (id, token) in rows.flatten() {
                if !on_disk.contains(&id.to_ascii_lowercase()) {
                    keys.push((format!("{}{KEY_SEP}{id}", db.display()), token));
                }
            }
            Ok(())
        });
        had_error |= read.is_err();
        Some(Store {
            keys,
            files: vec![db],
            had_error,
        })
    }

    fn parse_key(&self, key: &str) -> Result<Session> {
        let (db, thread) = key
            .split_once(KEY_SEP)
            .context("malformed Codex history key")?;
        let conn = open_ro(Path::new(db)).with_context(|| format!("open {db}"))?;
        // LIMIT caps memory on a hostile db; a real thread is far below it.
        let mut stmt = conn.prepare(
            "SELECT created_at_ms, item_json FROM thread_items
             WHERE thread_id = ?1 ORDER BY rollout_ordinal LIMIT 200000",
        )?;
        let items: Vec<(i64, String)> = stmt
            .query_map(params![thread], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();

        let mut messages = Vec::new();
        let mut touched = Vec::new();
        let mut first_cwd = None;
        let (mut started, mut ended) = (None, None);
        for (ms, raw) in &items {
            let Ok(item) = serde_json::from_str::<Value>(raw) else {
                continue;
            };
            let ts = DateTime::from_timestamp_millis(*ms);
            if ts.is_some() {
                started = started.or(ts);
                ended = ts;
            }
            if first_cwd.is_none() {
                first_cwd = item.get("cwd").and_then(Value::as_str).map(str::to_owned);
            }
            read_item(&item, ts, &mut messages, &mut touched);
        }

        let registry = Path::new(db)
            .parent()
            .and_then(|home| newest(home, "state_"));
        let (cwd, subagent) = registry
            .and_then(|state| thread_registry(&state, thread))
            .unwrap_or((None, false));
        Ok(Session {
            id: short_id(key),
            tool: self.name(),
            path: PathBuf::from(key),
            project: cwd.or(first_cwd).unwrap_or_default(),
            started,
            ended,
            title: title_from_messages(&messages),
            subagent,
            messages,
            touched: dedup_paths(touched),
            edits: Vec::new(),
        })
    }
}

/// A thread's cwd and whether it is a sub-agent's, from the registry. Read
/// tolerantly: a missing or older registry only loses these two fields.
fn thread_registry(state: &Path, thread: &str) -> Option<(Option<String>, bool)> {
    let conn = open_ro(state).ok()?;
    let (cwd, source): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT cwd, source FROM threads WHERE id = ?1",
            params![thread],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()?;
    let by_thread = conn
        .query_row(
            "SELECT thread_source FROM threads WHERE id = ?1",
            params![thread],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten();
    let by_source = source
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .is_some_and(|s| s.get("subagent").is_some());
    Some((
        cwd.filter(|c| !c.is_empty()),
        by_source || by_thread.as_deref() == Some("subagent"),
    ))
}

fn read_item(
    item: &Value,
    ts: Option<chrono::DateTime<chrono::Utc>>,
    messages: &mut Vec<Message>,
    touched: &mut Vec<String>,
) {
    let id = item.get("id").and_then(Value::as_str).map(str::to_owned);
    // `declined` is a call refused at approval: it never ran.
    let failed = matches!(
        item.get("status").and_then(Value::as_str),
        Some("failed" | "declined")
    );
    let call = |messages: &mut Vec<Message>,
                name: &str,
                mut args: Value,
                output: Option<(String, bool)>| {
        redact_json_strings(&mut args);
        messages.push(Message::tool_call(
            ts,
            ToolCall {
                id: id.clone(),
                name: name.to_owned(),
                args,
            },
        ));
        if let Some((text, is_error)) = output {
            messages.push(Message::tool_result(
                ts,
                ToolResult {
                    call_id: id.clone(),
                    text: bounded_redacted_output(&text),
                    is_error,
                },
            ));
        }
    };
    match item.get("type").and_then(Value::as_str) {
        Some("userMessage") => {
            for block in item
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    if !is_boilerplate(text) {
                        push(messages, Role::User, text, ts);
                    }
                }
            }
        }
        Some("agentMessage") => {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                push(messages, Role::Assistant, text, ts);
            }
        }
        Some("commandExecution") => {
            let output = item
                .get("aggregatedOutput")
                .and_then(Value::as_str)
                .unwrap_or("");
            call(
                messages,
                "exec_command",
                json!({"cmd": item.get("command").cloned().unwrap_or(Value::Null)}),
                Some((output.to_owned(), failed)),
            );
        }
        Some("fileChange") => {
            let mut patch = String::from("*** Begin Patch\n");
            for change in item
                .get("changes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(path) = change.get("path").and_then(Value::as_str) else {
                    continue;
                };
                touched.push(path.to_owned());
                let header = match change.pointer("/kind/type").and_then(Value::as_str) {
                    Some("add") => "Add File",
                    Some("delete") => "Delete File",
                    _ => "Update File",
                };
                patch.push_str(&format!("*** {header}: {path}\n"));
                if let Some(to) = change.pointer("/kind/move_path").and_then(Value::as_str) {
                    touched.push(to.to_owned());
                    patch.push_str(&format!("*** Move to: {to}\n"));
                }
                if let Some(diff) = change.get("diff").and_then(Value::as_str) {
                    patch.push_str(diff);
                    if !diff.ends_with('\n') {
                        patch.push('\n');
                    }
                }
            }
            patch.push_str("*** End Patch");
            call(
                messages,
                "apply_patch",
                json!({"patch": patch}),
                Some((String::new(), failed)),
            );
        }
        Some("mcpToolCall") => {
            let error = item.pointer("/error/message").and_then(Value::as_str);
            let text = match error {
                Some(message) => message.to_owned(),
                None => item
                    .pointer("/result/content")
                    .and_then(Value::as_array)
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter_map(|b| b.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default(),
            };
            call(
                messages,
                item.get("tool").and_then(Value::as_str).unwrap_or("?"),
                item.get("arguments").cloned().unwrap_or_else(|| json!({})),
                Some((text, failed || error.is_some())),
            );
        }
        Some("webSearch") => {
            call(
                messages,
                "web_search",
                json!({"query": item.get("query").cloned().unwrap_or(Value::Null)}),
                None,
            );
        }
        // Reasoning, compaction, sub-agent bookkeeping and media views carry no
        // conversation text.
        _ => {}
    }
}
