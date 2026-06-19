# sessionwiki - Claude Code plugin

Makes Claude Code use [sessionwiki](https://github.com/youdie006/sessionwiki) as
long-term memory: it recalls your past Claude Code / Codex / Gemini CLI sessions
when you start a task, and `/sessionwiki:recall <topic>` searches your history on
demand. The plugin shells out to the local `sessionwiki` binary, so it is fully
offline; if the binary isn't on `PATH` it degrades gracefully and Claude just
works without recall.

## Install

Install the `sessionwiki` CLI first (see the main README), then:

```console
/plugin marketplace add youdie006/sessionwiki
/plugin install sessionwiki@sessionwiki-marketplace
```

## What's in it

- **SessionStart hook** (automatic, deterministic): when you start a new Claude
  Code session, a small "prior work in this project" brief is injected into the
  context, so the agent has memory of this directory without being asked. It is
  `startup`-only, fast (queries the index with `--no-sync`, tens of ms), and
  prints nothing for a project with no history (zero noise). The injected recall
  is fenced as untrusted data and sanitized (see SECURITY.md); the brief reflects
  the last index refresh.
- **`session-recall` skill** (auto): Claude recalls prior work at the start of a
  task and when you reference earlier sessions. Read-only.
- **`/sessionwiki:recall <topic>` command** (manual): search your past sessions
  on a topic and get a summary of what was done.

All use only the local `sessionwiki` binary against your local index. If the
binary isn't on `PATH`, every piece degrades gracefully (no recall, no error).

## Smoke test

```bash
claude --plugin-dir ./plugin
# then in-session:
/sessionwiki:recall jwt retry        # command fires, shells out to sessionwiki
# or: start a task referencing prior work -> the skill auto-recalls
```
