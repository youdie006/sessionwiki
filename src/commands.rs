use crate::adapters;
use crate::index;
use crate::model::Role;
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
            r.oldest.map(|t| t.format("%Y-%m-%d").to_string()).unwrap_or_else(|| "-".into()),
            r.newest.map(|t| t.format("%Y-%m-%d").to_string()).unwrap_or_else(|| "-".into()),
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
    println!("{}", dim("Try: session-atlas search <query>"));
    Ok(())
}

pub fn list(limit: usize, tool: Option<&str>, project: Option<&str>, all: bool) -> Result<()> {
    let mut conn = index::open()?;
    index::sync(&mut conn, tool)?;
    let rows = index::recent(&conn, limit, tool, project, all)?;
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
        println!(
            "{:<13} {:<12} {:<10} {:>5}  {:<24} {}",
            yellow(&r.session_id),
            cyan(&r.tool),
            rel_time(when),
            r.msg_count,
            truncate(&project_label(&r.project), 24),
            truncate(&r.title, 60),
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
        let marker = if h.row.kind == "sub" { " [subagent]" } else { "" };
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
            snip.replace('\u{2}', "\x1b[1;33m").replace('\u{3}', "\x1b[0m")
        } else {
            snip.replace('\u{2}', "").replace('\u{3}', "")
        };
        println!("  {snip}");
        println!();
    }
    println!("{}", dim(&format!("{} sessions. Open one: session-atlas show <id>", hits.len())));
    Ok(())
}

pub fn show(id: &str, full: bool, json: bool) -> Result<()> {
    let mut conn = index::open()?;
    index::sync(&mut conn, None)?;
    let matches = index::resolve(&conn, id)?;
    let row = match matches.len() {
        0 => bail!("no session with id starting \"{id}\" (try: session-atlas list)"),
        1 => &matches[0],
        _ => {
            eprintln!("ambiguous id, candidates:");
            for m in &matches {
                eprintln!("  {} {} {}", m.session_id, m.tool, truncate(&m.title, 60));
            }
            bail!("be more specific");
        }
    };

    let adapter = adapters::by_name(&row.tool).context("unknown tool in index")?;
    let session = adapter.parse(std::path::Path::new(&row.path))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&session)?);
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
            let text = if full { m.text.clone() } else { truncate(&m.text, 2000) };
            println!("{text}");
        }
        println!();
    }
    Ok(())
}

/// Long absolute paths make poor labels; keep the tail.
fn project_label(p: &str) -> String {
    if p.len() > 28 && p.contains('/') {
        let tail: Vec<&str> = p.rsplit('/').take(2).collect();
        format!("\u{2026}/{}", tail.into_iter().rev().collect::<Vec<_>>().join("/"))
    } else {
        p.to_string()
    }
}
