use chrono::{DateTime, Utc};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn label(&self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Message {
    pub role: Role,
    pub text: String,
    pub ts: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct Session {
    /// Short stable id derived from the file path (FNV-1a hash, hex).
    pub id: String,
    pub tool: &'static str,
    pub path: PathBuf,
    pub project: String,
    pub started: Option<DateTime<Utc>>,
    pub ended: Option<DateTime<Utc>>,
    pub title: String,
    /// True for subagent transcripts spawned inside a parent session.
    pub subagent: bool,
    pub messages: Vec<Message>,
    /// Files the session edited or created, extracted from its tool calls
    /// (Claude's Edit/Write, Codex's apply_patch). This is the link between a
    /// session and the code it produced - the basis for `files` and `blame`.
    pub touched: Vec<String>,
}

/// One discovered session store on disk (for `scan`).
pub struct StoreReport {
    pub tool: &'static str,
    pub root: PathBuf,
    pub files: usize,
    pub bytes: u64,
    pub oldest: Option<DateTime<Utc>>,
    pub newest: Option<DateTime<Utc>>,
}
