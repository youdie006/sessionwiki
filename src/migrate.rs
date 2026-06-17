//! Path math for `migrate` - relocating a session so it can be resumed from a
//! different project directory. Each tool ties a session to a directory
//! differently, so the encoding lives here (and is unit-tested against the
//! schemes observed on disk).

use sha2::{Digest, Sha256};

/// Claude Code stores a session at `~/.claude/projects/<folder>/<uuid>.jsonl`,
/// where `<folder>` is the absolute project path with every `/`, `.`, and `_`
/// turned into `-` (the scheme observed across every project folder on disk).
/// Resume is scoped to this folder, so migrating to a new directory means
/// copying the transcript into that directory's folder.
pub fn claude_project_folder(abs_path: &str) -> String {
    abs_path
        .chars()
        .map(|c| match c {
            '/' | '.' | '_' => '-',
            other => other,
        })
        .collect()
}

/// Gemini CLI stores chats under `~/.gemini/tmp/<projectHash>/chats/`, where
/// `<projectHash>` is the SHA-256 (hex) of the absolute project path. The chat
/// JSON carries the same hash in its `projectHash` field.
pub fn gemini_project_hash(abs_path: &str) -> String {
    let digest = Sha256::digest(abs_path.as_bytes());
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_folder_matches_the_on_disk_encoding() {
        // Verified against ~/.claude/projects folder names on a real machine.
        assert_eq!(claude_project_folder("/home/dev/project"), "-mnt-d-MyProject");
        assert_eq!(
            claude_project_folder("/home/dev/project/coursework"),
            "-mnt-d-MyProject-KU-homework"
        );
        assert_eq!(
            claude_project_folder("/home/dev/.claude"),
            "-home-dev--claude"
        );
        assert_eq!(
            claude_project_folder("/home/dev/open-design/.od/x"),
            "-home-dev-open-design--od-x"
        );
    }

    #[test]
    fn gemini_hash_matches_the_on_disk_folder() {
        // sha256("/home/dev/project") - the folder that exists under ~/.gemini/tmp.
        assert_eq!(
            gemini_project_hash("/home/dev/project"),
            "47df4ac14c4d3f1e338e79a789a20a276ac099f7e598e3e2aa152de05da46083"
        );
    }
}
