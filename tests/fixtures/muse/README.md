# Muse Code fixture

`muse-1.4.24-linux/` is a sanitized capture of one real session from Muse Code
1.4.24 (`1.4.24-R9131.1`) on Linux, recorded on 2026-11-24 under an isolated
`XDG_DATA_HOME`. The layout is Muse's own:
`<sessions root>/YYYY/MM/DD/<session id>/session.jsonl`, plus the
`tool-outputs/` directory Muse writes beside it. The prompt asked Muse to read
`sample.txt`, replace `beta` with `gamma`, and reply `Done`, so the log has a
`read_file` call and an `edit_file` call, the retained permission frame at the
start of the file, a session name, and the end-of-session record.

Event types, field names, nesting and tool call/result shapes are exactly as
Muse wrote them. These parts were changed:

- The system prompt, the reminder definitions and the context messages in
  `model_request_configured` are replaced with `[elided from fixture]`, and so
  is every other string over 200 characters (encrypted reasoning). The
  surrounding fields stay.
- The workspace is `/work/example` and the sessions root appears as
  `/home/user/.local/share/muse/sessions`. The edited file is
  `/work/example/sample.txt`; the tool call names it relative to the workspace
  (`sample.txt`), as Muse logged it.
- Record, run, task, call and provider IDs, span IDs and content digests are
  replaced with synthetic values, and `recorded_at` values are shifted to
  start at 2026-11-24T12:00:00Z with the original spacing. The session ID is
  the original one; it is a random identifier and also names the directory.

`tests/muse_fixture.rs` reads this fixture through the public API.
