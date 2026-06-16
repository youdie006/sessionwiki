//! sessionwiki: index, search, and resume every AI coding session on your
//! machine, across Claude Code, Codex, and Gemini CLI.
//!
//! The binary is a thin CLI over this library. The pieces worth reusing as a
//! dependency are [`adapters`] (parse a session file into a [`model::Session`])
//! and [`index`] (an incremental SQLite FTS5 index over parsed sessions).

pub mod adapters;
pub mod commands;
pub mod index;
pub mod model;
pub mod resume;
pub mod util;
pub mod web;
