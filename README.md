# session-atlas

Find, search, and read every AI coding session you have ever had — across Claude Code, Codex, and Gemini CLI. One command, 100% local.

Your AI coding agents leave thousands of sessions scattered across tools, formats, and machines. That conversation where Claude fixed your CORS bug three weeks ago is still on your disk — you just cannot find it. session-atlas reads the traces your tools already leave and turns them into one searchable archive.

```console
$ session-atlas scan
TOOL            SESSIONS       SIZE  OLDEST       NEWEST        PATH
claude-code         1815   705.8 MB  2026-05-13   2026-06-12    ~/.claude/projects
codex               2340    45.9 GB  2025-08-21   2026-06-12    ~/.codex/sessions
gemini                50     1.2 MB  2026-04-02   2026-06-10    ~/.gemini/tmp

4205 sessions across 3 tools, 46.5 GB on disk.
```

```console
$ session-atlas search "CORS preflight"
3f9c2a1b04e7 claude-code 2026-05-21 14:02 .../MyProject/api [assistant]
  ...the preflight fails because the CORS middleware runs after auth; move it...

$ session-atlas show 3f9c2a1b04e7
```

## Why

- **No new habit required.** Every supported tool already writes its sessions to disk. session-atlas never asks you to log, tag, or save anything — it indexes what is already there.
- **Cross-tool.** You solved it with Claude Code, or was it Codex? It does not matter anymore. One search covers everything.
- **Your past sessions are a knowledge base.** Most developers re-solve problems they already solved with an AI once. Stop doing that.
- **100% local.** No accounts, no telemetry, no network calls. Your sessions never leave your machine.

## Install

Requires Rust (stable):

```console
cargo install --git https://github.com/youdie006/session-atlas
```

Prebuilt binaries are planned.

## Usage

```console
session-atlas scan                      # which tools, where, how many, how big
session-atlas list                      # recent sessions across all tools
session-atlas list --tool codex -n 50   # filter by tool
session-atlas search "jwt refresh"      # full-text search, all tools at once
session-atlas search "토큰 만료"          # CJK works (trigram index)
session-atlas show 3f9c2a1b04e7         # readable transcript (id prefix is enough)
session-atlas show 3f9c --full          # include full tool inputs
session-atlas show 3f9c --json          # parsed session as JSON
```

## Supported tools

| Tool | Store | Status |
|---|---|---|
| Claude Code | `~/.claude/projects/**/*.jsonl` (incl. subagent transcripts) | supported |
| Codex CLI | `~/.codex/sessions/**/rollout-*.jsonl` | supported |
| Gemini CLI | `~/.gemini/tmp/*/chats/*.json` | supported |
| Cursor, OpenCode, Aider, OpenClaw, ... | | planned — PRs welcome |

Adding a tool is one small Rust file implementing the `Adapter` trait (`src/adapters/`). If your agent writes sessions to disk, it belongs in the atlas.

## How it works

- `scan` is a pure filesystem walk — no parsing, no index, instant.
- `list`, `search`, and `show` maintain an incremental index (SQLite FTS5, trigram tokenizer) at `~/.local/share/session-atlas/index.db`. Only files whose mtime/size changed get re-parsed, so after the first run updates take seconds.
- Original session files are never modified. The index is read-only with respect to your stores and can be deleted at any time.
- Noise is filtered on purpose: repeated harness boilerplate (instruction dumps, environment context) and bulky tool outputs are excluded from the index so search results stay signal.

Notes:

- Search queries need at least 3 characters (trigram index). In exchange you get substring matching, which also makes Korean/Japanese/Chinese text searchable without a language-specific tokenizer.
- The first indexing pass streams through everything your agents ever wrote (tens of GB for heavy users); subsequent runs are incremental.

## Privacy

Sessions contain your code and your conversations. session-atlas:

- runs entirely offline — there is not a single network call in the codebase
- stores its index in your local data directory, nowhere else
- never modifies or uploads the original session files

## Roadmap

- `link` — connect sessions to the git commits they produced ("git blame for AI sessions")
- `sync` — merge archives from multiple machines
- `clean` — reclaim disk from huge old session stores, safely
- `stats` — usage breakdown per tool, project, and month
- more adapters

## License

MIT
