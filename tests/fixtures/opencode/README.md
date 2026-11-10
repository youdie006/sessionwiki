These fixtures are synthetic and contain no user data.

- `v1.sql` covers the existing `session` / `message` / `part` layout.
- `v2_1_1.sql` is a text dump of the synthetic v2.1.1 fixture contributed by
  AlexZander85 in [SessionWiki PR #85](https://github.com/youdie006/sessionwiki/pull/85)
  (commit `e0468cf`). Tests create temporary SQLite databases from this SQL.

The targeted released layout is independently confirmed by OpenCode's
[v2.1.1 migration](https://github.com/anomalyco/opencode/blob/v2.1.1/packages/core/src/database/v1-migration.bun.ts),
which copies `session` rows into `session_v2` while retaining their IDs and
projects their messages into `session_message`. Earlier development checkouts
can have a different layout; the fixtures do not claim to cover every beta.

Tool-error and shell-message cases follow the v2.1.1 definitions in
[`session-message.ts`](https://github.com/anomalyco/opencode/blob/v2.1.1/packages/schema/src/session-message.ts)
and [`shell.ts`](https://github.com/anomalyco/opencode/blob/v2.1.1/packages/schema/src/shell.ts).

Migration-policy tests also create the native `kv['migration.v1-v2']` marker:
`{"phase":"sessions","cursor":"..."}` tracks descending-ID progress, while
`{"phase":"completed"}` makes v2 authoritative. This keeps retained legacy
copies of deleted migrated sessions from becoming live again. Unmarked or
not-yet-processed v1 sessions remain readable.
