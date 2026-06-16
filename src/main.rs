use clap::{Parser, Subcommand};
use sessionwiki::{commands, web};

/// Find, search, and read every AI coding session on your machine -
/// across Claude Code, Codex, and Gemini CLI. 100% local.
#[derive(Parser)]
#[command(name = "sessionwiki", version, about, max_term_width = 100)]
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
        /// Filter by tag
        #[arg(long)]
        tag: Option<String>,
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
    /// Browse and search sessions in a local web UI
    Web {
        /// Port to listen on (localhost only)
        #[arg(long, default_value_t = 7575)]
        port: u16,
        /// Do not open the browser automatically
        #[arg(long)]
        no_open: bool,
        /// Refresh the index once before serving (otherwise it reflects the
        /// last `list`/`search`)
        #[arg(long)]
        sync: bool,
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
        /// Show a digest instead: every user turn plus how the session ended
        #[arg(long)]
        outline: bool,
    },
    /// Reopen a session in its original tool (claude --resume / codex resume)
    Resume {
        /// Session id (prefix is enough), from list/search output
        id: String,
        /// Print the resume command instead of running it
        #[arg(long)]
        print: bool,
    },
    /// Generate and cache LLM synopses for sessions (uses your own LLM CLI)
    Summarize {
        /// Session id to summarize; omit to batch over recent sessions
        id: Option<String>,
        /// How many recent unsummarized sessions to process in batch mode
        #[arg(long, default_value_t = 10)]
        recent: usize,
        /// Filter batch mode by tool
        #[arg(long)]
        tool: Option<String>,
        /// Summarizer command reading the session on stdin (default: `claude -p`,
        /// or the SESSIONWIKI_SUMMARIZER environment variable)
        #[arg(long)]
        cmd: Option<String>,
        /// Re-summarize even if a cached summary exists
        #[arg(long)]
        force: bool,
    },
    /// Emit a markdown briefing of a session, ready to paste into a new one
    Brief {
        /// Session id (prefix is enough), from list/search output
        id: String,
        /// Budget for the briefing body; the middle of long sessions is omitted
        #[arg(long, default_value_t = 24000)]
        max_chars: usize,
        /// Include tool calls in the briefing
        #[arg(long)]
        tools: bool,
    },
    /// Show, add, or remove tags on a session (no id: list all tags in use)
    Tag {
        /// Session id (prefix is enough); omit to list every tag in use
        #[arg(default_value = "")]
        id: String,
        /// Tags to add
        #[arg(value_name = "TAG")]
        add: Vec<String>,
        /// Tags to remove
        #[arg(long = "rm", value_name = "TAG")]
        remove: Vec<String>,
    },
    /// Attach or read a freeform note on a session
    Note {
        /// Session id (prefix is enough)
        id: String,
        /// Note text; omit to print the existing note
        text: Option<String>,
    },
    /// Find sessions related to one (shared project, files, and tags)
    Related {
        /// Session id (prefix is enough)
        id: String,
        /// Max related sessions to show
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
    },
    /// List the files a session edited or created
    Files {
        /// Session id (prefix is enough), from list/search output
        id: String,
    },
    /// Trace a file back to the AI sessions that edited it (newest first)
    Trace {
        /// File path as it appears in your editor (e.g. src/auth.rs)
        path: String,
    },
    /// Permanently drop an archived (or any) session from the index
    Forget {
        /// Session id (prefix is enough), from list/search output
        id: String,
    },
    /// List projects with session counts (a page per project)
    Projects,
    /// Usage breakdown across tools, projects, and months
    Stats,
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Scan => commands::scan(),
        Command::List {
            limit,
            tool,
            project,
            tag,
            all,
        } => commands::list(
            limit,
            tool.as_deref(),
            project.as_deref(),
            tag.as_deref(),
            all,
        ),
        Command::Search {
            query,
            limit,
            tool,
            project,
        } => commands::search(&query, limit, tool.as_deref(), project.as_deref()),
        Command::Web {
            port,
            no_open,
            sync,
        } => web::serve(port, no_open, sync),
        Command::Show {
            id,
            full,
            json,
            outline,
        } => commands::show(&id, full, json, outline),
        Command::Resume { id, print } => commands::resume_cmd(&id, print),
        Command::Summarize {
            id,
            recent,
            tool,
            cmd,
            force,
        } => commands::summarize(
            id.as_deref(),
            recent,
            tool.as_deref(),
            cmd.as_deref(),
            force,
        ),
        Command::Brief {
            id,
            max_chars,
            tools,
        } => commands::brief(&id, max_chars, tools),
        Command::Tag { id, add, remove } => commands::tag(&id, &add, &remove),
        Command::Note { id, text } => commands::note(&id, text.as_deref()),
        Command::Related { id, limit } => commands::related(&id, limit),
        Command::Files { id } => commands::files(&id),
        Command::Trace { path } => commands::trace(&path),
        Command::Forget { id } => commands::forget(&id),
        Command::Projects => commands::projects(),
        Command::Stats => commands::stats(),
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
