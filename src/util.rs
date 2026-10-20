use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use std::path::Path;

/// The largest session file we will read into memory. Session transcripts are
/// text; a file past this is malformed or hostile, so it is skipped rather than
/// allowed to exhaust memory.
pub const MAX_SESSION_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// Bytes read from the head/tail of an over-cap session for its window.
const WINDOW_HEAD_BYTES: u64 = 256 * 1024;
const WINDOW_TAIL_BYTES: u64 = 512 * 1024;

/// The JSONL lines to parse for a session, and whether it was WINDOWED. Under
/// the cap: every line. Over the cap: only the first 256 KB and last 512 KB of
/// lines (the partial line at each cut boundary is dropped), so a huge session
/// is still indexed - searchable, and its start + recent turns readable via
/// `session_window`/`show` - instead of being dropped entirely. Byte-bounded, so
/// even a 256 MB+ file is read cheaply (no full scan).
pub fn session_lines(path: &Path) -> Result<(Vec<String>, bool)> {
    let (cap, head, tail) = window_limits();
    session_lines_windowed(path, cap, head, tail)
}

#[cfg(test)]
thread_local! {
    /// Lets an adapter test window a small file: (cap, head, tail) bytes.
    pub(crate) static TEST_WINDOW: std::cell::Cell<Option<(u64, u64, u64)>> =
        const { std::cell::Cell::new(None) };
}

fn window_limits() -> (u64, u64, u64) {
    #[cfg(test)]
    if let Some(limits) = TEST_WINDOW.with(|w| w.get()) {
        return limits;
    }
    (MAX_SESSION_FILE_BYTES, WINDOW_HEAD_BYTES, WINDOW_TAIL_BYTES)
}

fn session_lines_windowed(
    path: &Path,
    cap: u64,
    head_bytes: u64,
    tail_bytes: u64,
) -> Result<(Vec<String>, bool)> {
    use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    if path.extension().is_some_and(|x| x == "zst") {
        let decoder = ruzstd::decoding::StreamingDecoder::new(BufReader::new(f))
            .map_err(|e| anyhow::anyhow!("decompress {}: {e}", path.display()))?;
        return decoded_lines_windowed(decoder, cap, head_bytes, tail_bytes)
            .with_context(|| format!("decompress {}", path.display()));
    }
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len <= cap {
        let lines = BufReader::new(f)
            .lines()
            .map_while(std::result::Result::ok)
            .collect();
        return Ok((lines, false));
    }
    let head_n = head_bytes.min(len) as usize;
    let mut head_buf = vec![0u8; head_n];
    f.read_exact(&mut head_buf)?;
    let tail_n = tail_bytes.min(len) as usize;
    let mut tail_buf = vec![0u8; tail_n];
    f.seek(SeekFrom::End(-(tail_n as i64)))?;
    f.read_exact(&mut tail_buf)?;
    Ok((window_lines(&head_buf, &tail_buf), true))
}

/// The same window over a stream that can only be read front to back: the
/// whole stream while it stays within `cap`, then its first `head_bytes` and
/// a rolling last `tail_bytes`.
fn decoded_lines_windowed(
    mut stream: impl std::io::Read,
    cap: u64,
    head_bytes: u64,
    tail_bytes: u64,
) -> Result<(Vec<String>, bool)> {
    let mut all = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    let mut tail: Option<(Vec<u8>, std::collections::VecDeque<u8>)> = None;
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        match &mut tail {
            Some((_, rolling)) => {
                rolling.extend(&chunk[..n]);
                let excess = rolling.len().saturating_sub(tail_bytes as usize);
                rolling.drain(..excess);
            }
            None => {
                all.extend_from_slice(&chunk[..n]);
                if all.len() as u64 > cap {
                    let head = all[..(head_bytes as usize).min(all.len())].to_vec();
                    let keep = all.len().saturating_sub(tail_bytes as usize);
                    let rolling = all.drain(keep..).collect();
                    all = Vec::new();
                    tail = Some((head, rolling));
                }
            }
        }
    }
    match tail {
        Some((head, rolling)) => {
            let rolling: Vec<u8> = rolling.into();
            Ok((window_lines(&head, &rolling), true))
        }
        None => Ok((
            String::from_utf8_lossy(&all)
                .lines()
                .map(str::to_owned)
                .collect(),
            false,
        )),
    }
}

