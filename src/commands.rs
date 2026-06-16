use crate::adapters;
use crate::index;
use crate::model::Role;
use crate::resume;
use crate::util::*;
use anyhow::{bail, Context, Result};

pub fn scan() -> Result<()> {
    let mut reports = Vec::new();
    for adapter in adapters::all() {
        if let Some(r) = adapters::report(adapter.as_ref()) {
            reports.push(r);
        }
    }
    if reports.is_empty() {
        println!("No session stores found on this machine.");
        return Ok(());
    }

    println!(
        "{}",
        bold(&format!(
            "{:<14} {:>9} {:>10}  {:<12} {:<12}  {}",
            "TOOL", "SESSIONS", "SIZE", "OLDEST", "NEWEST", "PATH"
        ))
    );
    let (mut files, mut bytes) = (0usize, 0u64);
    for r in &reports {
        files += r.files;
        bytes += r.bytes;
        println!(
            "{:<14} {:>9} {:>10}  {:<12} {:<12}  {}",
            cyan(r.tool),
            r.files,
            human_size(r.bytes),
            r.oldest
                .map(|t| t.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "-".into()),
            r.newest
                .map(|t| t.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "-".into()),
            dim(&r.root.display().to_string()),
        );
    }
    println!();
    println!(
        "{}",
        bold(&format!(
            "{} sessions across {} tools, {} on disk.",
            files,
            reports.len(),
            human_size(bytes)
        ))
    );
    println!("{}", dim("Try: sessionwiki search <query>"));
    Ok(())
}

pub fn list(
    limit: usize,
    tool: Option<&str>,
    project: Option<&str>,
    tag: Option<&str>,
    all: bool,
) -> Result<()> {
    let mut conn = index::open()?;
    index::sync(&mut conn, tool)?;
    let rows = index::recent(&conn, limit, tool, project, tag, all)?;
    if rows.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }
    println!(
        "{}",
        bold(&format!(
            "{:<13} {:<12} {:<10} {:>5}  {:<24} {}",
            "ID", "TOOL", "WHEN", "MSGS", "PROJECT", "TITLE"
        ))
    );
    for r in rows {
        let when = r
            .started
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&chrono::Utc));
        let tags = r
            .tags
            .as_deref()
            .map(|t| format!("  {}", dim(&format!("#{}", t.replace(',', " #")))))
            .unwrap_or_default();
        println!(
            "{:<13} {:<12} {:<10} {:>5}  {:<24} {}{}",
            yellow(&r.session_id),
            cyan(&r.tool),
            rel_time(when),
            r.msg_count,
            truncate(&project_label(&r.project), 24),
            truncate(&r.title, 60),
            tags,
        );
    }
    Ok(())
}

pub fn search(query: &str, limit: usize, tool: Option<&str>, project: Option<&str>) -> Result<()> {
    if query.chars().count() < 3 {
        bail!("query must be at least 3 characters (trigram index)");
    }
    let mut conn = index::open()?;
    index::sync(&mut conn, tool)?;
    let hits = index::search(&conn, query, limit, tool, project)?;
    if hits.is_empty() {
        println!("No matches for \"{query}\".");
        return Ok(());
    }
    for h in &hits {
        let when = h
            .row
            .started
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&chrono::Utc));
        let marker = if h.row.kind == "sub" {
            " [subagent]"
        } else {
            ""
        };
        println!(
            "{} {} {} {} {}",
            yellow(&h.row.session_id),
            cyan(&h.row.tool),
            dim(&fmt_date(when)),
            truncate(&project_label(&h.row.project), 28),
            dim(&format!("[{}]{marker}", h.role)),
        );
        // snippet() wraps matches in \x02 .. \x03; swap for ANSI here.
        let snip = h.snippet.replace('\n', " ");
        let snip = if color_enabled() {
            snip.replace('\u{2}', "\x1b[1;33m")
                .replace('\u{3}', "\x1b[0m")
        } else {
            snip.replace(['\u{2}', '\u{3}'], "")
        };
        println!("  {snip}");
        println!();
    }
    println!(
        "{}",
        dim(&format!(
            "{} sessions. Open one: sessionwiki show <id>",
            hits.len()
        ))
    );
    Ok(())
}

