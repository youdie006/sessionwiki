use sessionwiki::adapters;
use sessionwiki::model::{Role, Session};
use std::path::{Path, PathBuf};

fn fake_openai_key(fill: char) -> String {
    format!("{}{}", "sk-", fill.to_string().repeat(45))
}

fn parse(tool: &str, path: &Path) -> Session {
    let adapter = adapters::by_name(tool).expect("adapter exists");
    adapters::parse_session(adapter.as_ref(), path).expect("fixture parses")
}

fn write_json(path: &Path, value: &serde_json::Value) {
    std::fs::create_dir_all(path.parent().expect("fixture has a parent")).unwrap();
    std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

fn prodex_task(root: &Path, id: &str, prompt: &str) -> PathBuf {
    let path = root.join(".bridge/tasks").join(format!("{id}.json"));
    write_json(
        &path,
        &serde_json::json!({
            "id": id,
            "title": "GPT Pro consult",
            "prompt": prompt,
        }),
    );
    path
}

#[test]
fn common_title_redacts_before_the_eighty_character_cap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-test.jsonl");
    let secret = fake_openai_key('Z');
    let prompt = format!("{} {secret}", "T".repeat(65));
    let event = serde_json::json!({
        "type": "event_msg",
        "payload": {"type": "user_message", "message": prompt},
    });
    std::fs::write(&path, format!("{event}\n")).unwrap();

    let session = parse("codex", &path);

    assert!(
        !session.title.contains(&secret[..12]),
        "title: {}",
        session.title
    );
    assert_eq!(session.messages[0].text, prompt);
    assert!(session.messages[0].text.contains(&secret));
    assert!(std::fs::read_to_string(path).unwrap().contains(&secret));
}

#[test]
fn prodex_title_redacts_before_its_own_eighty_character_cap() {
    let dir = tempfile::tempdir().unwrap();
    let secret = fake_openai_key('P');
    let prompt = format!("{} {secret}", "T".repeat(65));
    let path = prodex_task(dir.path(), "task_20260914_120000_redaction-title", &prompt);

    let session = parse("prodex", &path);

    assert!(
        !session.title.contains(&secret[..12]),
        "title: {}",
        session.title
    );
    assert_eq!(session.messages[0].text, prompt);
    assert!(session.messages[0].text.contains(&secret));
    assert!(std::fs::read_to_string(path).unwrap().contains(&secret));
}

#[test]
fn prodex_redacts_a_multiline_pem_before_selecting_the_title_line() {
    let dir = tempfile::tempdir().unwrap();
    let prompt = concat!(
        "Inspect -----BEGIN OPENSSH PRIVATE KEY-----\n",
        "c3ludGhldGljLWtleS1ib2R5\n",
        "-----END OPENSSH PRIVATE KEY----- then preserve this suffix"
    );
    let path = prodex_task(dir.path(), "task_20260914_120100_multiline-title", prompt);

    let session = parse("prodex", &path);

    assert!(
        !session.title.contains("BEGIN OPENSSH"),
        "title: {}",
        session.title
    );
    assert!(
        session.title.contains("then preserve this suffix"),
        "title: {}",
        session.title
    );
    assert_eq!(session.messages[0].text, prompt);
}

#[test]
fn json_encoded_tool_args_are_structured_and_redacted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("continue-session.json");
    let secret = fake_openai_key('J');
    let raw_arguments = serde_json::json!({
        "provider": format!("{} {secret}", "R".repeat(275))
    })
    .to_string();
    write_json(
        &path,
        &serde_json::json!({
            "title": "New Session",
            "history": [
                {"message": {"role": "user", "content": "safe prompt"}},
                {"message": {
                    "role": "assistant",
                    "content": "done",
                    "toolCalls": [{
                        "function": {
                            "name": "provider_call",
                            "arguments": raw_arguments,
                        }
                    }]
                }}
            ]
        }),
    );

    let session = parse("continue", &path);
    let tool = session
        .messages
        .iter()
        .find(|message| message.role == Role::Tool)
        .expect("tool preview exists");

    assert!(tool.text.starts_with("→ provider_call("));
    assert!(tool.text.ends_with("⇒ pending"));
    assert!(
        !tool.text.contains(&secret[..10]),
        "tool line: {}",
        tool.text
    );
    let Some(sessionwiki::model::ToolEvent::Summary(summary)) = &tool.tool else {
        panic!("Continue tool call should retain structured args");
    };
    let provider = summary.args["provider"].as_str().expect("provider arg");
    assert!(
        !provider.contains(&secret[..10]),
        "structured arg: {provider}"
    );
    assert!(provider.contains("[redacted:openai]"));
    assert!(provider.contains(&"R".repeat(275)));
    assert!(std::fs::read_to_string(path).unwrap().contains(&secret));
}

