//! The hardened git layer against a real, deterministic tempdir repo. Also
//! pins arg-injection safety: a file literally named `-weird.rs` must be blamed,
//! not parsed as a git flag (the `--` separator).

use sessionwiki::blame;
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str], date: Option<&str>) {
    let mut c = Command::new("git");
    c.current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Dev")
        .env("GIT_AUTHOR_EMAIL", "dev@example.com")
        .env("GIT_COMMITTER_NAME", "Dev")
        .env("GIT_COMMITTER_EMAIL", "dev@example.com");
    if let Some(d) = date {
        c.env("GIT_AUTHOR_DATE", d).env("GIT_COMMITTER_DATE", d);
    }
    assert!(c.status().unwrap().success(), "git {args:?}");
}

#[test]
fn blames_a_committed_file_with_a_dash_named_path() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"], None);
    // A file literally named "-weird.rs" must not be parsed as a git flag.
    std::fs::write(root.join("-weird.rs"), "one\ntwo\n").unwrap();
    git(root, &["add", "--", "-weird.rs"], None);
    git(
        root,
        &["commit", "-q", "-m", "add"],
        Some("@1717840800 +0000"),
    );

    let file = root.join("-weird.rs");
    let resolved = blame::repo_root(&file).unwrap();
    let out = blame::run_git_blame(&resolved, &file, None).unwrap();
    let lines = blame::parse_line_porcelain(&out);
    assert_eq!(lines.len(), 2, "two lines blamed");
    assert_eq!(
        lines[0].author_time, 1_717_840_800,
        "author-time is the epoch we set"
    );
    assert_eq!(lines[1].line, 2);
}

#[test]
fn repo_root_errors_outside_a_repository() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("loose.txt"), "x\n").unwrap();
    assert!(blame::repo_root(&dir.path().join("loose.txt")).is_err());
}
