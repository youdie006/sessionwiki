# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/), and the project aims to follow
semantic versioning once it reaches 1.0.

## [Unreleased]

### Added
- Provenance: sessions are linked to the files they edited or created, read
  from their tool calls (Claude's `Edit`/`Write`/`MultiEdit`, Codex's
  `apply_patch`). New commands: `files <id>` (a session's edits) and
  `trace <path>` (the sessions that touched a file, newest first, matched by
  suffix so a relative path resolves to the absolute one on disk). The scope is
  honest — sessions that *touched* a file, not line-level authorship.
- `related` now also links sessions that edited the same file, even across
  projects; `stats` reports the count of files linked to a session.
- Web UI: a session's touched files appear as chips in its header; clicking one
  lists every session that touched it (new `/api/trace` endpoint).
- Archive mode: when a tool deletes a session's original file, sessionwiki
  keeps the indexed copy instead of dropping it, so `search`, `trace`, and
  `brief` keep working for it. A durable `archive` table survives schema
  rebuilds; archived sessions are served from the index (`show`/`brief`/web),
  flagged `[archived]` in `list`/`show`/web, and counted in `stats`. `sync`
  reports how many were kept. New `forget <id>` purges a session for good;
  re-appearing files un-archive automatically; `SESSIONWIKI_NO_ARCHIVE` reverts
  to delete-on-prune. A store that vanishes wholesale (uninstall, unmount) is
  not mass-archived.

### Changed
- CLI demo is a clean static full-frame terminal (no camera zoom). The web demo
  zooms to exact element regions measured from the live DOM, at 50fps with
  cubic ease-in-out moves and still holds.

## [0.7.0] - 2026-06-16

### Changed
- Renamed the project from `sessiondex` to **sessionwiki**, reframed around
  "session engineering": a session is a unit of context, and once you have
  hundreds they need curating, not just searching. The data directory migrates
  newest-name-first (`sessiondex` then `session-atlas`), carrying the index and
  curated tags/notes/summaries over.

### Added
- Curation layer (the editable "wiki" part): `tag` / `note`, stored in the
  index and surviving reindexing; the original session files are never touched.
- `related`: sessions about the same thing — same project first, then anything
  sharing a tag — surfaced in `show` and as a "see also" panel in the web UI.
- `projects` and `stats`: per-project and per-tool/per-month rollups.
- Web UI: clickable tag chips and a tag-filter bar, notes, a "see also" panel,
  a node-graph logo, and a globe-button language picker.
- Two demo GIFs: a narrated CLI recording (`docs/demo-cli.gif`) and a web UI
  tour (`docs/demo-web.gif`). `SECURITY.md`.

## [0.6.0] - 2026-06-16

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
- A "how this differs from a single-tool viewer" section in the README.

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

[0.7.0]: https://github.com/youdie006/sessionwiki/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/youdie006/sessionwiki/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/youdie006/sessionwiki/releases/tag/v0.5.0
[0.4.0]: https://github.com/youdie006/sessionwiki/releases/tag/v0.4.0
[0.3.0]: https://github.com/youdie006/sessionwiki/releases/tag/v0.3.0
[0.2.0]: https://github.com/youdie006/sessionwiki/releases/tag/v0.2.0
[0.1.0]: https://github.com/youdie006/sessionwiki/releases/tag/v0.1.0
