//! The focus envelope: where the operator was when they pressed prefix+/, as the
//! `<tx-command-prompt …/>` tag the TX-ASSISTANT role reads to resolve "this session", "this view"
//! and "here". A port of the old tx's `focus-envelope`: the firing pane's session, window and
//! pane, the session nested in that pane (or the remote one an ssh nest names), and whether each
//! session is a view or an agent, with the agent's tags.

use tx_client::tmux::Ui;
use tx_core::{AgentId, CoreError, TxCore};

/// What core sets on an agent's session.
const AGENT_ID_OPTION: &str = "@tx-id";
/// What an ssh nest sets on the pane it runs in.
const REMOTE_OPTION: &str = "@remote-session";
const FOCUS_FORMAT: &str = "#{session_name}|#{window_index}|#{window_name}|#{pane_index}|#{pane_title}|#{pane_current_command}|#{pane_current_path}";

/// The firing pane: its session's name and the envelope describing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Focus {
    pub session_name: String,
    pub envelope: String,
}

/// The focus of `pane_id`; `None` when the pane is gone (tmux still expands a missing target, to
/// empty fields), or its expansion does not split into the seven fields (a `|` in a title).
pub fn focus(core: &TxCore, pane_id: &str) -> Result<Option<Focus>, CoreError> {
    let ui = Ui::from_core(core);
    let Some(mut attributes) = ui
        .display_message(FOCUS_FORMAT, Some(pane_id))
        .and_then(|line| parse_focus_line(&line, pane_id))
    else {
        return Ok(None);
    };
    let session_name = attributes[0].1.clone();
    let inner = match ui.pane_option(pane_id, REMOTE_OPTION) {
        Some(remote) => {
            attributes.push(("inner-remote".to_owned(), "1".to_owned()));
            attributes.push(("inner-session-name".to_owned(), remote));
            None
        }
        None => ui.inner_for_pane(pane_id),
    };
    if let Some(inner) = &inner {
        attributes.push(("inner-session-name".to_owned(), inner.clone()));
    }
    push_kind(core, &ui, &mut attributes, &session_name, "session")?;
    if let Some(inner) = &inner {
        push_kind(core, &ui, &mut attributes, inner, "inner-session")?;
    }
    Ok(Some(Focus {
        session_name,
        envelope: format_envelope(&attributes),
    }))
}

/// `<prefix>-kind`: `view` for a view; `process` and `<prefix>-tag` for an agent's session.
/// Nothing for a session that is neither.
fn push_kind(
    core: &TxCore,
    ui: &Ui,
    attributes: &mut Vec<(String, String)>,
    session_name: &str,
    prefix: &str,
) -> Result<(), CoreError> {
    if ui.is_view(session_name) {
        attributes.push((format!("{prefix}-kind"), "view".to_owned()));
        return Ok(());
    }
    let Some(id) = ui
        .session_option(session_name, AGENT_ID_OPTION)
        .and_then(|id| id.parse::<AgentId>().ok())
    else {
        return Ok(());
    };
    let agent = match core.get_agent(&id) {
        Ok(agent) => agent,
        Err(CoreError::NotFound(_)) => return Ok(()),
        Err(error) => return Err(error),
    };
    attributes.push((format!("{prefix}-kind"), "process".to_owned()));
    attributes.push((format!("{prefix}-tag"), agent.tags.join(",")));
    Ok(())
}

fn parse_focus_line(line: &str, pane_id: &str) -> Option<Vec<(String, String)>> {
    let fields: Vec<&str> = line.split('|').collect();
    let [
        session_name,
        window_index,
        window_name,
        pane_index,
        pane_title,
        pane_command,
        pane_path,
    ] = fields.as_slice()
    else {
        return None;
    };
    if session_name.is_empty() {
        return None;
    }
    Some(
        [
            ("session-name", *session_name),
            ("window-index", *window_index),
            ("window-name", *window_name),
            ("pane-id", pane_id),
            ("pane-index", *pane_index),
            ("pane-title", *pane_title),
            ("pane-cmd", *pane_command),
            ("pane-path", *pane_path),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect(),
    )
}

/// Ordered attributes as a self-closing `<tx-command-prompt …/>` tag, values single-quoted.
fn format_envelope(attributes: &[(String, String)]) -> String {
    let mut out = String::from("<tx-command-prompt");
    for (key, value) in attributes {
        out.push_str(&format!(" {key}='{}'", xml_escape(value)));
    }
    out.push_str("/>");
    out
}

fn xml_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('\'', "&apos;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_focus_line_is_seven_fields_with_the_pane_id_inserted() {
        let attributes = parse_focus_line("home|0|main|1|title|zsh|/tmp/x", "%5").unwrap();
        assert_eq!(
            format_envelope(&attributes),
            "<tx-command-prompt session-name='home' window-index='0' window-name='main' \
             pane-id='%5' pane-index='1' pane-title='title' pane-cmd='zsh' pane-path='/tmp/x'/>"
        );
        assert!(parse_focus_line("a|title with | bar|0|1|t|zsh|/", "%5").is_none());
        assert!(parse_focus_line("||||||", "%999").is_none(), "a gone pane");
    }

    #[test]
    fn values_are_escaped_for_single_quotes() {
        let attributes = vec![("pane-title".to_owned(), "it's <b>&x".to_owned())];
        assert_eq!(
            format_envelope(&attributes),
            "<tx-command-prompt pane-title='it&apos;s &lt;b&gt;&amp;x'/>"
        );
    }
}
