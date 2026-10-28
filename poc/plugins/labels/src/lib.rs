//! tx-core plugin `labels`: what a session is called, seen from tmux. `whoami` and the internal
//! feeds tmux and fzf call (`_pane-info`, `_tmux-name`, `_relabel`, `_pane-label`), plus the
//! `keys/labels.tmux` fragment: prefix+s session tree, pane-border labels.

use std::sync::Arc;

use tx_core::say;
use tx_core::{
    CoreError, Launch, Lifecycle, ListFilter, Plugin, PluginError, Registry, Session, SessionId,
    TxCore, Verb, VerbError, VerbIo,
};

use tx_core::events::now;
use tx_ui::render::{
    ACCENT_HEX, DIM_FG_HEX, Hooks, WARN_HEX, WORKING_HEX, hooks, reltime, tag_hex,
};
use tx_ui::tmux::Ui;

pub const PLUGIN_ID: &str = "labels";

const NAME_OPTION: &str = "@tx-name";
const REMOTE_OPTION: &str = "@remote-session";
const NOT_NESTED: &str = "#[fg=colour240,nobold]\u{2014}#[default]";
/// The width of " 1h ": the bar fills the slot the resting age and its spaces take.
const WORKING_BAR: &str = "\u{1fb0b}\u{1fb0b}\u{1fb0b}\u{1fb0b}";
const SPINNER: [char; 4] = ['\u{25d0}', '\u{25d3}', '\u{25d1}', '\u{25d2}'];

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

/// `None` when nothing has that name or id.
fn resolve_or_not_found(core: &TxCore, token: &str) -> Result<Option<Session>, VerbError> {
    match core.resolve_session(token) {
        Ok(session) => Ok(Some(session)),
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
        say(io.stdout, &text)?;
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
        let Some(session) = resolve_or_not_found(core, token)? else {
            return Ok(0);
        };
        if let Some(target) = Ui::from_core(core).live_targets().get(&session.name) {
            say(io.stdout, &format!("{target}\n"))?;
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
        for session in core.sessions(&ListFilter::new().lifecycle(Lifecycle::Running))? {
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
            Some(session) => session.name,
            None => raw,
        };
        say(io.stdout, &format!("{name}\n"))?;
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
            nested_label(core, &inner, now())
        } else {
            NOT_NESTED.to_owned()
        };
        say(io.stdout, &label)?;
        Ok(0)
    }
}

/// The record's name in bold plus a chip per tag, between the hooks state's glyph and its
/// right-end mark; the raw tmux name when it has no record.
fn nested_label(core: &TxCore, inner: &str, now: f64) -> String {
    let session = core.resolve_session(inner).ok();
    let Some(session) = session else {
        return format!("#[bold]{inner}#[default]");
    };
    // Only agents have a state worth showing; hooks seeds `idle` on every record.
    let hooks = matches!(session.launch, Launch::Agent { .. })
        .then(|| hooks(&session))
        .flatten();
    let mut label = format!(
        "{}#[bold]{}#[default]",
        state_glyph(hooks.as_ref(), now),
        session.name
    );
    for tag in &session.tags {
        label.push_str(&format!(
            " #[fg={},#{{?pane_active,bold,nobold}}][{tag}]#[default]",
            tag_hex(tag)
        ));
    }
    label.push_str(&state_mark(hooks.as_ref(), now));
    label
}

/// Before the name: WORKING spins one step per redraw (tmux re-runs the label job once a
/// second, so the frame is the epoch second), WAITING a filled dot in the warn colour, IDLE a
/// hollow dot; nothing without a hooks state.
fn state_glyph(hooks: Option<&Hooks>, now: f64) -> String {
    match hooks.map(|hooks| hooks.state.as_str()) {
        Some("working") => {
            let frame = SPINNER[(now as u64 % 4) as usize];
            format!("#[fg={ACCENT_HEX},bold]{frame}#[default] ")
        }
        Some("waiting") => format!("#[fg={WARN_HEX},bold]\u{25cf}#[default] "),
        Some("idle") => format!("#[fg={DIM_FG_HEX},nobold]\u{25cb}#[default] "),
        _ => String::new(),
    }
}

/// At the border's right end: a green bar while WORKING, otherwise how long the session has
/// been resting (`0m` for the first minute, then `14m`, `2h`) between spaces.
fn state_mark(hooks: Option<&Hooks>, now: f64) -> String {
    match hooks.map(|hooks| (hooks.state.as_str(), hooks.since)) {
        Some(("working", _)) => format!("#[align=right]#[fg={WORKING_HEX}]{WORKING_BAR}#[default]"),
        Some(("waiting" | "idle", since)) => {
            let age = if now - since < 60.0 {
                "0m".to_owned()
            } else {
                reltime(Some(since), now)
            };
            format!("#[align=right] #[fg={DIM_FG_HEX}]{age}#[default] ")
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hooks(state: &str) -> Hooks {
        Hooks {
            state: state.to_owned(),
            since: 1000.0,
        }
    }

    #[test]
    fn state_marks_follow_the_hooks_state() {
        let working = hooks("working");
        assert_eq!(
            state_glyph(Some(&working), 1001.0),
            "#[fg=#7aa2f7,bold]\u{25d3}#[default] "
        );
        assert_eq!(
            state_glyph(Some(&working), 1004.0),
            "#[fg=#7aa2f7,bold]\u{25d0}#[default] "
        );
        assert!(state_mark(Some(&working), 1001.0).contains(WORKING_BAR));

        let idle = hooks("idle");
        assert_eq!(
            state_glyph(Some(&idle), 1001.0),
            "#[fg=#a9b1d6,nobold]\u{25cb}#[default] "
        );
        assert_eq!(
            state_mark(Some(&idle), 1030.0),
            "#[align=right] #[fg=#a9b1d6]0m#[default] "
        );
        assert_eq!(
            state_mark(Some(&idle), 8200.0),
            "#[align=right] #[fg=#a9b1d6]2h#[default] "
        );

        let waiting = hooks("waiting");
        assert_eq!(
            state_glyph(Some(&waiting), 1001.0),
            "#[fg=#e0af68,bold]\u{25cf}#[default] "
        );
        assert_eq!(
            state_mark(Some(&waiting), 1030.0),
            "#[align=right] #[fg=#a9b1d6]0m#[default] "
        );

        for hooks in [None, Some(&hooks("stuck"))] {
            assert_eq!(state_glyph(hooks, 1.0), "");
            assert_eq!(state_mark(hooks, 1.0), "");
        }
    }
}
