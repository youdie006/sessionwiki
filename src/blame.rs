//! `blame` core: git-blame porcelain parsing, run grouping, and the
//! commit -> session attribution heuristic. Pure functions, unit-testable
//! without invoking git or opening a database.

#[derive(Debug, Clone, PartialEq)]
pub struct LineBlame {
    pub line: usize,
    pub commit: String,
    pub author_time: i64,
}

/// Parse `git blame --line-porcelain` output. Every line is preceded by a
/// `<40-hex-sha> <orig> <final> [count]` header and repeated `key value`
/// lines; the content line begins with a tab. We capture the final line
/// number, the commit sha, and `author-time` (epoch seconds).
pub fn parse_line_porcelain(out: &str) -> Vec<LineBlame> {
    let mut result = Vec::new();
    let mut sha = String::new();
    let mut final_line = 0usize;
    let mut author_time = 0i64;
    for raw in out.lines() {
        if raw.starts_with('\t') {
            // The source line itself; emit the metadata gathered for it.
            if !sha.is_empty() {
                result.push(LineBlame {
                    line: final_line,
                    commit: sha.clone(),
                    author_time,
                });
            }
            continue;
        }
        if let Some(rest) = raw.strip_prefix("author-time ") {
            author_time = rest.trim().parse().unwrap_or(0);
            continue;
        }
        // Header line: "<sha> <orig> <final> [count]" - 40-hex sha + digits.
        let mut parts = raw.split(' ');
        if let (Some(maybe_sha), Some(_orig), Some(fin)) =
            (parts.next(), parts.next(), parts.next())
        {
            if maybe_sha.len() == 40 && maybe_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                if let Ok(f) = fin.parse::<usize>() {
                    sha = maybe_sha.to_string();
                    final_line = f;
                }
            }
        }
    }
    result
}

#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub start: usize,
    pub end: usize,
    pub commit: String,
    pub author_time: i64,
}

/// Collapse per-line blame into contiguous runs sharing one commit.
pub fn group_runs(lines: &[LineBlame]) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for lb in lines {
        if let Some(last) = runs.last_mut() {
            if last.commit == lb.commit && lb.line == last.end + 1 {
                last.end = lb.line;
                continue;
            }
        }
        runs.push(Run {
            start: lb.line,
            end: lb.line,
            commit: lb.commit.clone(),
            author_time: lb.author_time,
        });
    }
    runs
}

