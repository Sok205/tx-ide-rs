//! The `chat` plugin's capture: `_chat-hook` records the chat id and transcript path the agent
//! reports into `ext["chat"]`; `chat ls` and `chat hooks` are the user-facing verbs.

use std::sync::Arc;

use serde_json::Value;
use tx_core::events::now;
use tx_core::{Launch, SessionId, TxCore, Verb, VerbError, VerbIo};
use tx_ui::render::reltime;

use crate::ChatError;
use crate::ext::{ChatExt, ChatRef, HistoryExt, Role, chat_ext, history_ext, owned_elsewhere};
use crate::history_verbs::{SESSION_ID_ENV, home_dir, read_payload, resolve, say};
use crate::install::{Entries, install, uninstall};

pub const CHAT_ENTRIES: Entries = Entries {
    sidecar: ".tx-chat-managed.json",
    events: &[
        ("SessionStart", "session-start"),
        ("UserPromptSubmit", "prompt-submit"),
        ("Stop", "stop"),
        ("SessionEnd", "session-end"),
    ],
    verb: "_chat-hook",
};

const HOOK_EVENTS: &[&str] = &["session-start", "prompt-submit", "stop", "session-end"];
const HOOK_USAGE: &str = "usage: txc _chat-hook session-start|prompt-submit|stop|session-end\n";
const CHAT_USAGE: &str = "\
usage: txc chat ls <session>
       txc chat hooks install [--dry-run] | uninstall
";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Captured {
    pub chat_id: String,
    pub transcript: String,
}

/// The chat id and transcript path of a hook payload; both must be strings.
pub fn captured(payload: &Value) -> Option<Captured> {
    Some(Captured {
        chat_id: payload.get("session_id")?.as_str()?.to_owned(),
        transcript: payload.get("transcript_path")?.as_str()?.to_owned(),
    })
}

/// Index of the latest pending ref to fill with `captured_id`, or `None` when the id is already
/// recorded, nothing is pending, or the pending ref is a lazy fork still reporting its source id.
pub fn pending_index(ext: &ChatExt, captured_id: &str) -> Option<usize> {
    if ext
        .chats
        .iter()
        .any(|chat| chat.id.as_deref() == Some(captured_id))
    {
        return None;
    }
    let index = ext.chats.iter().rposition(|chat| chat.id.is_none())?;
    let pending = &ext.chats[index];
    if pending.role == Role::Fork
        && pending
            .origin
            .as_ref()
            .is_some_and(|origin| origin.chat == captured_id)
    {
        return None;
    }
    Some(index)
}

/// Record `c` on session `id`. Returns whether the record changed. A malformed `ext.chat`, a
/// chat another session already lists, and an already listed chat all write nothing. The
/// ownership check precedes the locked update, because the update must not call the core.
pub fn capture(core: &TxCore, id: &SessionId, c: &Captured) -> Result<bool, ChatError> {
    let session = core.get_session(id)?;
    let Ok(own) = chat_ext(&session) else {
        return Ok(false);
    };
    if own
        .chats
        .iter()
        .any(|chat| chat.id.as_deref() == Some(c.chat_id.as_str()))
        || owned_elsewhere(core, id, &c.chat_id)?
    {
        return Ok(false);
    }
    let launch_engine = match &session.launch {
        Launch::Agent { engine, .. } => Some(engine.clone()),
        Launch::Process { .. } => None,
    };
    let changed = core.update_ext(id, crate::CHAT_ID, |value| {
        let Ok(mut ext) = (match &*value {
            Value::Null => Ok(ChatExt::default()),
            other => serde_json::from_value::<ChatExt>(other.clone()),
        }) else {
            return;
        };
        if ext
            .chats
            .iter()
            .any(|chat| chat.id.as_deref() == Some(c.chat_id.as_str()))
        {
            return;
        }
        if let Some(index) = pending_index(&ext, &c.chat_id) {
            let pending = &mut ext.chats[index];
            pending.id = Some(c.chat_id.clone());
            pending.transcript = Some(c.transcript.clone());
        } else if ext.chats.is_empty() {
            ext.chats.push(ChatRef {
                id: Some(c.chat_id.clone()),
                role: Role::Primary,
                how: "spawn".into(),
                since: now(),
                ended_at: None,
                origin: None,
                transcript: Some(c.transcript.clone()),
                rest: Default::default(),
            });
        } else {
            return;
        }
        if ext.engine.is_none() {
            ext.engine = launch_engine;
        }
        if let Ok(updated) = serde_json::to_value(&ext) {
            *value = updated;
        }
    })?;
    Ok(changed)
}

