<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/banner-dark.png">
  <img src="docs/banner.png" alt="sessionwiki — a wiki of every AI coding session you've ever had: searchable, linkable, resumable. Claude Code, Codex, Gemini CLI, OpenCode. 100% local.">
</picture>

<a href="https://github.com/youdie006/sessionwiki/actions/workflows/ci.yml"><img src="https://github.com/youdie006/sessionwiki/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
<a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT license"></a>
<a href="https://github.com/youdie006/sessionwiki/releases"><img src="docs/release-badge.png" height="20" alt="Latest release v0.8.0"></a>
<img src="https://img.shields.io/badge/platform-linux%20%7C%20macos%20%7C%20windows-555" alt="Platforms: Linux, macOS, Windows">
<a href="#adding-an-adapter"><img src="https://img.shields.io/badge/adapters-PRs%20welcome-2ea44f" alt="Adapter PRs welcome"></a>

<img src="https://img.shields.io/badge/English-3b5bd6?style=for-the-badge" alt="English (current)">&nbsp;<a href="README.ko.md"><img src="https://img.shields.io/badge/%ED%95%9C%EA%B5%AD%EC%96%B4-eceae3?style=for-the-badge&labelColor=eceae3&color=6e6b62" alt="한국어"></a>

<a href="#install"><img src="docs/nav/install.png" height="20" alt="Install"></a>
<a href="#quick-start"><img src="docs/nav/quick-start.png" height="20" alt="Quick start"></a>
<a href="#commands"><img src="docs/nav/commands.png" height="20" alt="Commands"></a>
<a href="#trace-code-back-to-its-session"><img src="docs/nav/trace.png" height="20" alt="Trace"></a>
<a href="#nothing-gets-lost-archive-mode"><img src="docs/nav/archive.png" height="20" alt="Archive"></a>
<a href="#pick-up-where-you-left-off"><img src="docs/nav/resume.png" height="20" alt="Resume"></a>
<a href="#adding-an-adapter"><img src="docs/nav/add-a-tool.png" height="20" alt="Add a tool"></a>

<img src="docs/demo-cli.webp" width="780" alt="Terminal recording: sessionwiki scans 47 GB of sessions across three tools, searches every message at once, jumps to related sessions, tags them, and resumes one in its original tool">

</div>

That conversation where Claude fixed your CORS bug three weeks ago? It is still on your disk &mdash; you just can't find it. Every AI coding agent writes its sessions to disk: each tool in its own format, in its own folder, on every machine you use. After a few months that is thousands of conversations full of solved problems, and no way to get back to any of them.

**sessionwiki reads the traces your tools already leave and turns them into one searchable, linkable archive you can actually maintain.** No daemon, no logging habit to build, no cloud. It indexes what is already there, then lets you tag it, link it, and pick up where you left off.

```console
$ sessionwiki scan
TOOL            SESSIONS       SIZE  OLDEST       NEWEST        PATH
claude-code         1763     1.1 GB  2026-03-27   2026-06-12    ~/.claude/projects
codex               2340    45.9 GB  2025-08-21   2026-06-12    ~/.codex/sessions
gemini                50     1.2 MB  2026-04-02   2026-06-10    ~/.gemini/tmp

4153 sessions across 3 tools, 47.0 GB on disk.
```

That is one real machine. Run it on yours &mdash; the number is usually a surprise.

## What you can do with it

