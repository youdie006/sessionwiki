//! Detect which "oh-my-*" harness drove a session.
//!
//! These Korean tools - oh-my-claudecode (OMC), oh-my-codex (OmX), and the
//! oh-my-openagent / lazyclaudecode / lazycodex family (OmO) - are wrappers
//! around Claude Code, Codex, and OpenCode, so their conversations are written
//! to those tools' own stores and are already indexed. This recovers *which*
//! harness produced a session from the markers it injects, so the sessions can
//! be tagged and filtered like any other curation.
//!
//! Markers are source-verified (the tools inject these verbatim every session):
//! OMC writes a CLAUDE.md header "running with oh-my-claudecode" and dispatches
//! `oh-my-claudecode:<agent>` subagents; OmX puts `oh-my-codex` in Codex's
//! `developer_instructions`; the OmO family injects `<ultrawork-mode>` /
//! "ULTRAWORK MODE ENABLED" and is published as `omo@sisyphuslabs`.

use crate::model::Message;
use std::path::Path;

/// The harness tag for a session (e.g. `"oh-my-claudecode"`), or `None`. Only
/// meaningful for the tools these harnesses wrap (Claude Code, Codex, OpenCode).
pub fn detect(messages: &[Message], project: &str) -> Option<&'static str> {
    // Transcript markers - present even on archived sessions, no filesystem
    // access. The substrings are distinctive enough to match anywhere in the
    // text (injected prompt, a Task subagent_type, or assistant output).
    for m in messages {
        let t = m.text.as_str();
        if t.contains("oh-my-claudecode") {
            return Some("oh-my-claudecode");
        }
        if t.contains("oh-my-codex") {
            return Some("oh-my-codex");
        }
        if t.contains("oh-my-openagent")
            || t.contains("omo@sisyphuslabs")
            || t.contains("ULTRAWORK MODE ENABLED")
            || t.contains("<ultrawork-mode>")
        {
            return Some("oh-my-openagent");
        }
    }

    // Orchestration-state directory the harness leaves in the project - an
    // always-on signal that survives even when the injected prompt did not make
    // it into the captured transcript.
    if !project.is_empty() {
        let p = Path::new(project);
        if p.join(".omc").is_dir() {
            return Some("oh-my-claudecode");
        }
        if p.join(".omo").is_dir() {
            return Some("oh-my-openagent");
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Role;

    fn msg(role: Role, text: &str) -> Message {
        Message {
            role,
            text: text.to_string(),
            ts: None,
        }
    }

    #[test]
    fn detects_omc_from_subagent_type() {
        let ms = vec![
            msg(Role::User, "fix the build"),
            msg(
                Role::Tool,
                "Task {\"subagent_type\":\"oh-my-claudecode:executor\"}",
            ),
        ];
        assert_eq!(detect(&ms, ""), Some("oh-my-claudecode"));
    }

    #[test]
    fn detects_omx_from_developer_instructions() {
        let ms = vec![msg(
            Role::User,
            "You have oh-my-codex installed. AGENTS.md is the orchestration brain.",
        )];
        assert_eq!(detect(&ms, ""), Some("oh-my-codex"));
    }

    #[test]
    fn detects_omo_from_ultrawork() {
        let ms = vec![msg(Role::Assistant, "ULTRAWORK MODE ENABLED! starting.")];
        assert_eq!(detect(&ms, ""), Some("oh-my-openagent"));
    }

    #[test]
    fn plain_session_is_unlabeled() {
        let ms = vec![
            msg(Role::User, "add a retry to the client"),
            msg(Role::Assistant, "done"),
        ];
        assert_eq!(detect(&ms, "/tmp/does-not-exist-xyz"), None);
    }
}