#[test]
fn claude_edit_snippet_redacts_before_the_snippet_cap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("claude-session.jsonl");
    let secret = fake_openai_key('E');
    let content = format!("{} {secret}", "N".repeat(190));
    let user = serde_json::json!({
        "type": "user",
        "message": {"content": "safe prompt"},
    });
    let assistant = serde_json::json!({
        "type": "assistant",
        "message": {"content": [{
            "type": "tool_use",
            "name": "Write",
            "input": {"file_path": "src/safe.rs", "content": content},
        }]},
    });
    std::fs::write(&path, format!("{user}\n{assistant}\n")).unwrap();

    let session = parse("claude-code", &path);

    assert_eq!(session.edits.len(), 1);
    assert!(
        !session.edits[0].snippet.contains(&secret[..8]),
        "edit snippet: {}",
        session.edits[0].snippet
    );
    let tool = session
        .messages
        .iter()
        .find(|message| message.role == Role::Tool)
        .unwrap();
    assert_eq!(tool.text, "→ Write(src/safe.rs) ⇒ pending");
    let Some(sessionwiki::model::ToolEvent::Summary(summary)) = &tool.tool else {
        panic!("Claude tool call should retain structured args");
    };
    let args = summary.args["content"].as_str().unwrap();
    assert!(!args.contains(&secret[..8]));
    assert!(
        args.contains(&"N".repeat(190)),
        "args are not preview-truncated"
    );
    assert!(std::fs::read_to_string(path).unwrap().contains(&secret));
}

#[test]
fn claude_tool_args_and_results_keep_redaction_without_the_old_preview_cap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("claude-tool-redaction.jsonl");
    let secret = fake_openai_key('C');
    let command = format!("{} {secret}", "x".repeat(500));
    let call = serde_json::json!({
        "type": "assistant",
        "timestamp": "2026-07-01T10:00:00Z",
        "message": {"content": [{
            "type": "tool_use",
            "id": "call-1",
            "name": "Bash",
            "input": {"command": command},
        }]},
    });
    let result = serde_json::json!({
        "type": "user",
        "timestamp": "2026-07-01T10:00:01Z",
        "message": {"content": [{
            "type": "tool_result",
            "tool_use_id": "call-1",
            "content": format!("result {secret}"),
        }]},
    });
    std::fs::write(&path, format!("{call}\n{result}\n")).unwrap();

    let session = parse("claude-code", &path);
    let tool = session
        .messages
        .iter()
        .find(|message| message.role == Role::Tool)
        .unwrap();
    assert!(tool.text.starts_with("→ Bash("));
    let Some(sessionwiki::model::ToolEvent::Summary(summary)) = &tool.tool else {
        panic!("paired Claude call should retain its summary");
    };
    let command = summary.args["command"].as_str().unwrap();
    assert!(command.len() > 300, "full bounded argument retained");
    assert!(!command.contains(&secret[..8]));
    let output = summary.output.as_deref().unwrap();
    assert!(!output.contains(&secret[..8]));
    assert!(output.contains("[redacted:openai]"));
}

#[test]
fn prodex_answer_redacts_before_the_sixty_four_kib_cap() {
    const ANSWER_CAP: usize = 64 * 1024;

    let dir = tempfile::tempdir().unwrap();
    let id = "task_20260914_120200_answer-boundary";
    let task = prodex_task(dir.path(), id, "safe prompt");
    let artifact = dir
        .path()
        .join(".bridge/artifacts/pro-consults")
        .join(format!("{id}.md"));
    let secret = fake_openai_key('A');
    let answer = format!("{} {secret}", "A".repeat(ANSWER_CAP - 12));
    std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    std::fs::write(&artifact, &answer).unwrap();

    let session = parse("prodex", &task);
    let answer_message = session
        .messages
        .iter()
        .find(|message| message.role == Role::Assistant)
        .expect("answer artifact becomes an assistant message");

    assert!(
        !answer_message.text.contains(&secret[..8]),
        "bounded answer leaked a credential prefix"
    );
    assert!(std::fs::read_to_string(artifact).unwrap().contains(&secret));
}
