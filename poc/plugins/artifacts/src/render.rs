//! `artifact ls` and `artifact show` text, ported from main's `render.rs`.

use std::collections::HashMap;

use tx_ui::render::{chips, reltime};

use crate::artifact::{Artifact, USER_ACTOR};

/// A touch author as shown: `user`, the session's name, else the first 8 chars of its id.
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

pub fn updated_at(artifact: &Artifact) -> f64 {
    artifact.updated_at().as_f64().unwrap_or(0.0)
}

fn distinct_authors(artifact: &Artifact) -> Vec<&str> {
    let mut authors: Vec<&str> = Vec::new();
    for touch in artifact.history() {
        if !authors.contains(&touch.session_id.as_str()) {
            authors.push(&touch.session_id);
        }
    }
    authors
}

/// Newest-touched first; `names` is the caller-resolved id→name map.
pub fn render_artifacts(
    artifacts: &[Artifact],
    names: &HashMap<String, String>,
    now: f64,
) -> String {
    if artifacts.is_empty() {
        return "ARTIFACTS\n  (none)".into();
    }
    let mut ordered: Vec<&Artifact> = artifacts.iter().collect();
    ordered.sort_by(|a, b| updated_at(b).total_cmp(&updated_at(a)));
    let mut lines = vec!["ARTIFACTS".to_owned()];
    for artifact in ordered {
        let labels: Vec<String> = distinct_authors(artifact)
            .into_iter()
            .map(|author| actor_label(author, names))
            .collect();
        lines.push(format!(
            "  {:<8}  {:>3}r  {:>5}  {:<28}{}",
            artifact.id.chars().take(8).collect::<String>(),
            artifact.history().len(),
            reltime(Some(updated_at(artifact)), now),
            trunc(artifact.display_name(), 28),
            chips(&labels),
        ));
    }
    lines.join("\n")
}

/// Metadata + the full touch log; `dirty` and `resolved_group` are computed by the caller.
pub fn render_artifact_show(
    artifact: &Artifact,
    dirty: bool,
    names: &HashMap<String, String>,
    now: f64,
    resolved_group: &str,
) -> String {
    let working = if dirty {
        "dirty: un-snapshotted edits (close with `txc artifact modify`)"
    } else {
        "clean"
    };
    let group = match &artifact.group {
        Some(group) => group.clone(),
        None => format!("derived: {resolved_group}"),
    };
    let created = artifact.created_at.as_f64();
    let mut lines = vec![
        format!("{}  ({})", artifact.display_name(), artifact.id),
        format!("  filename:   {}", artifact.filename),
        format!("  created:    {} ago", reltime(created, now)),
        format!(
            "  updated:    {} ago",
            reltime(Some(updated_at(artifact)), now)
        ),
        format!("  group:      {group}"),
        format!("  revisions:  {}", artifact.history().len()),
        format!("  working:    {working}"),
        "  history:".into(),
    ];
    for touch in artifact.history() {
        let note = match touch.changes.as_deref() {
            Some(changes) if !changes.is_empty() => format!("  {changes}"),
            _ => String::new(),
        };
        lines.push(format!(
            "    rev {:<3} {:>5} ago  {:<20}{note}",
            touch.rev,
            reltime(touch.at.as_f64(), now),
            actor_label(&touch.session_id, names),
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const NOW: f64 = 1_000_000.0;

    fn artifact() -> Artifact {
        Artifact::from_value(&json!({
            "artifact_schema_version": 2, "id": "0123456789abcdef", "title": null,
            "filename": "plan.md", "created_at": NOW - 7200.0, "group": null,
            "history": [
                {"session_id": "s1", "at": NOW - 7200.0, "rev": 0, "changes": null},
                {"session_id": "user", "at": NOW - 90.0, "rev": 1, "changes": "tweak"},
                {"session_id": "s1", "at": NOW - 5.0, "rev": 2, "changes": ""},
            ],
        }))
        .unwrap()
    }

    #[test]
    fn ls_rows() {
        let names = HashMap::from([("s1".to_owned(), "worker".to_owned())]);
        assert_eq!(
            render_artifacts(&[artifact()], &names, NOW),
            "ARTIFACTS\n  01234567    3r     5s  plan.md                      [worker] [user]"
        );
        assert_eq!(render_artifacts(&[], &names, NOW), "ARTIFACTS\n  (none)");
    }

    #[test]
    fn show_lists_every_touch() {
        let text = render_artifact_show(&artifact(), true, &HashMap::new(), NOW, "feat");
        assert_eq!(
            text,
            "plan.md  (0123456789abcdef)\n  filename:   plan.md\n  created:    2h ago\n  \
             updated:    5s ago\n  group:      derived: feat\n  revisions:  3\n  working:    \
             dirty: un-snapshotted edits (close with `txc artifact modify`)\n  history:\n    \
             rev 0      2h ago  s1                  \n    rev 1      1m ago  user                  \
             tweak\n    rev 2      5s ago  s1                  "
        );
    }
}
