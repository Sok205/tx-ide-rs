//! Delivery of one message to one running session: the envelope is typed into the pane with
//! `send-keys -l`, a pause lets the engine's input handling settle, then `Enter` submits it. A
//! receipt means input was submitted: not read, not completed, not retried.

use std::time::Duration;

use crate::tmux::{Tmux, TmuxError};

/// Between typing the envelope and pressing `Enter`: long enough for an interactive engine to
/// register pasted-looking input as text rather than a shortcut.
pub const DELIVERY_PAUSE: Duration = Duration::from_millis(300);

/// `sender: None` is the user; `Some(name)` is another agent, named by its session name.
pub fn build_envelope(sender: Option<&str>, text: &str) -> String {
    match sender {
        None => format!("<from-user>{text}</from-user>"),
        Some(name) => format!("<from-agent session=\"{name}\">{text}</from-agent>"),
    }
}

/// Types `envelope` into `pane`, waits `pause`, submits with `Enter`.
pub fn deliver(
    tmux: &Tmux,
    pane: &str,
    envelope: &str,
    pause: Duration,
) -> Result<(), TmuxError> {
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
        let dir = tempfile::Builder::new().prefix("txc").tempdir_in("/tmp").unwrap();
        let tmux = Tmux::new("tmux", dir.path().join("tmux.sock"), "/dev/null");
        let out = dir.path().join("got.txt");
        let id: crate::session::SessionId = "m".parse().unwrap();
        tmux.new_session(
            "m",
            Path::new("/"),
            "sh",
            &["-c".into(), format!("head -n 2 > {}; sleep 30", out.display())],
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
        let _ = Command::new("tmux").args(tmux.argv(&["kill-server".into()])).output();
    }
}