/// Lines of a head and a tail cut at byte boundaries, dropping the partial
/// line at each cut.
fn window_lines(head: &[u8], tail: &[u8]) -> Vec<String> {
    let head_s = String::from_utf8_lossy(head);
    let tail_s = String::from_utf8_lossy(tail);
    let mut lines: Vec<String> = Vec::new();
    // Head: drop the last line (cut mid-line at the byte boundary).
    let mut hl: Vec<&str> = head_s.lines().collect();
    if hl.len() > 1 {
        hl.pop();
    }
    lines.extend(hl.iter().map(|s| s.to_string()));
    // Tail: drop the first line (cut mid-line at the seek boundary).
    let mut tl: Vec<&str> = tail_s.lines().collect();
    if tl.len() > 1 {
        tl.remove(0);
    }
    lines.extend(tl.iter().map(|s| s.to_string()));
    lines
}

/// Every line of a session file, plain or zstd, one at a time - for reading a
/// file past the byte cap end to end without holding it in memory.
pub fn for_each_line(path: &Path, mut f: impl FnMut(&str)) -> Result<()> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut reader: Box<dyn BufRead> = if path.extension().is_some_and(|x| x == "zst") {
        let decoder = ruzstd::decoding::StreamingDecoder::new(BufReader::new(file))
            .map_err(|e| anyhow::anyhow!("decompress {}: {e}", path.display()))?;
        Box::new(BufReader::new(decoder))
    } else {
        Box::new(BufReader::new(file))
    };
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        f(&String::from_utf8_lossy(&line));
    }
}

/// The last `max_bytes` of a file as whole lines, newest last.
///
/// For a file whose tail is the part that matters and whose size is another
/// program's business. A partial line at the seek boundary is dropped, so every
/// line handed back is one the writer finished.
pub fn read_tail(path: &Path, max_bytes: u64) -> Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len <= max_bytes {
        let mut s = String::new();
        f.read_to_string(&mut s)?;
        return Ok(s);
    }
    let n = max_bytes as usize;
    let mut buf = vec![0u8; n];
    f.seek(SeekFrom::End(-(n as i64)))?;
    f.read_exact(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    // Drop the line the seek cut in half.
    Ok(match text.find('\n') {
        Some(i) => text[i + 1..].to_string(),
        None => String::new(),
    })
}

/// Read a file to a string, refusing anything over [`MAX_SESSION_FILE_BYTES`]
/// so a malicious or corrupt session file can't OOM the process.
pub fn read_to_string_capped(path: &Path) -> Result<String> {
    read_capped(path, MAX_SESSION_FILE_BYTES)
}

/// The same with the cap given, so the refusal itself can be tested without
/// writing a quarter of a gigabyte to disk.
fn read_capped(path: &Path, cap: u64) -> Result<String> {
    let len = std::fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    if len > cap {
        bail!(
            "{} is {} - over the {} cap; skipping",
            path.display(),
            human_size(len),
            human_size(cap)
        );
    }
    std::fs::read_to_string(path).with_context(|| format!("open {}", path.display()))
}

/// Stable short id from a path string. FNV-1a is implemented by hand because
/// std's DefaultHasher is not guaranteed stable across Rust releases.
pub fn short_id(s: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")[..12].to_string()
}

/// Normalize text to Unicode NFC before it enters or queries the index.
///
/// The FTS5 trigram tokenizer windows over raw bytes, so the same grapheme in
/// two normalization forms never matches: macOS stores Hangul as NFD
/// (decomposed jamo - "회사" as combining scalars) while a typed query is NFC,
/// so without this an NFD-stored Korean session is invisible to an NFC search,
/// and the `trace` suffix match misses the same way. Normalizing both the
/// indexed text and the query to NFC makes them line up. Pure ASCII is already
/// NFC, so this is a cheap near-no-op for English; the cost is per-message and
/// negligible next to parsing and the SQLite write.
pub fn nfc(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.nfc().collect()
}

/// A project path as the index stores it: NFC, and spelled as on disk.
///
/// On a case-insensitive filesystem (WSL's /mnt/<drive>, macOS by default) a
/// shell can `cd` into the same directory as `apo` or `APO`, and each tool
/// records what it was given, so one project split into case-variant rows. A
/// component is respelled only when the path exists, its parent does not list
/// that exact name, and exactly one entry matches it ignoring case. A
/// case-sensitive filesystem never reaches that: a variant spelling there does
/// not exist. Results are cached; listing a drvfs directory is slow.
pub fn project_key(project: &str) -> String {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let normalized = nfc(project);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&normalized).cloned()) {
        return hit;
    }
    let keyed = on_disk_case(&normalized).unwrap_or_else(|| normalized.clone());
    if let Ok(mut c) = cache.lock() {
        c.insert(normalized, keyed.clone());
    }
    keyed
}

