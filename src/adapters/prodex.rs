//! prodex (github.com/youdie006/prodex) - a local bridge that lets coding
//! agents consult a logged-in ChatGPT Pro. Every consult is a durable task
//! (`.bridge/tasks/<id>.json`, the QUESTION) paired with a result
//! (`.bridge/results/<id>.json`, the ANSWER) and, for pro consults, a full
//! answer artifact (`.bridge/artifacts/pro-consults/<id>.md`).
//!
//! Bridges are per-repo and scattered; prodex >=0.11.0 registers every bridge
//! root in `~/.local/share/prodex/bridges.json`, which is the discovery
//! entry point here. `SESSIONWIKI_PRODEX_REGISTRY` overrides it for tests.

use super::{redacted_first_line, Adapter, Discovered};
use crate::model::{Message, Role, Session};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub struct Prodex;

fn registry_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SESSIONWIKI_PRODEX_REGISTRY") {
        return Some(p.into());
    }
    Some(
        dirs::home_dir()?
            .join(".local")
            .join("share")
            .join("prodex")
            .join("bridges.json"),
    )
}

/// Read the registry without turning a broken/partial registry into a clean
/// empty listing. A missing registry means Prodex is not installed and is
/// normal; every other read, parse, or shape failure must suppress deletion
/// reconciliation for this sync.
fn bridge_roots() -> (Vec<PathBuf>, bool) {
    let Some(path) = registry_path() else {
        return (Vec::new(), false);
    };
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return (Vec::new(), true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (Vec::new(), false);
        }
        Err(_) => return (Vec::new(), true),
    }
    let Ok(text) = crate::util::read_to_string_capped(&path) else {
        return (Vec::new(), true);
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return (Vec::new(), true);
    };
    let Some(values) = v.get("roots").and_then(Value::as_array) else {
        return (Vec::new(), true);
    };
    let mut roots = Vec::with_capacity(values.len());
    let mut had_error = false;
    for value in values {
        match value.as_str() {
            Some(root) if !root.is_empty() => roots.push(PathBuf::from(root)),
            _ => had_error = true,
        }
    }
    (roots, had_error)
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// `task_YYYYMMDD_HHMMSS_slug` -> a coarse UTC timestamp, the fallback when a
/// task has no `claimed_at` yet.
fn ts_from_id(id: &str) -> Option<DateTime<Utc>> {
    let mut parts = id.split('_');
    if parts.next() != Some("task") {
        return None;
    }
    let (d, t) = (parts.next()?, parts.next()?);
    // ASCII-digit gate BEFORE any byte slicing: ids are ASCII by construction,
    // but this parses untrusted on-disk data - a multibyte char at a slice
    // boundary must degrade to None, never panic (a parse panic would abort
    // the whole indexer).
    if d.len() != 8
        || t.len() != 6
        || !d.bytes().all(|b| b.is_ascii_digit())
        || !t.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let iso = format!(
        "{}-{}-{}T{}:{}:{}Z",
        &d[0..4],
        &d[4..6],
        &d[6..8],
        &t[0..2],
        &t[2..4],
        &t[4..6]
    );
    parse_ts(&iso)
}

