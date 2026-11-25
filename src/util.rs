use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
use ruzstd::decoding::{BlockDecodingStrategy, FrameDecoder, StreamingDecoder};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
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
/// even a 256 MB+ plain file is read cheaply (no full scan). `.zst` files
/// use their first frame and `.zstd` files use concatenated decoding.
pub fn session_lines(path: &Path) -> Result<(Vec<String>, bool)> {
    let (cap, head, tail) = window_limits();
    session_lines_windowed(path, cap, head, tail, false)
}

/// Like [`session_lines`], but also reads concatenated frames from `.zst`
/// files. `.zstd` files use concatenated decoding in either reader.
/// A torn trailing frame ends the stream at the last complete line.
pub fn session_lines_concat_zstd(path: &Path) -> Result<(Vec<String>, bool)> {
    let (cap, head, tail) = window_limits();
    session_lines_windowed(path, cap, head, tail, true)
}

/// Most text a windowed read keeps from the middle of a file. Conversation is a
/// small share of a large session - its bulk is tool output - so this is a
/// ceiling against a hostile file, not a budget a real one reaches.
const MIDDLE_KEPT_BYTES: u64 = 64 * 1024 * 1024;

/// [`session_lines`], keeping from the middle of a windowed file the lines
/// `keep` accepts - an adapter's conversation, without its tool output - so a
/// session over the cap is searchable for what was said between its first
/// and last turns. The whole file is read, a line at a time.
pub fn session_lines_keeping(
    path: &Path,
    keep: impl Fn(&str) -> bool,
) -> Result<(Vec<String>, bool)> {
    let (cap, head, tail) = window_limits();
    lines_keeping(path, cap, head, tail, MIDDLE_KEPT_BYTES, keep)
}

