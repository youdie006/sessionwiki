//! Detect which "oh-my-*" harness drove a session - from the filesystem only.
//!
//! oh-my-claudecode (OMC) and oh-my-openagent (OmO) leave an orchestration-state
//! directory (`.omc` / `.omo`) in the project they run in. That directory is a
//! fact about the cwd, so it cannot be confused with a session that merely
//! *discusses* the tool.
//!
//! We deliberately do NOT scan transcript text. A session that researches these
//! tools quotes the exact strings they inject (their source/doc literals), so
//! any text marker mislabels research and notes as a real run - measured at 0/7
//! precision on a real archive, and unfixable in principle (you cannot text-
//! distinguish "the harness injected this" from "a human wrote about it"). The
//! adapters also drop the injected-context messages where a marker would live
//! (Claude Code discards `isMeta`; Codex strips `user_instructions`). oh-my-codex
//! (OmX) leaves no directory and is intentionally not detectable.

use std::path::Path;

/// The harness tag for a session (`"oh-my-claudecode"` / `"oh-my-openagent"`),
/// or `None`, from the project's orchestration directory. Transcript text is
/// intentionally not consulted - see the module docs.
pub fn detect(project: &str) -> Option<&'static str> {
    if project.is_empty() {
        return None;
    }
    let p = Path::new(project);
    if p.join(".omc").is_dir() {
        return Some("oh-my-claudecode");
    }
    if p.join(".omo").is_dir() {
        return Some("oh-my-openagent");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::detect;

    #[test]
    fn empty_or_plain_project_is_unlabeled() {
        assert_eq!(detect(""), None);
        assert_eq!(detect("/tmp/sessionwiki-no-such-dir-xyz"), None);
    }

    #[test]
    fn detects_omc_from_the_orchestration_dir() {
        let base = std::env::temp_dir().join(format!("sw-omc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join(".omc")).unwrap();
        assert_eq!(detect(&base.to_string_lossy()), Some("oh-my-claudecode"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn discussing_a_harness_in_the_path_is_not_a_run() {
        // A project path that merely contains the tool's name (no .omc/.omo dir)
        // must not be tagged - the dir is the only signal.
        let base = std::env::temp_dir().join(format!("sw-oh-my-codex-talk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        assert_eq!(detect(&base.to_string_lossy()), None);
        let _ = std::fs::remove_dir_all(&base);
    }
}
