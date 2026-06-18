# Security

sessionwiki reads your AI coding session files (which contain your code and
conversations), builds a local index, and serves a local web UI. Its security
posture is deliberately small:

- **No network calls.** There is not a single one in the codebase. You can
  verify this with one grep over `src/`. The only feature that touches an LLM
  is `summarize`, which runs a shell command **you** supply (`--cmd` or the
  `SESSIONWIKI_SUMMARIZER` env var, default `claude -p`) with the session text
  on stdin. It runs exactly what you configured, on your machine; do not point
  it at a command you would not run yourself.
- **No telemetry, no accounts.**
- **The web UI binds to `127.0.0.1` only** and serves the index read-only; it
  never writes to your session stores.
- **Your existing session files are never modified.** Tags, notes, and summaries
  live in sessionwiki's own index, not in your tools' files.
- **Two commands act beyond the index, explicitly and on demand:**
  - `migrate <id> <dir>` *copies* a session into another tool's store so you can
    resume it from a different directory. It writes a new file; it never modifies
    or deletes an existing session.
  - `resume <id>` launches the original tool (`claude` / `codex`) in the session's
    recorded project directory. A session file is untrusted input and can claim
    any directory, so sessionwiki verifies that directory before launching (for
    Claude Code, against the store folder the session actually lives in) and will
    not auto-launch into one it can't confirm &mdash; it prints the command for
    you to run after a look instead.

## Reporting a vulnerability

If you find a security issue, please open a
[GitHub Security Advisory](https://github.com/youdie006/sessionwiki/security/advisories/new)
or a regular issue if it is low-risk. Since the tool handles potentially
sensitive local data, reports about accidental data exposure, a path that
could write outside the index directory, or any outbound network behavior are
especially welcome.
