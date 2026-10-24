//! Message classification and collection over the fixture tree.

use std::path::Path;

use serde_json::{Value, json};
use tx_chat::messages::{Recipient, SenderRole, collect_messages, parse_message, source_signature};
use tx_core::Session;

const TS: &str = "2026-01-02T03:04:05.123Z";

fn turn(content: Value, extra: Value) -> Value {
    let mut obj = json!({
        "type": "user", "message": {"role": "user", "content": content},
        "userType": "external", "timestamp": TS, "uuid": "u1",
    });
    for (key, value) in extra.as_object().unwrap() {
        obj[key] = value.clone();
    }
    obj
}

fn parse(obj: &Value, name: &str, cmd: &str) -> Option<Value> {
    let recipient = Recipient {
        id: "rid",
        name,
        cmd,
        chat_id: "c1",
    };
    let roles = |sender: &str| match sender {
        "alice" => Some(SenderRole::Agent),
        "home" => Some(SenderRole::Shell),
        _ => None,
    };
    parse_message(obj, &recipient, &roles).map(|message| message.to_value())
}

/// `parse_message(...).to_dict()` from CPython 3.14 against lib/tx (recipient_name "",
/// recipient_cmd "claude 'fix the bug'", alice → llm, home → shell).
#[test]
fn classification_matches_python() {
    let cmd = "claude 'fix the bug'";
    let expect = |kind: &str, via: &str, sender: &str, body: &str, uuid: &str, origin: &str| {
        json!({
            "ts": 1767323045.123, "iso": TS, "kind": kind, "via": via, "sender": sender,
            "recipient": "rid", "recipient_id": "rid", "body": body, "chat_id": "c1",
            "uuid": uuid, "origin": origin,
        })
    };
    let text = |s: &str| turn(json!(s), json!({}));
    let cases: Vec<(Value, Option<Value>)> = vec![
        (
            text("<from-agent session=\"alice\">hi </from-agent> there</from-agent>\n "),
            Some(expect(
                "agent",
                "send-message",
                "alice",
                "hi </from-agent> there",
                "u1",
                "",
            )),
        ),
        (
            text("<from-claude session=\"bob\">x</from-agent>"),
            Some(expect("agent", "send-message", "bob", "x", "u1", "")),
        ),
        (
            text("<from-agent session=\"home\">ping</from-agent>"),
            Some(expect("you", "send-message", "home", "ping", "u1", "")),
        ),
        (text("<from-agent session=\"alice\">  </from-agent>"), None),
        (
            text("  <from-user session=\"viewer\"> q? </from-user>"),
            Some(expect(
                "you",
                "send-user-message",
                "you",
                "q?",
                "u1",
                "viewer",
            )),
        ),
        (
            text("<tx-command-prompt focus=\"a/b\"/>\n  do it "),
            Some(expect("you", "prompt", "you", "do it", "u1", "")),
        ),
        (
            text("<tx-command-promptx/> no"),
            Some(expect(
                "you",
                "typed",
                "you",
                "<tx-command-promptx/> no",
                "u1",
                "",
            )),
        ),
        (
            text("<tx-command-prompt a>b/> no"),
            Some(expect(
                "you",
                "typed",
                "you",
                "<tx-command-prompt a>b/> no",
                "u1",
                "",
            )),
        ),
        (
            text("Act on these 3 tx sessions: go "),
            Some(expect(
                "you",
                "composer",
                "you",
                "Act on these 3 tx sessions: go",
                "u1",
                "",
            )),
        ),
        (
            text("Act on tx sessions go"),
            Some(expect(
                "you",
                "typed",
                "you",
                "Act on tx sessions go",
                "u1",
                "",
            )),
        ),
        (text("<task-notification>x"), None),
        (text("Read ~/.tx-ide/agents/COMMON.md now"), None),
        (text("please do this as your first actions"), None),
        (text("fix the bug"), None),
        (
            turn(
                json!([{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]),
                json!({"uuid": ""}),
            ),
            Some(expect(
                "you",
                "typed",
                "you",
                "a\nb",
                "c1:2026-01-02T03:04:05.123Z:3",
                "",
            )),
        ),
        (
            turn(
                json!([{"type": "text", "text": "a"}, {"type": "tool_result"}]),
                json!({}),
            ),
            None,
        ),
        (
            json!({"type": "assistant", "message": {"role": "user", "content": "q"}}),
            None,
        ),
        (
            text("\u{1c}<from-agent session=\"alice\">y</from-agent>\u{1f}"),
            Some(expect("agent", "send-message", "alice", "y", "u1", "")),
        ),
    ];
    for (obj, expected) in cases {
        assert_eq!(parse(&obj, "", cmd), expected, "{obj}");
    }
}

// ----- collection over the fixture tree ---------------------------------------------------------

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/messages")
        .join(name)
}

