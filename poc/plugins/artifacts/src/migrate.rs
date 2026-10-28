//! `artifact migrate`: v1 / v2 / v3 records to v4, ported from main's `migrate_artifacts`.
//!
//! The owner is the first touch's session, `modified_at` the last touch's time, the kind a
//! document on the record's `filename`. The working copy `current<ext>` is renamed to that file
//! and the `revs/` snapshots are removed. A record that cannot be upgraded is left untouched
//! and reported.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Number, Value};

use crate::artifact::{ARTIFACT_SCHEMA_VERSION, Artifact, Kind};
use crate::store::{ArtifactStore, is_valid_id};

const UPGRADABLE_FROM: [u64; 3] = [1, 2, 3];

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub migrated: Vec<String>,
    /// `(file name, reason)`.
    pub skipped: Vec<(String, String)>,
}

/// Upgrade every `*.json` record in the store, in name order.
pub fn migrate(store: &ArtifactStore) -> Report {
    let mut report = Report::default();
    let entries = match fs::read_dir(store.directory()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return report,
        Err(error) => {
            let dir = store.directory().display().to_string();
            report.skipped.push((dir, error.to_string()));
            return report;
        }
    };
    let mut files: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            (name.ends_with(".json") && !name.starts_with('.')).then(|| (name, entry.path()))
        })
        .collect();
    files.sort();
    for (name, path) in files {
        match migrate_one(&name, &path, store) {
            Ok(()) => report.migrated.push(name),
            Err(reason) => report.skipped.push((name, reason)),
        }
    }
    report
}

fn migrate_one(name: &str, path: &Path, store: &ArtifactStore) -> Result<(), String> {
    let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let raw: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
    let data = raw.as_object().ok_or("not a JSON object")?;
    // Compared by value, so `2.0` counts as 2.
    let version = data.get("artifact_schema_version").and_then(Value::as_f64);
    let is = |expected: u64| version == Some(expected as f64);
    if is(ARTIFACT_SCHEMA_VERSION) {
        return Err(format!("already v{ARTIFACT_SCHEMA_VERSION}"));
    }
    if !UPGRADABLE_FROM.into_iter().any(is) {
        return Err(format!(
            "not an upgradable artifact record (artifact_schema_version={})",
            data.get("artifact_schema_version").unwrap_or(&Value::Null)
        ));
    }
    let id = string(data, "id")?;
    let filename = string(data, "filename")?;
    // The record names paths inside the store: refuse anything that would reach outside it or
    // overwrite another record.
    if !is_valid_id(&id) || name != format!("{id}.json") {
        return Err(format!("id {id:?} does not match the file name"));
    }
    if filename.is_empty() || base_name(&filename) != filename || filename == ".." {
        return Err(format!("filename {filename:?} is not a bare file name"));
    }
    let title = match data.get("title") {
        Some(Value::String(title)) if !title.is_empty() => title.clone(),
        _ => filename.clone(),
    };
    let history = data
        .get("history")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let (Some(first), Some(last)) = (history.first(), history.last()) else {
        return Err("missing or empty history".into());
    };
    let owner = first
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or("history[0] has no session_id")?
        .to_owned();
    let modified_at = last
        .get("at")
        .and_then(Value::as_number)
        .ok_or("the last touch has no at")?
        .clone();
    let archived_at = match data.get("archived_at") {
        None | Some(Value::Null) => None,
        Some(Value::Number(at)) => Some(at.clone()),
        Some(other) => return Err(format!("archived_at is not a number: {other}")),
    };
    let artifact = Artifact {
        id: id.clone(),
        kind: Kind::Document {
            file: filename.clone(),
        },
        title,
        owner,
        created_at: number(data, "created_at")?,
        modified_at,
        archived_at,
    };
    // The record first: a failed save leaves the v2 layout untouched. A failure after it leaves
    // a v4 record whose file `doctor` reports missing.
    store
        .save(&artifact)
        .map_err(|error| format!("{}: {error}", store.record_path(&id).display()))?;
    let directory = store.directory().join(&id);
    let current = directory.join(format!("current{}", suffix(base_name(&filename))));
    let target = directory.join(&filename);
    if current.exists() && current != target {
        fs::rename(&current, &target).map_err(|error| format!("{}: {error}", current.display()))?;
    }
    let revs = directory.join("revs");
    if revs.is_dir() {
        fs::remove_dir_all(&revs).map_err(|error| format!("{}: {error}", revs.display()))?;
    }
    Ok(())
}

fn string(data: &Map<String, Value>, field: &str) -> Result<String, String> {
    data.get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("missing or non-string {field}"))
}

fn number(data: &Map<String, Value>, field: &str) -> Result<Number, String> {
    data.get(field)
        .and_then(Value::as_number)
        .cloned()
        .ok_or_else(|| format!("missing or non-number {field}"))
}

/// The last path component, ignoring empty and `.` components.
fn base_name(path: &str) -> &str {
    path.rsplit('/')
        .find(|part| !part.is_empty() && *part != ".")
        .unwrap_or("")
}

/// A bare file name's suffix: from the last dot, unless the part before it is empty or all dots
/// (`.bashrc`, `...` have none; `a.` has `.`). The v2 store named the working copy
/// `current<suffix>`.
fn suffix(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if !name[..i].trim_start_matches('.').is_empty() => &name[i..],
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_v2_record_becomes_a_v4_document() {
        let root = tempfile::tempdir().unwrap();
        let store = ArtifactStore::at(root.path());
        let v2 = json!({
            "artifact_schema_version": 2, "id": "a", "title": null, "filename": "plan.md",
            "created_at": 1.5, "group": "g",
            "history": [
                {"session_id": "s1", "at": 1.5, "rev": 0, "changes": null},
                {"session_id": "user", "at": 9, "rev": 1, "changes": "x"},
            ],
        });
        fs::write(root.path().join("a.json"), v2.to_string()).unwrap();
        fs::create_dir_all(root.path().join("a/revs")).unwrap();
        fs::write(root.path().join("a/current.md"), "body").unwrap();
        fs::write(root.path().join("a/revs/0.md"), "old").unwrap();
        fs::write(root.path().join("b.json"), "{}").unwrap();
        let escape = json!({
            "artifact_schema_version": 2, "id": "../x", "title": null, "filename": "p.md",
            "created_at": 1, "group": null,
            "history": [{"session_id": "s1", "at": 1, "rev": 0, "changes": null}],
        });
        fs::write(root.path().join("c.json"), escape.to_string()).unwrap();

        let report = migrate(&store);
        assert_eq!(report.migrated, ["a.json"]);
        assert_eq!(report.skipped.len(), 2);
        assert!(
            report.skipped[0].1.contains("not an upgradable"),
            "{report:?}"
        );
        assert!(report.skipped[1].1.contains("does not match"), "{report:?}");

        let artifact = store.load("a").unwrap().unwrap();
        assert_eq!(artifact.owner, "s1");
        assert_eq!(artifact.title, "plan.md");
        assert_eq!(artifact.modified_at, Number::from(9));
        assert_eq!(
            fs::read_to_string(root.path().join("a/plan.md")).unwrap(),
            "body"
        );
        assert!(!root.path().join("a/revs").exists());

        assert_eq!(migrate(&store).skipped[0].1, "already v4");
    }

    #[test]
    fn suffixes_follow_the_v2_naming() {
        for (name, expected) in [
            ("plan.md", ".md"),
            ("artifact", ""),
            ("a.", "."),
            (".bashrc", ""),
            ("a.tar.gz", ".gz"),
        ] {
            assert_eq!(suffix(base_name(name)), expected, "{name}");
        }
    }
}
