use crate::{adapters, index, resume};
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::json;
use tiny_http::{Header, Response, Server};

/// Local read-only viewer over the index. By default it does not sync, so it
/// stays snappy and can run while a CLI index pass is in progress (WAL allows
/// concurrent readers) - which means sessions created after the last
/// `list`/`search` won't show until the index is refreshed. Pass `sync` to
/// bring the index up to date once before serving.
pub fn serve(port: u16, no_open: bool, sync: bool) -> Result<()> {
    let mut conn = index::open()?;
    if sync {
        index::sync(&mut conn, None)?;
    }
    let addr = format!("127.0.0.1:{port}");
    let server = Server::http(&addr).map_err(|e| anyhow::anyhow!("bind {addr}: {e}"))?;
    let url = format!("http://{addr}");
    println!("sessionwiki web: {url}");
    if !sync {
        println!("(read-only view; run `sessionwiki list` or `web --sync` to refresh the index)");
    }
    if !no_open {
        open_browser(&url);
    }

    for request in server.incoming_requests() {
        let url = request.url().to_string();
        let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
        let result = match path {
            "/" => html(INDEX_HTML),
            "/api/stats" => api_stats(&conn),
            "/api/sessions" => api_sessions(&conn, query),
            "/api/search" => api_search(&conn, query),
            "/api/trace" => api_trace(&conn, query),
            "/api/projects" => api_projects(&conn),
            p if p.starts_with("/api/related/") => {
                api_related(&conn, p.trim_start_matches("/api/related/"))
            }
            p if p.starts_with("/api/session/") => {
                api_session(&conn, p.trim_start_matches("/api/session/"))
            }
            _ => Ok(Response::from_string("not found")
                .with_status_code(404)
                .boxed()),
        };
        let response = match result {
            Ok(r) => r,
            Err(e) => Response::from_string(json!({ "error": e.to_string() }).to_string())
                .with_status_code(500)
                .boxed(),
        };
        let _ = request.respond(response);
    }
    Ok(())
}

type Boxed = Response<Box<dyn std::io::Read + Send>>;

fn html(body: &str) -> Result<Boxed> {
    Ok(Response::from_string(body)
        .with_header(
            Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap(),
        )
        .boxed())
}

fn json_response(v: serde_json::Value) -> Result<Boxed> {
    Ok(Response::from_string(v.to_string())
        .with_header(
            Header::from_bytes(
                &b"Content-Type"[..],
                &b"application/json; charset=utf-8"[..],
            )
            .unwrap(),
        )
        .boxed())
}

