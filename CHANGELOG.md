# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/), and the project aims to follow
semantic versioning once it reaches 1.0.

## [Unreleased]

### Fixed

- `exclude_id` on the MCP `recent_sessions` and `related_sessions` drops a
  session named by the tool's own session id, as every MCP `id` argument
  already accepts. A running agent knows itself by that id (Claude Code puts
  it in `CLAUDE_CODE_SESSION_ID`), and passing it left the agent's own session
  at the top of "who is around".
- The MCP tools no longer read another user's home as yours when the name
  starts with yours: with home `/home/me`, a project in `/home/meow/x` was
  shown as `~ow/x`, keeping part of that user's name, and now shows as `~/x`
  like any other home.
- `resume` of a Claude Code session started in a folder typed with other
  letter case than it has on disk (`cd /mnt/d/myproject/brain` for
  `/mnt/d/MyProject/Brain`, on Windows, macOS or a Windows drive under WSL)
  runs it from the folder as it was typed. Claude Code files a session under
  the path as typed and finds it only from that spelling; sessionwiki started
  it from the path as stored on disk, where Claude Code answered that no
  conversation has that id, and printed the command instead of running it.
  The spelling is read back from the session's own folder name and used only
  when it reaches the same directory. On one machine this covered 6 sessions.
- On Windows, `resume`, the Claude Code SessionStart brief and `migrate` use
  a project folder as tools record it. Each resolved the folder with
  `canonicalize`, which on Windows answers `\\?\C:\...`, a spelling no tool
  records, and `resume` counted only a path starting with `/` as a folder:
  - `resume` launched a Claude Code session from wherever the command was
    typed, and Claude Code looks a session up by the folder it runs in. A
    Codex session now names its folder too, and, as on Linux and macOS,
    `resume` prints its command rather than launching it there unchecked.
  - The SessionStart brief was always empty. Now that it shows, a file
    outside the project is named by its file name, as on Linux and macOS,
    not by its full `C:\...` path.
  - `migrate` copied a Claude Code session into a `----C--...` folder that
    Claude Code never reads, and printed a `cd` into the `\\?\` path.
- On Windows, `trace`, `blame`, the edits behind a file and the MCP
  `trace_file` find a file by a relative path or its name. Each matched a
  relative path only after a `/`, while Windows tools record
  `D:\p\src\a.rs`, so `trace src\a.rs`, `trace src/a.rs` and `trace a.rs`
  found nothing and only the full path did; `blame` left every line
  unattributed. On Windows `\` now reads as `/` on both sides. Elsewhere `\`
  stays an ordinary character in a name, and nothing changes there.
- The SessionStart brief names a file in a sibling folder by its file name.
  It cut the project's path off as text, so for project `app` a file in
  `app-old` showed as `-old/notes.md`.

## [0.34.20] - 2026-10-28

### Fixed

- `--project` on `list` and `search` (and the MCP and web filters built on
  them) takes its text literally. It was a `LIKE` pattern, so `_` in a name
  matched any character and `%` matched everything: `--project harness_eg`
  also listed folders named `...-harness-eg`, 65 of the first 200 rows on one
  machine.
- `related` links sessions that edited the same file by the file, not by how
  each tool spelled its path. Codex records `src/auth.rs` relative to its
  folder and Claude Code records `/proj/api/src/auth.rs`; the two were never
  linked, while two relative `notes.md` from unrelated folders were. A relative
  path is now read against its session's folder, and case is ignored as
  `trace` ignores it. On one machine, 12 of 300 sampled sessions lost 10 such
  false links each (all throwaway runs under /tmp sharing `notes.md` or
  `tmp_a.log`); no related list between real projects changed there, because
  sessions of the same project usually fill the list first.
- `migrate` of a Claude Code session to a folder whose path has a space, a
  `:`, `@` or a non-ASCII letter, or is longer than about 200 characters,
  wrote the copy where Claude Code does not look, so `claude --resume` could
  not find it. sessionwiki turned only `/`, `.` and `_` into `-`; Claude Code
  turns every character but an ASCII letter or digit into `-`, and shortens a
  long name with a hash. The same rule decides whether `resume` trusts a
  session's folder, which failed for such folders too. On one machine the
  folder sessionwiki computes now matches Claude Code's for 706 of 717 Claude
  Code sessions, up from 693.
- The `cd ... && claude --resume ...` line `migrate` prints quotes a folder
  the shell would split, such as `My Folder`.

## [0.34.19] - 2026-10-27

### Fixed

- `trace` with a full path (and the `trace_file` MCP tool and the web file
  history) now finds a session that recorded the path in another letter case.
  On a case-insensitive filesystem (WSL's `/mnt/<drive>`, macOS by default) a
  tool records a path as it was typed, so `APO/scripts/x.py` and the editor's
  `APO/Scripts/x.py` are one file; a relative query already matched either
  case, a full one did not and could report that no session touched the file.
  On one machine 1488 of 10461 recorded paths differ from the on-disk case;
  traced by their on-disk spelling, a sample of 60 was found 15 times before
  and 59 times now (the other is past the 20-result cap).

## [0.34.18] - 2026-10-27

### Fixed

- In the web UI, clicking a file a Codex session edited showed the history of
  every file with that name. The session page listed the path as recorded,
  relative to the session's folder (`README.md`), and the file history searched
  for it as given: for one session, 100 sessions across unrelated projects
  instead of the 4 that touched its README. The session page now lists each
  path in full, read against the session's folder, and a file recorded both
  ways appears once.

## [0.34.17] - 2026-10-27

### Fixed

- The "Sessions that touched this file" list under `blame` now names only the
  sessions its lines are attributed among. It searched by the repository-relative
  path alone, so it listed sessions that edited a file of the same name in any
  other project: for `src/main.rs` in this repository all 20 were other
  projects' files, and for `README.md` all 20 were too. Now a session that
  edited another existing file of that name is left out, as it already was
  from the line attribution.
- `trace`'s file-name fallback looked at the newest 500 sessions with that
  name and filtered them afterwards, so 500 newer sessions elsewhere could hide
  a moved file's history. It now filters every match before taking 20.

## [0.34.16] - 2026-10-27

### Fixed

- `trace` with a full path (and the `trace_file` MCP tool, which agents call
  with one) no longer lists sessions that edited another project's file of the
  same name. A relative path such as Codex's `README.md` matched any full path
  ending in it; it now matches only when read against the folder its session
  ran in. On one machine, 6 of the 8 sessions `trace` gave for this
  repository's README had edited other projects' READMEs, and such hits could
  push real ones past the 20-result cap; across 115 files, 140 wrong hits are
  gone and no right one is lost.
- When no session recorded a path, `trace` falls back to the file name and
  says the folder has moved. It now lists only paths that are gone, as a moved
  folder's would be; before, it listed every same-named file that still exists
  elsewhere, such as 20 unrelated `.gitignore` files.

## [0.34.15] - 2026-10-27

### Fixed

- `blame` now finds a session that was started in a subfolder of the
  repository. Codex records an edit relative to the folder it ran in, so a
  session run from `sub/` that edited `x.rs` never matched the blamed path
  `sub/x.rs`, and its lines were left unattributed while `trace` listed it. A
  path that ends the blamed path now counts when, read against the session's
  folder, it is the blamed file; `x.rs` edited from the repository root is
  another file and still does not count.
- When several sessions fit a commit equally well, `blame` prefers the ones
  that ran on the repository's path, and that check now compares folders, not
  text. A session in `/w/deck` counted as running above the repository
  `/w/deckweb` and could take a line from the session that wrote it, while a
  session in one of the repository's own subfolders did not count and lost the
  line to one started in a folder above. Now a subfolder counts, a name that
  merely starts the same way does not, and a tie between a subfolder and a
  folder above stays ambiguous.

## [0.34.14] - 2026-10-27

### Fixed

- `blame` no longer weighs a session that edited a file of the same name in
  another repository. Its candidates were every session whose touched path
  ended in the blamed path, so a `README.md` edited elsewhere at the same time
  made a line "ambiguous", and alone it would have been named as the line's
  author. A candidate whose matching paths all name another existing file,
  outside this repository and its worktrees and clones, is left out; a path
  that no longer exists still counts. On one real repository this resolved 22
  of 27 README lines that another repository's session had made ambiguous.

## [0.34.13] - 2026-10-26

### Fixed

- An OpenCode, Continue or gajae-code edit that failed no longer counts as a
  file the session edited. Each tool records the outcome beside the call -
  OpenCode a tool part in `error` state (the user rejected the permission, or
  the text to replace was not found), Continue a `toolCallStates` status of
  errored or canceled, gajae-code a tool result with `isError` - but the
  readers took the file at call time, so `trace`, `files` and `blame` named the
  session for a file it never changed. A call with no outcome yet still counts.
  Sessions from these tools are re-parsed once on the next sync.

## [0.34.12] - 2026-10-26

### Fixed

- A Codex patch that did not apply no longer counts as a file the session
  edited. In a rollout, an `apply_patch` that failed verification ("Failed to
  find expected lines", "Failed to read file to update"), returned a nonzero
  exit code, or was aborted by the user writes none of the files it names, yet
  `trace`, `files` and `blame` named that session for each of them, including
  files that never existed. A patch run from an `exec` script is dropped only
  when apply_patch's own error ended the script and the script ran that one
  patch. Codex rollouts are re-parsed once on the next sync.

## [0.34.11] - 2026-10-26

### Fixed

- A file an edit was attempted on but never changed no longer counts as a
  file the session edited. A Claude Code Edit or Write the user rejected, or
  that failed ("File has not been read yet", a subagent told to return
  findings as text), and a Codex patch declined at approval or that failed
  to apply, made `trace`, `files` and `blame` name that session for the
  file. On one real index this removed 281 of 8179 session-file links, each
  one checked to have only failed edits. Claude sessions and Codex history
  threads are re-parsed once on the next sync.

## [0.34.10] - 2026-10-24

### Added

- The web interface is available in Russian (#89, thanks @AlexZander85).

### Changed

- The README notes that Google retired Gemini CLI for personal accounts on
  2026-06-18 in favour of Antigravity CLI. Gemini CLI chats already on disk
  are still indexed; Antigravity support is in review (#83).

### Fixed

- A large Codex rollout in the older format, which writes each message at the
  top level, keeps its conversation from the middle again. 0.34.9 kept only
  the current record types there.
- The 0.34.9 fixes for Codex calls declined at approval and Gemini `/rewind`
  now reach sessions indexed before it: Codex history threads and Gemini
  chats are re-parsed on the next sync. Before, a session that no longer
  changed kept its old reading. Each indexed session now records which reader
  parsed it, so a session that a `sessionwiki mcp` started before the upgrade
  re-parses with its old reader is re-parsed again instead of keeping that
  result.

## [0.34.9] - 2026-10-21

### Fixed

- A large Codex session keeps its whole conversation again. From the middle
  of a session over the size cap only conversation records are kept, within a
  64 MB budget, but `compacted` records (which carry the whole replacement
  history) were matched as conversation and filled the budget first. In one
  real 1.1 GB rollout 5025 of 6960 conversation lines were dropped; it now
  indexes 2883 messages instead of 926. Large sessions are re-parsed once on
  the next sync.
- A Codex session that changes path keeps its summary. Moving a rollout to or
  from `archived_sessions`, or between a rollout and the history database,
  carried its tags and note over and deleted its summary.
- A rollout taken back out of `archived_sessions` is listed once. The old row
  was still live when the new one was indexed, so both stayed, and its tags
  stayed on the archived one.
- A Codex call declined at approval is recorded as not having run, instead of
  as a success.
- A Gemini chat's `/rewind` is applied: the rewound turns are no longer
  indexed and searchable as part of the chat.
- Gemini subagent transcripts, written under their parent chat at
  `chats/<parent session id>/`, are indexed, as subagents.

## [0.34.8] - 2026-10-20

### Fixed

- `blame` no longer credits a session that edited a different file whose name
  differs only where the asked-for name has `_` or `%`. The path query used
  them as SQL wildcards, so `blame KBS/newdata_copy.py` also took the session
  that edited `KBS/newdata copy.py`. `trace` already treated them literally.

## [0.34.7] - 2026-10-20

### Fixed

- **Gemini CLI chats written as JSONL are indexed.** Gemini CLI now saves
  each chat as `chats/session-*.jsonl` - a header line, `$set` lines and one
  line per message, repeated as the message grows - and only `.json` chats
  were read, so every Gemini session since mid-June was missing (14 on one
  machine). These chats are now folded back into their messages, each
  message once.
- **Search finds what was said in the middle of a large session.** A session
  over the 256 MB cap was indexed from its first 256 KB and last 512 KB only,
  so its conversation in between could not be searched: one 352 MB session
  had 1,733 prompts and 7 of them were indexed. The user and assistant text
  of a windowed Claude or Codex session is now kept from the whole file (up to
  64 MB of it); tool output in the middle stays windowed. The live windowed
  sessions are re-parsed once on the next sync.

## [0.34.6] - 2026-10-20

### Fixed

- **`trace` finds the files a large session edited.** A session over the
  256 MB cap is indexed from its first 256 KB and last 512 KB, and the files
  it edited in between were dropped with the rest of the middle. On one
  machine 82 sessions (29 GB) were windowed, the longest-running work there
  was, and three of them that edited 574 to 585 files each traced to none.
  The edited files are now read from the whole file, parsing only the lines
  that carry a tool call or patch, so `trace`, `files` and `blame` see every
  edit; the text stays windowed. The live windowed sessions are re-parsed
  once on the next sync (about 4 s per 3 GB).
- **`doctor` no longer calls Codex's thread history unreadable.** Since
  0.34.4 Codex's thread history is indexed from `thread_history_1.sqlite`,
  one database file, and `doctor` read every store as a directory, so it
  reported `store: codex - present but unreadable: Not a directory` on any
  machine that has one. A store that is a file is now opened as a file.

## [0.34.5] - 2026-10-19

### Fixed

- **Compressed Codex rollouts are indexed.** Codex compresses older rollouts
  in place, `rollout-*.jsonl` becoming `rollout-*.jsonl.zst`, and only
  `.jsonl` files were read. On one Mac, 678 of 684 rollouts were compressed:
  none were indexed, and the 667 indexed earlier were listed as deleted
  sessions once their `.jsonl` disappeared. Compressed rollouts are now read
  (with the pure-Rust `ruzstd` decoder), and each one absorbs its row from
  before compression, tags and note included.

## [0.34.4] - 2026-10-19

### Fixed

- **Codex threads kept only in Codex's history database are indexed.**
  Codex now stores thread history in `thread_history_1.sqlite` and may
  remove a thread's rollout file after migrating it. Only rollouts in
  `~/.codex/sessions` were read, so one machine with 2,987 threads of history
  showed 128 Codex sessions. Threads without a rollout are now read from the
  history database, with cwd and sub-agent status taken from `state_5.sqlite`.
  Rollouts in `~/.codex/archived_sessions` are indexed too (#65).
- **A Codex session that changes location is listed once.** Codex moves a
  rollout into `archived_sessions` when a thread is archived, and the rollout
  may be removed while the thread continues in the history database. The old
  location's row was kept as a deleted session beside the new one. It is now
  merged into the new row, with its tags and note, unless it held more
  messages than the new copy.
- **`sync --tool codex` covers every Codex store.** It ran only the first
  adapter of that name.

- **Codex MCP calls recorded only as events are indexed.** Some Codex
  rollouts record an MCP call only as its `mcp_tool_call_end` event, with no
  tool call or output beside it, and those calls were not indexed at all -
  5698 on one machine, 1608 of them failures. Each is now indexed from its
  event, marked as an error when it failed; a call that also has a tool call
  record is not repeated. Codex sessions already indexed are re-parsed once
  on the next sync.

## [0.34.3] - 2026-10-13

### Fixed

- **A failed Codex MCP tool call shows as an error.** Codex states an MCP
  call's failure only in its `mcp_tool_call_end` event (`isError`, or an
  `Err` result); the tool output beside it is plain text, so every failed
  MCP call read `⇒ ok` - 257 calls on one machine (an earlier version of this
  entry said 1782, counting calls that were not indexed at all; see 0.34.4).
  The event is now read and the call is marked as an error. The first open after
  upgrading marks Codex sessions already indexed for one re-parse, so the
  next sync corrects them too; sessions whose rollout is gone keep what they
  had.

## [0.34.2] - 2026-10-10

### Fixed

- **Workflow journals are no longer indexed as sessions.** Claude Code keeps
  a workflow's bookkeeping beside its agents' transcripts
  (`subagents/workflows/wf_*/journal.jsonl`). Every `.jsonl` was indexed, so
  each journal became an empty subagent session whose project was the
  `wf_*` folder name - 257 on one machine. Only `agent-*.jsonl` under
  `subagents/` is read now, and the next sync removes the journal rows.
- **A session with nothing indexed is removed, not archived, when its file
  goes.** Archiving was documented to skip such sessions, but kept every one
  as an archived row with no transcript.
- **"Kept after the tool deleted them" counts sessions.** `stats`, `doctor`
  and the sync summary counted every archived transcript, subagent
  transcripts included, beside a session total that counts only main
  sessions - one store read "3959 sessions, 2050 kept" where 611 sessions
  were kept. The kept count now covers main sessions, like the total.

## [0.34.1] - 2026-10-10

### Fixed

- **An older sessionwiki no longer rebuilds an index a newer one wrote.** The
  cache is rebuilt whenever its schema version differs from the binary's, and
  that included a NEWER version. A `sessionwiki mcp` server started before an
  upgrade keeps running the old binary, so after 0.34.0 rebuilt the index each
  such server rebuilt it back in the old format on its next call - keeping
  only the last 15 minutes of sessions - and the next CLI run rebuilt it again.
  An older binary now refuses a newer index with an error that says to
  restart it. Servers already running 0.33.x or earlier do not have this
  check: restart agent sessions that run `sessionwiki mcp` after upgrading.

## [0.34.0] - 2026-10-10

### Changed

- **Tool calls are stored as one compact line each**, for example
  `→ Bash(cargo test) ⇒ error · 12 lines — error[E0425]: ...`: tool name,
  primary argument, status, output line count, and the first output line on
  error. Every adapter (Claude Code, Codex, Gemini, gptme, Cline/Roo/Kilo,
  Continue, Gajae, OpenCode) emits structured calls and results, and one shared
  renderer formats them. The line is what `show`, `brief`, the web UI, MCP and
  search see; `brief` now includes tool lines by default, and `--tools`
  (`brief`) or `--full` (`show`) append bounded output. The default `summary`
  mode keeps only the compact line in new archives, plus the first output line
  for failed calls. This is less than the previous behavior, which kept up to
  500 characters of tool output. Run `sessionwiki tool-output full` to retain up
  to 8 KB of redacted output per result in archive mode. The per-index setting
  lets embedders choose; switching to `summary` clears full output from live
  sessions, while already archived sessions keep what they have. Switching
  back to `full` marks live sessions for re-parsing.
  Tool output is no longer indexed, so a string that appeared only mid-output
  no longer matches a search. Schema v9 rebuilds each existing index once on
  first use; the first run after upgrading re-parses every session, which can
  take a while on a large store.

## [0.33.2] - 2026-10-08

### Fixed

- **A prodex consult is listed once, and `show <id>` opens it.** The bridge
  registry can name one repo under two spellings - a case-variant working
  directory on a case-insensitive drive, or a symlink. Each spelling was
  walked, so every consult in that repo was indexed twice under one id: it
  appeared twice in `list` and `projects`, and `show`, `brief` and `resume`
  stopped with "ambiguous id" even given the full id. On the machine this was
  found, 524 consults were doubled. Discovery now walks each repo once, and
  the next sync drops the second copy instead of archiving it.
- **Gemini CLI sessions carry their project's path.** Gemini CLI names each
  project's chat folder with a short slug and records the real directory in
  a `.project_root` file beside it. The adapter used the slug, so a Gemini
  session's project read `myrepo` while every other tool's read
  `/home/me/myrepo`: `list --project /home/me/myrepo` and the session-start
  recall hook left Gemini sessions out, and `projects` showed the repo twice.
  On the machine this was found, all 46 Gemini sessions were affected. The
  project is now the recorded path, and the first open after upgrading
  corrects rows already indexed.
## [0.33.1] - 2026-10-07

### Fixed

- **Stopping the npm-installed command stops sessionwiki.** The npm launcher
  ran the binary with a blocking call that cannot pass a signal on, so a
  SIGTERM or SIGHUP to the `sessionwiki` command - from a process manager, an
  MCP host or `kill` - ended only the launcher. The binary kept running with
  no parent; `sessionwiki web` kept its port, so the next start failed. The
  launcher now forwards SIGINT, SIGTERM and SIGHUP to the binary and exits
  with its status once it has stopped.

## [0.33.0] - 2026-10-07

### Added

- **npm package.** `npm i -g @youdie006/sessionwiki` installs the prebuilt
  binary for macOS (arm64, x64), Linux x64 or Windows x64 through a platform
  package, with no install script. On macOS it upgrades the binary in place,
  which keeps folder-access grants across releases.

### Fixed

- **macOS keeps a folder-access answer across upgrades, and stops asking
  about media folders.** macOS ties a grant to read Documents or Desktop to
  the binary's path and code signature. Releases were ad-hoc signed, so every
  version looked like a new program and asked again - on one Mac, Desktop,
  Documents and Downloads prompts plus 23 Photos prompts in three days. The
  macOS binaries are now signed with one stable identity, so at a stable
  install path (npm, the install script) an answer given once still applies
  after an upgrade; Homebrew installs each version under a new path and still
  asks once per upgrade. The aider walk also no longer enters Pictures,
  Movies, Music or the Trash, which cannot hold a project and raised the
  Photos and Media Library prompts.
- **Index fix-ups also catch rows written later by an older binary.** The
  one-off passes that respell project paths (0.30.2) and mark Codex sub-agent
  threads (0.32.0) ran once and recorded that they had. A `sessionwiki mcp`
  started before an upgrade keeps running the old code, often for weeks, and
  kept writing case-variant paths and sub-agent threads as main sessions
  afterwards; nothing looked at those rows again. On the machine this was
  found, four sub-agent threads reappeared in `list` within an hour of
  upgrading. Each pass now records how far it has checked and, on every
  open, looks only at rows written since, so a late write is fixed the next
  time the current version opens the index.

## [0.32.0] - 2026-10-07

### Added

- **Search can filter by message role.** `--role user,assistant` limits matches
  to conversation messages before ranking and grouping, so tool output cannot
  crowd conversational matches out of search results. The web search and MCP
  search accept the same role filter.

### Fixed

- **Codex sub-agent threads are sub-agent sessions, not main ones.** Codex
  marks a sub-agent's rollout in its `session_meta` (`thread_source:
  "subagent"`), and its task arrives encrypted, so these threads have no
  readable prompt. The adapter ignored the marker and listed every one as a
  main session: on the machine this was found, 2,924 of 4,843 Codex "main"
  sessions were sub-agent threads, crowding `list`, the `projects` counts,
  `recent_sessions` and the recall hook with "(no user prompt)" rows. They
  are now sub-agent sessions like Claude Code's, and the first open after
  upgrading reclassifies rows already indexed by reading each file's first
  line (a one-off update, not a rebuild).
## [0.31.0] - 2026-10-06

### Added

- **Configurable FTS5 tokenizer.** `sessionwiki tokenizer` shows the current
  setting; `sessionwiki tokenizer <SPEC>` changes it and rebuilds only the FTS
  table from indexed messages. The choice is stored in the index and shared by
  CLI, web, and MCP.

### Fixed

- **Search now ANDs terms within one message.** Previously a multi-word query
  matched only the exact substring, including its whitespace. Quote words to
  require a phrase; phrase whitespace follows the configured tokenizer, and
  short terms still work alongside longer indexed terms.

## [0.30.2] - 2026-10-06

### Fixed

- **`projects` LAST and its order follow last activity, as `list` does.**
  0.30.1 moved `list` to each session's last message, but `projects` still
  took LAST from when its newest session began, so a project worked in
  yesterday through a session started in May showed May and ranked below
  projects untouched since. LAST is now the latest activity of any session in
  the project (its start when it has none), and equally busy projects are
  ordered by it. The web UI's project list reads the same value.
- **One directory is one project, however its case was typed.** On a
  case-insensitive filesystem (WSL's `/mnt/<drive>`, macOS by default) a
  shell can enter the same directory as `apo` or `APO`, and each tool records
  what it was given, so one project split into case-variant rows - on the
  machine this was found, 14 projects, one of them 23 + 514 sessions. Project
  paths are now stored as spelled on disk, and the first open after upgrading
  respells the rows already indexed (a one-off update, not a rebuild). The
  recall hook's exact project match uses the same spelling. A case-sensitive
  filesystem is unaffected: two directories that differ only in case stay
  two projects.

## [0.30.1] - 2026-09-30

### Fixed

- **`list` and the `recent_sessions` MCP tool order sessions by last
  activity.** They sorted by when a session started, so one resumed day after
  day sank below everything begun since: on the machine this was found, eleven
  sessions active within the last day ranked as weeks or months old, including
  the one asking. They now order by the session's last message (its start when
  it has none), and `list`'s WHEN column shows that time.

### Changed

- Session rows from `recent` carry `last_active` (RFC 3339, the last message's
  time) alongside `started` in `--json` and MCP output. `started` keeps its
  meaning.

## [0.30.0] - 2026-09-27

### Added

- **`Codex::in_home` and `ClaudeCode::in_home` for embedders with several
  installs of one tool.** A program that drives more than one Codex or Claude
  Code install (`~/.codex2`, `~/.claude4`, and so on) can now build one adapter
  per install root; the adapter knows its own session sub-directory layout, so
  the caller passes only the home. Every install keeps the shared tool name, so
  parsing by tool name still works for rows from any of them. An adapter built
  this way scopes deletion reconciliation to its own root, so syncing one
  install never archives another install's rows. `Codex::default()` and
  `ClaudeCode::default()` keep reading the stock locations and scope
  reconciliation to those locations.

### Fixed

- Syncing a stock Codex or Claude Code install no longer archives rows from
  another install. A later sync restores archived rows when their source still
  exists, without reparsing an unchanged transcript.

### Maintenance

- `clap` is updated from 4.6.6 to 4.6.7.

## [0.29.0] - 2026-09-17

### Added

- Embedders can index custom adapters with `index::sync_with`, restrict deletion
  reconciliation to their own keys with `Adapter::reconcile_scope`, and render
  a redacted briefing without its source path with `commands::brief_markdown`.
  Custom tool names are preserved when reading sessions from the index.

### Fixed

- Sessions indexed by an external adapter stay readable in `show`, `brief`, MCP
  and the web viewer while their source files still exist. Readers without the
  adapter use the indexed transcript instead of failing with `unknown tool`;
  the embedder must sync again to publish later changes. Built-in adapters still
  reparse live files and report parse errors. The web viewer now shares the same
  reader as the CLI and MCP.
- Redirected synchronization logs omit terminal-only progress redraws while
  retaining warnings and per-tool summaries.

### Maintenance

- CI and the dependency auto-merge gate use `actions/setup-python` v7 with
  the existing Python 3.12 configuration.

Implementation and validation: [PR #26](https://github.com/youdie006/sessionwiki/pull/26)
and [PR #27](https://github.com/youdie006/sessionwiki/pull/27). Publication and
installation verification are recorded in the
[0.29.0 release](https://github.com/youdie006/sessionwiki/releases/tag/v0.29.0).

## [0.28.0] - 2026-09-15

### Fixed

- **Large MCP turn responses remain valid JSON.** Quote-heavy and backslash-heavy
  text can expand when JSON-encoded, exceeding the transport cap even when its
  raw character count fits. Single-turn responses now retain the longest text
  prefix that fits after serialization and set `clipped: true`, preserving the
  schema, turn identity and original retained byte count. If metadata alone
  exceeds the limit, window and turn responses return a small, parseable JSON
  error instead of a success payload cut in the middle of a string.

## [0.27.0] - 2026-09-15

### Fixed

- **Exported session fields are redacted before they are shortened.** Briefs,
  summarizer input and MCP session windows now redact the complete parsed
  session, including titles, project names, paths, messages and edit snippets,
  before rendering or applying length limits. This covers reparsed originals
  and archived sessions, including single-turn MCP output. A token cut by a
  preview or output budget can no longer survive as an unrecognized prefix.
- **Prompt-derived titles and previews no longer preserve credential fragments.**
  Redaction runs before adapters select a title line or shorten tool previews,
  edit snippets and Prodex answers, including structured tool input and
  multiline private keys.
- **Turning off swapdex serving clears subsequent account badges.**
  `serve-off` is an explicit attribution boundary until a later `use`,
  `restore` or `serve` event activates an account. It no longer leaves the
  previous account attached to later sessions.
- **Incomplete discovery no longer looks like deleted sessions.** Aider walk
  errors and unreadable, malformed or partial Prodex registry/task listings
  retain the incomplete-discovery signal, so synchronization preserves live
  sessions until a complete listing can reconcile deletions.

### Maintenance

- Update `dirs` from 6 to 7. Its upstream behavior change affects Windows
  `preference_dir()`, which sessionwiki does not call; the home, configuration
  and data directory functions used for session discovery remain unchanged.
  The update was checked against the current implementation with 205 Rust
  tests and clippy passing.
- Dependency auto-merge now verifies the workflow, repository, PR and tested
  commit, checks permitted manifest and lockfile changes structurally, and
  rechecks current state before merging. Incomplete or inconsistent evidence
  refuses the merge. The gate's regression tests run in CI.

Implementation and review: [a1ef3a8](https://github.com/youdie006/sessionwiki/commit/a1ef3a884cacecb1b495fff5710709318de8ffc0)
and [PR #21](https://github.com/youdie006/sessionwiki/pull/21). Publication and
installation verification are recorded in the
[0.27.0 release](https://github.com/youdie006/sessionwiki/releases/tag/v0.27.0).

## [0.26.0] - 2026-09-07

### Fixed

- **A brief no longer carries credentials off the machine.** `redact` has
  stripped them since the beginning, at INDEX time, because the index outlives
  the original session. `brief_text` does not read the index: `load_session`
  parses the ORIGINAL file whenever it still exists and falls back to the index
  only when it is gone, so the same session was redacted only in the case where
  the original had been deleted. Every consumer sends the text somewhere -
  `brief` is written to be pasted into another session and says so, `recall`
  prints it for the same, `summarize` pipes it to an external LLM CLI, and the
  MCP server hands it to a connected agent. That MCP handler already neutralized
  the title, redacted the username out of the project path, stripped control
  characters and framed the whole thing as untrusted data; credentials were not
  on the list. Stripped now in `brief_text`, where all four meet, and before the
  length budget - a truncated secret is still a leaked prefix.
- **The timeline read is bounded for real.** `load_events` said it capped the
  read as defence against a hand-edited file, then read the whole thing into
  memory and capped only the PARSE. It takes the last megabyte as whole lines
  now, dropping the one the seek cut in half so no fragment can parse as a
  different record.
- **A partial store walk says it skipped deletion reconciliation.** Two paths
  carry that rule - do not archive off an incomplete listing - and only one said
  so. The silent one is the path aider takes, and aider is rootless: its walk
  covers the whole home under a two-second budget, and the flag is set on any
  cap-hit. On a large home reconciliation could be skipped every run with
  nothing saying why a session the tool deleted was still listed.

## [0.25.0] - 2026-09-07

### Fixed

- **The account badge names the account that actually served the session.** It
  counted swapdex's `use` and `restore` events, which was right while every
  switch went through them. Switching now goes through swapdex's proxy, which
  writes `serve` - and `serve` was being dropped as "not a switch". True of the
  event, false of the question: on a machine driven by the proxy it is the only
  record of which account was live. Every claude-code session on this one was
  badged with nothing while 190 serve events named three accounts, and codex
  sessions carried a `use` from six weeks before the account last changed. The
  newest event now wins whatever kind it is; a session that predates every event
  still gets no badge rather than a guess. The parsing half is separated from
  the file read so which lines survive it is testable.
- **The prodex adapter's four reads are bounded.** The memory cap on session
  files exists so a corrupt or hostile one cannot exhaust the process. Eleven
  adapters went through it; prodex read its task JSON, its result JSON, its
  registry and its artifact with a plain `fs::read` - one of them under a comment
  saying "Bounded read; an artifact is normally a few KB", where the largest file
  in the real store on this machine is 35 MB.
- **`doctor` no longer reports a failed count as zero.** The count of sessions
  kept after their tool deleted them turned any error - a schema without
  `archived_at`, a locked database - into "0 kept", which reads as the good news
  that nothing was lost.
- **A redirected run leaves the real index alone.** The one-time migration from
  the old directory names looked under `dirs::data_dir()` whatever `SESSIONWIKI_DATA`
  said, then renamed what it found into the destination - so a redirected run
  could move the user's real index out of their home.
- `ResumeInfo`'s doc claimed every supported tool encodes its native session id
  in the filename. Two of twelve do, and the code already answers None for the
  rest; only the comment was wrong.

### Added

- `doctor` says when a store's location could not be worked out, instead of
  giving that the same silence as a tool that is not installed. Defence, not a
  reproduced failure - on Unix `dirs` falls back to the passwd database, so
  unsetting HOME does not make it fire.

## [0.24.0] - 2026-08-20

### Fixed

- **A renamed folder no longer hides a file's history.** `trace` matched paths by
  suffix, so a file traced by the path it has TODAY found nothing once its folder
  had been renamed - and said "no session touched a file matching ...", about a
  file whose entire history was in the index under the old directory. Found on a
  real machine where `~/Project/lunch` had become `~/Project/slack`: the sessions
  that built it were all there and unreachable by the path the owner had. A full
  path that matches nothing now retries with the file NAME, which is the part
  that survives a move, and says so - otherwise the older paths listed underneath
  read as the index pointing somewhere wrong.

## [0.23.0] - 2026-08-19

### Added

- **`migrate --config-dir`** - write a transcript into a specific account's
  store instead of always the default one. This is what lets a conversation be
  handed to another ACCOUNT: under swapdex's slot model each account has its own
  `CLAUDE_CONFIG_DIR` with its own `projects/` inside, and migrate wrote into
  `~/.claude` unconditionally, so on a slotted machine the copy succeeded and
  landed where the resuming account never looks. Without the flag
  `CLAUDE_CONFIG_DIR` is honoured, so running inside a slot's own shell already
  does the right thing; failing both, the single default store.

- **`doctor`** - a read-only, one-command health check of this machine's setup:
  which session stores exist (and how many sessions each holds), the index's
  schema currency and core-table presence, how many sessions are indexed (and
  kept after a tool deleted them), and the version. `--json` for scripts. When
  something looks empty or stale the cause is usually mundane; this surfaces it
  so a bug report starts from facts.
- **Secrets are redacted from message text, titles, edit snippets, and synopses
  at index time** (`[redacted:<kind>]`) - PEM private keys, JWTs, and prefixed
  tokens (OpenAI, AWS, GitHub, Stripe, Google, Slack). The index outlives the
  original session via archive mode, so credentials must not land in it. A
  schema bump scrubs sessions already indexed before this change. Dependency-free
  and tuned to high-confidence shapes to avoid redacting ordinary code.
- **File history - "how this file was written".** Clicking a file in the web UI
  now shows its evidence chain: the sessions that edited it, newest first, each
  with what it actually changed - the edit kind (edit/write/multiedit) and a
  bounded snippet of the new code, not just a bare list of sessions that touched
  the path. This answers *why does this file look like this*. Backed by a new
  `edits(session_id, path, kind, ts, snippet)` cache table (extracted at parse
  time from each tool call, rebuilt on a schema bump like the other cache
  tables), an `evidence_for(path)` assembly, and a `GET /api/file?path=`
  endpoint. `trace`/`blame` still give the path-level provenance.

## [0.22.0] - 2026-07-20

Open any live session by its native id (codex-rollout or claude-transcript
UUID), closing the harness tower to sessionwiki join gap.

### Added

- `session_window`/`show` now resolve a session by its native codex-rollout or
  claude-transcript UUID (full or prefix), in addition to the sessionwiki short
  id. Live sessions not yet indexed are found via a disk-scan fallback (no
  sync), so a session started today opens by native id in one call.
- `list`, `recent_sessions`, and `search` JSON now expose a `native_id` field
  (the UUID only, never the absolute path). Closes the harness tower to
  sessionwiki join gap.

## [0.21.0] - 2026-07-16

Harden the MCP read path for in-loop agent-to-agent reads: a stable window
contract and freshness on every read tool.

### Added
- **`session_window` (MCP) now returns versioned JSON** (schema `sessionwiki.window/1`) instead of an unversioned text render, so a reading agent can parse a sibling's window rather than scrape it. Fields: `id`, `tool`, `project`, `title`, `started`/`ended`, `messages` (total turns), `large` (indexed head+tail), `budget_tokens`, `omitted_leading`, `turns[]` (each with `i` = stable turn index, `role` = user/assistant/tool, `text`, and `truncated` or `folded`+`bytes`), and a `drilldown` hint. Deterministic for a fixed (id, budget); the recent tail is kept within budget and the whole object is size-guarded so it is always valid JSON. Tool output is folded head+tail and byte-bounded.
- **Per-turn drill-down:** `session_window(id, turn=<i>)` returns that one turn's full retained text (schema `sessionwiki.turn/1`), untruncated by the window's folding/cap - fetch-full-content-on-demand for a turn seen in a window. (Tool outputs are already capped at parse time, so this recovers folded-out lines, not adapter-dropped bulk.)

### Changed
- **`search_sessions` (MCP) freshens in the background**, matching `recent_sessions` (0.20.2): it serves the current index immediately and kicks a bounded incremental sync, so a just-created sibling shows on a subsequent search without the request ever waiting on the store walk. `session_window` already reads the target file directly (live). This closes the last MCP read tool that did no freshen; incremental parsing (only files whose mtime/size changed) was already in place.

## [0.20.2] - 2026-07-16

### Changed
- **`recent_sessions` (MCP) freshens in the background.** It returns the current index immediately (~200ms) and kicks a bounded incremental sync, so a just-started sibling session shows on the next call - without the multi-second blocking sync a full 46GB-corpus walk would cost. New `index::sync_bounded` re-parses only files modified since a floor and never archives a live session on a bounded run.

## [0.20.1] - 2026-07-16

### Fixed
- **Over-cap sessions are windowed (head+tail) instead of dropped.** A session past the 256MB read cap was skipped entirely - so the biggest sessions, the ones most worth referencing across sessions, vanished from search and `session_window`. They are now indexed from the first 256KB + last 512KB of lines (byte-bounded, no full scan), partially readable, and flagged `[large]` in the title.

## [0.20.0] - 2026-07-15

Cross-session context for agents: one agent can now see what a sibling session
(or its other self) is doing, as the ACTUAL recent conversation - not a lossy
summary - bounded so it can't flood the reader's context.

### Added
- **`sessionwiki show <id> --window [--budget N]`** renders a session's real
  recent turns with tool outputs folded to head+tail, capped to the recent tail
  by a token budget, an orientation header + role labels, and a
  `show <id> --full` drill-down hint. `--live` reads the session file directly
  (0-delay), bypassing the index sync.
- **MCP discovery + read tools** so an agent can find and read sibling sessions:
  - `session_window(id, budget_tokens)` - the bounded window above, over stdio.
  - `recent_sessions(limit, tool, exclude_id)` - the most recent sessions across
    every tool ("who's around"), bounded to a recent window.
  - `related_sessions(id, limit, exclude_id)` - a session's siblings by shared
    project, edited files, or tags.
  Both discovery tools take `exclude_id` to drop your own session. This closes
  the chain: `recent`/`search`/`related` -> id -> `session_window`. Verified
  cross-tool (Claude Code and Codex sessions see each other).

### Fixed
- (from 0.19.4) `sessionwiki web` no longer auto-opens a browser on WSL, where
  `xdg-open` pops a stray WSLg/Windows window on every start and often can't
  reach the loopback server; it prints the URL instead. Native Linux/macOS keep
  auto-open.

## [0.19.4] - 2026-07-14

### Fixed
- **`sessionwiki web` no longer pops a stray browser window on WSL.** On WSL,
  `xdg-open` launches a WSLg/Windows browser on every `web` start - a blank
  window each time you restart the server during development, and it often
  cannot even reach the loopback address. `web` now detects WSL and prints the
  URL instead of auto-opening; native Linux/macOS keep the existing auto-open
  (still overridable with `--no-open`).

## [0.19.3] - 2026-07-14

Codex/Claude transcript formats drifted; a comprehensive audit against the
real corpus caught five parsing gaps, all fixed here.

### Fixed
- **Codex user messages no longer index twice.** Current rollouts carry each
  prompt BOTH as an `event_msg` and as a `response_item`; the adapter now
  cancels each pair exactly once (counted, so a genuinely repeated prompt -
  "continue" twice - still keeps every occurrence).
- **AGENTS.md instructions no longer become session titles.** The new
  `# AGENTS.md instructions for <path>` / `<INSTRUCTIONS>` user-message shape
  is treated as boilerplate, like the older environment-context wrappers.
