# Grok Build fixture

`grok-1.0.92-linux/` is a real session captured from **Grok Build 1.0.92
(`b06756fd6256`) on Linux** on 2026-11-24, then sanitized. It is one headless
run of the prompt "Read sample.txt, replace beta with gamma in sample.txt, and
reply Done." that reads a file and edits it with `search_replace`.

Grok Build keeps one directory per conversation at
`~/.grok/sessions/<percent-encoded cwd>/<session uuid>/`. The fixture holds the
two files the reader uses, `summary.json` and `chat_history.jsonl`. Tests copy
them into `<home>/sessions/%2Fwork%2Fexample/<uuid>/` under a temporary home.

What was kept: every record type and field name, in the native order; the tool
call and result shapes for `read_file` and `search_replace`; the `synthetic_reason`
and `prompt_index` markers; and the `<user_info>` and `<user_query>` wrappers.

What was changed:

- The system prompt and the skill lists in the `system_reminder` records are
  replaced by a one-line placeholder, and the `<user_info>` rules are dropped.
- The working directory `/tmp/fixture-grok/work` is now `/work/example`, and the
  Grok home is `/home/user/.grok`.
- Session, request, agent, attempt, call and reasoning IDs are synthetic.
  `created_at` and `updated_at` are synthetic timestamps.
- `encrypted_content` of the reasoning records and `model_fingerprint` are
  placeholders. Reasoning `summary` arrays were already empty in the capture.

`chat_history.jsonl` carries no per-record timestamps in this release; session
times come from `summary.json`.
