//! Message classification and collection over the fixture tree.

use serde_json::{Value, json};
use tx_chat::messages::{Recipient, SenderRole, parse_message};

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