pub fn show(id: &str, full: bool, json: bool, outline: bool) -> Result<()> {
    let mut conn = index::open()?;
    index::sync(&mut conn, None)?;
    let row = resolve_one(&conn, id)?;

    let adapter = adapters::by_name(&row.tool).context("unknown tool in index")?;
    let session = adapter.parse(std::path::Path::new(&row.path))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&session)?);
        return Ok(());
    }

    if outline {
        // A session's user turns are its table of contents; the last
        // assistant message is where it ended. No LLM required.
        println!("{}", bold(&session.title));
        println!(
            "{}",
            dim(&format!(
                "{} | {} | {} | {} messages",
                session.tool,
                project_label(&session.project),
                fmt_date(session.started),
                session.messages.len()
            ))
        );
        if let Some(s) = &row.summary {
            println!("{}", s);
        }
        println!();
        let mut n = 0;
        for m in &session.messages {
            if m.role == Role::User && !is_harness_noise(&m.text) {
                n += 1;
                println!("{:>3}. {}", n, truncate(&m.text, 110));
            }
        }
        if let Some(last) = session
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
        {
            println!();
            println!("{}", bold("ended with:"));
            println!("{}", truncate(&last.text, 400));
        }
        return Ok(());
    }

    println!("{}", bold(&session.title));
    println!(
        "{}",
        dim(&format!(
            "{} | {} | {} | {} messages",
            session.tool,
            project_label(&session.project),
            fmt_date(session.started),
            session.messages.len()
        ))
    );
    println!("{}", dim(&session.path.display().to_string()));
    if let Some(s) = &row.summary {
        println!("{}", s);
    }
    if let Some(t) = &row.tags {
        println!("{}", cyan(&format!("#{}", t.replace(',', " #"))));
    }
    if let Some(note) = index::note_for(&conn, &row.session_id)? {
        println!("{} {}", dim("note:"), note);
    }
    println!();

    for m in &session.messages {
        match m.role {
            Role::User => println!("{}", bold(&cyan("[user]"))),
            Role::Assistant => println!("{}", bold(&green("[assistant]"))),
            Role::Tool => {
                if !full {
                    println!("{}", dim(&format!("[tool] {}", truncate(&m.text, 120))));
                    continue;
                }
                println!("{}", dim("[tool]"));
            }
        }
        if full || m.role != Role::Tool {
            let text = if full {
                m.text.clone()
            } else {
                truncate(&m.text, 2000)
            };
            println!("{text}");
        }
        println!();
    }

    let rel = index::related(&conn, &row.session_id, 4)?;
    if !rel.is_empty() {
        println!("{}", bold("see also:"));
        for r in rel {
            println!(
                "  {} {} {}",
                yellow(&r.session_id),
                dim(&cyan(&r.tool)),
                truncate(&r.title, 64)
            );
        }
    }
    Ok(())
}

/// Slash-command echoes and interruption markers are not conversation.
fn is_harness_noise(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with('<') || t.starts_with("[Request interrupted")
}

/// Resolve an id prefix to exactly one indexed session.
fn resolve_one(conn: &rusqlite::Connection, id: &str) -> Result<index::SessionRow> {
    let matches = index::resolve(conn, id)?;
    match matches.len() {
        0 => bail!("no session with id starting \"{id}\" (try: sessionwiki list)"),
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => {
            eprintln!("ambiguous id, candidates:");
            for m in &matches {
                eprintln!("  {} {} {}", m.session_id, m.tool, truncate(&m.title, 60));
            }
            bail!("be more specific");
        }
    }
}

