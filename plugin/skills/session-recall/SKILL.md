---
name: session-recall
description: Recall past AI coding sessions as long-term memory. Use at the START of any non-trivial task, and whenever the user references earlier work ("like we did before", "the X we set up", "last time", "that bug fix"), asks who/when a file was changed, or you need context that predates this conversation. Runs the local `sessionwiki` CLI to search prior sessions, pull a session briefing, and trace a file back to the sessions that edited it.
user-invocable: false
allowed-tools: Bash(sessionwiki search:*), Bash(sessionwiki brief:*), Bash(sessionwiki trace:*), Bash(command -v sessionwiki)
---

# Session recall: use sessionwiki as long-term memory

The user's machine has a `sessionwiki` index of every past Claude Code, Codex,
and Gemini CLI session. Treat it as the agent's long-term memory. It is local
and offline - no network. Before solving something from scratch, check whether
it was already solved.

## When to recall
- Starting a non-trivial task -> search for prior work on the same topic first.
- The user references earlier work ("the auth flow we built", "last week", "that
  CORS fix") -> recall it instead of asking them to re-explain.
- You need to know which session changed a file -> trace it.

## How to recall (run these, do not guess)

1. **First time only**, confirm the tool exists:
   ```
   command -v sessionwiki
   ```
   If it prints nothing, the tool is not installed - see "If sessionwiki is
   missing" below and stop using this skill for the rest of the turn.

2. **Search prior sessions** on the task topic:
   ```
   sessionwiki search --json "<topic keywords>"
   ```
   Returns an array of hits: `id`, `tool`, `project`, `started`, `role`,
   `snippet`. Pick the 1-3 most relevant `id`s.

3. **Pull context** from a promising session before acting:
   ```
   sessionwiki brief --json <id>
   ```
   Returns the session's title, project, date, and a budgeted markdown
   transcript. Read it; carry forward decisions, file names, and gotchas. Do not
   re-derive what the prior session already settled.

4. **Trace a file** to the sessions that touched it, newest first:
   ```
   sessionwiki trace --json <path-as-it-appears-in-the-editor>
   ```
   e.g. `sessionwiki trace --json src/auth.rs`. Returns sessions that *touched*
   the file (not line-level authorship) - use it to find the conversation behind
   a change, then `brief` it.

## Rules
- Prefer recall over re-asking the user for context that already exists on disk.
- Keep queries specific: "jwt refresh retry", not "auth". Two-word topics beat
  one. The index matches substrings, so partial identifiers and non-English
  (e.g. Korean, including short words) text work.
- Cite what you recalled: "Found it - session `3f9c` set the retry to 3 attempts."
- Recall is read-only. It never modifies sessions or code.

## If sessionwiki is missing or a command fails
- `command -v sessionwiki` empty, or any call errors with "command not found":
  the tool is not installed. Tell the user once:
  "sessionwiki isn't installed, so I can't recall past sessions
  (https://github.com/youdie006/sessionwiki). Continuing without recall."
  Then proceed with the task normally - do not retry, do not block.
- A `--json` flag is rejected ("unexpected argument"): the installed version
  predates JSON output. Fall back to the same command without `--json` and read
  the plain-text result, or tell the user to upgrade sessionwiki.
- An empty result is normal (no prior session on that topic) - just continue.
