//! Path math for `migrate` - relocating a session so it can be resumed from a
//! different project directory. Each tool ties a session to a directory
//! differently, so the encoding lives here (and is unit-tested against the
//! schemes observed on disk).

use sha2::{Digest, Sha256};

/// Which Claude store a transcript should be written into.
///
/// A handoff copies a conversation so ANOTHER account can resume it, and with
/// swapdex's slot model each account has its own `CLAUDE_CONFIG_DIR` with its
/// own `projects/` inside. Writing into `~/.claude` would land it where that
/// account never looks - the copy would succeed and the resume would find
/// nothing.
///
/// Explicit target first, then the environment (so running inside a slot's own
/// shell already does the right thing), then the single default store, which is
/// the classic model and the common case.
pub fn claude_store_root(
    explicit: Option<&std::path::Path>,
    env_dir: Option<&str>,
    home: &std::path::Path,
) -> std::path::PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Some(e) = env_dir.map(str::trim).filter(|e| !e.is_empty()) {
        return std::path::PathBuf::from(e);
    }
    home.join(".claude")
}

/// The folder Claude Code keeps a project's sessions in, under `projects/`.
/// Resume is scoped to it, so migrating to a new directory means copying the
/// transcript into that directory's folder. It is the absolute path with every
/// UTF-16 unit that is not an ASCII letter or digit turned into '-', and past
/// 200 characters the first 200 plus '-' and a base-36 hash of the path
/// (Claude Code 2.1's `replace(/[^a-zA-Z0-9]/g, "-")` and its 32-bit string
/// hash).
pub fn claude_project_folder(abs_path: &str) -> String {
    const MAX: usize = 200;
    let units: Vec<u16> = abs_path.encode_utf16().collect();
    let name: String = units
        .iter()
        .map(|&u| match u8::try_from(u) {
            Ok(b) if b.is_ascii_alphanumeric() => char::from(b),
            _ => '-',
        })
        .collect();
    if name.len() <= MAX {
        return name;
    }
    let hash = units.iter().fold(0i32, |h, &u| {
        h.wrapping_shl(5).wrapping_sub(h).wrapping_add(i32::from(u))
    });
    format!(
        "{}-{}",
        &name[..MAX],
        base36(u64::from(hash.unsigned_abs()))
    )
}

fn base36(mut n: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
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
        // Claude Code names the project folder after the absolute path, so '.'
        // after a separator yields a doubled dash.
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
        // Every character but an ASCII letter or digit becomes '-', one per
        // UTF-16 unit, as Claude Code's `replace(/[^a-zA-Z0-9]/g, "-")` does.
        assert_eq!(
            claude_project_folder("/home/dev/My Folder"),
            "-home-dev-My-Folder"
        );
        assert_eq!(claude_project_folder("/tmp/a:b@c"), "-tmp-a-b-c");
        assert_eq!(claude_project_folder("/home/dev/한글"), "-home-dev---");
        // Past 200 characters it keeps the first 200 and appends a hash of the
        // path, so a deep folder still gets a name the filesystem accepts.
        let deep = format!("/home/dev/{}app", "deep/".repeat(40));
        assert_eq!(
            claude_project_folder(&deep),
            format!("{}-sguhne", &deep.replace('/', "-")[..200])
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

#[cfg(test)]
mod store_root_tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// A handoff writes the transcript into the store of the account that will
    /// resume it. With swapdex's slot model each account has its own
    /// `CLAUDE_CONFIG_DIR`, and `projects/` lives inside it - so a copy into
    /// `~/.claude` lands where the OTHER account will never look.
    #[test]
    fn an_explicit_config_dir_is_where_the_transcript_goes() {
        let home = Path::new("/home/u");
        let slot = PathBuf::from("/data/slots/abc");
        assert_eq!(
            claude_store_root(Some(&slot), Some("/env/dir"), home),
            slot,
            "an explicit target outranks the environment"
        );
    }

    /// Run from inside a slot's shell, the environment already names the store -
    /// honouring it is what makes `migrate` do the right thing there without
    /// anyone having to spell the path out.
    #[test]
    fn the_environment_names_the_store_when_nothing_else_does() {
        let home = Path::new("/home/u");
        assert_eq!(
            claude_store_root(None, Some("/env/dir"), home),
            PathBuf::from("/env/dir")
        );
    }

    /// The classic model, and the default: one store under the home directory.
    #[test]
    fn without_either_it_is_the_default_store() {
        let home = Path::new("/home/u");
        assert_eq!(
            claude_store_root(None, None, home),
            PathBuf::from("/home/u/.claude")
        );
        // An empty environment value is not a path; it must not win.
        assert_eq!(
            claude_store_root(None, Some("   "), home),
            PathBuf::from("/home/u/.claude")
        );
    }
}
