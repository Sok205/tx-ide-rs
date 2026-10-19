//! An artifact's nvim view: a process session running the editor on `current<ext>`, bound to the
//! artifact through `ext["artifacts"].bound`. Shared by `artifact open` and prefix+a.

use std::path::Path;

use serde_json::{Value, json};
use tx_core::{CreateOptions, LaunchOptions, Lifecycle, ListFilter, SessionId, TxCore, VerbError};

use crate::PLUGIN_ID;
use crate::artifact::{Artifact, USER_ACTOR};
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

/// The view's name: the sanitized title, with the id's first 8 chars appended when a running
/// session already holds that name.
pub fn view_name(core: &TxCore, artifact: &Artifact) -> Result<String, VerbError> {
    let base = sanitize(artifact.display_name());
    let running = core.list_sessions(&ListFilter::new().lifecycle(Lifecycle::Running))?;
    let taken = running
        .iter()
        .filter_map(|id| core.get_session(id).ok())
        .any(|session| session.name == base);
    if taken {
        let short: String = artifact.id.chars().take(8).collect();
        return Ok(format!("{base}-{}", sanitize(&short)));
    }
    Ok(base)
}

/// Starts the view, binds it, logs the open. Returns the view's id.
pub fn open(
    core: &TxCore,
    artifacts: &ArtifactService,
    artifact: &Artifact,
    opener: &Opener,
    tags: Option<Vec<String>>,
    cwd: &Path,
    editor: &str,
) -> Result<SessionId, VerbError> {
    let current = artifacts.files().current_path(artifact);
    let tags = tags
        .or_else(|| (!opener.tags.is_empty()).then(|| opener.tags.clone()))
        .unwrap_or_else(|| vec!["artifact".to_owned()]);
    let launch = LaunchOptions::process(editor, [current.to_string_lossy().into_owned()]);
    let mut options = CreateOptions::new(view_name(core, artifact)?, cwd, launch).tags(tags);
    if let Some(parent) = &opener.id {
        options = options.parent(parent.clone());
    }
    let id = core.create_session(options)?;
    let bound = Value::String(artifact.id.clone());
    core.update_ext(&id, PLUGIN_ID, |value| *value = json!({ "bound": bound }))?;
    let actor = opener
        .id
        .as_ref()
        .map_or(USER_ACTOR.to_owned(), ToString::to_string);
    artifacts
        .opened(&artifact.id, &actor)
        .map_err(|error| VerbError::Failed(error.to_string()))?;
    Ok(id)
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