fn lines_keeping(
    path: &Path,
    cap: u64,
    head_bytes: u64,
    tail_bytes: u64,
    middle_bytes: u64,
    keep: impl Fn(&str) -> bool,
) -> Result<(Vec<String>, bool)> {
    let mut all: Vec<String> = Vec::new();
    let mut total = 0u64;
    // A plain file's size is known before reading, so one over the cap is
    // windowed from its first line instead of being buffered up to the cap; a
    // compressed one is measured as it decodes.
    let mut windowed = !is_zstd(path) && std::fs::metadata(path).is_ok_and(|m| m.len() > cap);
    let mut head: Vec<String> = Vec::new();
    let mut middle: Vec<String> = Vec::new();
    let mut middle_used = 0u64;
    let mut tail: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let mut tail_used = 0u64;
    let mut head_used = 0u64;
    // Once over the cap: a line goes to the head while it fits, then through
    // the tail, and a line leaving the tail survives only if it is kept.
    let mut place = |line: String,
                     head: &mut Vec<String>,
                     middle: &mut Vec<String>,
                     tail: &mut std::collections::VecDeque<String>| {
        let n = line.len() as u64 + 1;
        if head_used + n <= head_bytes && middle.is_empty() && tail.is_empty() {
            head_used += n;
            head.push(line);
            return;
        }
        tail_used += n;
        tail.push_back(line);
        while tail_used > tail_bytes {
            let Some(old) = tail.pop_front() else { break };
            let m = old.len() as u64 + 1;
            tail_used -= m;
            if middle_used + m <= middle_bytes && keep(&old) {
                middle_used += m;
                middle.push(old);
            }
        }
    };
    for_each_line(path, |raw| {
        let line = raw.trim_end_matches(['\n', '\r']).to_owned();
        total += raw.len() as u64;
        if !windowed {
            all.push(line);
            if total > cap {
                windowed = true;
                for line in std::mem::take(&mut all) {
                    place(line, &mut head, &mut middle, &mut tail);
                }
            }
        } else {
            place(line, &mut head, &mut middle, &mut tail);
        }
    })?;
    if !windowed {
        return Ok((all, false));
    }
    head.extend(middle);
    head.extend(tail);
    Ok((head, true))
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
    concat_zstd: bool,
) -> Result<(Vec<String>, bool)> {
    let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    if is_zstd(path) {
        return zstd_reader(path, BufReader::new(f), concat_zstd)
            .and_then(|stream| decoded_lines_windowed(stream, cap, head_bytes, tail_bytes))
            .with_context(|| format!("decompress {}", path.display()));
    }
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len <= cap {
        let mut lines = Vec::new();
        read_lines(BufReader::new(f), |line| {
            let line = line
                .strip_suffix('\n')
                .map_or(line, |line| line.strip_suffix('\r').unwrap_or(line));
            lines.push(line.to_owned());
        })
        .with_context(|| format!("read {}", path.display()))?;
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
/// a rolling last `tail_bytes`. A stream over the cap is reported as windowed
/// even when a torn frame leaves every complete line in the window.
fn decoded_lines_windowed(
    mut stream: impl std::io::Read,
    cap: u64,
    head_bytes: u64,
    tail_bytes: u64,
) -> io::Result<(Vec<String>, bool)> {
    let mut all = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    let mut tail: Option<(Vec<u8>, DecodedTail)> = None;
    let torn = loop {
        let n = match stream.read(&mut chunk) {
            Ok(0) => break false,
            Ok(n) => n,
            Err(error) if is_torn_frame(&error) => break true,
            Err(error) => return Err(error),
        };
        match &mut tail {
            Some((_, rolling)) => rolling.push(&chunk[..n]),
            None => {
                all.extend_from_slice(&chunk[..n]);
                if all.len() as u64 > cap {
                    let head = all[..(head_bytes as usize).min(all.len())].to_vec();
                    let mut rolling = DecodedTail::new(tail_bytes as usize);
                    rolling.push(&all);
                    all = Vec::new();
                    tail = Some((head, rolling));
                }
            }
        }
    };
    match tail {
        Some((mut head, rolling)) => {
            if torn && !head.contains(&b'\n') {
                head.clear();
            }
            let tail = rolling.finish(torn);
            Ok((
                window_records(
                    &head,
                    &tail.bytes,
                    Some((tail.first_line, tail.starts_line)),
                ),
                true,
            ))
        }
        None => {
            if torn {
                all.truncate(last_line_end(&all));
            }
            Ok((
                String::from_utf8_lossy(&all)
                    .lines()
                    .map(str::to_owned)
                    .collect(),
                false,
            ))
        }
    }
}

fn last_line_end(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(0, |i| i + 1)
}

/// Keep the latest complete lines available even if a long partial line must
/// be discarded after a torn frame. Each buffer is bounded by the tail budget.
/// It also counts bytes and lines so the kept tail can be placed in the stream.
struct DecodedTail {
    complete: VecDeque<u8>,
    partial: VecDeque<u8>,
    budget: usize,
    /// Newlines pushed.
    lines: usize,
    /// The last byte dropped from the front of `complete`, if any.
    dropped: Option<u8>,
    /// The last byte dropped from the front of `partial`, if any.
    partial_dropped: Option<u8>,
}

/// The kept tail and where it sits in the stream.
struct TailWindow {
    bytes: Vec<u8>,
    /// Index of the first line in `bytes`, counted from the start of the stream.
    first_line: usize,
    /// Whether `bytes` begins at a line start, so its first line is whole
    /// rather than cut at the tail budget.
    starts_line: bool,
}

impl DecodedTail {
    fn new(budget: usize) -> Self {
        Self {
            complete: VecDeque::new(),
            partial: VecDeque::new(),
            budget,
            lines: 0,
            dropped: None,
            partial_dropped: None,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        let (complete, partial) = bytes.split_at(last_line_end(bytes));
        if !complete.is_empty() {
            self.lines += complete.iter().filter(|&&byte| byte == b'\n').count();
            let skip = complete.len().saturating_sub(self.budget);
            if skip > 0 {
                self.complete.clear();
                self.partial.clear();
                self.partial_dropped = None;
                self.dropped = Some(complete[skip - 1]);
                self.complete.extend(&complete[skip..]);
            } else {
                if let Some(byte) = self.partial_dropped.take() {
                    self.complete.clear();
                    self.dropped = Some(byte);
                }
                self.complete.append(&mut self.partial);
                self.complete.extend(complete);
                self.trim_complete();
            }
        }
        let pskip = partial.len().saturating_sub(self.budget);
        if pskip > 0 {
            self.partial.clear();
            self.partial_dropped = Some(partial[pskip - 1]);
            self.partial.extend(&partial[pskip..]);
        } else {
            self.partial.extend(partial);
            let excess = self.partial.len().saturating_sub(self.budget);
            if excess > 0 {
                self.partial_dropped = self.partial.drain(..excess).next_back();
            }
        }
    }

    fn trim_complete(&mut self) {
        let excess = self.complete.len().saturating_sub(self.budget);
        if excess > 0 {
            self.dropped = self.complete.drain(..excess).next_back();
        }
    }

    fn finish(mut self, torn: bool) -> TailWindow {
        if !torn {
            if let Some(byte) = self.partial_dropped.take() {
                self.complete.clear();
                self.dropped = Some(byte);
            }
            self.complete.append(&mut self.partial);
            self.trim_complete();
        }
        let bytes: Vec<u8> = self.complete.into();
        // `bytes` ends at the stream's last kept newline or at its end, so
        // every newline after its start is in it.
        let newlines = bytes.iter().filter(|&&byte| byte == b'\n').count();
        TailWindow {
            starts_line: matches!(self.dropped, None | Some(b'\n')),
            first_line: self.lines - newlines,
            bytes,
        }
    }
}

/// Lines of a head and a tail cut at byte boundaries, dropping the partial
/// line at each cut.
fn window_lines(head: &[u8], tail: &[u8]) -> Vec<String> {
    window_records(head, tail, None)
}

/// [`window_lines`] for a tail whose place in the stream is known as
/// `(index of its first line, whether that line is whole)`. A tail
/// line the head already kept is not repeated, which happens when a torn frame
/// shrinks the tail back to the start of the stream.
fn window_records(head: &[u8], tail: &[u8], tail_start: Option<(usize, bool)>) -> Vec<String> {
    let head_s = String::from_utf8_lossy(head);
    let tail_s = String::from_utf8_lossy(tail);
    let mut lines: Vec<String> = Vec::new();
    // Head: drop the last line (cut mid-line at the byte boundary).
    let mut hl: Vec<&str> = head_s.lines().collect();
    if hl.len() > 1 {
        hl.pop();
    }
    let head_lines = hl.len();
    lines.extend(hl.iter().map(|s| s.to_string()));
    // Tail: drop the first line (cut mid-line at the seek boundary).
    let mut tl: Vec<&str> = tail_s.lines().collect();
    let (first_line, drop_first) = match tail_start {
        Some((first_line, whole)) => (first_line, !whole && !tl.is_empty()),
        None => (head_lines, tl.len() > 1),
    };
    let first_line = if drop_first {
        tl.remove(0);
        first_line + 1
    } else {
        first_line
    };
    let already_kept = head_lines.saturating_sub(first_line).min(tl.len());
    lines.extend(tl[already_kept..].iter().map(|s| s.to_string()));
    lines
}

/// Every line of a session file, plain or zstd, one at a time - for reading a
/// file past the byte cap end to end without holding it in memory. `.zst`
/// files use their first frame and `.zstd` files use concatenated decoding.
pub fn for_each_line(path: &Path, f: impl FnMut(&str)) -> Result<()> {
    for_each_line_with_zstd(path, false, f)
}

/// Like [`for_each_line`], but also reads concatenated frames from `.zst`
/// files. `.zstd` files use concatenated decoding in either reader.
/// A torn trailing frame ends the stream at the last complete line.
pub fn for_each_line_concat_zstd(path: &Path, f: impl FnMut(&str)) -> Result<()> {
    for_each_line_with_zstd(path, true, f)
}

fn for_each_line_with_zstd(path: &Path, concat_zstd: bool, f: impl FnMut(&str)) -> Result<()> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let reader = BufReader::new(file);
    if is_zstd(path) {
        zstd_reader(path, reader, concat_zstd)
            .and_then(|stream| read_lines(BufReader::new(stream), f))
            .with_context(|| format!("decompress {}", path.display()))
    } else {
        read_lines(reader, f).with_context(|| format!("read {}", path.display()))
    }
}

fn read_lines(mut reader: impl BufRead, mut f: impl FnMut(&str)) -> io::Result<()> {
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => return Ok(()),
            Ok(_) => f(&String::from_utf8_lossy(&line)),
            // read_until may have accumulated a partial line before this error.
            Err(error) if is_torn_frame(&error) => return Ok(()),
            Err(error) => return Err(error),
        }
    }
}

