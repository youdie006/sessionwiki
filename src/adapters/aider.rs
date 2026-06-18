//! Aider adapter: read-only index of per-repo `.aider.chat.history.md`.
//! One file accumulates many runs (one per aider launch) delimited by
//! `# aider chat started at` headers. Markdown-derived, so roles are
//! reconstructed from line prefixes (lower fidelity than the JSONL adapters);
//! an assistant `#### ` heading or `> ` blockquote is a known misclassification.
//! No per-message timestamps; `started` is the run header (local time, assumed
//! UTC). Reads are size-capped; discovery is bounded and logs nothing.

use crate::model::{Message, Role};
use chrono::{DateTime, Utc};

struct Run {
    started: Option<DateTime<Utc>>,
    body: String,
}

/// Parse aider's header timestamp `%Y-%m-%d %H:%M:%S` (local naive, no tz) and
/// assume UTC. `parse_ts` in mod.rs is RFC3339-only and returns None for these.
fn parse_aider_ts(s: &str) -> Option<DateTime<Utc>> {
    chrono::NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|n| DateTime::<Utc>::from_naive_utc_and_offset(n, Utc))
}

/// Split a history file into runs on the `# aider chat started at ` header (the
/// only single-`#` line aider writes). Bytes before the first header belong to
/// no run and are dropped. A header with no body keeps its slot, so positional
/// run indices never renumber when new runs are appended.
fn split_runs(content: &str) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut cur: Option<Run> = None;
    for line in content.lines() {
        if let Some(ts) = line.strip_prefix("# aider chat started at ") {
            if let Some(r) = cur.take() {
                runs.push(r);
            }
            cur = Some(Run {
                started: parse_aider_ts(ts),
                body: String::new(),
            });
        } else if let Some(r) = cur.as_mut() {
            r.body.push_str(line);
            r.body.push('\n');
        }
        // lines before the first header (cur == None) are dropped
    }
    if let Some(r) = cur.take() {
        runs.push(r);
    }
    runs
}

fn push_unique(v: &mut Vec<String>, s: &str) {
    let s = s.trim().to_string();
    if !s.is_empty() && !v.contains(&s) {
        v.push(s);
    }
}

fn flush(messages: &mut Vec<Message>, role: Option<Role>, buf: &mut Vec<String>) {
    if let Some(role) = role {
        let text = buf.join("\n").trim().to_string();
        // Keep empty user turns (aider writes `#### ` for empty input); drop
        // empty assistant/tool noise.
        if !text.is_empty() || role == Role::User {
            messages.push(Message {
                role,
                text,
                ts: None,
            });
        }
    }
    buf.clear();
}

/// Reconstruct turns from one run body. `#### ` = user, `> ` = tool (every aider
/// tool/warning/error line is blockquoted), blank lines continue the current
/// turn, everything else is assistant (the default container). Edited files come
/// from `> Applied edit to` / `> Creating empty file` (relative paths). Known
/// limitation: an assistant `#### ` heading or `> ` blockquote is misclassified.
fn parse_turns(body: &str) -> (Vec<Message>, Vec<String>) {
    let mut messages: Vec<Message> = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    let mut cur_role: Option<Role> = None;
    let mut buf: Vec<String> = Vec::new();

    for raw in body.lines() {
        let (line_role, content): (Option<Role>, String) =
            if let Some(r) = raw.strip_prefix("#### ") {
                (Some(Role::User), r.trim_end().to_string())
            } else if raw == "####" {
                (Some(Role::User), String::new())
            } else if let Some(r) = raw.strip_prefix("> ") {
                if let Some(p) = r.strip_prefix("Applied edit to ") {
                    push_unique(&mut touched, p);
                } else if let Some(p) = r.strip_prefix("Creating empty file ") {
                    push_unique(&mut touched, p);
                }
                (Some(Role::Tool), r.to_string())
            } else if raw == ">" {
                (Some(Role::Tool), String::new())
            } else if raw.trim().is_empty() {
                (None, String::new()) // blank: continue the current turn
            } else {
                (Some(Role::Assistant), raw.to_string())
            };

        match line_role {
            Some(role) => {
                if cur_role != Some(role) {
                    flush(&mut messages, cur_role, &mut buf);
                    cur_role = Some(role);
                }
                buf.push(content);
            }
            None => {
                if cur_role.is_some() {
                    buf.push(String::new());
                }
            }
        }
    }
    flush(&mut messages, cur_role, &mut buf);
    (messages, touched)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_runs_keeps_empty_and_drops_preamble() {
        let c = "junk before any header\n\
                 # aider chat started at 2026-06-09 14:01:00\n\
                 #### hi\n\
                 answer\n\
                 # aider chat started at 2026-06-09 15:00:00\n\
                 # aider chat started at 2026-06-09 16:00:00\n\
                 #### again\n";
        let runs = split_runs(c);
        assert_eq!(runs.len(), 3, "empty middle run keeps its slot");
        assert_eq!(runs[0].started, parse_aider_ts("2026-06-09 14:01:00"));
        assert!(
            runs[1].body.trim().is_empty(),
            "header-only run has empty body"
        );
        assert!(runs[0].body.contains("#### hi"));
    }

    #[test]
    fn parse_aider_ts_handles_naive_local_as_utc_and_rejects_garbage() {
        assert!(parse_aider_ts("2026-06-09 14:01:00").is_some());
        assert!(parse_aider_ts("not a date").is_none());
    }

    #[test]
    fn assistant_markdown_and_tool_lines_classified() {
        let body = "#### fix the bug\n\
                    Here is the fix.\n\
                    Some prose.\n\
                    > Applied edit to src/a.py\n\
                    > Applied edit to src/a.py\n\
                    > Creating empty file src/b.py\n\
                    > Did not apply edit to src/c.py (--dry-run)\n";
        let (msgs, touched) = parse_turns(body);
        let roles: Vec<Role> = msgs.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec![Role::User, Role::Assistant, Role::Tool]);
        assert_eq!(msgs[0].text, "fix the bug");
        assert!(msgs[1].text.contains("Here is the fix."));
        assert_eq!(touched, vec!["src/a.py", "src/b.py"]); // dedup; dry-run ignored
        assert!(msgs.iter().all(|m| m.ts.is_none()));
    }

    #[test]
    fn blank_lines_do_not_split_an_assistant_message() {
        let body = "#### q\n\
                    para one\n\
                    \n\
                    para two\n";
        let (msgs, _) = parse_turns(body);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1].role, Role::Assistant);
        assert!(msgs[1].text.contains("para one") && msgs[1].text.contains("para two"));
    }
}
