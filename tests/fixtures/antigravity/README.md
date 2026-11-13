`agy-1.5.7-linux.jsonl` was captured from a running Antigravity CLI **1.5.7**
conversation on Linux on 2026-11-13. It preserves the native field names,
step order, tool names and argument shapes. User text, model output, commands,
paths and timestamps were replaced with synthetic examples; reasoning and
usage metadata were omitted. It contains one user turn, three tool calls and
results, and a final assistant response. The local installed CLI is 1.5.8;
the captured live target reported 1.5.7.

The adapter also recognizes the older IDE transcript directory layout from
PR #83. Tests for writes, denied/unfinished edits, malformed lines, URI
metadata, windowing and store reconciliation use explicitly synthetic records.
Native protobuf-only conversations without JSONL exports are not decoded.

The public `in_home` / `cli_in_home` constructors take the Gemini configuration
directory (normally `~/.gemini`, or the path passed to `agy --gemini-dir`).
Both variants keep their own reconciliation scope. Summary SQLite metadata is
optional; transcripts remain the authority for conversation discovery.