fn is_zstd(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext == "zst" || ext == "zstd")
}

fn zstd_reader(
    path: &Path,
    source: BufReader<File>,
    concat_zstd: bool,
) -> io::Result<Box<dyn Read>> {
    if concat_zstd || path.extension().is_some_and(|ext| ext == "zstd") {
        Ok(Box::new(ZstdFrames::new(source)))
    } else {
        // Codex rollouts historically ignore all bytes after their first frame.
        Ok(Box::new(
            StreamingDecoder::new(source).map_err(io::Error::other)?,
        ))
    }
}

#[derive(Debug)]
struct TornZstdFrame;

impl std::fmt::Display for TornZstdFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("incomplete trailing zstd frame")
    }
}

impl std::error::Error for TornZstdFrame {}

fn is_torn_frame(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|source| source.is::<TornZstdFrame>())
}

/// A single decoder is reset at each frame boundary, reusing its allocation
/// and keeping the buffered source. Report a typed end marker so the line
/// readers can distinguish a torn stream from ordinary EOF.
struct ZstdFrames<R> {
    source: R,
    decoder: FrameDecoder,
    completed_frame: bool,
    pending_torn: bool,
}

impl<R: Read> ZstdFrames<R> {
    fn new(source: R) -> Self {
        Self {
            source,
            decoder: FrameDecoder::new(),
            completed_frame: false,
            pending_torn: false,
        }
    }

    fn frame_error(&self, error: FrameDecoderError) -> io::Error {
        // Transport EOF is nested under the header, block, or checksum error.
        // Decoder errors such as invalid magic or reserved block types are not EOF.
        let error_chain = std::iter::successors(
            Some(&error as &(dyn std::error::Error + 'static)),
            |error| error.source(),
        );
        if self.completed_frame
            && error_chain
                .filter_map(|error| error.downcast_ref::<io::Error>())
                .any(|error| error.kind() == io::ErrorKind::UnexpectedEof)
        {
            io::Error::other(TornZstdFrame)
        } else {
            io::Error::other(error)
        }
    }

    fn start_frame(&mut self) -> io::Result<bool> {
        loop {
            // A short magic read is a torn header only if the bytes are a
            // prefix of a zstd magic number. Replay the complete magic into ruzstd.
            let mut magic = [0; 4];
            let n =
                io::copy(&mut self.source.by_ref().take(4), &mut magic.as_mut_slice())? as usize;
            if n < magic.len() {
                if self.completed_frame && n == 0 {
                    return Ok(false);
                }
                let prefix = &magic[..n];
                let valid_prefix = [0x28, 0xb5, 0x2f, 0xfd].starts_with(prefix)
                    || ((0x50..=0x5f).contains(&magic[0])
                        && [0x2a, 0x4d, 0x18].starts_with(&prefix[1..]));
                return Err(if !valid_prefix {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid zstd magic number")
                } else if self.completed_frame {
                    io::Error::other(TornZstdFrame)
                } else {
                    io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "incomplete first zstd frame header",
                    )
                });
            }
            match self
                .decoder
                .reset(io::Cursor::new(magic).chain(&mut self.source))
            {
                Ok(()) => return Ok(true),
                Err(FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::SkipFrame {
                    length,
                    ..
                })) => {
                    let length = u64::from(length);
                    if io::copy(&mut self.source.by_ref().take(length), &mut io::sink())? < length {
                        return Err(if self.completed_frame {
                            io::Error::other(TornZstdFrame)
                        } else {
                            io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "incomplete skippable zstd frame",
                            )
                        });
                    }
                }
                Err(error) => return Err(self.frame_error(error)),
            }
        }
    }
}

