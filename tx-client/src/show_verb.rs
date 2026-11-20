//! `tx _show <client> focus|attach-to|popup <session> [<pane>]` ([`Ui::show_session`]) and
//! `tx _attach-into <client> <session> <pane>` (attach-to's Enter, [`Ui::attach_into`]): showing
//! a session for plugins and key binds that go through `tx`. A failure goes to `client`'s status
//! line as well.

use tx_core::{TxCore, Verb, VerbError, VerbIo};

use crate::show::Show;
use crate::tmux::Ui;

const SHOW_USAGE: &str = "usage: tx _show <client> focus|attach-to|popup <session> [<pane>]\n";
const ATTACH_INTO_USAGE: &str = "usage: tx _attach-into <client> <session> <pane>\n";

fn usage(io: &mut VerbIo<'_>, text: &str) -> Result<i32, VerbError> {
    io.stderr
        .write_all(text.as_bytes())
        .map_err(|error| VerbError::Failed(error.to_string()))?;
    Ok(2)
}

/// `Ok(0)`, else the reason on `client`'s status line and exit 1.
fn reported(ui: &Ui, client: &str, session: &str, outcome: Result<(), String>) -> i32 {
    match outcome {
        Ok(()) => 0,
        Err(reason) => {
            ui.display_status(client, &format!("{session}: {reason}"));
            1
        }
    }
}

pub struct ShowVerb;

impl Verb for ShowVerb {
    fn name(&self) -> &'static str {
        "_show"
    }

    fn usage(&self) -> &'static str {
        "_show <client> focus|attach-to|popup <session> [<pane>]   show a session on the terminal (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let (client, show, session, pane) = match argv {
            [client, show, session] => (client, show, session, None),
            [client, show, session, pane] => (client, show, session, Some(pane.as_str())),
            _ => return usage(io, SHOW_USAGE),
        };
        let Some(show) = Show::parse(show) else {
            return usage(io, SHOW_USAGE);
        };
        let ui = Ui::from_core(core);
        let outcome = ui.show_session(client, show, session, pane);
        Ok(reported(&ui, client, session, outcome))
    }
}

pub struct AttachIntoVerb;

impl Verb for AttachIntoVerb {
    fn name(&self) -> &'static str {
        "_attach-into"
    }

    fn usage(&self) -> &'static str {
        "_attach-into <client> <session> <pane>   attach a session into a pane (attach-to's Enter, internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [client, session, pane] = argv else {
            return usage(io, ATTACH_INTO_USAGE);
        };
        let ui = Ui::from_core(core);
        let outcome = ui.attach_into(pane, session);
        Ok(reported(&ui, client, session, outcome))
    }
}
