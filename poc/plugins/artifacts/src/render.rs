//! `artifact ls` and `artifact show` text, ported from main's v4 `render.rs`.

use std::collections::HashMap;
use std::path::Path;

use tx_ui::render::{chips, reltime};

use crate::artifact::{Artifact, Kind, USER_ACTOR};

/// An owner as shown: `user`, the session's name, else the first 8 chars of its id.
pub fn actor_label(session_id: &str, names: &HashMap<String, String>) -> String {
    if session_id == USER_ACTOR {
        return USER_ACTOR.into();
    }
    match names.get(session_id) {
        Some(name) if !name.is_empty() => name.clone(),
        _ => session_id.chars().take(8).collect(),
    }
}

fn trunc(text: &str, width: usize) -> String {
    if text.chars().count() > width {
        let mut out: String = text.chars().take(width - 1).collect();
        out.push('…');
        out
    } else {
        text.to_owned()
    }
}

/// Newest-modified first; `names` is the caller-resolved id→name map.
pub fn render_artifacts(
    artifacts: &[Artifact],
    names: &HashMap<String, String>,
    now: f64,
) -> String {
    if artifacts.is_empty() {
        return "ARTIFACTS\n  (none)".into();
    }
    let mut ordered: Vec<&Artifact> = artifacts.iter().collect();
    ordered.sort_by(|a, b| b.modified_secs().total_cmp(&a.modified_secs()));
    let mut lines = vec!["ARTIFACTS".to_owned()];
    for artifact in ordered {
        lines.push(format!(
            "  {:<8}  {:<8}  {:>5}  {:<28}{}",
            artifact.id.chars().take(8).collect::<String>(),
            artifact.kind.name(),
            reltime(Some(artifact.modified_secs()), now),
            trunc(&artifact.title, 28),
            chips(&[actor_label(&artifact.owner, names)]),
        ));
    }
    lines.join("\n")
}

/// The kind's subfield on one line; `worktree_present` is the caller's check of the checkout a
/// diff or tour points into (`None` for a document).
pub(crate) fn describe_kind(kind: &Kind, worktree_present: Option<bool>) -> String {
    let worktree = |path: &Path| {
        let missing = if worktree_present == Some(false) {
            " (missing)"
        } else {
            ""
        };
        format!("worktree={}{missing}", path.display())
    };
    match kind {
        Kind::Document { file } => format!("file={file}"),
        Kind::Diff {
            worktree: path,
            base,
        } => format!("{}  base={base}", worktree(path)),
        Kind::Tour {
            worktree: path,
            stops,
        } => format!("{}  stops={stops}", worktree(path)),
    }
}

/// The record's metadata and its kind's subfield; `resolved_group` is computed by the caller.
pub fn render_artifact_show(
    artifact: &Artifact,
    names: &HashMap<String, String>,
    now: f64,
    resolved_group: &str,
) -> String {
    let worktree_present = artifact.kind.worktree().map(Path::is_dir);
    let mut lines = vec![
        format!("{}  ({})", artifact.title, artifact.id),
        format!("  kind:       {}", artifact.kind.name()),
        format!("  owner:      {}", actor_label(&artifact.owner, names)),
        format!(
            "  created:    {} ago",
            reltime(artifact.created_at.as_f64(), now)
        ),
        format!(
            "  modified:   {} ago",
            reltime(Some(artifact.modified_secs()), now)
        ),
        format!("  group:      derived: {resolved_group}"),
        format!(
            "  {:<11} {}",
            format!("{}:", artifact.kind.name()),
            describe_kind(&artifact.kind, worktree_present)
        ),
    ];
    if let Some(archived_at) = &artifact.archived_at {
        lines.push(format!(
            "  archived:   {} ago (hidden from pickers; `txc artifact unarchive` restores)",
            reltime(archived_at.as_f64(), now)
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use serde_json::Number;

    use super::*;

    const NOW: f64 = 1_000_000.0;

    fn artifact() -> Artifact {
        let mut artifact = Artifact::new(
            "0123456789abcdef".into(),
            Kind::Document {
                file: "plan.md".into(),
            },
            "Plan".into(),
            "s1".into(),
            Number::from_f64(NOW - 7200.0).unwrap(),
        );
        artifact.modified_at = Number::from_f64(NOW - 5.0).unwrap();
        artifact
    }

    #[test]
    fn ls_rows() {
        let names = HashMap::from([("s1".to_owned(), "worker".to_owned())]);
        assert_eq!(
            render_artifacts(&[artifact()], &names, NOW),
            "ARTIFACTS\n  01234567  document     5s  Plan                         [worker]"
        );
        assert_eq!(render_artifacts(&[], &names, NOW), "ARTIFACTS\n  (none)");
    }

    #[test]
    fn show_has_the_kind_line_and_the_archive_note() {
        let mut archived = artifact();
        archived.archived_at = Some(Number::from_f64(NOW - 60.0).unwrap());
        assert_eq!(
            render_artifact_show(&archived, &HashMap::new(), NOW, "feat"),
            "Plan  (0123456789abcdef)\n  kind:       document\n  owner:      s1\n  created:    \
             2h ago\n  modified:   5s ago\n  group:      derived: feat\n  document:   \
             file=plan.md\n  archived:   1m ago (hidden from pickers; `txc artifact unarchive` \
             restores)"
        );
        let diff = Kind::Diff {
            worktree: "/nonexistent/w".into(),
            base: "abc".into(),
        };
        assert_eq!(
            describe_kind(&diff, Some(false)),
            "worktree=/nonexistent/w (missing)  base=abc"
        );
    }
}
