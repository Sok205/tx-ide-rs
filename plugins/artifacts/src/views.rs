//! An artifact's nvim view, bound to the artifact through `@tx-view-bound`: for a document a
//! tx-client view of kind `editor` running the editor on `current<ext>`; for a diff one of kind
//! `diff` running diffview against the base in the worktree (`kinds::open` decides). No core
//! record. Shared by `artifact open` and prefix+a.

use std::path::Path;

use tx_client::tmux::Ui;
use tx_client::views::{self, View, ViewKind, ViewRequest};
use tx_core::{Agent, AgentId, VerbError};

use crate::artifact::{Artifact, USER_ACTOR};
use crate::kinds::view_suffix;
use crate::service::ArtifactService;

/// Who opens the view: the calling session (its id becomes the view's parent) and the tags the
/// view inherits.
pub struct Opener {
    pub id: Option<AgentId>,
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

/// The view's name: the sanitized title (plus `-diff` for a diff), with the id's first 8 chars
/// appended when a tmux session (an agent's or a view) already holds that name.
pub fn view_name(ui: &Ui, artifact: &Artifact) -> String {
    let base = format!(
        "{}{}",
        sanitize(artifact.display_name()),
        view_suffix(&artifact.kind)
    );
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

impl From<Agent> for Shown {
    fn from(session: Agent) -> Self {
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

/// The session showing the artifact: the tx-client view bound to it. Every open path looks
/// here first.
pub fn live_view(ui: &Ui, artifact_id: &str) -> Result<Option<Shown>, VerbError> {
    Ok(views::list(ui)
        .map_err(failed)?
        .into_iter()
        .find(|view| view.bound.as_deref() == Some(artifact_id))
        .map(Shown::from))
}

fn failed(error: impl std::fmt::Display) -> VerbError {
    VerbError::Failed(error.to_string())
}

/// Starts the view, bound to the artifact, and logs the open. Close steps left by earlier views
/// of the artifact that have ended run first; the one this open leaves (a diff's temporary
/// worktree) is recorded under the view's name. `cwd` overrides where a document's editor runs.
pub fn open(
    ui: &Ui,
    artifacts: &ArtifactService,
    artifact: &Artifact,
    opener: &Opener,
    tags: Option<Vec<String>>,
    cwd: Option<&Path>,
    editor: &str,
) -> Result<View, VerbError> {
    let actor = opener
        .id
        .as_ref()
        .map_or(USER_ACTOR.to_owned(), ToString::to_string);
    artifacts
        .close_stale(&artifact.id, &|name| ui.has_session(name))
        .map_err(failed)?;
    let (artifact, opened) = artifacts.open(&artifact.id, &actor).map_err(failed)?;
    let tags = tags
        .or_else(|| (!opener.tags.is_empty()).then(|| opener.tags.clone()))
        .unwrap_or_else(|| vec!["artifact".to_owned()]);
    let name = view_name(ui, &artifact);
    let kind = if opened.diffview {
        ViewKind::Diff
    } else {
        ViewKind::Editor
    };
    let working_directory = match (&artifact.kind, cwd) {
        (crate::artifact::Kind::Document { .. }, Some(cwd)) => cwd,
        _ => &opened.working_directory,
    };
    let view = views::open(
        ui,
        &ViewRequest {
            name: &name,
            kind,
            program: editor,
            arguments: &opened.arguments,
            working_directory,
            tags: &tags,
            parent_id: opener.id.as_ref(),
            bound: Some(&artifact.id),
        },
    )
    .map_err(failed)?;
    if let Some(cleanup) = opened.cleanup {
        artifacts
            .record_cleanup(&artifact.id, &view.name, cleanup)
            .map_err(failed)?;
    }
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
