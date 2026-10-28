//! An artifact's view: a process session running the editor as the artifact's kind decides
//! (`kinds::open`), bound to the artifact through `ext["artifacts"].bound`. Shared by
//! `artifact open` and prefix+a.

use serde_json::{Value, json};
use tx_core::{CreateOptions, LaunchOptions, Lifecycle, ListFilter, SessionId, TxCore, VerbError};

use crate::PLUGIN_ID;
use crate::artifact::{Artifact, USER_ACTOR};
use crate::kinds::{self, Opened, view_suffix};
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

/// The view's name: the sanitized title (`-diff` / `-tour` suffixed for those kinds), with the
/// id's first 8 chars appended when a running session already holds that name.
pub(crate) fn view_name(core: &TxCore, artifact: &Artifact) -> Result<String, VerbError> {
    let base = sanitize(&format!(
        "{}{}",
        artifact.title,
        view_suffix(&artifact.kind)
    ));
    let taken = core
        .sessions(&ListFilter::new().lifecycle(Lifecycle::Running))?
        .iter()
        .any(|session| session.name == base);
    if taken {
        let short: String = artifact.id.chars().take(8).collect();
        return Ok(format!("{base}-{}", sanitize(&short)));
    }
    Ok(base)
}

/// Decides the launch (logging the open), starts the view, binds it and records any close step
/// the open left behind. Returns the view's id.
pub fn open(
    core: &TxCore,
    artifacts: &ArtifactService,
    artifact: &Artifact,
    opener: &Opener,
    tags: Option<Vec<String>>,
    editor: &str,
) -> Result<SessionId, VerbError> {
    let failed = |error: crate::service::ArtifactError| VerbError::Failed(error.to_string());
    let tags = tags
        .or_else(|| (!opener.tags.is_empty()).then(|| opener.tags.clone()))
        .unwrap_or_else(|| vec!["artifact".to_owned()]);
    let actor = opener
        .id
        .as_ref()
        .map_or(USER_ACTOR.to_owned(), ToString::to_string);
    let name = view_name(core, artifact)?;
    let Opened { cwd, args, cleanup } = artifacts.open(&artifact.id, &actor).map_err(failed)?;
    let launch = LaunchOptions::process(editor, args);
    let mut options = CreateOptions::new(name, cwd, launch).tags(tags);
    if let Some(parent) = &opener.id {
        options = options.parent(parent.clone());
    }
    let id = match core.create_session(options) {
        Ok(id) => id,
        Err(error) => {
            if let Some(cleanup) = &cleanup {
                let _ = kinds::close(cleanup); // best effort: the open failed anyway
            }
            return Err(error.into());
        }
    };
    let bound = Value::String(artifact.id.clone());
    core.update_ext(&id, PLUGIN_ID, |value| *value = json!({ "bound": bound }))?;
    if let Some(cleanup) = cleanup {
        artifacts
            .record_cleanup(&artifact.id, id.as_str(), cleanup)
            .map_err(failed)?;
        // An editor that died at once ended before the step was recorded, so the end sink found
        // nothing to run: run it now (close is idempotent).
        if core.get_session(&id)?.lifecycle.is_terminal() {
            artifacts.close(&artifact.id, id.as_str()).map_err(failed)?;
        }
    }
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
