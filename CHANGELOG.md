# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/), and the project aims to follow
semantic versioning once it reaches 1.0.

## [Unreleased]

### Added
- Library + binary split (`src/lib.rs`): the adapter and index logic is now a
  reusable crate, not just a CLI.
- Golden-file parser tests for all three adapters, covering boilerplate
  dropping, subagent detection, schema variants, multi-block content, and
  malformed-line tolerance (`tests/parse.rs`, `tests/fixtures/`).
- GitHub Actions: CI (fmt + clippy + test) and a release workflow that builds
  prebuilt binaries for Linux, macOS (x86_64 + arm64), and Windows on tag push.
- `scripts/install.sh` one-line installer that fetches the right prebuilt
  binary for the platform.
- Animated terminal demo (`docs/demo.gif`) and a "how this differs from a
  single-tool viewer" section in the README.

## [0.5.0] - 2026-06-12

### Changed
- Renamed the project from `session-atlas` to **sessiondex**. The data
  directory migrates automatically on first run, carrying the existing index
  and cached summaries over.

### Added
- Multilingual web UI (English, Korean, Japanese, Chinese), auto-detected from
  the browser locale and switchable from the sidebar.
- Korean README (`README.ko.md`).

## [0.4.0] - 2026-06-12

### Added
- `summarize`: cache 1–2 sentence LLM synopses using your own LLM CLI. Stored
  in a table that survives index schema rebuilds.
- README polish: logo, light/dark adaptive hero, architecture diagram, FAQ.

## [0.3.0] - 2026-06-12

### Added
- Session previews (last assistant message) in the web sidebar.
- `show --outline`: a deterministic digest of a session (every user turn plus
  how it ended), and a clickable outline in the web transcript.

## [0.2.0] - 2026-06-12

### Added
- `resume`: reopen a session in its original tool (`claude --resume` /
  `codex resume`) in the right project directory.
- `brief`: emit a markdown briefing of a session to carry context across tools.

## [0.1.0] - 2026-06-12

### Added
- Initial release: `scan`, `list`, `search`, `show`, and a local web UI over an
  incremental SQLite FTS5 index. Adapters for Claude Code, Codex, and Gemini
  CLI. 100% local, no telemetry.

[Unreleased]: https://github.com/youdie006/sessiondex/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/youdie006/sessiondex/releases/tag/v0.5.0
[0.4.0]: https://github.com/youdie006/sessiondex/releases/tag/v0.4.0
[0.3.0]: https://github.com/youdie006/sessiondex/releases/tag/v0.3.0
[0.2.0]: https://github.com/youdie006/sessiondex/releases/tag/v0.2.0
[0.1.0]: https://github.com/youdie006/sessiondex/releases/tag/v0.1.0