- **Find** every session store on the machine: which tools, where, how many, how big &mdash; instant.
- **Search** every message of every tool at once. Substring matching, so partial identifiers and CJK text work with zero language setup.
- **Read** any session as a clean transcript: rendered code blocks, collapsed tool calls, an outline of long sessions.
- **Summarize** sessions into cached one-line synopses using your own LLM CLI &mdash; then never wonder "what was this one about" again.
- **Resume** a session in its original tool, in the right project directory, with one command.
- **Carry context across tools**: brief a Claude Code session into Codex, or anywhere else.
- **Trace a file back to the sessions that edited it** &mdash; `trace src/auth.rs` lists the AI conversations behind a file, across every tool. The link between your sessions and the code they produced. See [provenance](#trace-code-back-to-its-session).
- **Keep what your tools delete** &mdash; sessions are [archived](#nothing-gets-lost-archive-mode) when the tool prunes them, so search and trace never go dark on old work.
- **Curate and connect**: tag and annotate sessions, jump to related ones, and see where your agent time goes &mdash; [session engineering](#session-engineering), not just search.

And a web UI when you would rather read than grep &mdash; `sessionwiki web`:

<img src="docs/demo-web.webp" width="820" alt="The sessionwiki web UI: real typing in the search, an open transcript with tags, a note and a resume command, file provenance, tag filtering, and dark mode">

<sub>Real typing in the search, an open transcript with tags and a one-command resume, file provenance, tag filtering, and dark mode. <a href="docs/demo-web.mp4">&#9654; Smoother HD version (MP4)</a>.</sub>

## Install

**Prebuilt binary** (no toolchain needed). macOS / Linux:

```console
curl -sSL https://raw.githubusercontent.com/youdie006/sessionwiki/main/scripts/install.sh | sh
```

The script downloads the right archive for your platform from the
[latest release](https://github.com/youdie006/sessionwiki/releases/latest) and
installs it to `~/.local/bin`. On Windows, download the `.zip` from the
releases page.

**With Rust** (stable):

```console
cargo install --git https://github.com/youdie006/sessionwiki
```

Either way it is a single binary with no runtime dependencies.

### Claude Code plugin (automatic session recall)

Make Claude Code recall your past sessions automatically. Install the
`sessionwiki` CLI first (above), then add the plugin from this repo:

```console
/plugin marketplace add youdie006/sessionwiki
/plugin install sessionwiki@sessionwiki-marketplace
```

Now Claude pulls in prior work when you start a task, and `/sessionwiki:recall
<topic>` searches your history on demand. The plugin shells out to the local
`sessionwiki` binary &mdash; fully offline. If the binary isn't on `PATH`, the
plugin degrades gracefully and Claude just works without recall.

## Quick start

```console
sessionwiki scan                # where are my sessions?
sessionwiki search "jwt retry"  # full-text search across every tool
sessionwiki show 3f9c           # read the matching conversation
sessionwiki web                 # or browse everything in a local web UI
```

The first `search` or `list` builds the index; expect a few minutes per
gigabyte of history (a one-time cost &mdash; heavy Codex users can have tens of
GB). After that, updates are incremental and take seconds.

## Commands

| Command | What it does |
|---|---|
| `scan` | Discover session stores on this machine. Pure filesystem walk, instant. |
| `list` | Recent sessions across all tools in one timeline. `--tool codex`, `--project api`, `--tag spike`, `-n 50`, `--all` (include subagent transcripts). |
| `search <query>` | Full-text search over every message of every tool. Minimum 3 characters. |
| `show <id>` | One session as a readable transcript. `--full` expands tool calls, `--json` emits the parsed session, `--outline` prints a digest: every question you asked plus how it ended. |
| `summarize [id]` | Generate 1&ndash;2 sentence synopses with **your own LLM CLI** (`claude -p` by default, `--cmd` / `SESSIONWIKI_SUMMARIZER` to change) and cache them in the index. Without an id, batches over the `--recent N` newest sessions. Summaries survive reindexing and show up in `show`, `--outline`, and the web sidebar. |
| `resume <id>` | Reopen the session in its original tool: `claude --resume` / `codex resume`, run in the right project directory. Subagent transcripts resume their parent. `--print` to just show the command. |
| `brief <id>` | Emit the session as a markdown briefing (head and tail, middle omitted) to carry context into any tool &mdash; including across tools. `--max-chars`, `--tools`. |
| `web` | Local viewer on `127.0.0.1:7575`: day-grouped sessions with synopsis previews, live search with highlighted snippets, rendered transcripts with outlines, tags, and "see also" related sessions, resume commands, light and dark themes, UI in English, Korean, Japanese, and Chinese (auto-detected). It reads the existing index (sessions created after your last `list`/`search` show up once you refresh); `web --sync` refreshes first. Never leaves localhost. |

### Session engineering

A session is a unit of context, and once you have hundreds they need curating
and managing &mdash; not just searching. These commands turn the flat archive into
a navigable, maintained one. They read the index, so they are instant.

| Command | What it does |
|---|---|
| `related <id>` | Sessions about the same thing: same project first, then sessions that edited the same files, then anything sharing a tag. The "see also" for your work. |
| `files <id>` | The files a session edited or created &mdash; its side of the provenance link. |
| `trace <path>` | The AI sessions that touched a file, newest first. Matches a relative path against the absolute one on disk, so `trace src/auth.rs` just works. See [below](#trace-code-back-to-its-session). |
| `tag <id> <tag>...` | Tag a session (`--rm` to remove). No id lists every tag in use. Filter with `list --tag`. Tags are stored in the index and survive reindexing &mdash; the original session files are never touched. |
| `note <id> "text"` | Pin a freeform note on a session; omit the text to read it back. |
| `forget <id>` | Permanently drop a session from the index and archive. The escape hatch for [archive mode](#nothing-gets-lost-archive-mode) when you want a kept session gone. |
| `projects` | One row per project: session count, message volume, last activity. A page per codebase. |
| `stats` | Totals plus a breakdown by tool, by month, files linked to sessions, and how many sessions were kept after the tools deleted them. |

### Trace code back to its session

AI writes most of the code now, so the question is no longer "who wrote this
line" but "which conversation produced it, and why." sessionwiki reads the file
edits out of each session's tool calls &mdash; Claude's `Edit`/`Write`, Codex's
`apply_patch` &mdash; and links every session to the files it changed.

```console
$ sessionwiki trace src/middleware/mod.rs
2 session(s) touched "src/middleware/mod.rs", newest first:
35a59790  claude-code  2026-06-09  Fix CORS preflight failing on /auth routes
4fd0ce37  claude-code  2026-06-08  Add retry with backoff to the payment webhook handler
```

It works retroactively, with no hooks or setup, over every session already on
disk &mdash; nothing to install before the fact. The honest scope: this points
you at the conversations that *touched* a file, not at line-level authorship; a
later edit may have replaced the code, so `trace` is a way back to the relevant
discussion, not a claim that a given line came from one session. In the web UI,
the files a session touched are chips in its header &mdash; click one to see
every other session that touched it.

### Nothing gets lost (archive mode)

Claude Code and Codex prune old sessions over time. The first time `trace`
comes up empty for a file you *know* an agent wrote &mdash; because the session
behind it was deleted &mdash; the whole link is worthless. So once sessionwiki
has indexed a session, it keeps it: when a tool deletes the original, the
session is **archived**, not dropped, and `search`, `trace`, and `brief` keep
working for it.

```console
$ sessionwiki list          # after Claude pruned an old session
archived 1 session(s) the tool removed (1 kept that your tools have deleted)
...
a1b2c3d4  claude-code  3w ago  12  …/api-server  Fix CORS preflight…  [archived]
```

It is automatic and silent (no copy step to remember), costs almost nothing
(the distilled transcript was already in the index), and is reversible:
`forget <id>` drops an archived session for good, and a session that reappears
on disk un-archives itself. The original tool can no longer reopen it, but you
can still read it, `brief` it into a new session, and `trace` the code back to
it. This is the part a generation-time hook can't do: it works for the
thousands of sessions that already exist, and for the ones the tool already
deleted while you weren't looking.

## Pick up where you left off

Finding an old session is half the point; the other half is continuing it.

```console
$ sessionwiki search "rate limiter"
76a614028a63 codex 2026-06-11 13:00 .../projects/api-server [assistant]
  ...the bucket invariant 0 <= tokens <= capacity holds after every step...

$ sessionwiki resume 76a6           # reopens that conversation in Codex

$ sessionwiki brief 76a6 | claude -p \
    "Continue this work: add the missing edge-case tests"

$ sessionwiki summarize --recent 20  # synopses for your latest sessions
```

`resume` uses each tool's native mechanism, so it needs the original session
file to still exist. `brief` works even across tools. `summarize` runs your
LLM, on your machine, at your command &mdash; sessionwiki itself never makes a
network call.

## How it works

```mermaid
flowchart LR
    subgraph stores["Already on your disk"]
        A["~/.claude/projects"]
        B["~/.codex/sessions"]
        C["~/.gemini/tmp"]
    end
    A & B & C --> AD["adapters<br>(one small file per tool)"]
    AD --> IDX[("SQLite FTS5 index<br>trigram tokenizer")]
    IDX --> CLI["CLI<br>scan / list / search / show<br>summarize / resume / brief"]
    IDX --> WEB["web viewer<br>127.0.0.1 only"]
```

- `scan` walks the filesystem and reports; it touches no index.
- Everything else maintains an incremental index at
  `~/.local/share/sessionwiki/index.db` (platform equivalent; override with
  `SESSIONWIKI_DATA`). Only files whose mtime or size changed are re-parsed.
- Original session files are never modified &mdash; the index is a disposable
  cache. Cached summaries survive schema upgrades on purpose: rebuilding an
  index is cheap, re-running an LLM over your history is not.
- Noise is filtered deliberately: repeated harness boilerplate and bulky tool
  outputs stay out of the index so search results stay signal.

<details>
<summary><b>FAQ: why not just grep the session folders?</b></summary>
<br>

You can, but the files are JSONL event streams with escaped text in three
different schemas. grep gives you raw matching lines out of context; the
trigram index gives ranked results with snippets in milliseconds, joined to
session metadata, including nested subagent transcripts, across all tools at
once &mdash; and the id it returns plugs straight into `show`, `resume`, and `brief`.
</details>

<details>
<summary><b>FAQ: does anything ever leave my machine?</b></summary>
<br>

No. There is not a single network call in the codebase &mdash; it is small enough
to verify with one grep. No telemetry, no accounts. The web UI binds to
127.0.0.1 only. `summarize` is the one feature that touches an LLM, and it
does so by running a CLI you chose, locally, only when you invoke it.
</details>

<details>
<summary><b>FAQ: how big is the index?</b></summary>
<br>

Expect a low double-digit percentage of your stores' size; tens of GB of
history produce a few GB of index. It is a cache &mdash; delete it whenever you
want and the next run rebuilds it. Cached summaries, curation, and archived
sessions are kept separately so they survive.
</details>

<details>
<summary><b>FAQ: what about sessions my tool deletes?</b></summary>
<br>

Sessions sessionwiki already indexed are kept: when a tool prunes the original,
the session is [archived](#nothing-gets-lost-archive-mode), not lost, so
`search`/`trace`/`brief` keep working for it. Sessions that were deleted
*before* you ever ran sessionwiki are gone &mdash; it can only keep what it has
seen, so install early. Archiving is automatic; `forget <id>` drops one for
good.
</details>

## Privacy

Sessions contain your code and your conversations, so the bar is simple:
no network calls, no telemetry, index stored locally, originals opened
read-only. See the FAQ above.

## Supported tools

| Tool | Session store | Status |
|---|---|---|
| Claude Code | `~/.claude/projects/**/*.jsonl` (incl. nested subagent transcripts) | supported |
| Codex CLI | `~/.codex/sessions/**/rollout-*.jsonl` | supported |
| Gemini CLI | `~/.gemini/tmp/*/chats/*.json` | supported |
| OpenCode | `~/.local/share/opencode/storage/{session,message,part}/**` | supported |
| Cline, Roo Code, Kilo Code | VS Code `globalStorage/<ext>/tasks/<id>/` (one parser, three tools) | supported |
| gajae-code (& Pi) | `~/.gjc/agent/sessions/**/*.jsonl` | supported |
| Continue | `~/.continue/sessions/*.json` | supported |
| Cursor, Aider, Zed, ... | | planned &mdash; PRs welcome |

**Using a wrapper like oh-my-claudecode, oh-my-codex, oh-my-openagent (OmO), or
lazyclaudecode/lazycodex?** Those are harnesses over Claude Code, Codex, and
OpenCode &mdash; the conversations are written to those tools' own stores, so
sessionwiki already indexes them with no extra setup, and it detects which
harness drove a session and tags it (e.g. `oh-my-claudecode`) so you can filter
with `list --tag oh-my-claudecode`. (gajae-code is the exception: it is a
standalone agent with its own store, and has its own adapter.)

### Where sessionwiki fits

Browsing AI session history is an active space &mdash; there are native GUI apps
and other multi-tool CLIs. sessionwiki makes a few specific bets:

- **It links sessions to the code they produced.** [`trace <file>`](#trace-code-back-to-its-session)
  goes from a file to the conversations that edited it &mdash; retroactively,
  with no hooks, over sessions that already exist. Other tools show you the
  conversation; this connects it to your codebase.
- **Cross-platform, CLI _and_ web, one static binary.** It runs the same on
  Linux, macOS, and Windows, over SSH, in a container &mdash; not a single-OS app.
- **CJK search with zero setup.** The trigram index searches Korean, Japanese,
  and Chinese (and partial words) out of the box, which most tools handle poorly.
- **A curation layer, not just search.** Tags, notes, `related`, `brief`, and
  `stats` &mdash; [session engineering](#session-engineering), so the archive stays
  navigable as it grows.

Honest tradeoff: if you want the widest tool coverage *today*, sessionwiki
supports nine (Claude Code, Codex, Gemini CLI, OpenCode, Cline, Roo Code, Kilo Code,
gajae-code, Continue) and is growing &mdash; adapters are
the #1 thing [PRs](#adding-an-adapter) help with. If you mainly use those tools,
care about CJK, or want one binary that works everywhere, this is built for you.

## Adding an adapter

If your agent writes sessions to disk, it belongs here. An adapter is
one small Rust file implementing four methods:

```rust
pub trait Adapter {
    fn name(&self) -> &'static str;               // "my-tool"
    fn root(&self) -> Option<PathBuf>;            // where it keeps sessions
    fn discover(&self) -> Vec<PathBuf>;           // every session file
    fn parse(&self, path: &Path) -> Result<Session>; // tolerant; skip bad lines
}
```

Look at [`src/adapters/gemini.rs`](src/adapters/gemini.rs) for the smallest
example (~100 lines), register your type in [`src/adapters/mod.rs`](src/adapters/mod.rs),
and open a PR. Parsers must never panic on malformed input &mdash; session formats
drift between tool versions, so parse defensively and return what you can.

## Roadmap

- more adapters &mdash; Cursor, Aider, Zed, gptme, ... the #1 thing PRs
  help with (see [adding an adapter](#adding-an-adapter))
- richer provenance &mdash; correlate a session's edits with the file's git
  history so `trace` can narrow to the commits and line ranges around it
- `sync` &mdash; merge indexes from multiple machines
- `clean` &mdash; reclaim disk from huge old session stores, safely
- prebuilt binaries for every platform

Shipped recently: [provenance](#trace-code-back-to-its-session) (`trace` /
`files`) and [archive mode](#nothing-gets-lost-archive-mode).

## Contributing

Issues and PRs are welcome. The most valuable contributions right now:

1. **Adapters** for tools you use (see [Adding an adapter](#adding-an-adapter))
2. **Format fixes** when a tool update changes its session schema
3. **Bug reports** with the first few lines of a session file that fails to parse (redact freely)

## License

[MIT](LICENSE). Free for any use, including commercial &mdash; just keep the license notice.

<div align="center">
<br>

<a href="https://github.com/youdie006/sessionwiki/issues/new">Report a bug</a> &middot;
<a href="https://github.com/youdie006/sessionwiki/issues/new">Request an adapter</a> &middot;
<a href="#roadmap">Roadmap</a>

</div>
