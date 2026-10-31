//! tx-core plugin `labels`: what a session is called, seen from tmux. `whoami` and the internal
//! feeds tmux and fzf call (`_pane-info`, `_tmux-name`, `_relabel`, `_pane-label`), plus the
//! `keys/labels.tmux` fragment: prefix+s session tree, pane-border labels.

use std::sync::Arc;

use tx_core::{
    CoreError, ListFilter, Plugin, PluginError, Registry, SessionId, TxCore, Verb, VerbError,
    VerbIo,
};

use tx_client::render::tag_hex;
use tx_client::tmux::Ui;

pub const PLUGIN_ID: &str = "labels";

const NAME_OPTION: &str = "@tx-name";
const REMOTE_OPTION: &str = "@remote-session";
const NOT_NESTED: &str = "#[fg=colour240,nobold]\u{2014}#[default]";

#[derive(Clone, Copy, Debug, Default)]
pub struct LabelsPlugin;

impl Plugin for LabelsPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.verb(Arc::new(PaneInfo));
        r.verb(Arc::new(TmuxName));
        r.verb(Arc::new(Relabel));
        r.verb(Arc::new(Whoami));
        r.verb(Arc::new(PaneLabel));
        r.resource("keys/labels.tmux", include_bytes!("../tmux/keys.tmux"))
    }
}

fn write_out(io: &mut VerbIo<'_>, text: &str) -> Result<(), VerbError> {
    io.stdout
        .write_all(text.as_bytes())
        .map_err(|e| VerbError::Failed(format!("write: {e}")))
}

/// `None` when nothing has that name or id.
fn resolve_or_not_found(core: &TxCore, token: &str) -> Result<Option<SessionId>, VerbError> {
    match core.resolve(token) {
        Ok(id) => Ok(Some(id)),
        Err(CoreError::NotFound(_)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub struct PaneInfo;

impl Verb for PaneInfo {
    fn name(&self) -> &'static str {
        "_pane-info"
    }

    fn usage(&self) -> &'static str {
        "_pane-info <session_id>   name and tags, two lines (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let text = argv
            .first()
            .and_then(|raw| raw.parse::<SessionId>().ok())
            .and_then(|id| core.get_session(&id).ok())
            .map_or_else(
                || "\n\n".to_owned(),
                |session| format!("{}\n{}\n", session.name, session.tags.join(",")),
            );
        write_out(io, &text)?;
        Ok(0)
    }
}

pub struct TmuxName;

impl Verb for TmuxName {
    fn name(&self) -> &'static str {
        "_tmux-name"
    }

    fn usage(&self) -> &'static str {
        "_tmux-name <name>   the live tmux target, if any (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let Some(token) = argv.first() else {
            let _ = writeln!(io.stderr, "usage: txc _tmux-name <name>");
            return Ok(2);
        };
        let Some(id) = resolve_or_not_found(core, token)? else {
            return Ok(0);
        };
        let session = core.get_session(&id)?;
        if let Some(target) = Ui::from_core(core).live_targets().get(&session.name) {
            write_out(io, &format!("{target}\n"))?;
        }
        Ok(0)
    }
}

pub struct Relabel;

impl Verb for Relabel {
    fn name(&self) -> &'static str {
        "_relabel"
    }

    fn usage(&self) -> &'static str {
        "_relabel   refresh every session's @tx-name (internal)"
    }

    fn run(&self, core: &TxCore, _io: &mut VerbIo<'_>, _argv: &[String]) -> Result<i32, VerbError> {
        let ui = Ui::from_core(core);
        let live = ui.live_targets();
        let ids = core.list_sessions(&ListFilter::new().running())?;
        for id in ids {
            let session = core.get_session(&id)?;
            if !live.contains_key(&session.name) {
                continue;
            }
            let label = if session.tags.is_empty() {
                session.name.clone()
            } else {
                format!("{} [{}]", session.name, session.tags.join(","))
            };
            let _ = ui.set_session_option(&session.name, NAME_OPTION, &label);
        }
        Ok(0)
    }
}

pub struct Whoami;

impl Verb for Whoami {
    fn name(&self) -> &'static str {
        "whoami"
    }

    fn usage(&self) -> &'static str {
        "whoami   the session this terminal runs in"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, _argv: &[String]) -> Result<i32, VerbError> {
        let ui = Ui::from_core(core);
        if !ui.inside() {
            let _ = writeln!(io.stderr, "txc whoami: not inside the tx tmux server");
            return Ok(1);
        }
        let raw = ui
            .current_session_name()
            .ok_or_else(|| VerbError::Failed("could not read the tmux session name".into()))?;
        let name = match resolve_or_not_found(core, &raw)? {
            Some(id) => core.get_session(&id)?.name,
            None => raw,
        };
        write_out(io, &format!("{name}\n"))?;
        Ok(0)
    }
}

pub struct PaneLabel;

impl Verb for PaneLabel {
    fn name(&self) -> &'static str {
        "_pane-label"
    }

    fn usage(&self) -> &'static str {
        "_pane-label <pane_id> [rev]   pane-border label (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let Some(pane) = argv.first().filter(|pane| !pane.is_empty()) else {
            return Ok(0);
        };
        let ui = Ui::from_core(core);
        let label = if let Some(remote) = ui.pane_option(pane, REMOTE_OPTION) {
            format!("(r) {remote}")
        } else if let Some(inner) = ui.inner_for_pane(pane) {
            nested_label(core, &inner)
        } else {
            NOT_NESTED.to_owned()
        };
        write_out(io, &label)?;
        Ok(0)
    }
}

/// The record's name in bold plus a chip per tag; the raw tmux name when it has no record.
fn nested_label(core: &TxCore, inner: &str) -> String {
    let session = core
        .resolve(inner)
        .and_then(|id| core.get_session(&id))
        .ok();
    let Some(session) = session else {
        return format!("#[bold]{inner}#[default]");
    };
    let mut label = format!("#[bold]{}#[default]", session.name);
    for tag in &session.tags {
        label.push_str(&format!(
            " #[fg={},#{{?pane_active,bold,nobold}}][{tag}]#[default]",
            tag_hex(tag)
        ));
    }
    label
}
