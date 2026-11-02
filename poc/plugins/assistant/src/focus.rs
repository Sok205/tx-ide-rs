//! The focus envelope: where the operator was when they pressed prefix+/, as the
//! `<tx-command-prompt …/>` tag the TX-ASSISTANT role reads to resolve "this session", "this
//! window", "here" and "attach it here". The old tx's `focus-envelope`, cut to what those use:
//! the firing pane's session, window and pane position, its directory, the session nested in it,
//! and whether the firing session is a view or an agent.

use tx_client::tmux::Ui;

/// What core sets on an agent's session.
const AGENT_ID_OPTION: &str = "@tx-id";
const FOCUS_FORMAT: &str = "#{session_name}|#{window_index}|#{pane_index}|#{pane_current_path}";

/// The firing pane: its session's name and the envelope describing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Focus {
    pub session_name: String,
    pub envelope: String,
}

/// The focus of `pane_id`; `None` when the pane is gone (tmux still expands a missing target, to
/// empty fields), or its expansion does not split into the four fields (a `|` in the directory).
pub fn focus(ui: &Ui, pane_id: &str) -> Option<Focus> {
    let line = ui.display_message(FOCUS_FORMAT, Some(pane_id))?;
    let mut attributes = parse_focus_line(&line, pane_id)?;
    let session_name = attributes[0].1.clone();
    if let Some(inner) = ui.inner_for_pane(pane_id) {
        attributes.push(("inner-session-name".to_owned(), inner));
    }
    if ui.is_view(&session_name) {
        attributes.push(("session-kind".to_owned(), "view".to_owned()));
    } else if ui.session_option(&session_name, AGENT_ID_OPTION).is_some() {
        attributes.push(("session-kind".to_owned(), "process".to_owned()));
    }
    Some(Focus {
        session_name,
        envelope: format_envelope(&attributes),
    })
}

fn parse_focus_line(line: &str, pane_id: &str) -> Option<Vec<(String, String)>> {
    let fields: Vec<&str> = line.split('|').collect();
    let [session_name, window_index, pane_index, pane_path] = fields.as_slice() else {
        return None;
    };
    if session_name.is_empty() {
        return None;
    }
    Some(
        [
            ("session-name", *session_name),
            ("window-index", *window_index),
            ("pane-index", *pane_index),
            ("pane-id", pane_id),
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
    fn a_focus_line_is_four_fields_with_the_pane_id_inserted() {
        let attributes = parse_focus_line("home|1|3|/tmp/x", "%5").unwrap();
        assert_eq!(
            format_envelope(&attributes),
            "<tx-command-prompt session-name='home' window-index='1' pane-index='3' \
             pane-id='%5' pane-path='/tmp/x'/>"
        );
        assert!(parse_focus_line("home|1|3|/tmp/a|b", "%5").is_none());
        assert!(parse_focus_line("|||", "%999").is_none(), "a gone pane");
    }

    #[test]
    fn values_are_escaped_for_single_quotes() {
        let attributes = vec![("pane-path".to_owned(), "/it's <b>&x".to_owned())];
        assert_eq!(
            format_envelope(&attributes),
            "<tx-command-prompt pane-path='/it&apos;s &lt;b&gt;&amp;x'/>"
        );
    }
}
