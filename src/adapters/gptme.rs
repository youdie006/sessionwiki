use super::{title_from_messages, Adapter};
use crate::model::{Message, Role, Session};
use crate::util::short_id;
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// gptme timestamps are naive ISO-8601 (Python's `datetime.now().isoformat()`,
/// no UTC offset). Try RFC 3339 first; fall back to parsing as UTC naive.
fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
        .or_else(|| {
            // Python naive format: "2026-06-08T10:00:01.000000"
            NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|n| n.and_utc())
        })
}

/// gptme stores one JSONL file per session under
/// `~/.local/share/gptme/logs/<session-name>/conversation.jsonl`.
/// Each line is a JSON object: `{ "role": "system"|"user"|"assistant",
/// "content": "...", "timestamp": "ISO-8601", ... }`.
/// Lines with `"pinned": true` are system-prompt boilerplate and are dropped.
pub struct Gptme;

impl Adapter for Gptme {
    fn name(&self) -> &'static str {
        "gptme"
    }

    fn root(&self) -> Option<PathBuf> {
        // XDG_DATA_HOME / gptme / logs on Linux; ~/Library/Application Support / gptme / logs on macOS
        Some(dirs::data_local_dir()?.join("gptme").join("logs"))
    }

    fn discover(&self) -> Vec<PathBuf> {
        let Some(root) = self.root() else {
            return vec![];
        };
        // Layout: <root>/<session-name>/conversation.jsonl (depth 2)
        WalkDir::new(root)
            .max_depth(2)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file() && e.file_name() == "conversation.jsonl")
            .map(|e| e.into_path())
            .collect()
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let reader = BufReader::new(file);

        let mut messages: Vec<Message> = Vec::new();
        let mut started = None;
        let mut ended = None;

        for line in reader.lines() {
            let Ok(line) = line else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                // Malformed line: skip without panicking
                continue;
            };

            // Pinned lines are injected system-prompt boilerplate; skip them.
            if v.get("pinned").and_then(Value::as_bool) == Some(true) {
                continue;
            }

            let ts = v
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_ts);
            if let Some(t) = ts {
                if started.is_none() {
                    started = Some(t);
                }
                ended = Some(t);
            }

            let role = match v.get("role").and_then(Value::as_str) {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                // system and any unknown roles: skip
                _ => continue,
            };

            let text = match v.get("content") {
                Some(Value::String(s)) => s.clone(),
                // Content is sometimes a list of blocks in newer versions; join them.
                Some(Value::Array(blocks)) => blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => continue,
            };
            let text = text.trim().to_string();
            if !text.is_empty() {
                messages.push(Message { role, text, ts });
            }
        }

        // The session name (human-readable label) is the directory name,
        // e.g. "run-autonomous-research-20260531102253-ef8c".
        let session_name = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let title = title_from_messages(&messages);

        Ok(Session {
            id: short_id(&path.to_string_lossy()),
            tool: self.name(),
            path: path.to_path_buf(),
            project: session_name,
            started,
            ended,
            title,
            subagent: false,
            messages,
            touched: Vec::new(),
        })
    }
}