#[derive(Debug, Clone, PartialEq)]
pub struct TouchingSession {
    pub session_id: String,
    pub tool: String,
    pub title: String,
    pub project: String,
    pub started: Option<i64>,
    pub ended: Option<i64>,
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Attribution {
    Confident(TouchingSession),
    Ambiguous(Vec<TouchingSession>),
    Unattributed,
}

/// How long after a session ends a commit may still be attributed to it
/// (commits often land well after the conversation). Tunable.
pub const LAG_WINDOW_SECS: i64 = 14 * 24 * 3600;

fn project_is_ancestor(project: &str, repo_path: &str) -> bool {
    !project.is_empty() && repo_path.starts_with(project)
}

/// Map a commit's author-time to the session most likely behind it, among the
/// sessions that touched the file. (a) sessions whose [started,ended] window
/// contains author_time; (b) else the most recent session that ended at/before
/// author_time within LAG_WINDOW_SECS; project-ancestor breaks ties. >=2 keep
/// as ambiguous; none -> unattributed.
pub fn attribute_commit(
    author_time: i64,
    repo_path: &str,
    candidates: &[TouchingSession],
) -> Attribution {
    let containing: Vec<&TouchingSession> = candidates
        .iter()
        .filter(|s| {
            matches!((s.started, s.ended), (Some(a), Some(b)) if a <= author_time && author_time <= b)
        })
        .collect();
    if containing.len() == 1 {
        return Attribution::Confident(containing[0].clone());
    }
    if containing.len() > 1 {
        return disambiguate(containing, repo_path);
    }
    // (b) most-recent-before within the lag window
    let mut before: Vec<&TouchingSession> = candidates
        .iter()
        .filter(|s| matches!(s.ended, Some(e) if e <= author_time && author_time - e <= LAG_WINDOW_SECS))
        .collect();
    if before.is_empty() {
        return Attribution::Unattributed;
    }
    let newest = before
        .iter()
        .filter_map(|s| s.ended)
        .max()
        .unwrap_or(i64::MIN);
    before.retain(|s| s.ended == Some(newest));
    if before.len() == 1 {
        Attribution::Confident(before[0].clone())
    } else {
        disambiguate(before, repo_path)
    }
}

fn disambiguate(mut tied: Vec<&TouchingSession>, repo_path: &str) -> Attribution {
    let ancestors: Vec<&TouchingSession> = tied
        .iter()
        .copied()
        .filter(|s| project_is_ancestor(&s.project, repo_path))
        .collect();
    if ancestors.len() == 1 {
        return Attribution::Confident(ancestors[0].clone());
    }
    if !ancestors.is_empty() {
        tied = ancestors;
    }
    Attribution::Ambiguous(tied.into_iter().cloned().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sess(id: &str, started: i64, ended: i64, project: &str) -> TouchingSession {
        TouchingSession {
            session_id: id.into(),
            tool: "claude-code".into(),
            title: id.into(),
            project: project.into(),
            started: Some(started),
            ended: Some(ended),
            archived: false,
        }
    }

    #[test]
    fn confident_when_one_window_contains_the_commit() {
        let c = vec![sess("s1", 100, 200, "/repo")];
        match attribute_commit(150, "/repo", &c) {
            Attribution::Confident(s) => assert_eq!(s.session_id, "s1"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ambiguous_when_two_windows_contain_the_commit() {
        let c = vec![sess("s1", 100, 200, "/repo"), sess("s2", 140, 260, "/repo")];
        match attribute_commit(150, "/repo", &c) {
            Attribution::Ambiguous(v) => assert_eq!(v.len(), 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn falls_back_to_most_recent_before_within_lag_window() {
        let c = vec![sess("s1", 100, 200, "/repo"), sess("s2", 900, 990, "/repo")];
        match attribute_commit(1000, "/repo", &c) {
            Attribution::Confident(s) => assert_eq!(s.session_id, "s2"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unattributed_when_nothing_qualifies() {
        let c = vec![sess("s1", 100, 200, "/repo")];
        assert_eq!(
            attribute_commit(200 + LAG_WINDOW_SECS + 1, "/repo", &c),
            Attribution::Unattributed
        );
    }

    #[test]
    fn project_ancestor_breaks_a_tie_in_the_lag_window() {
        let a = sess("a", 100, 500, "/other");
        let b = sess("b", 100, 500, "/repo");
        match attribute_commit(1000, "/repo/src", &[a, b]) {
            Attribution::Confident(s) => assert_eq!(s.session_id, "b"),
            other => panic!("{other:?}"),
        }
    }

    const SAMPLE: &str = "\
abc123abc123abc123abc123abc123abc123abcd 1 1 2
author Dev
author-time 1700000000
author-tz +0000
summary first
filename src/a.rs
\tline one
abc123abc123abc123abc123abc123abc123abcd 2 2
author-time 1700000000
\tline two
def456def456def456def456def456def456def4 3 3 1
author Dev
author-time 1700000500
summary second
filename src/a.rs
\tline three
";

    #[test]
    fn parses_line_to_commit_and_time() {
        let got = parse_line_porcelain(SAMPLE);
        assert_eq!(got.len(), 3);
        assert_eq!(
            got[0],
            LineBlame {
                line: 1,
                commit: "abc123abc123abc123abc123abc123abc123abcd".into(),
                author_time: 1_700_000_000
            }
        );
        assert_eq!(
            got[2],
            LineBlame {
                line: 3,
                commit: "def456def456def456def456def456def456def4".into(),
                author_time: 1_700_000_500
            }
        );
    }

    #[test]
    fn groups_consecutive_lines_by_commit() {
        let lines = vec![
            LineBlame {
                line: 1,
                commit: "a".into(),
                author_time: 10,
            },
            LineBlame {
                line: 2,
                commit: "a".into(),
                author_time: 10,
            },
            LineBlame {
                line: 3,
                commit: "b".into(),
                author_time: 20,
            },
            LineBlame {
                line: 4,
                commit: "a".into(),
                author_time: 10,
            },
        ];
        let runs = group_runs(&lines);
        assert_eq!(
            runs,
            vec![
                Run {
                    start: 1,
                    end: 2,
                    commit: "a".into(),
                    author_time: 10
                },
                Run {
                    start: 3,
                    end: 3,
                    commit: "b".into(),
                    author_time: 20
                },
                Run {
                    start: 4,
                    end: 4,
                    commit: "a".into(),
                    author_time: 10
                },
            ]
        );
    }
}
