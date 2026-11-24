use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;
use std::str::FromStr;

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

impl FromStr for Role {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "user" => Ok(Role::User),
            "assistant" => Ok(Role::Assistant),
            "tool" => Ok(Role::Tool),
            _ => Err("role must be user, assistant, or tool"),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Message {
    pub role: Role,
    pub text: String,
    pub ts: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolEvent>,
}

/// Structured tool data before or after the shared summary pass.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ToolEvent {
    Part(ToolPart),
    Summary(ToolSummary),
}

/// A tool call or result emitted by an adapter before pairing.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPart {
    Call(ToolCall),
    Result(ToolResult),
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCall {
    pub id: Option<String>,
    pub name: String,
    pub args: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolResult {
    pub call_id: Option<String>,
    pub text: String,
    pub is_error: bool,
}

/// Paired tool data retained in memory after its compact line is rendered.
#[derive(Debug, Clone, Serialize)]
pub struct ToolSummary {
    pub name: String,
    pub args: Value,
    pub output: Option<String>,
    pub is_error: Option<bool>,
    pub lines: Option<usize>,
}
impl Message {
    pub fn tool_call(ts: Option<DateTime<Utc>>, call: ToolCall) -> Self {
        Self {
            role: Role::Tool,
            text: String::new(),
            ts,
            tool: Some(ToolEvent::Part(ToolPart::Call(call))),
        }
    }

    pub fn tool_result(ts: Option<DateTime<Utc>>, result: ToolResult) -> Self {
        Self {
            role: Role::Tool,
            text: String::new(),
            ts,
            tool: Some(ToolEvent::Part(ToolPart::Result(result))),
        }
    }
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