fn prefix(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// `chat ls`: one line per chat ref.
pub fn render_chats(name: &str, ext: &ChatExt, hist: &HistoryExt, now: f64) -> String {
    let header = format!("{name} — {} chat(s)", ext.chats.len());
    if ext.chats.is_empty() {
        return header + "\n  (none)";
    }
    let mut lines = vec![header];
    for chat in &ext.chats {
        let identifier = match chat.id.as_deref() {
            Some(id) if !id.is_empty() => prefix(id, 8),
            _ => "pending".into(),
        };
        let mut origin = chat.how.clone();
        if let Some(parent) = chat.origin.as_ref().filter(|o| !o.chat.is_empty()) {
            origin.push('←');
            origin.push_str(&prefix(&parent.chat, 8));
        }
        let bundle = chat
            .id
            .as_deref()
            .and_then(|id| hist.chats.get(id))
            .and_then(|h| h.bundle.as_deref())
            .filter(|path| !path.is_empty())
            .unwrap_or("—");
        let role = serde_json::to_value(chat.role)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        lines.push(format!(
            "  {identifier:<8}  {role:<9} {origin:<18} {:>4} ago   {bundle}",
            reltime(Some(chat.since), now),
        ));
    }
    lines.join("\n")
}

pub fn verbs() -> Vec<Arc<dyn Verb>> {
    vec![Arc::new(ChatHook), Arc::new(Chat)]
}

struct ChatHook;

impl Verb for ChatHook {
    fn name(&self) -> &'static str {
        "_chat-hook"
    }

    fn usage(&self) -> &'static str {
        "_chat-hook <event>   record the agent's chat id (agent hook)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [event] = argv else {
            say(io.stderr, HOOK_USAGE)?;
            return Ok(2);
        };
        if !HOOK_EVENTS.contains(&event.as_str()) {
            return Ok(0);
        }
        let Some(id) = std::env::var(SESSION_ID_ENV)
            .ok()
            .and_then(|raw| raw.parse::<SessionId>().ok())
        else {
            return Ok(0);
        };
        let payload = read_payload(io.stdin);
        let Some(c) = captured(&payload) else {
            return Ok(0);
        };
        match capture(core, &id, &c) {
            Ok(_) | Err(ChatError::Core(tx_core::CoreError::NotFound(_))) => Ok(0),
            Err(error) => Err(error.into()),
        }
    }
}

struct Chat;

impl Verb for Chat {
    fn name(&self) -> &'static str {
        "chat"
    }

    fn usage(&self) -> &'static str {
        "chat ls <session> | hooks install [--dry-run] | uninstall   a session's chats"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let strs: Vec<&str> = argv.iter().map(String::as_str).collect();
        match strs[..] {
            ["ls", token] => ls(core, io, token),
            ["hooks", "install"] => hooks_install(io, false),
            ["hooks", "install", "--dry-run"] => hooks_install(io, true),
            ["hooks", "uninstall"] => {
                uninstall(&home_dir()?, &CHAT_ENTRIES, io.stdout)
                    .map_err(|e| VerbError::Failed(e.to_string()))?;
                Ok(0)
            }
            _ => {
                say(io.stderr, CHAT_USAGE)?;
                Ok(2)
            }
        }
    }
}

fn hooks_install(io: &mut VerbIo<'_>, dry_run: bool) -> Result<i32, VerbError> {
    let exe =
        std::env::current_exe().map_err(|e| VerbError::Failed(format!("current_exe: {e}")))?;
    install(&home_dir()?, &exe, &CHAT_ENTRIES, dry_run, io.stdout)
        .map_err(|e| VerbError::Failed(e.to_string()))?;
    Ok(0)
}