pub fn resume_cmd(id: &str, print_only: bool) -> Result<()> {
    let mut conn = index::open()?;
    index::sync(&mut conn, None)?;
    let row = resolve_one(&conn, id)?;

    let path = std::path::Path::new(&row.path);
    if !path.exists() {
        bail!(
            "the session file is gone ({}) - the tool's own cleanup likely deleted it,\n\
             so a native resume is not possible. Try: sessionwiki brief {id}",
            row.path
        );
    }
    let Some(info) = resume::for_session(&row.tool, path, &row.project) else {
        bail!(
            "{} sessions cannot be resumed headlessly. For Gemini CLI, open `gemini` in\n\
             the project and use /chat resume. You can still carry the context over:\n\
             sessionwiki brief {id}",
            row.tool
        );
    };

    println!("{}", bold(&truncate(&row.title, 80)));
    if let Some(note) = &info.note {
        println!("{}", dim(&format!("note: {note}")));
    }
    let cwd_display = info.cwd.as_ref().map(|c| c.display().to_string());
    match (&info.cwd, cwd_display.as_deref()) {
        (Some(c), Some(d)) if !c.exists() => {
            println!(
                "{}",
                dim(&format!("project dir not found on this machine: {d}"))
            );
            println!("run it where the project lives:");
            println!("  {}", cyan(&info.command_line()));
            return Ok(());
        }
        (Some(_), Some(d)) => println!("{} {}", dim("in"), d),
        _ => {}
    }
    println!("  {}", cyan(&info.command_line()));
    if print_only {
        return Ok(());
    }

    let mut cmd = std::process::Command::new(info.program);
    cmd.args(&info.args);
    if let Some(c) = &info.cwd {
        cmd.current_dir(c);
    }
    match cmd.status() {
        Ok(status) => {
            if !status.success() {
                bail!("{} exited with {status}", info.program);
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "`{}` is not installed or not on PATH - run the command above manually",
                info.program
            )
        }
        Err(e) => Err(e.into()),
    }
}

pub fn brief(id: &str, max_chars: usize, include_tools: bool) -> Result<()> {
    let mut conn = index::open()?;
    index::sync(&mut conn, None)?;
    let row = resolve_one(&conn, id)?;
    let adapter = adapters::by_name(&row.tool).context("unknown tool in index")?;
    let session = adapter.parse(std::path::Path::new(&row.path))?;
    print!("{}", brief_text(&session, max_chars, include_tools));
    Ok(())
}

/// The markdown briefing used by `brief` and as LLM input for `summarize`.
fn brief_text(session: &crate::model::Session, max_chars: usize, include_tools: bool) -> String {
    let mut blocks: Vec<String> = Vec::new();
    for m in &session.messages {
        match m.role {
            Role::User => blocks.push(format!("**User:**\n{}", m.text.trim())),
            Role::Assistant => blocks.push(format!("**Assistant:**\n{}", m.text.trim())),
            Role::Tool => {
                if include_tools {
                    blocks.push(format!("> [tool] {}", truncate(&m.text, 200)));
                }
            }
        }
    }

    // Budgeting: keep the head and the tail, drop the middle. The opening
    // frames the task and the tail holds the latest state - both matter
    // more than the middle of a long session. Cap individual blocks first,
    // or a single giant message starves both ends.
    let block_cap = (max_chars / 4).max(400);
    let blocks: Vec<String> = blocks
        .into_iter()
        .map(|b| {
            if b.chars().count() > block_cap {
                let cut: String = b.chars().take(block_cap).collect();
                format!("{cut}\n*[... message truncated ...]*")
            } else {
                b
            }
        })
        .collect();
    let total: usize = blocks.iter().map(|b| b.len() + 2).sum();
    let body = if total <= max_chars {
        blocks.join("\n\n")
    } else {
        let half = max_chars / 2;
        let mut head: Vec<&String> = Vec::new();
        let mut used = 0;
        for b in &blocks {
            if used + b.len() > half {
                break;
            }
            used += b.len() + 2;
            head.push(b);
        }
        let mut tail: Vec<&String> = Vec::new();
        let mut used_tail = 0;
        for b in blocks.iter().rev() {
            if used_tail + b.len() > half || head.len() + tail.len() >= blocks.len() {
                break;
            }
            used_tail += b.len() + 2;
            tail.push(b);
        }
        tail.reverse();
        let omitted = blocks.len() - head.len() - tail.len();
        let mut parts: Vec<String> = head.into_iter().cloned().collect();
        if omitted > 0 {
            parts.push(format!("*[... {omitted} messages omitted ...]*"));
        }
        parts.extend(tail.into_iter().cloned());
        parts.join("\n\n")
    };

    format!(
        "# Previous session: {}\n\n- Tool: {} | Project: {} | Date: {}\n- Source: {}\n\n{}\n",
        session.title,
        session.tool,
        session.project,
        fmt_date(session.started),
        session.path.display(),
        body
    )
}

