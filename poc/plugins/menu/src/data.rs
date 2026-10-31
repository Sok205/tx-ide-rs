//! Gathering the menu's [`Context`]: plugin status and the cheat sheet from the core's
//! registry, past sessions from the records, the fork check from tmux. Reads only; another
//! plugin is seen through its status, its resources and `Session.ext` (D3).

use serde_json::Value;
use tx_core::{CoreError, Launch, Lifecycle, ListFilter, PluginStatus, Session, TxCore};
use tx_ui::tmux::Ui;

use crate::model::{Context, KeyHint, Past};

/// `# keys:` and the first prose comment of a key fragment, up to its first sentence; `None`
/// when the fragment declares no key.
pub fn key_hint(plugin: &str, text: &str) -> Option<KeyHint> {
    let mut keys = Vec::new();
    let mut prose: Vec<&str> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# keys:") {
            keys.extend(
                rest.split_whitespace()
                    .filter(|word| *word != "none")
                    .map(str::to_owned),
            );
            continue;
        }
        if ["#!", "# unset:", "# unhook:"]
            .iter()
            .any(|prefix| line.starts_with(prefix))
        {
            continue;
        }
        let words = line.strip_prefix('#').map(str::trim);
        match words {
            Some(words) if !words.is_empty() => prose.push(words),
            _ if !prose.is_empty() => break,
            _ => {}
        }
    }
    if keys.is_empty() {
        return None;
    }
    let joined = prose.join(" ");
    let mut text = joined.as_str();
    if let Some((_, rest)) = text
        .strip_prefix("tx key plugin `")
        .and_then(|rest| rest.split_once("`:"))
    {
        text = rest.trim_start();
    }
    let sentence = text.split(". ").next().unwrap_or_default();
    Some(KeyHint {
        plugin: plugin.to_owned(),
        keys,
        text: sentence.trim_end_matches('.').to_owned(),
    })
}

fn ended_or_created(session: &Session) -> f64 {
    session
        .ended_at
        .filter(|ended| *ended != 0.0)
        .unwrap_or(session.created_at)
}

