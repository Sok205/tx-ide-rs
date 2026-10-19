//! tx-core plugin `edit`: `_edit-session` (prefix+e, the modal form) and `_edit-tag` (the
//! picker's Ctrl-T popup, an fzf query prompt). `ext["edit"]` records the group a session
//! belongs to. Publishes `keys/edit.tmux` and its helper script.

use std::process::{Command, Stdio};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tx_core::{
    CoreError, Plugin, PluginError, Registry, Session, SessionId, TxCore, Verb, VerbError, VerbIo,
};

use tx_ui::editor::edit_session;
use tx_ui::render::{ACCENT_ANSI, BOLD, RESET};
use tx_ui::tmux::Ui;

pub const PLUGIN_ID: &str = "edit";

#[derive(Clone, Copy, Debug, Default)]
pub struct EditPlugin;

impl Plugin for EditPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.verb(Arc::new(EditSession));
        r.verb(Arc::new(EditTag));
        r.resource("keys/edit.tmux", include_bytes!("../tmux/keys.tmux"))?;
        r.resource(
            "bin/tmux-edit-tx-session",
            include_bytes!("../bin/tmux-edit-tx-session"),
        )
    }
}

/// The `ext["edit"]` shape on a session record.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ext {
    pub group: Option<String>,
}

const NO_SESSION: &str = "prefix+e: no tx session in this pane (view homes cannot be edited)";
const NOT_MANAGED: &str = "prefix+e: the session in this pane is not tx-managed";

/// Comma-separated tags; empty items are dropped.
pub fn split_tags(s: &str) -> Vec<String> {
    s.split(',')
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Writes what the form changed, each part only when it differs from `before`. Returns the
/// first error; earlier parts stay written.
pub fn apply_edit(
    core: &TxCore,
    id: &SessionId,
    before: &Session,
    edited: (String, String, String),
) -> Result<(), CoreError> {
    let (name, tags, group) = edited;
    if name != before.name {
        core.rename_session(id, &name)?;
    }
    let tags = split_tags(&tags);
    if tags != before.tags {
        core.set_session_tags(id, tags)?;
    }
    let group = Some(group.trim().to_owned()).filter(|group| !group.is_empty());
    if group != current_group(before) {
        let group = group.map_or(serde_json::Value::Null, serde_json::Value::String);
        core.update_ext(id, PLUGIN_ID, |value| match value.as_object_mut() {
            Some(object) => {
                object.insert("group".to_owned(), group);
            }
            None => *value = serde_json::json!({ "group": group }),
        })?;
    }
    Ok(())
}

fn current_group(session: &Session) -> Option<String> {
    session
        .ext::<Ext>(PLUGIN_ID)
        .ok()
        .flatten()
        .and_then(|ext| ext.group)
}

pub struct EditSession;

impl Verb for EditSession {
    fn name(&self) -> &'static str {
        "_edit-session"
    }

    fn usage(&self) -> &'static str {
        "_edit-session <pane> <client>   modal session editor (prefix+e, internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [pane, client] = argv else {
            let _ = writeln!(io.stderr, "usage: txc _edit-session <pane> <client>");
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
        let before = core.get_session(&id)?;
        let group = current_group(&before).unwrap_or_default();
        let edited = edit_session(&before.name, &before.tags.join(","), &group, "")
            .map_err(|e| VerbError::Failed(format!("editor: {e}")))?;
        let Some(edited) = edited else {
            return Ok(0);
        };
        if let Err(error) = apply_edit(core, &id, &before, edited) {
            ui.display_status(client, &format!("edit of {} failed: {error}", before.name));
            return Ok(1);
        }
        ui.refresh_labels();
        Ok(0)
    }
}

pub struct EditTag;

impl Verb for EditTag {
    fn name(&self) -> &'static str {
        "_edit-tag"
    }

    fn usage(&self) -> &'static str {
        "_edit-tag <name>   prefilled tag editor for the picker's Ctrl-T popup (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [token] = argv else {
            let _ = writeln!(io.stderr, "usage: txc _edit-tag <name>");
            return Ok(2);
        };
        let Ok(id) = core.resolve(token) else {
            let _ = writeln!(io.stderr, "txc: session '{token}' not found");
            return Ok(1);
        };
        let session = core.get_session(&id)?;
        let header = format!("Tags for {ACCENT_ANSI}{BOLD}{}{RESET}", session.name);
        let Some(answer) = prompt_with_default(&header, &session.tags.join(","))? else {
            return Ok(1);
        };
        core.set_session_tags(&id, split_tags(&answer))?;
        Ok(0)
    }
}

/// fzf's query editor prefilled with `default`; `None` when cancelled.
fn prompt_with_default(header: &str, default: &str) -> Result<Option<String>, VerbError> {
    let output = Command::new("fzf")
        .args([
            "--disabled",
            "--query",
            default,
            "--ansi",
            "--prompt",
            "  ❯ ",
            "--header",
            header,
            "--header-first",
            "--reverse",
            "--no-info",
            "--no-separator",
            "--bind",
            "enter:accept-or-print-query",
            "--color",
            "fg:#a9b1d6,header:#a9b1d6,prompt:#a9b1d6,query:#c0caf5",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| VerbError::Failed(format!("could not run fzf: {e}")))?;
    if !output.status.success() || output.stdout.is_empty() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(Some(text.strip_suffix('\n').unwrap_or(&text).to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_tags_drops_empties() {
        assert_eq!(split_tags("a,,b,"), ["a", "b"]);
        assert!(split_tags("").is_empty());
    }
}
