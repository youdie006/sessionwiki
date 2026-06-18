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

#[cfg(test)]
mod tests {
    use super::*;

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
}
