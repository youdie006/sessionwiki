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
    fn claude_folder_matches_the_observed_encoding() {
        // Claude Code names the project folder by turning every '/', '.', and
        // '_' in the absolute path into '-' (so '.' yields a doubled dash after
        // the leading separator).
        assert_eq!(
            claude_project_folder("/home/dev/myproject"),
            "-home-dev-myproject"
        );
        assert_eq!(
            claude_project_folder("/home/dev/my_project"),
            "-home-dev-my-project"
        );
        assert_eq!(
            claude_project_folder("/home/dev/.config"),
            "-home-dev--config"
        );
        assert_eq!(
            claude_project_folder("/home/dev/a.b/c_d"),
            "-home-dev-a-b-c-d"
        );
    }

    #[test]
    fn gemini_hash_is_sha256_of_the_path() {
        // Gemini's tmp folder is the SHA-256 hex of the absolute project path.
        assert_eq!(
            gemini_project_hash("/home/dev/myproject"),
            "5ea998fd0e431a6b5f864ca7d6386eacfa7b33d53df16df5f0faeb1c0cd2d021"
        );
    }
}
