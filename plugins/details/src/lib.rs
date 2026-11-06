//! tx-core plugin `details`: `tx details <agent>` prints an agent's spawn configuration — its
//! record, its last launch's prompt, and the tools, MCP servers and skills its definition and
//! roles resolve to — and `_details-popup` (prefix+i) shows the same for the session in the
//! active pane. Publishes `keys/details.tmux` and its helper script.

pub mod model;
pub mod popup;
pub mod rows;

use std::sync::Arc;

use tx_client::tmux::Ui;
use tx_core::{Plugin, PluginError, Registry, TxCore, Verb, VerbError, VerbIo};

use crate::rows::Row;

pub const PLUGIN_ID: &str = "details";

const NO_SESSION: &str = "prefix+i: no tx session in this pane";
const NOT_MANAGED: &str = "prefix+i: the session in this pane is not tx-managed";

#[derive(Clone, Copy, Debug, Default)]
pub struct DetailsPlugin;

impl Plugin for DetailsPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.verb(Arc::new(DetailsVerb));
        r.verb(Arc::new(DetailsPopup));
        r.resource("keys/details.tmux", include_bytes!("../tmux/keys.tmux"))?;
        r.resource(
            "bin/tmux-details-tx-session",
            include_bytes!("../bin/tmux-details-tx-session"),
        )
    }
}

pub struct DetailsVerb;

impl Verb for DetailsVerb {
    fn name(&self) -> &'static str {
        "details"
    }

    fn usage(&self) -> &'static str {
        "details TARGET [--json] [--prompt]   the agent's spawn configuration: record, prompt, \
         tools, MCP servers, skills"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let mut target = None;
        let mut json = false;
        let mut with_prompt = false;
        for argument in argv {
            match argument.as_str() {
                "--json" => json = true,
                "--prompt" => with_prompt = true,
                _ if target.is_none() && !argument.starts_with('-') => target = Some(argument),
                _ => {
                    let _ = writeln!(io.stderr, "usage: tx {}", self.usage());
                    return Ok(2);
                }
            }
        }
        let Some(target) = target else {
            let _ = writeln!(io.stderr, "usage: tx {}", self.usage());
            return Ok(2);
        };
        let agent = core.get_agent(&core.resolve(target)?)?;
        let details = model::details(core, &agent, with_prompt)?;
        let output = if json {
            serde_json::to_string_pretty(&details)
                .map_err(|error| VerbError::Failed(error.to_string()))?
        } else {
            plain_text(&rows::rows(&details))
        };
        writeln!(io.stdout, "{output}").map_err(|error| VerbError::Failed(error.to_string()))?;
        Ok(0)
    }
}

pub struct DetailsPopup;

impl Verb for DetailsPopup {
    fn name(&self) -> &'static str {
        "_details-popup"
    }

    fn usage(&self) -> &'static str {
        "_details-popup <pane> <client>   the details of the pane's session in a popup (prefix+i, internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [pane, client] = argv else {
            let _ = writeln!(io.stderr, "usage: tx {}", self.usage());
            return Ok(2);
        };
        let ui = Ui::from_core(core);
        let target = ui
            .inner_for_pane(pane)
            .or_else(|| ui.display_message("#{session_name}", Some(pane)));
        let Some(target) = target.filter(|target| !ui.is_view(target)) else {
            ui.display_status(client, NO_SESSION);
            return Ok(1);
        };
        let Ok(id) = core.resolve(&target) else {
            ui.display_status(client, NOT_MANAGED);
            return Ok(1);
        };
        let agent = core.get_agent(&id)?;
        let details = match model::details(core, &agent, false) {
            Ok(details) => details,
            Err(error) => {
                ui.display_status(client, &format!("prefix+i: {}: {error}", agent.name));
                return Ok(1);
            }
        };
        popup::run(&rows::rows(&details))
            .map_err(|error| VerbError::Failed(format!("popup: {error}")))?;
        Ok(0)
    }
}

pub fn plain_text(rows: &[Row]) -> String {
    let width = rows::label_width(rows);
    rows.iter()
        .map(|row| match row {
            Row::Title(name, id) => format!("{name}  {id}"),
            Row::Heading(text) | Row::Note(text) => text.clone(),
            Row::Field(label, value) => format!("  {label:<width$}  {value}"),
            Row::Item(text) | Row::Text(text) => format!("  {text}"),
            Row::Blank => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
