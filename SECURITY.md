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
- **The web UI binds to `127.0.0.1` only** and is read-only with respect to
  your session stores.
- **Original session files are never modified.** Tags, notes, and summaries
  live in sessionwiki's own index, not in your tools' files.

## Reporting a vulnerability

If you find a security issue, please open a
[GitHub Security Advisory](https://github.com/youdie006/sessionwiki/security/advisories/new)
or a regular issue if it is low-risk. Since the tool handles potentially
sensitive local data, reports about accidental data exposure, a path that
could write outside the index directory, or any outbound network behavior are
especially welcome.