- **2025-era Codex rollouts parse again.** Files that predate the `payload`
  envelope (bare `{"type":"message"}` lines) indexed as empty "(no user
  prompt)" sessions; they are now parsed like their modern counterparts.
- **`custom_tool_call` items are indexed** and their inputs feed `trace` /
  `blame` path extraction, so edits made through custom tools are no longer
  invisible.
- **Claude titles use the `ai-title` event.** Current Claude Code transcripts
  stopped emitting `summary` events; titles now fall back summary ->
  ai-title -> first user message instead of always the first prompt.

## [0.19.2] - 2026-07-07

### Fixed
- A crafted/corrupt prodex task id with a multibyte character at a slice
  boundary could panic the parser - and a parse panic aborts the WHOLE
  indexer, so one bad `.bridge` file bricked every `list`/`search`/`scan`.
  The id timestamp parse now gates on ASCII digits before slicing (found by
  an adversarial audit; same bug class swapdex fixed a release earlier).
- `list --account` no longer silently returns fewer rows than `-n`: the
  badge filter is computed post-query, so the query now over-fetches and
  truncates after filtering.

## [0.19.1] - 2026-07-07

Ecosystem-walkthrough fixes, from a fresh user's chair.

### Added
- `--account <name>` on `list` and `search`: filter by the swapdex @badge,
  which the help text now explains.
