//! An artifact's nvim view: a tx-client view of kind `editor` running the editor on
//! `current<ext>`, bound to the artifact through `@tx-view-bound`. No core record. Shared by
//! `artifact open` and prefix+a.

use std::path::Path;

use tx_client::tmux::Ui;
use tx_client::views::{self, View, ViewKind, ViewRequest};
use tx_core::{Lifecycle, ListFilter, Session, SessionId, TxCore, VerbError};

use crate::artifact::{Artifact, USER_ACTOR};
use crate::grouping::bound_artifact;
use crate::service::ArtifactService;

/// Who opens the view: the calling session (its id becomes the view's parent) and the tags the
/// view inherits.
pub struct Opener {
    pub id: Option<SessionId>,
    pub tags: Vec<String>,
}

/// A session name from an artifact's title: every byte outside `[A-Za-z0-9_-]` becomes `-`,
/// at most 64 bytes, `artifact` when nothing is left.
pub fn sanitize(title: &str) -> String {
    let name: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .take(64)
        .collect();
    if name.trim_matches('-').is_empty() {
        "artifact".to_owned()
    } else {
        name
    }
}

/// The view's name: the sanitized title, with the id's first 8 chars appended when a tmux
/// session (an agent's or a view) already holds that name.
pub fn view_name(ui: &Ui, artifact: &Artifact) -> String {
    let base = sanitize(artifact.display_name());
    if ui.has_session(&base) {
        let short: String = artifact.id.chars().take(8).collect();
        return format!("{base}-{}", sanitize(&short));
    }
    base
}

/// A tmux session prefix+a or `artifact open` shows: an agent's or a view's. The tags go on the popup's title.
pub struct Shown {
    pub name: String,
    pub tags: Vec<String>,
}

impl From<Session> for Shown {
    fn from(session: Session) -> Self {
        Self {
            name: session.name,
            tags: session.tags,
        }
    }
}

impl From<View> for Shown {
    fn from(view: View) -> Self {
        Self {
            name: view.name,
            tags: view.tags,
        }
    }
}

/// The session showing the artifact: a tx-client view bound to it, else (legacy, PR 11 deletes)
/// a running core record carrying `ext.artifacts.bound`. Every open path looks here first.
pub fn live_view(ui: &Ui, core: &TxCore, artifact_id: &str) -> Result<Option<Shown>, VerbError> {
    let bound = views::list(ui)
        .map_err(failed)?
        .into_iter()
        .find(|view| view.bound.as_deref() == Some(artifact_id));
    if let Some(view) = bound {
        return Ok(Some(view.into()));
    }
    let _ = core.refresh_all();
    let running = core.list_sessions(&ListFilter::new().lifecycle(Lifecycle::Running))?;
    Ok(running
        .iter()
        .filter_map(|id| core.get_session(id).ok())
        .find(|session| bound_artifact(session).as_deref() == Some(artifact_id))
        .map(Shown::from))
}

fn failed(error: impl std::fmt::Display) -> VerbError {
    VerbError::Failed(error.to_string())
}

/// Starts the view, bound to the artifact, and logs the open.
pub fn open(
    ui: &Ui,
    artifacts: &ArtifactService,
    artifact: &Artifact,
    opener: &Opener,
    tags: Option<Vec<String>>,
    cwd: &Path,
    editor: &str,
) -> Result<View, VerbError> {
    let current = artifacts.files().current_path(artifact);
    let tags = tags
        .or_else(|| (!opener.tags.is_empty()).then(|| opener.tags.clone()))
        .unwrap_or_else(|| vec!["artifact".to_owned()]);
    let arguments = [current.to_string_lossy().into_owned()];
    let name = view_name(ui, artifact);
    let view = views::open(
        ui,
        &ViewRequest {
            name: &name,
            kind: ViewKind::Editor,
            program: editor,
            arguments: &arguments,
            working_directory: cwd,
            tags: &tags,
            parent_id: opener.id.as_ref(),
            bound: Some(&artifact.id),
        },
    )
    .map_err(failed)?;
    let actor = opener
        .id
        .as_ref()
        .map_or(USER_ACTOR.to_owned(), ToString::to_string);
    artifacts.opened(&artifact.id, &actor).map_err(failed)?;
    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::sanitize;

    #[test]
    fn titles_become_session_names() {
        assert_eq!(sanitize("plan.md"), "plan-md");
        assert_eq!(sanitize("My plan: v2"), "My-plan--v2");
        assert_eq!(sanitize("..."), "artifact");
        assert_eq!(sanitize(""), "artifact");
        assert_eq!(sanitize(&"x".repeat(100)).len(), 64);
        assert!(tx_core::validate_name(&sanitize("é/ü")).is_ok());
    }
}