const SUMMARIZE_INSTRUCTION: &str = "You are summarizing a transcript of an AI coding session. \
Reply with ONLY the summary, 1-2 sentences: what was asked and what the outcome was. \
Write it in the same language the session is in.";

pub fn summarize(
    id: Option<&str>,
    recent: usize,
    tool: Option<&str>,
    cmd: Option<&str>,
    force: bool,
) -> Result<()> {
    let mut conn = index::open()?;
    index::sync(&mut conn, tool)?;

    let targets = match id {
        Some(id) => vec![resolve_one(&conn, id)?],
        None => index::unsummarized(&conn, recent, tool)?,
    };
    if targets.is_empty() {
        println!("Nothing to summarize - the most recent sessions already have summaries.");
        return Ok(());
    }

    let cmd = cmd
        .map(String::from)
        .or_else(|| std::env::var("SESSIONWIKI_SUMMARIZER").ok())
        .unwrap_or_else(|| "claude -p".to_string());
    eprintln!(
        "{}",
        dim(&format!(
            "summarizer: `{cmd}` ({} session(s); your LLM, your cost)",
            targets.len()
        ))
    );

    let total = targets.len();
    for (i, row) in targets.iter().enumerate() {
        if row.summary.is_some() && !force {
            println!(
                "{} already summarized (use --force to redo)",
                yellow(&row.session_id)
            );
            continue;
        }
        let adapter = adapters::by_name(&row.tool).context("unknown tool in index")?;
        let session = match adapter.parse(std::path::Path::new(&row.path)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{} parse failed: {e:#}", yellow(&row.session_id));
                continue;
            }
        };
        eprintln!(
            "{}",
            dim(&format!(
                "[{}/{}] {}",
                i + 1,
                total,
                truncate(&row.title, 70)
            ))
        );
        let input = format!(
            "{SUMMARIZE_INSTRUCTION}\n\n{}",
            brief_text(&session, 16000, false)
        );
        match run_summarizer(&cmd, &input) {
            Ok(summary) => {
                index::set_summary(&conn, &row.session_id, &summary)?;
                println!("{} {}", yellow(&row.session_id), summary);
            }
            Err(e) => eprintln!("{} summarizer failed: {e:#}", yellow(&row.session_id)),
        }
    }
    Ok(())
}

fn run_summarizer(cmd: &str, input: &str) -> Result<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("spawn summarizer")?;
    child
        .stdin
        .take()
        .context("summarizer stdin")?
        .write_all(input.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("exited with {}", out.status);
    }
    let summary = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if summary.is_empty() {
        bail!("summarizer printed nothing");
    }
    Ok(truncate(&summary, 600))
}

pub fn tag(id: &str, add: &[String], remove: &[String]) -> Result<()> {
    // Reads/writes the index only; no filesystem sync, so it is instant.
    // The session id comes from list/search, which already indexed it.
    let conn = index::open()?;

    // No id and no edits: list all tags in use (the wiki tag cloud).
    if id.is_empty() {
        let counts = index::tag_counts(&conn)?;
        if counts.is_empty() {
            println!("No tags yet. Add one: sessionwiki tag <id> <tag>");
            return Ok(());
        }
        for (t, n) in counts {
            println!("{:>4}  {}", n, cyan(&format!("#{t}")));
        }
        return Ok(());
    }

    let row = resolve_one(&conn, id)?;
    for t in remove {
        index::remove_tag(&conn, &row.session_id, t)?;
    }
    for t in add {
        index::add_tag(&conn, &row.session_id, t)?;
    }
    let tags = index::resolve(&conn, &row.session_id)?
        .into_iter()
        .next()
        .and_then(|r| r.tags)
        .unwrap_or_else(|| "(none)".into());
    println!(
        "{} {}",
        yellow(&row.session_id),
        cyan(&format!("#{}", tags.replace(',', " #")))
    );
    Ok(())
}

