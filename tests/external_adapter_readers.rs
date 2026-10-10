//! Sessions supplied by an embedder must remain readable by the standalone
//! CLI, MCP server and web viewer, which do not have the embedder's adapter.

use sessionwiki::adapters::{self, Adapter, Discovered};
use sessionwiki::index;
use sessionwiki::model::{Message, Role, Session, ToolEvent, ToolSummary};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static INDEX_ENV: Mutex<()> = Mutex::new(());
const TRANSCRIPT: &str = "read the embedded conversation";

struct OneFileAdapter {
    source: PathBuf,
    tool: &'static str,
}

impl Adapter for OneFileAdapter {
    fn name(&self) -> &'static str {
        self.tool
    }
    fn root(&self) -> Option<PathBuf> {
        self.source.parent().map(Path::to_path_buf)
    }
    fn discover(&self) -> Discovered {
        vec![self.source.clone()].into()
    }
    fn parse(&self, path: &Path) -> anyhow::Result<Session> {
        if let Some(adapter) = adapters::by_name(self.tool) {
            return adapter.parse(path);
        }
        Ok(Session {
            id: "embedded-session".into(),
            tool: self.tool,
            path: path.into(),
            project: "/fixture".into(),
            started: None,
            ended: None,
            title: "embedded conversation".into(),
            subagent: false,
            messages: vec![Message {
                role: Role::User,
                text: fs::read_to_string(path)?,
                ts: None,
                tool: None,
            }],
            touched: vec![],
            edits: vec![],
        })
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    data: PathBuf,
    source: PathBuf,
    id: String,
}

impl Fixture {
    fn new(tool: &'static str, text: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let source = dir.path().join("embedded.jsonl");
        fs::write(&source, text).unwrap();
        // Only index::open reads this process-global variable. Restore it
        // before the lock is released; subprocesses receive their own value.
        let mut conn = {
            let _guard = INDEX_ENV.lock().unwrap();
            let previous = std::env::var_os("SESSIONWIKI_DATA");
            std::env::set_var("SESSIONWIKI_DATA", &data);
            let result = index::open();
            match previous {
                Some(value) => std::env::set_var("SESSIONWIKI_DATA", value),
                None => std::env::remove_var("SESSIONWIKI_DATA"),
            }
            result.unwrap()
        };
        let adapters: Vec<Box<dyn Adapter>> = vec![Box::new(OneFileAdapter {
            source: source.clone(),
            tool,
        })];
        index::sync_with(&mut conn, &adapters, None).unwrap();
        let rows = index::recent(&conn, 10, Some(tool), None, None, false).unwrap();
        assert_eq!(rows.len(), 1);
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
        Self {
            _dir: dir,
            data,
            source,
            id: rows[0].session_id.clone(),
        }
    }

    fn external(source_exists: bool) -> Self {
        assert!(adapters::by_name("external-test").is_none());
        let fixture = Self::new("external-test", TRANSCRIPT);
        if !source_exists {
            fs::remove_file(&fixture.source).unwrap();
        }
        fixture
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sessionwiki"));
        command.env("SESSIONWIKI_DATA", &self.data);
        command
    }
}

fn cli_reads_external(source_exists: bool) {
    let fixture = Fixture::external(source_exists);
    for command in ["brief", "show"] {
        let output = fixture
            .command()
            .args([command, &fixture.id, "--no-sync", "--json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{command}, source exists={source_exists}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["tool"], "external-test");
        assert!(value.to_string().contains(TRANSCRIPT));
    }
}

#[test]
fn cli_reads_external_with_existing_source() {
    cli_reads_external(true);
}

#[test]
fn cli_reads_external_with_missing_source() {
    cli_reads_external(false);
}

struct ToolOutputAdapter {
    source: PathBuf,
}

impl Adapter for ToolOutputAdapter {
    fn name(&self) -> &'static str {
        "external-tool-output"
    }

    fn root(&self) -> Option<PathBuf> {
        self.source.parent().map(Path::to_path_buf)
    }

    fn discover(&self) -> Discovered {
        if self.source.exists() {
            vec![self.source.clone()].into()
        } else {
            Vec::new().into()
        }
    }

    fn parse(&self, path: &Path) -> anyhow::Result<Session> {
        Ok(Session {
            id: "archived-tool-output".into(),
            tool: self.name(),
            path: path.into(),
            project: "/fixture".into(),
            started: None,
            ended: None,
            title: "archived tool output".into(),
            subagent: false,
            messages: vec![Message {
                role: Role::Tool,
                text: "→ shell(cargo test) ⇒ ok · 2 lines".into(),
                ts: None,
                tool: Some(ToolEvent::Summary(ToolSummary {
                    name: "shell".into(),
                    args: serde_json::json!({"command": "cargo test"}),
                    output: Some("output-from-deleted-source\nsecond diagnostic".into()),
                    is_error: Some(false),
                    lines: Some(2),
                })),
            }],
            touched: Vec::new(),
            edits: Vec::new(),
        })
    }
}

#[test]
fn archived_tool_output_is_available_to_show_full_and_brief_tools() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let source = dir.path().join("sessions/session.jsonl");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, "fixture").unwrap();

    let mut conn = {
        let _guard = INDEX_ENV.lock().unwrap();
        let previous = std::env::var_os("SESSIONWIKI_DATA");
        std::env::set_var("SESSIONWIKI_DATA", &data);
        let result = index::open();
        match previous {
            Some(value) => std::env::set_var("SESSIONWIKI_DATA", value),
            None => std::env::remove_var("SESSIONWIKI_DATA"),
        }
        result.unwrap()
    };
    index::set_tool_output_mode(&conn, index::ToolOutputMode::Full).unwrap();
    index::sync_with(
        &mut conn,
        &[Box::new(ToolOutputAdapter {
            source: source.clone(),
        })],
        None,
    )
    .unwrap();
    fs::remove_file(&source).unwrap();
    index::sync_with(&mut conn, &[Box::new(ToolOutputAdapter { source })], None).unwrap();
    drop(conn);

    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sessionwiki"));
        command.env("SESSIONWIKI_DATA", &data);
        command
    };
    let shown = command()
        .args(["show", "archived-tool-output", "--full", "--no-sync"])
        .output()
        .unwrap();
    assert!(
        shown.status.success(),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    assert!(String::from_utf8(shown.stdout)
        .unwrap()
        .contains("output-from-deleted-source"));

    let brief = command()
        .args(["brief", "archived-tool-output", "--tools", "--no-sync"])
        .output()
        .unwrap();
    assert!(
        brief.status.success(),
        "{}",
        String::from_utf8_lossy(&brief.stderr)
    );
    assert!(String::from_utf8(brief.stdout)
        .unwrap()
        .contains("output-from-deleted-source"));

    let mode = command().arg("tool-output").output().unwrap();
    assert!(mode.status.success());
    assert_eq!(String::from_utf8(mode.stdout).unwrap().trim(), "full");

    let changed = command().args(["tool-output", "summary"]).output().unwrap();
    assert!(changed.status.success());
    assert!(String::from_utf8(changed.stdout)
        .unwrap()
        .contains("removed for live sessions. Archived sessions keep their existing output"));
}

