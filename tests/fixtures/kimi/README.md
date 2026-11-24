# Kimi Code fixtures

`kimi-2.3.2-linux/` is a sanitized copy of a real `$KIMI_CODE_HOME` written by
**Kimi Code 2.3.2 on Linux (x64)**, captured on 2026-11-24. The session ran
non-interactively in auto permission mode with one prompt:
`Read sample.txt, replace beta with gamma in sample.txt, and reply Done.`
It covers one `Read`, one `Edit`, thinking parts, and a final text reply.

Kept as written by Kimi Code: file names and directory layout, record types,
field names and values, tool call and result shapes, `file-history` contents and
their hashes, durations and token counts.

Changed:

- Paths: the home is `/home/example/.kimi-code` and the working directory is
  `/work/example`, so the edited file is `/work/example/sample.txt`.
  `session_index.jsonl` stores absolute session directories, so tests copy the
  fixture and rewrite the home prefix before reading it.
- Identifiers: session, workspace, prompt, step, tool-call and message IDs are
  synthetic. So are the trace ID and the system prompt, tool and skill hashes.
- Times: every timestamp is shifted by one fixed offset, so the capture starts at
  2026-09-21T14:13:20Z (`createdAt` 1790000000000). Intervals are unchanged.
- Elided: the 8.7 KB system prompt is replaced by a placeholder, and the 27 tool
  definitions in `llm.tools_snapshot` keep only their names.

Left out because they hold no session data: `device_id`, `region`, `updates/`,
`logs/` (the home-level log), `cache/` and Kimi Code's runtime cache.

This capture has no failed tool call, no `Write`, no subagent
(`agents/<id>/wire.jsonl`), and no title (`isCustomTitle` is false and `state.json`
has no `title`). Those cases are covered by synthetic records in
`src/adapters/kimi.rs`, which follow the same record shapes. Their field names
have not been confirmed against a real capture.