fn api_stats(conn: &Connection) -> Result<Boxed> {
    let mut stmt = conn.prepare(
        "SELECT tool, count(*), sum(size), sum(msg_count) FROM files GROUP BY tool ORDER BY 2 DESC",
    )?;
    let rows: Vec<serde_json::Value> = stmt
        .query_map([], |r| {
            Ok(json!({
                "tool": r.get::<_, String>(0)?,
                "sessions": r.get::<_, i64>(1)?,
                "bytes": r.get::<_, i64>(2)?,
                "messages": r.get::<_, i64>(3)?,
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    json_response(json!({ "tools": rows }))
}

fn api_sessions(conn: &Connection, query: &str) -> Result<Boxed> {
    let tool = param(query, "tool");
    let project = param(query, "project");
    let tag = param(query, "tag");
    let limit = param(query, "limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let rows = index::recent(
        conn,
        limit,
        tool.as_deref(),
        project.as_deref(),
        tag.as_deref(),
        false,
    )?;
    json_response(json!(rows.iter().map(row_json).collect::<Vec<_>>()))
}

fn api_projects(conn: &Connection) -> Result<Boxed> {
    let rows = index::projects(conn)?;
    json_response(json!(rows
        .iter()
        .map(|p| json!({
            "project": p.project,
            "sessions": p.sessions,
            "messages": p.messages,
            "newest": p.newest,
        }))
        .collect::<Vec<_>>()))
}

fn api_related(conn: &Connection, id: &str) -> Result<Boxed> {
    let rel = index::related(conn, id, 8)?;
    json_response(json!(rel.iter().map(row_json).collect::<Vec<_>>()))
}

fn api_search(conn: &Connection, query: &str) -> Result<Boxed> {
    let q = param(query, "q").unwrap_or_default();
    let qt = q.trim();
    if qt.is_empty() {
        return json_response(json!([]));
    }
    let tool = param(query, "tool");
    let limit = param(query, "limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    // <3 chars (incl. 2-syllable Korean) is below the trigram floor; LIKE-scan it.
    let hits = if crate::util::nfc(qt).chars().count() < 3 {
        index::search_like(conn, qt, limit, tool.as_deref(), None)?
    } else {
        index::search(conn, qt, limit, tool.as_deref(), None)?
    };
    json_response(json!(hits
        .iter()
        .map(|h| {
            let mut v = row_json(&h.row);
            v["snippet"] = json!(h.snippet);
            v["role"] = json!(h.role);
            v
        })
        .collect::<Vec<_>>()))
}

/// Reverse provenance lookup: sessions that touched a file. Each result
/// carries the matched stored path, so the UI can show what it resolved to.
fn api_trace(conn: &Connection, query: &str) -> Result<Boxed> {
    let path = param(query, "path").unwrap_or_default();
    if path.is_empty() {
        return json_response(json!([]));
    }
    let hits = index::sessions_for_file(conn, &path, 50)?;
    json_response(json!(hits
        .iter()
        .map(|(r, matched)| {
            let mut v = row_json(r);
            v["matched"] = json!(matched);
            v
        })
        .collect::<Vec<_>>()))
}

fn api_session(conn: &Connection, id: &str) -> Result<Boxed> {
    let matches = index::resolve(conn, id)?;
    let row = matches.first().context("session not found")?;
    let path = std::path::Path::new(&row.path);
    // Archived sessions (original deleted by the tool) are served from the
    // index; live ones are re-parsed from the file for full fidelity.
    let session = if path.exists() {
        let adapter = adapters::by_name(&row.tool).context("unknown tool")?;
        adapter.parse(path)?
    } else {
        index::session_from_index(conn, row)?
    };
    let mut v = serde_json::to_value(&session)?;
    if row.archived {
        v["archived"] = json!(true);
    }
    if let Some(info) = resume::for_session(&row.tool, path, &row.project) {
        v["resume"] = json!(info.command_line());
    }
    if let Some(s) = &row.summary {
        v["summary"] = json!(s);
    }
    if let Some(t) = &row.tags {
        v["tags"] = json!(t.split(',').collect::<Vec<_>>());
    }
    if let Some(note) = index::note_for(conn, &row.session_id)? {
        v["note"] = json!(note);
    }
    json_response(v)
}

fn row_json(r: &index::SessionRow) -> serde_json::Value {
    json!({
        "id": r.session_id,
        "tool": r.tool,
        "project": r.project,
        "title": r.title,
        "started": r.started,
        "msgs": r.msg_count,
        "kind": r.kind,
        "preview": r.preview,
        "summary": r.summary,
        "tags": r.tags.as_ref().map(|t| t.split(',').collect::<Vec<_>>()),
        "archived": r.archived,
    })
}

fn param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key && !v.is_empty()).then(|| url_decode(v))
    })
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => match (hex(bytes.get(i + 1)), hex(bytes.get(i + 2))) {
                (Some(h), Some(l)) => {
                    out.push(h * 16 + l);
                    i += 3;
                }
                _ => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: Option<&u8>) -> Option<u8> {
    (*b? as char).to_digit(16).map(|d| d as u8)
}

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "windows")]
    let cmd = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let cmd = "xdg-open";
    let _ = std::process::Command::new(cmd)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

const INDEX_HTML: &str = include_str!("webui.html");
