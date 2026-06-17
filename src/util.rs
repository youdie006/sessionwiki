use chrono::{DateTime, Utc};

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
    let clean: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
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
