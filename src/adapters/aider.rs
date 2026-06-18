//! Aider adapter: read-only index of per-repo `.aider.chat.history.md`.
//! One file accumulates many runs (one per aider launch) delimited by
//! `# aider chat started at` headers. Markdown-derived, so roles are
//! reconstructed from line prefixes (lower fidelity than the JSONL adapters);
//! an assistant `#### ` heading or `> ` blockquote is a known misclassification.
//! No per-message timestamps; `started` is the run header (local time, assumed
//! UTC). Reads are size-capped; discovery is bounded and logs nothing.

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
}
