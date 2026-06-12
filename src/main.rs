mod adapters;
mod commands;
mod index;
mod model;
mod util;

use clap::{Parser, Subcommand};

/// Find, search, and read every AI coding session on your machine -
/// across Claude Code, Codex, and Gemini CLI. 100% local.
#[derive(Parser)]
#[command(name = "session-atlas", version, about, max_term_width = 100)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Discover session stores on this machine (which tools, where, how much)
    Scan,
    /// List recent sessions across all tools
    List {
        /// Max sessions to show
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        /// Filter by tool (claude-code, codex, gemini)
        #[arg(long)]
        tool: Option<String>,
        /// Filter by project path substring
        #[arg(long)]
        project: Option<String>,
        /// Include subagent transcripts in the listing
        #[arg(long)]
        all: bool,
    },
    /// Full-text search across every session of every tool
    Search {
        /// Text to look for (substring match, works for CJK too)
        query: String,
        /// Max sessions to show
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
        /// Filter by tool (claude-code, codex, gemini)
        #[arg(long)]
        tool: Option<String>,
        /// Filter by project path substring
        #[arg(long)]
        project: Option<String>,
    },
    /// Print one session as a readable transcript
    Show {
        /// Session id (prefix is enough), from list/search output
        id: String,
        /// Include full tool inputs/outputs instead of summaries
        #[arg(long)]
        full: bool,
        /// Emit the parsed session as JSON
        #[arg(long)]
        json: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Scan => commands::scan(),
        Command::List { limit, tool, project, all } => {
            commands::list(limit, tool.as_deref(), project.as_deref(), all)
        }
        Command::Search { query, limit, tool, project } => {
            commands::search(&query, limit, tool.as_deref(), project.as_deref())
        }
        Command::Show { id, full, json } => commands::show(&id, full, json),
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