impl<R: Read> Read for ZstdFrames<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.decoder.can_collect() > 0 {
                return self.decoder.read(buf);
            }
            if self.pending_torn {
                return Err(io::Error::other(TornZstdFrame));
            }
            if self.decoder.is_finished() && !self.start_frame()? {
                return Ok(0);
            }
            match self
                .decoder
                .decode_blocks(&mut self.source, BlockDecodingStrategy::UptoBlocks(1))
            {
                Ok(_) => self.completed_frame |= self.decoder.is_finished(),
                Err(error) => {
                    // The decoder holds back one window of decoded bytes until
                    // its frame finishes. A frame torn at a block header, block
                    // body or checksum would strand those bytes, so finish the
                    // frame with the bytes the stream lacks. The torn block
                    // itself added nothing: ruzstd advances its buffer only
                    // after a block body is read in full.
                    let finisher: &[u8] = match &error {
                        FrameDecoderError::FailedToReadBlockHeader(_)
                        | FrameDecoderError::FailedToReadBlockBody(_) => {
                            // An empty final raw block, then a placeholder
                            // checksum that a frame without one never reads.
                            &[0x01, 0x00, 0x00, 0, 0, 0, 0]
                        }
                        FrameDecoderError::FailedToReadChecksum(_) => &[0; 4],
                        _ => &[],
                    };
                    let error = self.frame_error(error);
                    if !is_torn_frame(&error) {
                        return Err(error);
                    }
                    self.decoder
                        .decode_from_to(finisher, &mut [])
                        .map_err(io::Error::other)?;
                    // The stream still ends as torn, after the released bytes.
                    self.pending_torn = true;
                }
            }
        }
    }
}