fn copy_fixture(from: &str, to: &Path) {
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    std::fs::copy(fixture(from), to).unwrap();
}

fn record(
    id: &str,
    name: &str,
    lifecycle: &str,
    launch: Value,
    created: f64,
    ended: Value,
    ext: Value,
) -> Session {
    serde_json::from_value(json!({
        "id": id, "name": name, "parent": null, "tags": [], "cwd": "/r",
        "created_at": created, "ended_at": ended, "lifecycle": lifecycle,
        "launch": launch, "ext": ext,
    }))
    .unwrap()
}

fn agent() -> Value {
    json!({"kind": "agent", "engine": "claude", "profile": [], "model": null, "effort": null})
}

fn process() -> Value {
    json!({"kind": "process", "program": "sh", "args": []})
}

struct Tree {
    _root: tempfile::TempDir,
    history: std::path::PathBuf,
    live: std::path::PathBuf,
    sessions: Vec<Session>,
}

fn tree() -> Tree {
    let root = tempfile::tempdir().unwrap();
    let history = root.path().join("history");
    let live = root.path().join("mirror/live.jsonl");
    copy_fixture("bundle.jsonl", &history.join("t1/c1/transcript.jsonl"));
    copy_fixture("gone.jsonl", &history.join("gone/c9/transcript.jsonl"));
    copy_fixture("live.jsonl", &live);
    let sessions = vec![
        record(
            "t1",
            "bob",
            "running",
            agent(),
            900.0,
            Value::Null,
            json!({"history": {"chats": {"c1": {"transcript": live, "at": 950.0}}}}),
        ),
        record(
            "s1",
            "alice",
            "exited",
            agent(),
            900.0,
            json!(960.0),
            json!({}),
        ),
        record(
            "s0",
            "alice",
            "running",
            process(),
            900.0,
            Value::Null,
            json!({}),
        ),
        record(
            "h1",
            "home",
            "running",
            process(),
            900.0,
            Value::Null,
            json!({}),
        ),
    ];
    Tree {
        _root: root,
        history,
        live,
        sessions,
    }
}

/// `print(uuid, kind, via, sender, recipient, recipient_id, chat_id, repr(body), ts)`.
#[test]
fn collect_over_the_fixture_tree() {
    let tree = tree();
    let rows: Vec<String> = collect_messages(&tree.sessions, &tree.history)
        .into_iter()
        .map(|m| {
            format!(
                "{} {} {} {} {} {} {} {:?} {:.1}",
                m.uuid,
                m.kind.as_str(),
                m.via.as_str(),
                m.sender,
                m.recipient,
                m.recipient_id,
                m.chat_id,
                m.body,
                m.ts
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            "u2 you typed you bob t1 c1 \"typed by you\" 1767315600.0",
            "c1:2026-01-02T01:30:00Z:8 you typed you bob t1 c1 \"kick off\" 1767317400.0",
            "u3 you send-message home gone gone c9 \"from home\" 1767319200.0",
            "u1 agent send-message alice bob t1 c1 \"peer\" 1767322800.0",
            "u4 you typed you bob t1 c1 \"later\" 1767326400.0",
        ]
    );
}

#[test]
fn only_running_records_contribute_live_transcripts() {
    let mut tree = tree();
    tree.sessions[0].lifecycle = tx_core::Lifecycle::Exited;
    let uuids: Vec<String> = collect_messages(&tree.sessions, &tree.history)
        .into_iter()
        .map(|m| m.uuid)
        .collect();
    assert!(!uuids.contains(&"u4".to_owned()), "{uuids:?}");
}

#[test]
fn source_signature_changes_only_when_a_source_changes() {
    let tree = tree();
    let first = source_signature(&tree.sessions, &tree.history);
    assert_eq!(first.len(), 40);
    assert_eq!(source_signature(&tree.sessions, &tree.history), first);
    let mut data = std::fs::read(&tree.live).unwrap();
    data.extend_from_slice(b"\n");
    std::fs::write(&tree.live, data).unwrap();
    let grown = source_signature(&tree.sessions, &tree.history);
    assert_ne!(grown, first);
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(100);
    std::fs::File::options()
        .write(true)
        .open(&tree.live)
        .unwrap()
        .set_modified(past)
        .unwrap();
    assert_ne!(source_signature(&tree.sessions, &tree.history), grown);
}