fn ls(core: &TxCore, io: &mut VerbIo<'_>, token: &str) -> Result<i32, VerbError> {
    let Some(id) = resolve(core, token)? else {
        say(
            io.stderr,
            &format!("chat ls: session '{token}' not found\n"),
        )?;
        return Ok(1);
    };
    let session = core.get_session(&id)?;
    let ext = match chat_ext(&session) {
        Ok(ext) => ext,
        Err(error) => {
            say(io.stderr, &format!("chat ls: {error}\n"))?;
            return Ok(1);
        }
    };
    let hist = match history_ext(&session) {
        Ok(hist) => hist,
        Err(error) => {
            say(io.stderr, &format!("chat ls: {error}\n"))?;
            return Ok(1);
        }
    };
    say(
        io.stdout,
        &format!("{}\n", render_chats(&session.name, &ext, &hist, now())),
    )?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};

    use super::*;
    use crate::ext::{HistoryChat, Origin};

    fn chat(id: Option<&str>, role: Role, origin: Option<&str>, since: f64) -> ChatRef {
        ChatRef {
            id: id.map(str::to_owned),
            role,
            how: if role == Role::Primary {
                "spawn"
            } else {
                "fork"
            }
            .into(),
            since,
            ended_at: None,
            origin: origin.map(|chat| Origin {
                session: "s".into(),
                chat: chat.into(),
                rest: Map::new(),
            }),
            transcript: None,
            rest: Map::new(),
        }
    }

    fn ext(chats: Vec<ChatRef>) -> ChatExt {
        ChatExt {
            chats,
            ..ChatExt::default()
        }
    }

    #[test]
    fn pending_index_skips_a_lazy_fork_reporting_its_source() {
        let e = ext(vec![
            chat(Some("src"), Role::Primary, None, 1.0),
            chat(None, Role::Fork, Some("src"), 2.0),
        ]);
        assert_eq!(pending_index(&e, "src"), None);
        assert_eq!(pending_index(&e, "other"), Some(1));
    }

    #[test]
    fn pending_index_none_when_already_recorded() {
        let e = ext(vec![
            chat(Some("a"), Role::Primary, None, 1.0),
            chat(None, Role::Fork, Some("zzz"), 2.0),
        ]);
        assert_eq!(pending_index(&e, "a"), None);
        assert_eq!(pending_index(&ext(vec![]), "a"), None);
    }

    #[test]
    fn captured_needs_both_strings() {
        let ok = captured(&json!({"session_id": "c", "transcript_path": "/t"}));
        assert_eq!(
            ok,
            Some(Captured {
                chat_id: "c".into(),
                transcript: "/t".into()
            })
        );
        for bad in [
            json!({}),
            json!({"session_id": "c"}),
            json!({"transcript_path": "/t"}),
            json!({"session_id": 3, "transcript_path": "/t"}),
            json!({"session_id": "c", "transcript_path": null}),
            json!([1]),
        ] {
            assert_eq!(captured(&bad), None, "{bad}");
        }
    }

    #[test]
    fn render_chats_rows() {
        let now = 10_000.0;
        let e = ext(vec![
            chat(Some("0123456789abcdef"), Role::Primary, None, now - 125.0),
            chat(None, Role::Fork, Some("fedcba9876543210"), now - 5.0),
        ]);
        let mut hist = HistoryExt::default();
        hist.chats.insert(
            "0123456789abcdef".into(),
            HistoryChat {
                transcript: "/t".into(),
                bundle: Some("/b/c1".into()),
                at: 1.0,
                rest: Map::new(),
            },
        );
        assert_eq!(
            render_chats("alice", &e, &hist, now),
            "alice — 2 chat(s)\n\
             \x20 01234567  primary   spawn                2m ago   /b/c1\n\
             \x20 pending   fork      fork←fedcba98        5s ago   —"
        );
        assert_eq!(
            render_chats("bob", &ext(vec![]), &hist, now),
            "bob — 0 chat(s)\n  (none)"
        );
    }
}
