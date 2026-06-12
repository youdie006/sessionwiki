use std::path::{Path, PathBuf};

/// How to reopen a session in its original tool. Built from the session
/// file path, since every supported tool encodes its native session id
/// in the filename.
pub struct ResumeInfo {
    pub program: &'static str,
    pub args: Vec<String>,
    /// The tool resolves sessions relative to the project, so the command
    /// should run there. None when the session has no usable cwd.
    pub cwd: Option<PathBuf>,
    pub note: Option<String>,
}

impl ResumeInfo {
    pub fn command_line(&self) -> String {
        let mut s = String::from(self.program);
        for a in &self.args {
            s.push(' ');
            s.push_str(a);
        }
        s
    }
}

pub fn for_session(tool: &str, path: &Path, project: &str) -> Option<ResumeInfo> {
    let cwd = (!project.is_empty() && project.starts_with('/'))
        .then(|| PathBuf::from(project));
    match tool {
        "claude-code" => {
            let p = path.to_string_lossy();
            if p.contains("/subagents/") {
                // Subagent transcripts cannot be resumed directly, but the
                // parent session id is the directory just above "subagents":
                // <project>/<parent-uuid>/subagents/agent-*.jsonl
                let comps: Vec<String> = path
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                let idx = comps.iter().position(|c| c == "subagents")?;
                let parent = comps.get(idx.checked_sub(1)?)?.clone();
                if !looks_like_uuid(&parent) {
                    return None;
                }
                return Some(ResumeInfo {
                    program: "claude",
                    args: vec!["--resume".into(), parent],
                    cwd,
                    note: Some(
                        "this is a subagent transcript; resuming its parent session".into(),
                    ),
                });
            }
            let id = path.file_stem()?.to_string_lossy().into_owned();
            looks_like_uuid(&id).then(|| ResumeInfo {
                program: "claude",
                args: vec!["--resume".into(), id],
                cwd,
                note: None,
            })
        }
        "codex" => {
            // rollout-<timestamp>-<uuid>.jsonl
            let stem = path.file_stem()?.to_string_lossy().into_owned();
            let id = stem.get(stem.len().checked_sub(36)?..)?.to_string();
            looks_like_uuid(&id).then(|| ResumeInfo {
                program: "codex",
                args: vec!["resume".into(), id],
                cwd,
                note: None,
            })
        }
        // Gemini CLI saved chats are resumed from inside the REPL
        // (/chat resume); there is no headless resume-by-file today.
        _ => None,
    }
}

fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}
