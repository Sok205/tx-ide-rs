//! Delivery of one message to one running session: the envelope is typed into the pane with
//! `send-keys -l`, a pause lets the engine's input handling settle, then `Enter` submits it. A
//! receipt means input was submitted: not read, not completed, not retried.

use std::time::Duration;

use crate::tmux::{Tmux, TmuxError};

/// Between typing the envelope and pressing `Enter`: long enough for an interactive engine to
/// register pasted-looking input as text rather than a shortcut.
pub const DELIVERY_PAUSE: Duration = Duration::from_millis(300);

const USER_TAG: &str = "from-user";
/// `from-claude` is the legacy name of the agent tag; either opens or closes an agent envelope.
const AGENT_TAGS: [&str; 2] = ["from-agent", "from-claude"];

/// `sender: None` is the user; `Some(name)` is another agent, named by its session name.
pub fn build_envelope(sender: Option<&str>, text: &str) -> String {
    match sender {
        None => format!("<{USER_TAG}>{text}</{USER_TAG}>"),
        Some(name) => format!(
            "<{tag} session=\"{name}\">{text}</{tag}>",
            tag = AGENT_TAGS[0]
        ),
    }
}

/// Who an envelope says sent its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sender<'a> {
    /// The operator; `origin` is the session they typed in, when the envelope names one.
    User { origin: Option<&'a str> },
    /// Another agent, by session name.
    Agent(&'a str),
}

/// The inverse of [`build_envelope`]: the sender and body of a text that is exactly one
/// envelope (trailing whitespace is the caller's to trim). Reads the legacy agent tag and a user
/// envelope that names its origin session, which older writers produced.
pub fn parse_envelope(text: &str) -> Option<(Sender<'_>, &str)> {
    let rest = text.strip_prefix('<')?;
    let (family, rest): (&[&str], &str) =
        match AGENT_TAGS.iter().find_map(|tag| rest.strip_prefix(tag)) {
            Some(rest) => (&AGENT_TAGS, rest),
            None => (&[USER_TAG], rest.strip_prefix(USER_TAG)?),
        };
    let (session, rest) = match rest.strip_prefix(" session=\"") {
        Some(rest) => {
            let (session, rest) = rest.split_once('"')?;
            (Some(session), rest)
        }
        None => (None, rest),
    };
    let rest = rest.strip_prefix('>')?;
    let body = family.iter().find_map(|tag| {
        rest.strip_suffix('>')?
            .strip_suffix(tag)?
            .strip_suffix("</")
    })?;
    let sender = if family == [USER_TAG] {
        Sender::User { origin: session }
    } else {
        Sender::Agent(session?)
    };
    Some((sender, body))
}

/// Types `envelope` into `pane`, waits `pause`, submits with `Enter`.
pub fn deliver(tmux: &Tmux, pane: &str, envelope: &str, pause: Duration) -> Result<(), TmuxError> {
    tmux.send_keys_literal(pane, envelope)?;
    std::thread::sleep(pause);
    tmux.send_enter(pane)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn user_envelope() {
        assert_eq!(build_envelope(None, "hi"), "<from-user>hi</from-user>");
    }

    #[test]
    fn parse_reads_what_build_writes_and_the_legacy_forms() {
        let user = build_envelope(None, "hi <b>");
        assert_eq!(
            parse_envelope(&user),
            Some((Sender::User { origin: None }, "hi <b>"))
        );
        let agent = build_envelope(Some("w1"), "done");
        assert_eq!(parse_envelope(&agent), Some((Sender::Agent("w1"), "done")));
        assert_eq!(
            parse_envelope("<from-user session=\"viewer\">q?</from-user>"),
            Some((
                Sender::User {
                    origin: Some("viewer")
                },
                "q?"
            ))
        );
        assert_eq!(
            parse_envelope("<from-claude session=\"bob\">x</from-agent>"),
            Some((Sender::Agent("bob"), "x"))
        );
        for not_one in [
            "hi",
            "<from-agent>no session</from-agent>",
            "<from-user>open only",
            "<from-user>mixed</from-agent>",
            "<from-agent session=\"a\">x</from-user>",
        ] {
            assert_eq!(parse_envelope(not_one), None, "{not_one}");
        }
    }

    #[test]
    fn agent_envelope_names_the_sender() {
        assert_eq!(
            build_envelope(Some("worker-1"), "hi"),
            "<from-agent session=\"worker-1\">hi</from-agent>"
        );
    }

    #[test]
    fn text_is_kept_verbatim() {
        let text = "line one\nline two; <tag> \"q\" ; é 😀";
        assert_eq!(
            build_envelope(None, text),
            format!("<from-user>{text}</from-user>")
        );
        assert_eq!(build_envelope(None, ""), "<from-user></from-user>");
    }

    #[test]
    fn pause_is_the_documented_value() {
        assert_eq!(DELIVERY_PAUSE, Duration::from_millis(300));
    }

    #[test]
    fn deliver_types_the_envelope_and_submits_it() {
        use crate::tmux::Tmux;
        use std::process::Command;
        if Command::new("tmux").arg("-V").output().is_err() {
            eprintln!("tmux not available; skipping");
            return;
        }
        let dir = tempfile::Builder::new()
            .prefix("txc")
            .tempdir_in("/tmp")
            .unwrap();
        let tmux = Tmux::new("tmux", dir.path().join("tmux.sock"), "/dev/null");
        let out = dir.path().join("got.txt");
        let id: crate::session::SessionId = "m".parse().unwrap();
        tmux.new_session(
            "m",
            Path::new("/"),
            "sh",
            &[
                "-c".into(),
                format!("head -n 2 > {}; sleep 30", out.display()),
            ],
            &[],
        )
        .unwrap();
        tmux.set_tx_id("m", &id).unwrap();
        let pane = tmux.is_live(&id).unwrap().unwrap().pane_target;
        std::thread::sleep(Duration::from_millis(200));
        deliver(&tmux, &pane, "-x ; one;", Duration::from_millis(10)).unwrap();
        deliver(&tmux, &pane, "two", Duration::from_millis(10)).unwrap();
        for _ in 0..50 {
            if out.exists() && std::fs::read_to_string(&out).unwrap().lines().count() == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "-x ; one;\ntwo\n");
        assert!(deliver(&tmux, "%999", "x", Duration::ZERO).is_err());
        let _ = Command::new("tmux")
            .args(tmux.argv(&["kill-server"]))
            .output();
    }
}