/// The last `max_bytes` of a file as whole lines, newest last.
///
/// For a file whose tail is the part that matters and whose size is another
/// program's business. A partial line at the seek boundary is dropped, so every
/// line handed back is one the writer finished.
pub fn read_tail(path: &Path, max_bytes: u64) -> Result<String> {
    let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
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
/// `std::fs::canonicalize`, spelled as tools record a folder. On Windows it
/// answers `\\?\C:\...`, a spelling no tool records, so it never matched a
/// recorded project or the folder a session is filed under.
pub fn canonicalize_as_recorded(path: impl AsRef<Path>) -> std::io::Result<std::path::PathBuf> {
    let canon = std::fs::canonicalize(path)?;
    if cfg!(windows) {
        let s = canon.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            return Ok(format!(r"\\{rest}").into());
        }
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return Ok(rest.into());
        }
    }
    Ok(canon)
}

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
    use super::*;

    #[test]
    fn a_canonical_folder_is_spelled_as_tools_record_it() {
        let dir = tempfile::tempdir().unwrap();
        let got = super::canonicalize_as_recorded(dir.path()).unwrap();
        assert!(!got.to_string_lossy().starts_with(r"\\?\"), "{got:?}");
        assert_eq!(
            std::fs::canonicalize(&got).unwrap(),
            std::fs::canonicalize(dir.path()).unwrap()
        );
    }

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
        let (all, w) = session_lines_windowed(&p, 10_000, 22, 22, false).unwrap();
        assert_eq!(all.len(), 10);
        assert!(!w);
        // Over the cap with small budgets: head + tail only, middle dropped.
        let (win, w2) = session_lines_windowed(&p, 30, 22, 22, false).unwrap();
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

    /// Over the cap, the lines a reader asks to keep survive from the middle,
    /// in order, once each; everything else in the middle is dropped.
    #[test]
    fn a_windowed_read_keeps_the_middle_lines_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jsonl");
        let content: String = (0..100)
            .map(|i| {
                if i % 10 == 5 {
                    format!("keep{i:03}\n")
                } else {
                    format!("drop{i:03}\n")
                }
            })
            .collect();
        std::fs::write(&p, &content).unwrap();
        let (lines, windowed) =
            lines_keeping(&p, 100, 24, 24, 1 << 20, |l: &str| l.starts_with("keep")).unwrap();
        assert!(windowed);
        assert_eq!(
            &lines[..3],
            ["drop000", "drop001", "drop002"],
            "head: {lines:?}"
        );
        let middle: Vec<&str> = lines[3..lines.len() - 3]
            .iter()
            .map(String::as_str)
            .collect();
        let kept: Vec<String> = (0..10).map(|i| format!("keep{:03}", i * 10 + 5)).collect();
        assert_eq!(middle, kept, "{lines:?}");
        assert_eq!(&lines[lines.len() - 3..], ["drop097", "drop098", "drop099"]);
        // Under the cap nothing is dropped.
        let (all, windowed) = lines_keeping(&p, 10_000, 24, 24, 1 << 20, |_: &str| false).unwrap();
        assert!(!windowed);
        assert_eq!(all.len(), 100);
        // The middle is bounded.
        let (bounded, _) =
            lines_keeping(&p, 100, 24, 24, 20, |l: &str| l.starts_with("keep")).unwrap();
        assert_eq!(bounded.len(), 3 + 2 + 3, "{bounded:?}");
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
        let (all, w) = session_lines_windowed(&p, 10_000, 22, 22, false).unwrap();
        assert_eq!(all.len(), 10, "{all:?}");
        assert_eq!(all[9], "line09");
        assert!(!w);
        let (win, w2) = session_lines_windowed(&p, 30, 22, 22, false).unwrap();
        assert!(w2, "flagged windowed");
        assert!(win.iter().any(|l| l == "line00"), "head kept: {win:?}");
        assert!(win.iter().any(|l| l == "line09"), "tail kept: {win:?}");
        assert!(
            !win.iter().any(|l| l == "line05"),
            "middle dropped: {win:?}"
        );
    }

    #[test]
    fn a_small_plain_session_continues_after_invalid_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversation.jsonl");
        std::fs::write(&path, b"start\r\nbad: \xff\r\nafter\nclean final line\r").unwrap();
        let (lines, windowed) = session_lines(&path).unwrap();
        assert!(!windowed);
        assert_eq!(
            lines,
            ["start", "bad: \u{fffd}", "after", "clean final line\r"]
        );
    }

    #[test]
    fn a_small_plain_session_decodes_a_torn_final_character() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversation.jsonl");
        std::fs::write(&path, b"start\nlast: \xf0\x9f\x92").unwrap();
        assert_eq!(
            session_lines(&path).unwrap(),
            (vec!["start".into(), "last: \u{fffd}".into()], false)
        );
    }

    struct FailingSource<R>(R);

    impl<R: Read> Read for FailingSource<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.0.read(buf)? {
                0 => Err(io::Error::other("source read failed")),
                n => Ok(n),
            }
        }
    }

    #[test]
    fn the_plain_line_loop_propagates_a_read_error() {
        let source = FailingSource(&b"valid first line\npartial"[..]);
        let mut lines = Vec::new();
        let error =
            read_lines(BufReader::new(source), |line| lines.push(line.to_owned())).unwrap_err();
        assert_eq!(lines, ["valid first line\n"]);
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(error.to_string(), "source read failed");
    }

    fn zstd_frame(bytes: &[u8]) -> Vec<u8> {
        use ruzstd::encoding::{compress_to_vec, CompressionLevel};
        compress_to_vec(bytes, CompressionLevel::Fastest)
    }

    #[test]
    fn default_zst_readers_ignore_everything_after_the_first_frame() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-example.jsonl.zst");
        let first_frame = zstd_frame(b"first\r\nfinal");
        let next_frame = zstd_frame(b"ignore this frame\n");
        for suffix in [b"trailing junk".as_slice(), &next_frame[..2], &next_frame] {
            std::fs::write(&path, [first_frame.as_slice(), suffix].concat()).unwrap();
            assert_eq!(
                session_lines(&path).unwrap(),
                (vec!["first".into(), "final".into()], false)
            );
            assert_eq!(
                session_lines_keeping(&path, |_| true).unwrap(),
                session_lines(&path).unwrap()
            );
            let mut lines = Vec::new();
            for_each_line(&path, |line| lines.push(line.to_owned())).unwrap();
            assert_eq!(lines, ["first\r\n", "final"]);
        }
    }

    #[test]
    fn concatenated_frames_preserve_lines_across_empty_and_skippable_frames() {
        let dir = tempfile::tempdir().unwrap();
        let frames = [
            zstd_frame(b"first\nsec"),
            zstd_frame(b""),
            // Skippable frame with a one-byte payload.
            vec![0x50, 0x2a, 0x4d, 0x18, 1, 0, 0, 0, 42],
            zstd_frame(b"ond\r\nthird"),
        ]
        .concat();
        for suffix in ["zst", "zstd"] {
            let path = dir.path().join(format!("session.jsonl.{suffix}"));
            std::fs::write(&path, &frames).unwrap();
            let mut raw = Vec::new();
            let (lines, windowed) = if suffix == "zst" {
                for_each_line_concat_zstd(&path, |line| raw.push(line.to_owned())).unwrap();
                session_lines_concat_zstd(&path).unwrap()
            } else {
                for_each_line(&path, |line| raw.push(line.to_owned())).unwrap();
                let result = session_lines(&path).unwrap();
                assert_eq!(session_lines_keeping(&path, |_| true).unwrap(), result);
                result
            };
            assert!(!windowed);
            assert_eq!(lines, ["first", "second", "third"]);
            assert_eq!(raw, ["first\n", "second\r\n", "third"]);
        }
    }

    fn push_raw_block(frame: &mut Vec<u8>, size: u32, last: bool, bytes: &[u8]) {
        let header = (size << 3) | u32::from(last);
        frame.extend(&header.to_le_bytes()[..3]);
        frame.extend(bytes);
    }

    /// Enough decoded data to emit a complete line and a partial line before
    /// the decoder reaches the truncated final block or checksum.
    fn torn_zstd_body(checksum: bool) -> Vec<u8> {
        // Multi-segment frame with a 1 KiB window and an optional checksum.
        let mut frame = vec![0x28, 0xb5, 0x2f, 0xfd, if checksum { 4 } else { 0 }, 0];
        let mut block = b"second\n".to_vec();
        block.resize(1024, b'x');
        push_raw_block(&mut frame, 1024, false, &block);
        push_raw_block(&mut frame, 1024, false, &[b'x'; 1024]);
        if checksum {
            push_raw_block(&mut frame, 1, true, b"x");
            frame.extend([0x00, 0x00]); // half of the four-byte checksum
        } else {
            push_raw_block(&mut frame, 10, true, b"x"); // nine missing payload bytes
        }
        frame
    }

    #[test]
    fn concatenated_readers_recover_a_torn_trailing_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl.zst");
        let frame = zstd_frame(b"first\n");
        for prefix_len in [2, 5] {
            std::fs::write(&path, [&frame[..], &frame[..prefix_len]].concat()).unwrap();
            assert_eq!(
                session_lines_concat_zstd(&path).unwrap(),
                (vec!["first".into()], false)
            );
            let mut raw = Vec::new();
            for_each_line_concat_zstd(&path, |line| raw.push(line.to_owned())).unwrap();
            assert_eq!(raw, ["first\n"]);
        }
    }

    #[test]
    fn concatenated_readers_discard_a_partial_line_after_payload_or_checksum_eof() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl.zstd");
        for checksum in [false, true] {
            let frames = [zstd_frame(b"first\n"), torn_zstd_body(checksum)].concat();
            std::fs::write(&path, frames).unwrap();
            let (lines, windowed) = session_lines(&path).unwrap();
            assert!(!windowed);
            assert_eq!(lines, ["first", "second"]);
            let mut raw = Vec::new();
            for_each_line(&path, |line| raw.push(line.to_owned())).unwrap();
            assert_eq!(raw, ["first\n", "second\n"]);
            // An emitted fragment far larger than the tail budget cannot push
            // the preceding complete record out of the recovered tail.
            assert_eq!(
                session_lines_windowed(&path, 40, 6, 7, false).unwrap(),
                (vec!["first".into(), "second".into()], true)
            );
        }
    }

    /// Every line `for_each_line` (or its concatenated-`.zst` variant) yields,
    /// without line endings.
    fn trimmed_lines(path: &Path, concat: bool) -> Vec<String> {
        let mut lines = Vec::new();
        let push = |line: &str| lines.push(line.trim_end().to_owned());
        if concat {
            for_each_line_concat_zstd(path, push).unwrap();
        } else {
            for_each_line(path, push).unwrap();
        }
        lines
    }

    /// A second frame with a 1 KiB window, optionally a checksum and a declared
    /// content size, whose first block is non-final and which then ends in
    /// `torn`: the start of a block that never completes.
    fn torn_second_frame(
        checksum: bool,
        content_size: Option<u16>,
        first_block: &[u8],
        torn: &[u8],
    ) -> Vec<u8> {
        let flags = u8::from(checksum) << 2 | u8::from(content_size.is_some()) << 6;
        let mut frame = vec![0x28, 0xb5, 0x2f, 0xfd, flags, 0];
        if let Some(content_size) = content_size {
            frame.extend((content_size - 256).to_le_bytes());
        }
        push_raw_block(&mut frame, first_block.len() as u32, false, first_block);
        frame.extend(torn);
        frame
    }

    /// Everything a frame decoded before its torn block comes out, and
    /// nothing from the torn block: the decoder holds one window of bytes back
    /// until its frame finishes, and that window must be released.
    #[test]
    fn a_torn_block_after_a_complete_one_releases_the_decoded_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.zstd");
        let mut raw_block = Vec::new();
        push_raw_block(&mut raw_block, 10, true, b"thi");
        let torn_ends: [&[u8]; 4] = [
            &raw_block,      // header and 3 of 10 body bytes
            &raw_block[..3], // header only
            &raw_block[..1], // part of a header
            b"",             // the stream ends between blocks
        ];
        for (checksum, content_size) in [
            (false, None),
            (true, None),
            (false, Some(300)),
            (true, Some(300)),
        ] {
            for (first_block, expected) in [
                (&b"second\n"[..], vec!["first", "second"]),
                (b"second\npart", vec!["first", "second"]),
                (b"second\nthird\n", vec!["first", "second", "third"]),
            ] {
                for torn in torn_ends {
                    let second = torn_second_frame(checksum, content_size, first_block, torn);
                    std::fs::write(&path, [zstd_frame(b"first\n"), second].concat()).unwrap();
                    let context = format!(
                        "checksum={checksum} size={content_size:?} block={first_block:?} torn={torn:?}"
                    );
                    let expected_lines: Vec<String> =
                        expected.iter().map(|line| line.to_string()).collect();
                    for result in [
                        session_lines(&path).unwrap(),
                        session_lines_concat_zstd(&path).unwrap(),
                        session_lines_keeping(&path, |_| true).unwrap(),
                    ] {
                        assert_eq!(result, (expected_lines.clone(), false), "{context}");
                    }
                    for concat in [false, true] {
                        assert_eq!(trimmed_lines(&path, concat), expected_lines, "{context}");
                    }
                    // The same stream through the windowed reader, over the cap.
                    let windowed =
                        session_lines_windowed(&path, 8, 1 << 20, 1 << 20, false).unwrap();
                    assert_eq!(windowed, (expected_lines, true), "{context}");
                }
            }
        }
    }

    /// A reproduction from review: a torn raw block after a non-final block
    /// that is `second\n`, a first frame of `first\n`.
    #[test]
    fn a_torn_raw_block_keeps_the_previous_blocks_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.zstd");
        let mut second = vec![0x28, 0xb5, 0x2f, 0xfd, 0, 0];
        push_raw_block(&mut second, 7, false, b"second\n");
        push_raw_block(&mut second, 10, true, b"thi"); // cut after 3 of 10 bytes
        std::fs::write(&path, [zstd_frame(b"first\n"), second].concat()).unwrap();
        let expected = vec!["first".to_owned(), "second".to_owned()];
        assert_eq!(session_lines(&path).unwrap(), (expected.clone(), false));
        assert_eq!(
            session_lines_concat_zstd(&path).unwrap(),
            (expected.clone(), false)
        );
        for concat in [false, true] {
            assert_eq!(trimmed_lines(&path, concat), expected);
        }
    }

    /// The same torn stream with a block that ends mid-line: the line before
    /// the break survives and the fragment is dropped.
    #[test]
    fn a_torn_raw_block_drops_the_fragment_ending_the_previous_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.zstd");
        let mut second = vec![0x28, 0xb5, 0x2f, 0xfd, 0, 0];
        push_raw_block(&mut second, 11, false, b"second\npart");
        push_raw_block(&mut second, 10, true, b"thi");
        std::fs::write(&path, [zstd_frame(b"first\n"), second].concat()).unwrap();
        let expected = vec!["first".to_owned(), "second".to_owned()];
        assert_eq!(session_lines(&path).unwrap(), (expected.clone(), false));
        assert_eq!(
            session_lines_concat_zstd(&path).unwrap(),
            (expected.clone(), false)
        );
        for concat in [false, true] {
            assert_eq!(trimmed_lines(&path, concat), expected);
        }
    }

    /// A torn stream rewinds the tail to records the head already holds; the
    /// window lists each record once.
    #[test]
    fn a_torn_stream_does_not_repeat_records_held_by_head_and_tail() {
        let mut torn = b"first\nsecond\n".to_vec();
        torn.extend([b'x'; 100]);
        let stream = ChunkThenTorn(Some(torn));
        assert_eq!(
            decoded_lines_windowed(stream, 40, 20, 20).unwrap(),
            (vec!["first".into(), "second".into()], true)
        );
        // Not torn: the same bytes end at an unterminated line.
        let mut plain = b"first\nsecond\n".to_vec();
        plain.extend([b'x'; 100]);
        let (lines, windowed) = decoded_lines_windowed(io::Cursor::new(plain), 40, 20, 20).unwrap();
        assert!(windowed);
        assert_eq!(
            lines.iter().filter(|l| *l == "second").count(),
            1,
            "{lines:?}"
        );
    }

    /// Yields each chunk in turn, then end of stream (or a torn-frame error).
    struct Chunks {
        chunks: std::collections::VecDeque<Vec<u8>>,
        torn: bool,
    }

    impl Chunks {
        fn new(chunks: &[&[u8]], torn: bool) -> Self {
            Self {
                chunks: chunks.iter().map(|chunk| chunk.to_vec()).collect(),
                torn,
            }
        }
    }

    impl Read for Chunks {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.chunks.pop_front() {
                Some(chunk) => {
                    buf[..chunk.len()].copy_from_slice(&chunk);
                    Ok(chunk.len())
                }
                None if self.torn => Err(io::Error::other(TornZstdFrame)),
                None => Ok(0),
            }
        }
    }

    fn windowed_chunks(
        chunks: &[&[u8]],
        torn: bool,
        cap: u64,
        head: u64,
        tail: u64,
    ) -> Vec<String> {
        let (lines, windowed) =
            decoded_lines_windowed(Chunks::new(chunks, torn), cap, head, tail).unwrap();
        assert!(windowed);
        lines
    }

    /// The push that crosses the cap holds more complete bytes than the tail
    /// budget. When the cut falls inside a line, that line's tail end is not a
    /// record; when it falls on a line start, the line is whole.
    #[test]
    fn a_tail_cut_inside_a_line_drops_the_clipped_line_and_a_cut_on_a_boundary_keeps_it() {
        let stream: &[u8] = b"aaaa\nbbbb\ncccc\n"; // 15 bytes, one chunk over the cap
        for torn in [false, true] {
            // The last 8 bytes are `bb\ncccc\n`: `bb` is the end of `bbbb`.
            assert_eq!(windowed_chunks(&[stream], torn, 10, 5, 8), ["aaaa", "cccc"]);
            // The last 10 bytes are `bbbb\ncccc\n`: the cut is just after a newline.
            assert_eq!(
                windowed_chunks(&[stream], torn, 10, 5, 10),
                ["aaaa", "bbbb", "cccc"]
            );
            // The last 5 bytes are exactly `cccc\n`.
            assert_eq!(windowed_chunks(&[stream], torn, 10, 5, 5), ["aaaa", "cccc"]);
            // A budget of zero keeps no tail.
            assert_eq!(windowed_chunks(&[stream], torn, 10, 5, 0), ["aaaa"]);
        }
    }

    /// A line longer than the tail budget is never emitted as a clipped record,
    /// whether it ends in a newline, at the end of the stream, or in a tear.
    #[test]
    fn a_partial_line_longer_than_the_tail_budget_is_not_emitted_clipped() {
        // `aaaa\n`, then a 20-byte unterminated line: both arrive in one push.
        let long_first: &[u8] = b"aaaa\nxxxxxxxxxxxxxxxxxxxx";
        // The unterminated line ends in a newline, then another line follows.
        assert_eq!(
            windowed_chunks(&[long_first, b"yy\nzz\n"], false, 10, 5, 8),
            ["aaaa", "zz"]
        );
        // The same, torn after those lines.
        assert_eq!(
            windowed_chunks(&[long_first, b"yy\nzz\n"], true, 10, 5, 8),
            ["aaaa", "zz"]
        );
        // The stream ends inside the long line.
        assert_eq!(windowed_chunks(&[long_first], false, 10, 5, 8), ["aaaa"]);
        // The stream is torn inside the long line: the line before it stays.
        assert_eq!(windowed_chunks(&[long_first], true, 10, 5, 8), ["aaaa"]);
        // The long line grows over several pushes before its newline.
        assert_eq!(
            windowed_chunks(
                &[b"aaaa\n", b"xxxxxxxxxxxx", b"xxxxxxxxxxxx", b"\nzz\n"],
                false,
                10,
                5,
                8
            ),
            ["aaaa", "zz"]
        );
        // A final line shorter than the budget, after a long one, is whole.
        assert_eq!(
            windowed_chunks(&[long_first, b"\nend"], false, 10, 5, 8),
            ["aaaa", "end"]
        );
    }

    /// Yields its bytes once, then a torn-frame error.
    struct ChunkThenTorn(Option<Vec<u8>>);

    impl Read for ChunkThenTorn {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Some(bytes) = self.0.take() else {
                return Err(io::Error::other(TornZstdFrame));
            };
            assert!(bytes.len() <= buf.len());
            buf[..bytes.len()].copy_from_slice(&bytes);
            Ok(bytes.len())
        }
    }

    /// A zero-byte file is not a session: both extensions report the missing
    /// first frame, in either reader, rather than an empty session.
    #[test]
    fn an_empty_zstd_file_is_an_error_like_an_empty_zst_file() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["empty.jsonl.zst", "empty.jsonl.zstd"] {
            let path = dir.path().join(name);
            std::fs::write(&path, b"").unwrap();
            assert!(session_lines(&path).is_err(), "{name}");
            assert!(session_lines_concat_zstd(&path).is_err(), "{name}");
            assert!(session_lines_keeping(&path, |_| true).is_err(), "{name}");
            assert!(for_each_line(&path, |_| {}).is_err(), "{name}");
            assert!(for_each_line_concat_zstd(&path, |_| {}).is_err(), "{name}");
        }
    }

    #[test]
    fn concatenated_readers_keep_records_from_a_small_frame_with_a_torn_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl.zstd");
        let mut frames = zstd_frame(b"first\n");
        let payload = b"second\nthird\npartial";
        // A single-segment frame with a checksum and a one-byte content size.
        frames.extend([0x28, 0xb5, 0x2f, 0xfd, 0x24, payload.len() as u8]);
        push_raw_block(&mut frames, payload.len() as u32, true, payload);
        frames.extend([0, 0]); // half of the four-byte checksum
        std::fs::write(&path, frames).unwrap();

        let expected = vec!["first".into(), "second".into(), "third".into()];
        assert_eq!(
            session_lines_concat_zstd(&path).unwrap(),
            (expected.clone(), false)
        );
        assert_eq!(
            session_lines_windowed(&path, 15, 6, 19, false).unwrap(),
            (expected, true)
        );
        let mut raw = Vec::new();
        for_each_line_concat_zstd(&path, |line| raw.push(line.to_owned())).unwrap();
        assert_eq!(raw, ["first\n", "second\n", "third\n"]);
    }

    #[test]
    fn concatenated_readers_reject_corruption_and_first_frame_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl.zstd");
        let first = zstd_frame(b"first\n");
        let reserved_block = [0x28, 0xb5, 0x2f, 0xfd, 0, 0, 7, 0, 0];
        for frames in [
            [&first[..], b"junk"].concat(),
            [&first[..], b"xx"].concat(),
            [&first[..], &reserved_block].concat(),
            reserved_block.to_vec(),
            first[..2].to_vec(),
            torn_zstd_body(false),
            torn_zstd_body(true),
        ] {
            std::fs::write(&path, frames).unwrap();
            assert!(session_lines(&path).is_err());
            assert!(for_each_line(&path, |_| {}).is_err());
        }
    }

    #[test]
    fn non_eof_io_error_inside_a_trailing_frame_propagates() {
        let frames = [zstd_frame(b"first\n"), torn_zstd_body(false)].concat();
        let stream = ZstdFrames::new(BufReader::new(FailingSource(frames.as_slice())));
        let error = decoded_lines_windowed(stream, 10_000, 100, 100).unwrap_err();
        let frame_error = error
            .get_ref()
            .unwrap()
            .downcast_ref::<FrameDecoderError>()
            .unwrap();
        assert!(matches!(
            frame_error,
            FrameDecoderError::FailedToReadBlockBody(_)
        ));
        assert!(frame_error.to_string().contains("source read failed"));
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


/// Query available disk space in bytes on the filesystem of `path`.
#[cfg(windows)]
pub fn available_space(path: &Path) -> Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    let dir = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| std::path::PathBuf::from("."))
    };
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut free_bytes: u64 = 0;
    extern "system" {
        fn GetDiskFreeSpaceExW(
            lpDirectoryName: *const u16,
            lpFreeBytesAvailableToCaller: *mut u64,
            lpTotalNumberOfBytes: *mut u64,
            lpTotalNumberOfFreeBytes: *mut u64,
        ) -> i32;
    }
    let res = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_bytes,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if res == 0 {
        return Err(anyhow::anyhow!(
            "failed to query disk space: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(free_bytes)
}

#[cfg(unix)]
pub fn available_space(path: &Path) -> Result<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let dir = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| std::path::PathBuf::from("."))
    };
    let c_path = CString::new(dir.as_os_str().as_bytes())
        .context("invalid path for statvfs")?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let res = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if res != 0 {
        return Err(anyhow::anyhow!(
            "failed to query disk space: {}",
            std::io::Error::last_os_error()
        ));
    }
    let stat = unsafe { stat.assume_init() };
    let bsize = if stat.f_frsize > 0 { stat.f_frsize as u64 } else { stat.f_bsize as u64 };
    Ok(stat.f_bavail as u64 * bsize)
}

#[cfg(test)]
mod space_tests {
    use super::*;

    #[test]
    fn available_space_returns_positive_bytes_on_temp_dir() {
        let temp = std::env::temp_dir();
        let bytes = available_space(&temp).expect("query available space on temp dir");
        assert!(bytes > 0);
    }
}