fn on_disk_case(path: &str) -> Option<String> {
    use std::path::{Component, Path, PathBuf};
    let p = Path::new(path);
    if !p.is_absolute() || !p.exists() {
        return None;
    }
    let mut components = p.components();
    if components.next() != Some(Component::RootDir) {
        return None;
    }
    let mut out = PathBuf::from("/");
    for component in components {
        let Component::Normal(name) = component else {
            return None;
        };
        let names: Vec<std::ffi::OsString> = std::fs::read_dir(&out)
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.file_name()))
            .collect();
        if names.iter().any(|n| n == name) {
            out.push(name);
            continue;
        }
        let wanted = name.to_string_lossy().to_lowercase();
        let mut matches = names
            .iter()
            .filter(|n| n.to_string_lossy().to_lowercase() == wanted);
        match (matches.next(), matches.next()) {
            (Some(one), None) => out.push(one),
            _ => out.push(name),
        }
    }
    let respelled = out.to_string_lossy().into_owned();
    // Keep a trailing slash if the caller had one.
    Some(if path.ends_with('/') && !respelled.ends_with('/') {
        respelled + "/"
    } else {
        respelled
    })
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

pub fn fmt_date(ts: Option<DateTime<Utc>>) -> String {
    match ts {
        Some(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        None => "-".into(),
    }
}

pub fn rel_time(ts: Option<DateTime<Utc>>) -> String {
    let Some(t) = ts else { return "-".into() };
    let secs = (Utc::now() - t).num_seconds().max(0);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86399 => format!("{}h ago", secs / 3600),
        86400..=2591999 => format!("{}d ago", secs / 86400),
        _ => t.format("%Y-%m-%d").to_string(),
    }
}

pub fn truncate(s: &str, max: usize) -> String {
    // Session titles (and other indexed strings) are untrusted input: a planted
    // or prompt-poisoned session can set any title, and this is the choke point
    // that renders titles to the terminal across list/search/trace/resume/blame.
    // Strip control characters so a title can't smuggle ANSI/terminal-control
    // escapes into our output. \n and \t become spaces; every other C0 control,
    // DEL, and the C1 range is dropped (same posture as clean_snippet).
    let clean: String = s
        .chars()
        .filter_map(|c| match c {
            '\n' | '\t' => Some(' '),
            c if (c as u32) < 0x20 || c == '\u{7f}' || ('\u{80}'..='\u{9f}').contains(&c) => None,
            c => Some(c),
        })
        .collect();
    let clean = clean.trim();
    if clean.chars().count() <= max {
        clean.to_string()
    } else {
        let cut: String = clean.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}\u{2026}")
    }
}

// Minimal ANSI helpers. Respect NO_COLOR and non-tty stdout.
pub fn color_enabled() -> bool {
    use std::io::IsTerminal;
    std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
}