/// The number of chats `ext.chat` lists; 0 when it is missing or malformed.
fn chat_count(session: &Session) -> usize {
    session
        .ext
        .get("chat")
        .and_then(|chat| chat.get("chats"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

/// A session `resume` can bring back: an engine session, or one that recorded a chat (a fork
/// runs the engine as a process). An exited shell is not.
fn resumable(session: &Session) -> bool {
    matches!(session.launch, Launch::Agent { .. }) || chat_count(session) > 0
}

/// Resumable exited and stopped sessions, newest end first (the `history` verb's list, minus
/// what `resume` would refuse).
pub fn past_sessions(core: &TxCore) -> Result<Vec<Past>, CoreError> {
    let mut sessions = Vec::new();
    for lifecycle in [Lifecycle::Exited, Lifecycle::Stopped] {
        for id in core.list_sessions(&ListFilter::new().lifecycle(lifecycle))? {
            match core.get_session(&id) {
                Ok(session) if resumable(&session) => sessions.push(session),
                Ok(_) => {}
                Err(CoreError::NotFound(_)) => {}
                Err(error) => return Err(error),
            }
        }
    }
    sessions.sort_by(|a, b| ended_or_created(b).total_cmp(&ended_or_created(a)));
    Ok(sessions
        .iter()
        .map(|session| Past {
            id: session.id.to_string(),
            name: session.name.clone(),
            tag: session.tags.first().cloned().unwrap_or_default(),
            at: ended_or_created(session),
            chats: chat_count(session),
            cwd: session.cwd.display().to_string(),
        })
        .collect())
}

/// The tx session `pane` shows: the session nested in it, else the pane's own, never a view.
pub fn session_in_pane(core: &TxCore, ui: &Ui, pane: &str) -> Option<Session> {
    let name = ui
        .inner_for_pane(pane)
        .or_else(|| ui.display_message("#{session_name}", Some(pane)))
        .filter(|name| !ui.is_view(name))?;
    let id = core.resolve(&name).ok()?;
    core.get_session(&id).ok()
}

pub fn context(core: &TxCore, ui: &Ui, pane: &str) -> Result<Context, CoreError> {
    let plugins: Vec<(String, bool)> = core
        .plugin_status()
        .iter()
        .map(|(id, status)| (id.clone(), *status == PluginStatus::Active))
        .collect();
    let keys = core
        .resources("keys/")
        .iter()
        .filter_map(|resource| key_hint(&resource.plugin, &String::from_utf8_lossy(resource.bytes)))
        .collect();
    Ok(Context {
        active: plugins
            .iter()
            .filter(|(_, active)| *active)
            .map(|(id, _)| id.clone())
            .collect(),
        plugins,
        pane_session: session_in_pane(core, ui, pane).is_some(),
        history: past_sessions(core)?,
        keys,
        now: tx_core::events::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(launch: Launch, ext: Option<Value>) -> Session {
        Session {
            id: "00000000-0000-4000-8000-000000000001".parse().unwrap(),
            name: "s".into(),
            parent: None,
            tags: Vec::new(),
            cwd: "/w".into(),
            created_at: 1.0,
            ended_at: Some(2.0),
            lifecycle: Lifecycle::Exited,
            launch,
            ext: ext
                .map(|chat| [("chat".to_owned(), chat)].into())
                .unwrap_or_default(),
            status: None,
        }
    }

    #[test]
    fn only_engine_sessions_or_sessions_with_chats_are_resumable() {
        let agent = Launch::Agent {
            engine: "claude".into(),
            profile: Vec::new(),
            model: None,
            effort: None,
            resume: None,
        };
        let shell = || Launch::Process {
            program: "zsh".into(),
            args: Vec::new(),
        };
        assert!(resumable(&session(agent, None)));
        assert!(!resumable(&session(shell(), None)));
        assert!(!resumable(&session(
            shell(),
            Some(serde_json::json!({"chats": []}))
        )));
        assert!(resumable(&session(
            shell(),
            Some(serde_json::json!({"chats": [{"id": "c1"}]}))
        )));
        assert!(!resumable(&session(
            shell(),
            Some(serde_json::json!("bad"))
        )));
    }

    #[test]
    fn a_fragment_hint_is_its_keys_and_first_sentence() {
        let text = "#!/usr/bin/env bash\n# keys: t\n# tx key plugin `picker`: prefix+t opens the \
                    tx session picker in a popup. `txc pick` finds the\n# pane to nest into.\n\
                    #\n# The popup is wide.\nset -u\n";
        assert_eq!(
            key_hint("picker", text),
            Some(KeyHint {
                plugin: "picker".into(),
                keys: vec!["t".into()],
                text: "prefix+t opens the tx session picker in a popup".into(),
            })
        );
    }

    #[test]
    fn declarations_are_not_prose_and_several_keys_add_up() {
        let text = "#!/usr/bin/env bash\n# keys: s\n# unset: pane-border-format\n\
                    # unhook: after-new-window\n# keys: S\n# tx key plugin `labels`: prefix+s \
                    lists sessions\n# with their tags\nset -u\n";
        let hint = key_hint("labels", text).unwrap();
        assert_eq!(hint.keys, ["s", "S"]);
        assert_eq!(hint.text, "prefix+s lists sessions with their tags");
    }

    #[test]
    fn a_fragment_without_keys_has_no_hint() {
        assert_eq!(key_hint("x", "#!/bin/sh\n# keys: none\n# words\n"), None);
        assert_eq!(key_hint("x", "#!/bin/sh\n# words\n"), None);
    }

    #[test]
    fn the_shipped_fragment_has_a_hint() {
        let hint = key_hint("menu", include_str!("../tmux/keys.tmux")).unwrap();
        assert_eq!(hint.keys, ["m"]);
        assert!(hint.text.starts_with("prefix+m opens"), "{}", hint.text);
    }
}