- prodex consult titles are the QUESTION (prompt first line) instead of
  prodex's identical auto-title, so a list of consults is tellable apart.
  The derived-cache version was bumped, so existing rows re-index once.

### Fixed
- A consult whose answer artifact was written but whose result JSON was not
  (the crash path the durable ledger exists for) is indexed again: the
  artifact is read independently of the result.
- `trace` with an ABSOLUTE editor path now finds sessions whose stored paths
  are repo-relative (prodex bundles relative paths; matching honors the `/`
  boundary, so suffix lookalikes cannot match).
- `--tool` help enumerates prodex and points at `scan` for the full list;
  the scan footer pluralizes correctly; scan's prodex PATH shows the store
  directory (not the registry file); the prodex resume error message no
  longer has a hanging indent.

## [0.19.0] - 2026-07-07

### Added
- prodex adapter: every ChatGPT Pro consult made through
  [prodex](https://github.com/youdie006/prodex) becomes a searchable session -
  the bridge task is the question, the result/answer artifact is the answer,
  and files bundled with a task become `trace` provenance links. Discovery
  reads the machine-wide bridge registry prodex >=0.11.0 maintains
  (`~/.local/share/prodex/bridges.json`); registered repos that no longer
  exist are skipped silently. `resume` on a prodex session prints the ChatGPT
  thread URL it ran in (only an `https://chatgpt.com/` URL is ever surfaced).
- Session ids for prodex are the same short stable 12-hex shape as every
  other tool (task ids share a long date prefix that would defeat prefix
  addressing).

### Changed
- `list`/`search` truncate the ID column display at 13 chars so an unusually
  long id cannot shear the table (ids are prefix-addressable).

## [0.18.1] - 2026-07-07

### Fixed
- `trace` and `related` rows now carry the swapdex `account` field like
  `list`/`search` do (the annotate call was missing on those two query paths,
  so the same session showed a badge in search but never in a file trace -
  across CLI, web, and MCP).

### Changed
- Timeline reads are capped to the newest 4000 lines (defense in depth;
  swapdex itself bounds the file to ~1000 events), and account names are
  control-char-stripped at the source so every consumer gets a terminal-safe
  badge. `search` no longer prints a trailing space on badge-less lines.
- The web UI shows the account badge in each session's meta line.

## [0.18.0] - 2026-07-07

### Added
- swapdex integration: `list` and `search` badge each session with the
  account profile active when it ran (`@work`), and `--json` rows carry an
  `account` field (null when unknown). Attribution reads swapdex's switch
  timeline read-only and never guesses - sessions that predate the first
  recorded switch stay unbadged. No swapdex on the machine, no change.
  The MCP server neutralizes the new field like every other free-text one.

## [0.17.0] - 2026-07-03

### Added
- `sessionwiki mcp`: a stdio MCP server exposing three read-only tools
  (`search_sessions`, `trace_file`, `get_session_brief`) to any MCP client
  (Claude Code, Cursor, and others). Cross-tool session search as agent tools;
  100% local, read-only, no sync. Register with
  `claude mcp add sessionwiki -- sessionwiki mcp`.
- Homebrew: `brew install youdie006/tap/sessionwiki` (macOS and Linux).

### Changed
- aider re-indexing is now linear instead of O(runs^2): a long
  `.aider.chat.history.md` is split once per sync and cached, so a file with
  thousands of runs re-indexes in a fraction of the time (measured 1600 runs:
  ~11s to ~0.2s).

### Fixed
- A partial directory walk (an unreadable project directory) can no longer
  archive live sessions: file-per-session adapters now report a partial listing
  and sync skips deletion reconciliation for that run, the same guard shared
  stores already had.
- `sync` warns when a session fails to parse and reports honest counts
  ("indexed 12/14 (2 failed to parse)") instead of silently dropping it.
- Tags are NFC-normalized on add, filter, and remove, so a tag typed in
  decomposed form (macOS IME) matches everywhere, like every other indexed
  string.
- `resume` checks tool support before file existence, so tools without headless
  resume (aider, OpenCode) get the right message instead of a false "the
  session file is gone".
- `digest --since` with an out-of-range duration returns an error instead of
  panicking.
- A failed data-directory migration now warns with instructions instead of
  silently starting an empty index.

## [0.16.1] - 2026-06-19

### Fixed
- crates.io README: the dark-mode banner now uses an absolute image URL so it
  renders on the crates.io page, which (unlike GitHub) does not rewrite relative
  `<source srcset>` paths. Docs only; no code change.

## [0.16.0] - 2026-06-19

### Added
- `aider` adapter (11th tool): indexes Aider sessions from per-repo
  `.aider.chat.history.md`. Markdown-derived (roles reconstructed from line
  prefixes, no per-message timestamps); edited files come from aider's
  `Applied edit to` / `Creating empty file` lines. Discovery is a bounded,
  capped, symlink-safe walk (home by default, `SESSIONWIKI_AIDER_ROOTS` to
  scope) that opens only `.aider.chat.history.md` and logs nothing.
- Recall loop (Claude Code plugin): a SessionStart hook auto-injects a small,
  fenced "prior work in this project" brief at the start of a new session, so the
  agent gets long-term memory without being asked. Exact-project scoped,
  `--no-sync` (tens of ms), zero output on fresh projects, always exits 0. The
  injected recall is fenced as untrusted data and sanitized; the LLM synopsis is
  not auto-injected. New internal `sessionwiki hook session-start` command.

### Changed
- Schema stability on the road to 1.0: the durable index tables (summaries,
  tags, notes, archive) now version independently of the disposable cache via a
  `meta.durable_version` and a forward-only, additive-only migration runner, so
  user curation and archived sessions survive every upgrade. All index
  connections now set a `busy_timeout`. No behavior change in this release (zero
  migrations registered).

### Security
- `search` strips control bytes (ESC/OSC/C1/DEL) from terminal snippets, so an
  untrusted session body can no longer inject terminal escape sequences into the
  operator's terminal. The `--json` output and other commands already did this.

## [0.15.0] - 2026-06-18

### Added
- `blame <file> [-L start,end]`: git blame for the AI era - attributes each line
  to the AI session most likely behind the commit that last changed it, by
  joining `git blame --line-porcelain` with the index's per-file touch records
  and session time windows. Best-effort by design (`ambiguous`/`unattributed`
  are normal) and it falls back to file-level `trace` whenever git can't carry
  the weight. The git child runs with a hardened environment (no system config,
  no pager, `core.fsmonitor`/hooks neutralized, inherited `GIT_*` cleared), the
  path is passed after `--`, and `-L` is validated. `--json` for agents.

### Security
- `truncate()` - the choke point that renders session titles to the terminal in
  `list`/`search`/`trace`/`resume` - now strips C0/C1/DEL control characters, not
  just newlines and tabs. A session title is untrusted input (a planted session
  can set any title), so an unstripped ESC could have injected ANSI/terminal
  escape sequences into output. The web UI was already safe (it builds the DOM
  with `textContent`, never `innerHTML`).

## [0.14.0] - 2026-06-18

### Added
- **gptme adapter** (10th tool): indexes sessions from [gptme](https://github.com/gptme/gptme)
  (`~/.local/share/gptme/logs/<session>/conversation.jsonl`). Drops `pinned: true`
  system-prompt boilerplate and bare `system` role lines; handles Python's naive
  `datetime.now().isoformat()` timestamps (no UTC offset) by falling back to
  UTC-assumed parsing. Size-capped against `MAX_SESSION_FILE_BYTES`.
- Windows install: `scripts/install.ps1`, a PowerShell one-liner (`irm ... | iex`)
  that downloads + checksum-verifies the latest Windows binary and adds it to
  PATH. The shell installer already covers WSL, which is Linux.

## [0.13.0] - 2026-06-18

### Security
Hardening from a red-team pass of the launch surface (all confirmed with PoCs):
- The local web server now rejects requests whose `Host` header isn't the
  loopback name it bound (defeats DNS rebinding) and whose `Origin` is a
  different site (defeats a plain cross-origin `fetch`), so a malicious page you
  visit while `web` is running can't read your sessions from `127.0.0.1`.
- `resume` no longer auto-launches Codex or Gemini in a directory the session
  file claims. Those tools aren't directory-scoped, so a planted session could
  name any directory (with an attacker's `AGENTS.md` there) and have `resume`
  load it; it now prints the command for you to run unless the directory is
  verified, which only Claude Code's store layout allows.
- `resolve` escapes `LIKE` wildcards in a session-id prefix, so a web request for
  id `%` can no longer match and dump an arbitrary session.
- install.sh documents honestly that its checksum only detects a corrupted
  download, not a malicious release (the hash is co-located with the binary).

## [0.12.0] - 2026-06-18

### Added
- `digest [--since 7d]`: a markdown rollup of recent sessions grouped by project
  &mdash; what you worked on, the files each session touched, and any cached
  synopsis. It composes the timeline, provenance, and summaries the index already
  has, over a time window (`--since 2w`/`24h`/`90m`, `--project`, `--tool`,
  `--json`). The standup / PR-description / "what did I ship this week" view.

### Security
- `resume` no longer auto-launches the upstream tool (`claude`/`codex`) in the
  directory a session file *claims* unless that directory can be verified as the
  session's own &mdash; for Claude Code, by matching the store folder the session
  lives in (the folder name encodes the cwd). A session file is untrusted input;
  before this, a planted or prompt-poisoned session could point its `cwd` at an
  attacker directory and have `resume` launch the agent there, loading that
  directory's `CLAUDE.md` / `.mcp.json` / settings. When the directory can't be
  verified, `resume` now prints the command for you to run instead of launching.
- Session-file reads are size-capped (256 MB) and the OpenCode SQLite message/
  part reads are `LIMIT`ed, so a malicious oversized file or a db packing
  millions of rows into one session can't exhaust memory.

### Changed
- `SECURITY.md` now documents that `migrate` writes copies into the target tool's
  store and that `resume` validates the session-derived directory, instead of
  implying the tool only ever reads.

## [0.11.0] - 2026-06-18

### Added
- `migrate <id> <dir>`: copy a session so it can be resumed from a different
  project directory. Each tool ties a session to a directory differently, so
  migrate does the right thing per tool: Claude Code's resume is scoped to the
  project folder, so the transcript is copied into the target's folder; Codex
  resumes by id from any directory, so nothing is copied (it just prints the
  command); Gemini copies the chat into the target project's store
  (`~/.gemini/tmp/<sha256(dir)>/chats/`) and rewrites its `projectHash`. The
  original session is never modified.

## [0.10.0] - 2026-06-17

### Added
- `recall <query>`: one-shot recall — searches, lists the candidate matches, and
  briefs the top one, collapsing the usual search -> pick id -> brief loop into a
  single command (`--tool`/`--project`/`-n`/`--max-chars`/`--json`/`--no-sync`).
- `sync [--tool]`: build or refresh the index on demand, so later queries can run
  with `--no-sync`.
- `--no-sync` on `search`/`list`/`recall`/`show`/`brief`/`resume`/`trace`: query
  the already-built index without walking the stores — the fast path when the
  index is kept warm (e.g. a cron running `sessionwiki sync`).

### Changed
- `show`/`brief`/`resume`/`recall` resolve the id against the existing index first
  and only sync (all tools) when it is genuinely unknown (no prefix match) - an
  already-indexed id, including an *ambiguous* prefix, no longer triggers a full
  walk of every store (notably the large Codex one). The tradeoff: a session that
  has grown since the last sync is served at its last-synced length until the next
  `sync`.
- Harness labeling (the oh-my-* tags from 0.9.0) is now detected from the
  filesystem only - the `.omc`/`.omo` orchestration directory in a session's
  project - dropping the transcript-text markers. Those markers were the tools'
  own source/doc literals, so a session that merely *discusses* a harness (e.g.
  while building OSS) was mislabeled as run by it; the filesystem signal cannot
  be confused that way. oh-my-codex leaves no directory and is no longer tagged.
- A tag filter (`list --tag`, web) now includes subagent transcripts, and `list`
  marks them `[subagent]`, so a tag carried only by a subagent is no longer shown
  in the tag cloud yet hidden from the listing.

### Fixed
- `recall --json` includes the plain `snippet` next to `snippet_marked` in each
  candidate, matching `search --json`; `recall`'s candidate list marks subagent
  transcripts `[subagent]`.
- `sync` reports the count of top-level live sessions (matching `stats`/`list`)
  instead of every file row, which had included subagent transcripts.

## [0.9.0] - 2026-06-17

### Added
- Five new adapters, bringing supported tools to nine: the **Cline family**
  (Cline, Roo Code, Kilo Code) from one parser over their shared VS Code
  `globalStorage/<ext>/tasks/<id>/` layout - handling both native `tool_use`
  blocks and the XML-in-text tool calls Cline writes, and pulling edited files
  from either; **gajae-code** (and upstream Pi) from `~/.gjc/agent/sessions`
  JSONL; and **Continue** from `~/.continue/sessions/*.json`. Each extracts the
  files a session edited so they show up in `trace`.
- Harness labeling for the Korean "oh-my-*" wrappers. oh-my-claudecode,
  oh-my-codex, and the oh-my-openagent / lazyclaudecode / lazycodex family run
  on top of Claude Code, Codex, and OpenCode, so their conversations are already
  indexed via those adapters; sessionwiki now detects which harness drove a
  session (from the markers each injects) and auto-tags it `oh-my-claudecode` /
  `oh-my-codex` / `oh-my-openagent`, so `list --tag oh-my-claudecode` works. No
  new adapter required.

### Changed
- The OpenCode adapter now reads OpenCode's SQLite store
  (`~/.local/share/opencode/opencode.db`, plus `opencode-<channel>.db` and
  `OPENCODE_DB`), which has been the default since OpenCode v1.2.0 - the
  previous JSON-only reader missed every session created on a current install.
  The legacy `storage/**` JSON layout is still read on pre-1.2.0 installs. This
  needed a small shared-store path in the indexer (one SQLite db holds many
  sessions, so the file-per-session + mtime model did not fit); `scan` sizes the
  db and re-indexes only sessions whose updated-time changed.

## [0.8.0] - 2026-06-17

### Added
- OpenCode adapter: indexes sst/opencode sessions from
  `~/.local/share/opencode/storage` (the multi-file session/message/part JSON
  layout, joined in id order), including the files each session edited (the
  `edit`/`write` tools' `filePath` and `patch` file lists) for `trace`. Brings
  supported tools to four.
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
- Korean/CJK search: 2-syllable words (회사, 검색) - the most common Korean word
  length - are now searchable via a LIKE fallback below the trigram floor, and
  all indexed text + queries are NFC-normalized so macOS NFD input no longer
  silently returns nothing. "Zero-setup CJK search" is now actually true.
- `--json` on `search`, `list`, `related`, `brief`, `trace`, and `files`: stable
  snake_case output (matching the web API) for agent consumption, with the
  absolute path hidden. Snippets are control-code-free with a `snippet_marked`
  variant. The agent-native half of the tool.
- Claude Code plugin (`/plugin marketplace add youdie006/sessionwiki`): a
  `session-recall` skill + `/sessionwiki:recall` command that use the `--json`
  commands so an agent recalls past sessions as long-term memory, fully offline,
  degrading gracefully when the CLI isn't installed.

### Changed
- `show` pages long transcripts through your pager (`SESSIONWIKI_PAGER`, then
  `PAGER`, default `less -FRX`) when stdout is a terminal, so `show --full` of a
  huge session no longer floods the screen, and falls back to plain printing when
  no pager is installed; piped/redirected output is unchanged.
- `summarize` states up front that it pipes each transcript to the summarizer
  (the default `claude -p` sends them to the Anthropic API) - the one place data
  can leave the machine, and only on explicit `summarize`.
- CLI demo is a clean static full-frame terminal (no camera zoom). The web demo
  zooms to exact element regions measured from the live DOM, at 50fps with
  cubic ease-in-out moves and still holds.

### Fixed
- `tag` now rejects a tag containing a comma or an empty tag: a comma corrupted
  the comma-joined `tags` array in `--json` and the web API, and an empty tag
  emitted `""`. JSON serialization of session rows in `search`/`trace` can no
  longer panic mid-array.

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

[0.15.0]: https://github.com/youdie006/sessionwiki/compare/v0.14.0...v0.15.0
[0.14.0]: https://github.com/youdie006/sessionwiki/compare/v0.13.0...v0.14.0
[0.13.0]: https://github.com/youdie006/sessionwiki/compare/v0.12.0...v0.13.0
[0.12.0]: https://github.com/youdie006/sessionwiki/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/youdie006/sessionwiki/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/youdie006/sessionwiki/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/youdie006/sessionwiki/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/youdie006/sessionwiki/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/youdie006/sessionwiki/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/youdie006/sessionwiki/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/youdie006/sessionwiki/releases/tag/v0.5.0
[0.4.0]: https://github.com/youdie006/sessionwiki/releases/tag/v0.4.0
[0.3.0]: https://github.com/youdie006/sessionwiki/releases/tag/v0.3.0
[0.2.0]: https://github.com/youdie006/sessionwiki/releases/tag/v0.2.0
[0.1.0]: https://github.com/youdie006/sessionwiki/releases/tag/v0.1.0