pub fn paint(code: &str, s: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String {
    paint("1", s)
}
pub fn dim(s: &str) -> String {
    paint("2", s)
}
pub fn cyan(s: &str) -> String {
    paint("36", s)
}
pub fn yellow(s: &str) -> String {
    paint("33", s)
}
pub fn green(s: &str) -> String {
    paint("32", s)
}

#[cfg(test)]
mod tests {
    use super::{decoded_lines_windowed, session_lines_windowed, truncate};

    #[test]
    fn truncate_strips_control_characters() {
        // ESC, DEL, and C1 controls are dropped so an untrusted title cannot
        // inject terminal escape sequences; the remaining literal text stays.
        let title = "ok\u{1b}[31mred\u{7f}\u{9b}end";
        let out = truncate(title, 100);
        assert!(!out.contains('\u{1b}'), "ESC must be stripped");
        assert!(!out.contains('\u{7f}'), "DEL must be stripped");
        assert!(!out.contains('\u{9b}'), "C1 must be stripped");
        assert_eq!(out, "ok[31mredend");
    }

    #[test]
    fn truncate_collapses_whitespace_controls() {
        assert_eq!(truncate("a\nb\tc", 100), "a b c");
    }

    #[test]
    fn truncate_adds_ellipsis_when_too_long() {
        assert_eq!(truncate("abcdef", 4), "abc\u{2026}");
    }

    #[test]
    fn truncate_keeps_unicode_titles() {
        assert_eq!(truncate("한국어 검색", 100), "한국어 검색");
    }

    #[test]
    fn windows_an_over_cap_session_to_head_and_tail() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jsonl");
        // 10 lines of "lineNN\n" (7 bytes each = 70 bytes total).
        let content: String = (0..10).map(|i| format!("line{i:02}\n")).collect();
        std::fs::write(&p, &content).unwrap();
        // Under the cap: every line, not windowed.
        let (all, w) = session_lines_windowed(&p, 10_000, 22, 22).unwrap();
        assert_eq!(all.len(), 10);
        assert!(!w);
        // Over the cap with small budgets: head + tail only, middle dropped.
        let (win, w2) = session_lines_windowed(&p, 30, 22, 22).unwrap();
        assert!(w2, "flagged windowed");
        assert!(win.iter().any(|l| l == "line00"), "head kept: {win:?}");
        assert!(win.iter().any(|l| l == "line09"), "tail kept: {win:?}");
        assert!(
            !win.iter().any(|l| l == "line05"),
            "middle dropped: {win:?}"
        );
    }

    /// A stream far past the cap keeps only its head and a bounded tail, not
    /// everything read after the cap was crossed.
    #[test]
    fn a_long_stream_keeps_only_a_bounded_tail() {
        let content: String = (0..20_000).map(|i| format!("line{i:05}\n")).collect();
        let (win, windowed) =
            decoded_lines_windowed(std::io::Cursor::new(content), 30, 22, 22).unwrap();
        assert!(windowed);
        assert_eq!(win.first().map(String::as_str), Some("line00000"));
        assert_eq!(win.last().map(String::as_str), Some("line19999"));
        assert!(win.len() <= 6, "{} lines kept", win.len());
    }

    /// Codex compresses older rollouts in place (`rollout-*.jsonl.zst`).
    #[test]
    fn a_zstd_session_reads_as_its_lines_and_windows_on_its_decoded_size() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jsonl.zst");
        let content: String = (0..10).map(|i| format!("line{i:02}\n")).collect();
        std::fs::write(
            &p,
            ruzstd::encoding::compress_to_vec(
                content.as_bytes(),
                ruzstd::encoding::CompressionLevel::Fastest,
            ),
        )
        .unwrap();
        let (all, w) = session_lines_windowed(&p, 10_000, 22, 22).unwrap();
        assert_eq!(all.len(), 10, "{all:?}");
        assert_eq!(all[9], "line09");
        assert!(!w);
        let (win, w2) = session_lines_windowed(&p, 30, 22, 22).unwrap();
        assert!(w2, "flagged windowed");
        assert!(win.iter().any(|l| l == "line00"), "head kept: {win:?}");
        assert!(win.iter().any(|l| l == "line09"), "tail kept: {win:?}");
        assert!(
            !win.iter().any(|l| l == "line05"),
            "middle dropped: {win:?}"
        );
    }
}

#[cfg(test)]
mod cap_tests {
    use super::*;

    /// The cap exists so a corrupt or hostile session file cannot exhaust
    /// memory. Eleven adapters went through it; the prodex one read its task
    /// JSON and its artifact with a plain `fs::read`, under a comment claiming
    /// the read was bounded.
    #[test]
    fn a_file_over_the_cap_is_refused_by_name_and_size() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("huge.json");
        std::fs::write(&p, b"{}").unwrap();
        // Cheap: assert the guard reads the SIZE, by capping at zero.
        let err = read_capped(&p, 0).unwrap_err().to_string();
        assert!(err.contains("over the"), "should name the cap: {err}");
        assert!(err.contains("huge.json"), "should name the file: {err}");
    }

    #[test]
    fn a_file_under_the_cap_reads_whole() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("small.json");
        std::fs::write(&p, b"{\"id\":\"x\"}").unwrap();
        assert_eq!(read_capped(&p, 1024).unwrap(), "{\"id\":\"x\"}");
    }

    #[test]
    fn read_tail_returns_whole_lines_from_the_end() {
        let p = std::env::temp_dir().join(format!("sw-tail-{}", std::process::id()));
        std::fs::write(&p, "first\nsecond\nthird\n").unwrap();
        // Under the cap: the whole file, byte for byte.
        assert_eq!(read_tail(&p, 1000).unwrap(), "first\nsecond\nthird\n");
        // Over it: only whole lines, and never the one the seek cut in half.
        // 12 bytes from the end is "ond\nthird\n" plus part of "sec".
        assert_eq!(read_tail(&p, 12).unwrap(), "third\n");
        // A cap too small to hold even one full line yields nothing, not a
        // fragment that would parse as a different record.
        assert_eq!(read_tail(&p, 3).unwrap(), "");
        let _ = std::fs::remove_file(&p);
    }
}