pub fn note(id: &str, text: Option<&str>) -> Result<()> {
    let conn = index::open()?;
    let row = resolve_one(&conn, id)?;
    match text {
        Some(t) => {
            index::set_note(&conn, &row.session_id, t)?;
            println!("{} note saved", yellow(&row.session_id));
        }
        None => match index::note_for(&conn, &row.session_id)? {
            Some(n) => println!("{n}"),
            None => println!(
                "{}",
                dim("(no note; add one: sessionwiki note <id> \"...\")")
            ),
        },
    }
    Ok(())
}

pub fn related(id: &str, limit: usize) -> Result<()> {
    let conn = index::open()?;
    let row = resolve_one(&conn, id)?;
    println!(
        "{}",
        dim(&format!("related to: {}", truncate(&row.title, 70)))
    );
    let rel = index::related(&conn, &row.session_id, limit)?;
    if rel.is_empty() {
        println!("No related sessions found.");
        return Ok(());
    }
    for r in rel {
        let when = r
            .started
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&chrono::Utc));
        println!(
            "{} {} {} {}",
            yellow(&r.session_id),
            cyan(&r.tool),
            dim(&fmt_date(when)),
            truncate(&r.title, 64),
        );
    }
    Ok(())
}

pub fn projects() -> Result<()> {
    let conn = index::open()?;
    let rows = index::projects(&conn)?;
    if rows.is_empty() {
        println!("No projects indexed yet.");
        return Ok(());
    }
    println!(
        "{}",
        bold(&format!(
            "{:>5} {:>7}  {:<11} {}",
            "SESS", "MSGS", "LAST", "PROJECT"
        ))
    );
    for p in rows {
        let last = p
            .newest
            .as_deref()
            .map(|s| s.get(0..10).unwrap_or(s).to_string())
            .unwrap_or_else(|| "-".into());
        println!(
            "{:>5} {:>7}  {:<11} {}",
            p.sessions,
            p.messages,
            dim(&last),
            project_label(&p.project)
        );
    }
    Ok(())
}

pub fn stats() -> Result<()> {
    let conn = index::open()?;
    let s = index::stats(&conn)?;

    println!(
        "{}",
        bold(&format!(
            "{} sessions · {} messages · {} projects · {} tags · {} summarized",
            s.total_sessions, s.total_messages, s.projects, s.tags, s.summarized
        ))
    );
    println!();
    println!("{}", bold("by tool"));
    for (tool, sess, msgs) in &s.per_tool {
        println!(
            "  {:<14} {:>6} sessions  {:>8} messages",
            cyan(tool),
            sess,
            msgs
        );
    }
    if !s.per_month.is_empty() {
        println!();
        println!("{}", bold("by month"));
        let max = s
            .per_month
            .iter()
            .map(|(_, n)| *n)
            .max()
            .unwrap_or(1)
            .max(1);
        for (ym, n) in &s.per_month {
            let bar = "\u{2588}".repeat(((*n as f64 / max as f64) * 24.0).round() as usize);
            println!("  {}  {:>5}  {}", ym, n, cyan(&bar));
        }
    }
    Ok(())
}

/// Long absolute paths make poor labels; keep the tail.
fn project_label(p: &str) -> String {
    if p.len() > 28 && p.contains('/') {
        let tail: Vec<&str> = p.rsplit('/').take(2).collect();
        format!(
            "\u{2026}/{}",
            tail.into_iter().rev().collect::<Vec<_>>().join("/")
        )
    } else {
        p.to_string()
    }
}