fn mcp_reads_external(source_exists: bool) {
    let fixture = Fixture::external(source_exists);
    let mut child = fixture
        .command()
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for (i, (name, arguments)) in [
            ("get_session_brief", serde_json::json!({"id": fixture.id})),
            ("session_window", serde_json::json!({"id": fixture.id})),
            (
                "session_window",
                serde_json::json!({"id": fixture.id, "turn": 0}),
            ),
        ]
        .iter()
        .enumerate()
        {
            writeln!(
                stdin,
                "{}",
                serde_json::json!({"jsonrpc":"2.0", "id":i, "method":"tools/call",
                    "params":{"name":name,"arguments":arguments}})
            )
            .unwrap();
        }
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let replies: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies.len(), 3);
    for (i, reply) in replies.iter().enumerate() {
        assert_eq!(reply["id"], i);
        assert!(reply.get("error").is_none(), "{reply}");
        assert_ne!(reply["result"]["isError"], true, "{reply}");
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains(TRANSCRIPT), "{text}");
    }
}

#[test]
fn mcp_reads_external_with_existing_source() {
    mcp_reads_external(true);
}

#[test]
fn mcp_reads_external_with_missing_source() {
    mcp_reads_external(false);
}

// The web server runs indefinitely. Reap it even when an assertion panics.
struct WebChild(Child);
impl Drop for WebChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn web_reads_external(source_exists: bool) {
    let fixture = Fixture::external(source_exists);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut child = WebChild(
        fixture
            .command()
            .args(["web", "--no-open", "--port", &address.port().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stream = loop {
        assert!(child.0.try_wait().unwrap().is_none(), "web server exited");
        if let Ok(stream) = TcpStream::connect(address) {
            break stream;
        }
        assert!(Instant::now() < deadline, "web server did not start");
        std::thread::sleep(Duration::from_millis(10));
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET /api/session/{} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n",
        fixture.id
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
    let (_, body) = response.split_once("\r\n\r\n").unwrap();
    let session: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(session["tool"], "external-test");
    assert_eq!(session["id"], fixture.id);
    assert_eq!(session["messages"][0]["text"], TRANSCRIPT);
    assert_ne!(session["archived"], true, "fallback does not archive a row");
}

#[test]
fn web_reads_external_with_existing_source() {
    web_reads_external(true);
}

#[test]
fn web_reads_external_with_missing_source() {
    web_reads_external(false);
}

#[test]
fn builtin_sources_are_reparsed_and_read_errors_are_preserved() {
    let old = r#"{"messages":[{"type":"user","content":"old indexed text"}]}"#;
    let fixture = Fixture::new("gemini", old);
    fs::write(
        &fixture.source,
        old.replace("old indexed text", "fresh builtin text"),
    )
    .unwrap();
    let output = fixture
        .command()
        .args(["show", &fixture.id, "--no-sync", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["messages"][0]["text"], "fresh builtin text");

    // An existing but malformed source must surface the parser error,
    // not silently substitute the older indexed transcript.
    fs::write(&fixture.source, "not valid JSON").unwrap();
    let output = fixture
        .command()
        .args(["show", &fixture.id, "--no-sync", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("parse"));
}
