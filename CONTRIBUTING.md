# Contributing to sessionwiki

Thanks for helping. The most valuable contributions, in order:

1. **Adapters** for tools you use (see below).
2. **Format fixes** when a tool update changes its session schema.
3. **Bug reports** with a minimal session file that fails to parse.

## Development

```console
git clone https://github.com/youdie006/sessionwiki
cd sessionwiki
cargo build
cargo test
```

CI runs `cargo fmt --check`, `cargo clippy -D warnings`, and `cargo test` on
every push and PR. Run them locally before opening a PR:

```console
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all
```

The crate is a library (`src/lib.rs`) with a thin CLI binary (`src/main.rs`),
so the parsing and indexing logic is testable and reusable as a dependency.

## Adding an adapter

An adapter teaches sessionwiki where one tool stores sessions and how to parse
one. It is a single file implementing the `Adapter` trait:

```rust
pub trait Adapter {
    fn name(&self) -> &'static str;               // "my-tool"
    fn root(&self) -> Option<PathBuf>;            // where it keeps sessions
    fn discover(&self) -> Vec<PathBuf>;           // every session file
    fn parse(&self, path: &Path) -> Result<Session>; // tolerant; skip bad lines
}
```

1. Copy [`src/adapters/gemini.rs`](src/adapters/gemini.rs) (the smallest
   example) to `src/adapters/<tool>.rs` and adapt it.
2. Register it in [`src/adapters/mod.rs`](src/adapters/mod.rs) `all()`.
3. Add a fixture under `tests/fixtures/<tool>/` and assertions in
   [`tests/parse.rs`](tests/parse.rs). Include the awkward cases: boilerplate
   that should be dropped, multi-block message content, and a deliberately
   malformed line that must be skipped without panicking.

**Parsers must never panic on malformed input.** Tool formats drift between
versions; parse defensively and return whatever you could read.

## Reporting a parse bug

The cleanest bug report is a fixture: the first ~10 lines of a session file
that parses wrong, with anything sensitive redacted (the structure is what
matters, not the content). Open an issue with that snippet, the tool, and its
version.

## Scope

sessionwiki is deliberately local-only and read-only with respect to your
session stores. Features that send data anywhere, or that modify the original
session files, are out of scope. The `summarize` command shells out to an LLM
CLI *you* configure and run; the tool itself makes no network calls.

## Durable schema migrations

The index has two schemas with two version counters. The **cache** (files,
messages, msgs, touched) is disposable: bump `SCHEMA_VERSION` when its shape
changes and `open()` drops and rebuilds it. The **durable** tables (summaries,
tags, notes, archive, meta) hold what cannot be re-derived - LLM output, user
curation, and archived sessions whose originals the tool deleted - so they are
versioned separately by `meta.durable_version` and must survive every upgrade.

To change a durable table, do NOT edit its `CREATE TABLE` (those are frozen at
the baseline shape). Append a `Migration` to `DURABLE_MIGRATIONS` in `index.rs`:

- `version` = the previous max + 1.
- `up` = additive DDL only: `ALTER TABLE ... ADD COLUMN`, `CREATE TABLE`,
  `CREATE INDEX`, or a backfill `UPDATE`. Never `DROP TABLE`, `DROP COLUMN`, or
  `RENAME COLUMN` on a durable table - deprecate a column by leaving it unused.

The runner applies migrations in one transaction, gated on `durable_version`
(so re-running `open()` is a no-op), and `VACUUM INTO`s a backup of `index.db`
before the first migration of a run. Add a survival test in `tests/schema.rs`.
