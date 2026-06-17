mod claude_code;
mod codex;
mod gemini;
mod opencode;

use crate::model::{Session, StoreReport};
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};

/// One supported agent tool. An adapter knows where the tool keeps its
/// session files on disk and how to parse one file into a `Session`.
///
/// Adding support for a new tool means implementing this trait in a new
/// module and registering it in `all()`. PRs for new adapters are the main
/// way this project grows.
pub trait Adapter {
    fn name(&self) -> &'static str;
    /// Store root, e.g. ~/.claude/projects. May not exist on this machine.
    fn root(&self) -> Option<PathBuf>;
    /// All session files under the root.
    fn discover(&self) -> Vec<PathBuf>;
    /// Parse one session file. Must never panic on malformed input;
    /// skip bad lines and return what could be read.
    fn parse(&self, path: &Path) -> Result<Session>;
}

pub fn all() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(claude_code::ClaudeCode),
        Box::new(codex::Codex),
        Box::new(gemini::Gemini),
        Box::new(opencode::OpenCode),
    ]
}

pub fn by_name(name: &str) -> Option<Box<dyn Adapter>> {
    all().into_iter().find(|a| a.name() == name)
}

/// Filesystem-only summary of a store, used by `scan`. No parsing involved.
pub fn report(adapter: &dyn Adapter) -> Option<StoreReport> {
    let root = adapter.root()?;
    if !root.exists() {
        return None;
    }
    let files = adapter.discover();
    let mut bytes: u64 = 0;
    let mut oldest: Option<DateTime<Utc>> = None;
    let mut newest: Option<DateTime<Utc>> = None;
    let mut count = 0usize;
    for f in &files {
        let Ok(meta) = f.metadata() else { continue };
        count += 1;
        bytes += meta.len();
        if let Ok(modified) = meta.modified() {
            let t: DateTime<Utc> = modified.into();
            if oldest.is_none_or(|o| t < o) {
                oldest = Some(t);
            }
            if newest.is_none_or(|n| t > n) {
                newest = Some(t);
            }
        }
    }
    Some(StoreReport {
        tool: adapter.name(),
        root,
        files: count,
        bytes,
        oldest,
        newest,
    })
}

/// Shared helper: pick a session title from messages when the tool does not
/// store one. First user message that is not harness boilerplate wins.
pub(crate) fn title_from_messages(messages: &[crate::model::Message]) -> String {
    messages
        .iter()
        .find(|m| {
            m.role == crate::model::Role::User
                && !m.text.trim_start().starts_with('<')
                && !m.text.trim().is_empty()
        })
        .map(|m| crate::util::truncate(&m.text, 80))
        .unwrap_or_else(|| "(no user prompt)".into())
}

pub(crate) fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Tidy the list of files a session touched: trim, drop empties and obvious
/// non-paths, and de-duplicate while preserving first-seen order. A session
/// edits the same file many times; the link cares only that it did.
pub(crate) fn dedup_paths(paths: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    paths
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty() && !p.contains('\n') && p.len() <= 4096)
        .filter(|p| seen.insert(p.clone()))
        .collect()
}
