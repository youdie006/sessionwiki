---
description: Search past AI coding sessions on a topic and summarize what was done. Pulls from the local sessionwiki index (Claude Code, Codex, Gemini CLI).
argument-hint: <topic keywords>
disable-model-invocation: true
allowed-tools: Bash(sessionwiki search:*), Bash(sessionwiki brief:*), Bash(command -v sessionwiki)
---

# /recall - what did I do about "$ARGUMENTS"?

Search the user's past AI coding sessions for **$ARGUMENTS** and report what was
done, using the local `sessionwiki` index. Read-only, offline.

Steps:

1. Confirm the tool is present:
   ```
   command -v sessionwiki
   ```
   If empty, reply: "sessionwiki isn't installed
   (https://github.com/youdie006/sessionwiki) - nothing to recall." and stop.

2. Search:
   ```
   sessionwiki search --json "$ARGUMENTS"
   ```

3. From the JSON hits, take the 3-5 most relevant `id`s. For each, pull a
   briefing:
   ```
   sessionwiki brief --json <id>
   ```

4. Summarize for the user, grouped by session, newest first. For each: the
   session id (short prefix), the project, the date, and 1-2 lines on what was
   asked and what the outcome was. End with the single most relevant session id
   so they can `sessionwiki show <id>` or resume it.

If `search --json` returns no hits, say so plainly ("No past session mentions
'$ARGUMENTS'.") and suggest a broader or more specific topic. If a `--json` flag
is rejected, rerun the same command without `--json`, read the plain text, and
note the installed sessionwiki is older than the JSON output feature.
