# Contributing to sessiondex

Thanks for helping. The most valuable contributions, in order:

1. **Adapters** for tools you use (see below).
2. **Format fixes** when a tool update changes its session schema.
3. **Bug reports** with a minimal session file that fails to parse.

## Development

```console
git clone https://github.com/youdie006/sessiondex
cd sessiondex
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

An adapter teaches sessiondex where one tool stores sessions and how to parse
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

sessiondex is deliberately local-only and read-only with respect to your
session stores. Features that send data anywhere, or that modify the original
session files, are out of scope. The `summarize` command shells out to an LLM
CLI *you* configure and run; the tool itself makes no network calls.