impl Adapter for Prodex {
    fn name(&self) -> &'static str {
        "prodex"
    }

    fn root(&self) -> Option<PathBuf> {
        // For display/presence (`scan` shows this): the registry's directory,
        // consistent with every other adapter showing a store DIRECTORY.
        // Discovery reads the registry file itself. Do not report its surviving
        // parent as a present store after the registry disappears: sync would
        // otherwise interpret absence of the entire store as deletion of every
        // registered task.
        registry_path()
            .filter(|p| p.try_exists().unwrap_or(false))
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    }

    fn discover(&self) -> Discovered {
        let mut files = Vec::new();
        let (roots, mut had_error) = bridge_roots();
        // The registry can name one repo under several spellings (a
        // case-variant cwd on a case-insensitive drive, a symlink). Walking
        // each spelling indexed every task once per spelling under one session
        // id, which listed it twice and made `show <id>` ambiguous.
        let mut walked = std::collections::HashSet::new();
        for root in roots {
            let root = PathBuf::from(crate::util::project_key(&root.to_string_lossy()));
            if !walked.insert(std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone())) {
                continue;
            }
            let tasks = root.join(".bridge").join("tasks");
            match std::fs::read_dir(&tasks) {
                Ok(rd) => {
                    for entry in rd {
                        match entry {
                            Ok(e) => {
                                let p = e.path();
                                if p.extension().is_some_and(|x| x == "json") {
                                    files.push(p);
                                }
                            }
                            Err(_) => had_error = true,
                        }
                    }
                }
                // A registered repo or a not-yet-used task store may be gone;
                // both are legitimate absence, not incomplete discovery.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => had_error = true,
            }
        }
        Discovered { files, had_error }
    }

    fn parse(&self, path: &Path) -> Result<Session> {
        // Capped like every other adapter's session read. This one went
        // straight to `fs::read`, so the bound that exists to stop a corrupt or
        // hostile file exhausting memory was applied by eleven adapters and
        // skipped by this one.
        let task: Value = serde_json::from_str(&crate::util::read_to_string_capped(path)?)
            .with_context(|| format!("parse {}", path.display()))?;
        let task_id = task["id"].as_str().context("task has no id")?.to_string();
        // Task ids share a long `task_YYYYMMDD_...` prefix, so as SESSION ids
        // they would defeat prefix addressing (list shows 13 chars; several
        // tasks per day collide there). Derive the same short, stable 12-hex
        // id shape every other tool uses; the real task id stays reachable
        // via the stored path.
        let id = {
            let d = Sha256::digest(task_id.as_bytes());
            let mut hex = String::with_capacity(12);
            for b in &d[..6] {
                hex.push_str(&format!("{b:02x}"));
            }
            hex
        };
        let prompt = task["prompt"].as_str().unwrap_or("").trim().to_string();
        // The QUESTION is the most informative title a consult can have -
        // prodex auto-titles most consults identically ("GPT Pro consult"),
        // which makes a list of them indistinguishable. Task title is the
        // fallback for promptless tasks.
        let title = {
            let head = redacted_first_line(&prompt, 80);
            if !head.is_empty() {
                head
            } else {
                task["title"]
                    .as_str()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .unwrap_or("(untitled task)")
                    .to_string()
            }
        };
        // tasks/<id>.json -> the bridge root is two levels up from tasks/.
        let bridge = path.parent().and_then(|p| p.parent());
        let repo = bridge.and_then(|b| b.parent());
        let project = repo
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(unknown)".into());

        let started = task["claimed_at"]
            .as_str()
            .and_then(parse_ts)
            .or_else(|| ts_from_id(&task_id));

        let mut messages = Vec::new();
        if !prompt.is_empty() {
            messages.push(Message {
                role: Role::User,
                text: prompt,
                ts: started,
                tool: None,
            });
        }
        // The answer: the full pro-consult artifact when present, else the
        // result summary. The artifact is read INDEPENDENTLY of the result -
        // a crash between the two writes must not make an answer that exists
        // on disk unindexable. The comment here used to claim this read was
        // bounded while calling plain `read_to_string`, and "normally a few KB"
        // is not what is on disk: the largest file in a real prodex store was
        // 35 MB. It is bounded now.
        let mut ended = None;
        if let Some(bridge) = bridge {
            let artifact_text = crate::util::read_to_string_capped(
                &bridge
                    .join("artifacts")
                    .join("pro-consults")
                    .join(format!("{task_id}.md")),
            )
            .ok()
            .map(|t| {
                let mut t = crate::redact::redact(t.trim()).into_owned();
                const CAP: usize = 64 * 1024;
                if t.len() > CAP {
                    let mut end = CAP;
                    while !t.is_char_boundary(end) {
                        end -= 1;
                    }
                    t.truncate(end);
                }
                t
            })
            .filter(|t| !t.is_empty());
            let mut summary = None;
            if let Ok(text) = crate::util::read_to_string_capped(
                &bridge.join("results").join(format!("{task_id}.json")),
            ) {
                if let Ok(result) = serde_json::from_str::<Value>(&text) {
                    ended = result["created_at"].as_str().and_then(parse_ts);
                    summary = result["summary"]
                        .as_str()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                }
            }
            if let Some(text) = artifact_text.or(summary) {
                messages.push(Message {
                    role: Role::Assistant,
                    text,
                    ts: ended.or(started),
                    tool: None,
                });
            }
        }

        // Files the task bundled become provenance links (trace integration).
        let touched: Vec<String> = task["files"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|f| {
                        f.as_str()
                            .map(str::to_string)
                            .or_else(|| f["path"].as_str().map(str::to_string))
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(Session {
            id,
            tool: "prodex",
            path: path.to_path_buf(),
            project,
            started,
            ended: ended.or(started),
            title,
            subagent: false,
            messages,
            touched,
            edits: Vec::new(),
        })
    }
}

/// The ChatGPT thread URL a bridge's consults land in, from the newest
/// `.bridge/sessions/*.json` next to `task_path`. Only an `https://chatgpt.com/`
/// URL is ever surfaced - a tampered session file cannot inject anything else.
pub fn thread_url_for_task(task_path: &Path) -> Option<String> {
    let sessions = task_path.parent()?.parent()?.join("sessions");
    let mut candidates: Vec<(std::time::SystemTime, String)> = Vec::new();
    for e in std::fs::read_dir(sessions).ok()?.flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&crate::util::read_to_string_capped(&p).ok()?)
        else {
            continue;
        };
        let Some(url) = v["thread"].as_str() else {
            continue;
        };
        if !url.starts_with("https://chatgpt.com/") {
            continue;
        }
        let mtime = e
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        candidates.push((mtime, url.to_string()));
    }
    candidates.sort_by_key(|(t, _)| *t);
    candidates.pop().map(|(_, u)| u)
}
