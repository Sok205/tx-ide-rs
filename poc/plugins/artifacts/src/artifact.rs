//! The artifact record (`Artifact`) and its history entries (`Touch`), ported from main's
//! `artifact.rs`.
//!
//! The record is read through a strict boundary: `artifact_schema_version` must be 2, the key sets
//! must be exact, the history non-empty with revs contiguous from 0. `updated_at` is derived from
//! the last touch, never stored. Timestamps stay `serde_json::Number`, so an integer written by
//! hand stays an integer.
//!
//! `kind` (what the artifact is, with the facts its kind needs) and `archived` are the two keys
//! records written before artifact kinds lack: such a record reads as an unarchived document.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

pub const ARTIFACT_SCHEMA_VERSION: u64 = 2;

/// Actor for a touch made outside any tx session (a manual edit, or a plain-terminal call).
pub const USER_ACTOR: &str = "user";

const ARTIFACT_KEYS: [&str; 7] = [
    "artifact_schema_version",
    "id",
    "title",
    "filename",
    "created_at",
    "group",
    "history",
];
/// Written always, read when present.
const OPTIONAL_ARTIFACT_KEYS: [&str; 2] = ["kind", "archived"];
const TOUCH_KEYS: [&str; 4] = ["session_id", "at", "rev", "changes"];

/// What an artifact is, with the structured facts its kind needs. The plugin derives the open
/// behaviour of a document and a diff from this in code; any other kind is opened by convention
/// through the verb named after it (`txc <kind> open <id>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Kind {
    /// A hand-edited file: the working copy `current<ext>`. The default when a record has no kind.
    Document { file: String },
    /// `git diff <base>` of a repository checkout, with `base` a commit id pinned at create.
    /// `repository` is the checkout's git common directory, recorded at create so the diff can
    /// be shown from a temporary worktree once `worktree` is gone (`None` on records written
    /// before it was recorded).
    Diff {
        worktree: PathBuf,
        base: String,
        #[serde(default)]
        repository: Option<PathBuf>,
    },
    /// A kind another plugin owns; `facts` are its subfields, kept as written.
    #[serde(untagged)]
    Other {
        kind: String,
        #[serde(flatten)]
        facts: Map<String, Value>,
    },
}

impl Kind {
    /// The kind's name: `document`, `diff`, or the other plugin's word.
    pub fn name(&self) -> &str {
        match self {
            Self::Document { .. } => "document",
            Self::Diff { .. } => "diff",
            Self::Other { kind, .. } => kind,
        }
    }

    pub fn is_diff(&self) -> bool {
        matches!(self, Self::Diff { .. })
    }

    /// The checkout a diff points into (`None` for the other kinds).
    pub fn worktree(&self) -> Option<&Path> {
        match self {
            Self::Diff { worktree, .. } => Some(worktree),
            Self::Document { .. } | Self::Other { .. } => None,
        }
    }

    /// A short label for listings: `document`, `diff vs <base7>`, or the kind's name.
    pub fn describe(&self) -> String {
        match self {
            Self::Diff { base, .. } => format!("diff vs {}", &base[..base.len().min(7)]),
            other => other.name().to_owned(),
        }
    }
}

/// A persisted record is not a current-version artifact record.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct UnsupportedArtifactError(String);

/// One `history` entry: who touched the artifact, when, and which `revs/<rev>` it produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Touch {
    pub session_id: String,
    pub at: Number,
    pub rev: u64,
    pub changes: Option<String>,
}

impl Touch {
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("session_id".into(), Value::String(self.session_id.clone()));
        map.insert("at".into(), Value::Number(self.at.clone()));
        map.insert("rev".into(), Value::from(self.rev));
        map.insert("changes".into(), opt_str(self.changes.as_deref()));
        Value::Object(map)
    }
}

/// A durable, versioned deliverable (the record half; files live under `<data_dir>/<id>/`).
///
/// `history` is private so it stays non-empty with revs `0..len`; extend it with `record_touch`.
#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    pub id: String,
    pub kind: Kind,
    pub title: Option<String>,
    pub filename: String,
    pub created_at: Number,
    pub group: Option<String>,
    /// Hidden from prefix+a and `ls` (content kept); `ls --archived` shows it.
    pub archived: bool,
    history: Vec<Touch>,
}

impl Artifact {
    /// A fresh artifact whose history is the single create touch (rev 0) at `created_at`.
    pub fn new(
        id: String,
        kind: Kind,
        title: Option<String>,
        filename: String,
        created_at: Number,
        group: Option<String>,
        session_id: String,
    ) -> Self {
        let create = Touch {
            session_id,
            at: created_at.clone(),
            rev: 0,
            changes: None,
        };
        Self {
            id,
            kind,
            title,
            filename,
            created_at,
            group,
            archived: false,
            history: vec![create],
        }
    }

    pub fn history(&self) -> &[Touch] {
        &self.history
    }

    fn last_touch(&self) -> &Touch {
        // Invariant: `new` seeds one touch, `from_value` rejects an empty history, and history is
        // only ever appended to.
        self.history.last().expect("artifact history is non-empty")
    }

    /// Last-touched time, derived from the final history entry (never stored).
    pub fn updated_at(&self) -> &Number {
        &self.last_touch().at
    }

    /// The rev the working copy last snapshotted from.
    pub fn latest_rev(&self) -> u64 {
        self.last_touch().rev
    }

    /// The rev the next touch will produce.
    pub fn next_rev(&self) -> u64 {
        self.latest_rev() + 1
    }

    /// Append a touch for `next_rev()` and return that rev.
    pub fn record_touch(&mut self, session_id: String, at: Number, changes: Option<String>) -> u64 {
        let rev = self.next_rev();
        self.history.push(Touch {
            session_id,
            at,
            rev,
            changes,
        });
        rev
    }

    /// The filename's suffix (`plan.md` → `.md`), reused for every rev and `current`.
    pub fn extension(&self) -> &str {
        suffix(base_name(&self.filename))
    }

    /// The title, or the filename when there is none.
    pub fn display_name(&self) -> &str {
        self.title
            .as_deref()
            .filter(|title| !title.is_empty())
            .unwrap_or(&self.filename)
    }

    /// The on-disk shape, in canonical key order; `updated_at` is absent (derived).
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert(
            "artifact_schema_version".into(),
            Value::from(ARTIFACT_SCHEMA_VERSION),
        );
        map.insert("id".into(), Value::String(self.id.clone()));
        map.insert("title".into(), opt_str(self.title.as_deref()));
        map.insert("filename".into(), Value::String(self.filename.clone()));
        map.insert("created_at".into(), Value::Number(self.created_at.clone()));
        map.insert("group".into(), opt_str(self.group.as_deref()));
        map.insert(
            "history".into(),
            Value::Array(self.history.iter().map(Touch::to_value).collect()),
        );
        map.insert(
            "kind".into(),
            serde_json::to_value(&self.kind).expect("a kind serializes"),
        );
        map.insert("archived".into(), Value::Bool(self.archived));
        Value::Object(map)
    }

    /// Deserialize at the strict boundary, in main's check order: version, record key set, each
    /// entry's key set, non-empty history, contiguous revs, then field types.
    pub fn from_value(data: &Value) -> Result<Self, UnsupportedArtifactError> {
        let Value::Object(map) = data else {
            return Err(unsupported(format!("record is not a JSON object: {data}")));
        };
        let version = map.get("artifact_schema_version");
        if version.and_then(Value::as_u64) != Some(ARTIFACT_SCHEMA_VERSION) {
            return Err(unsupported(format!(
                "record artifact_schema_version={} is unsupported (expected \
                 {ARTIFACT_SCHEMA_VERSION}); older artifact records are not migrated",
                repr_opt(version)
            )));
        }
        let id_repr = repr_opt(map.get("id"));
        require_exact_keys(
            map,
            &ARTIFACT_KEYS,
            &OPTIONAL_ARTIFACT_KEYS,
            &format!("artifact {id_repr}"),
        )?;

        let Value::Array(entries) = &map["history"] else {
            return Err(invalid(&id_repr, "history", &map["history"]));
        };
        let mut touch_maps = Vec::with_capacity(entries.len());
        for entry in entries {
            let Value::Object(touch) = entry else {
                return Err(unsupported(format!(
                    "a history entry is not an object: {entry}"
                )));
            };
            require_exact_keys(touch, &TOUCH_KEYS, &[], "a history entry")?;
            touch_maps.push(touch);
        }
        validate_history(&touch_maps, &id_repr)?;

        let history = touch_maps
            .iter()
            .zip(0u64..)
            .map(|(touch, rev)| {
                Ok(Touch {
                    session_id: req_str(touch, "session_id", &id_repr)?,
                    at: req_num(touch, "at", &id_repr)?,
                    rev,
                    changes: opt_str_field(touch, "changes", &id_repr)?,
                })
            })
            .collect::<Result<_, UnsupportedArtifactError>>()?;
        let filename = req_str(map, "filename", &id_repr)?;
        let kind = match map.get("kind") {
            None => Kind::Document {
                file: filename.clone(),
            },
            Some(value) => serde_json::from_value(value.clone())
                .map_err(|_| invalid(&id_repr, "kind", value))?,
        };
        let archived = match map.get("archived") {
            None => false,
            Some(Value::Bool(archived)) => *archived,
            Some(other) => return Err(invalid(&id_repr, "archived", other)),
        };
        Ok(Self {
            id: req_str(map, "id", &id_repr)?,
            kind,
            title: opt_str_field(map, "title", &id_repr)?,
            filename,
            created_at: req_num(map, "created_at", &id_repr)?,
            group: opt_str_field(map, "group", &id_repr)?,
            archived,
            history,
        })
    }
}

fn unsupported(message: String) -> UnsupportedArtifactError {
    UnsupportedArtifactError(message)
}

fn invalid(id_repr: &str, field: &str, value: &Value) -> UnsupportedArtifactError {
    unsupported(format!(
        "artifact {id_repr} has an invalid {field}: {value}"
    ))
}

/// Every `expected` key present, nothing outside `expected` and `optional`.
fn require_exact_keys(
    data: &Map<String, Value>,
    expected: &[&str],
    optional: &[&str],
    what: &str,
) -> Result<(), UnsupportedArtifactError> {
    let keys: BTreeSet<&str> = data
        .keys()
        .map(String::as_str)
        .filter(|key| !optional.contains(key))
        .collect();
    let expected: BTreeSet<&str> = expected.iter().copied().collect();
    if keys == expected {
        return Ok(());
    }
    let list = |items: Vec<&str>| format!("[{}]", items.join(", "));
    let mut parts = Vec::new();
    let unexpected: Vec<&str> = keys.difference(&expected).copied().collect();
    if !unexpected.is_empty() {
        parts.push(format!("unexpected {}", list(unexpected)));
    }
    let missing: Vec<&str> = expected.difference(&keys).copied().collect();
    if !missing.is_empty() {
        parts.push(format!("missing {}", list(missing)));
    }
    Err(unsupported(format!(
        "{what} has an invalid key set: {}",
        parts.join("; ")
    )))
}

fn validate_history(
    touches: &[&Map<String, Value>],
    id_repr: &str,
) -> Result<(), UnsupportedArtifactError> {
    if touches.is_empty() {
        return Err(unsupported(format!(
            "artifact {id_repr} has an empty history (entry 0 must be the create)"
        )));
    }
    let contiguous = touches
        .iter()
        .zip(0u64..)
        .all(|(touch, expected)| touch["rev"].as_u64() == Some(expected));
    if !contiguous {
        let revs: Vec<String> = touches
            .iter()
            .map(|touch| touch["rev"].to_string())
            .collect();
        return Err(unsupported(format!(
            "artifact {id_repr} has non-contiguous rev numbers [{}] (expected 0..{}, one per touch)",
            revs.join(", "),
            touches.len() - 1
        )));
    }
    Ok(())
}

fn req_str(
    map: &Map<String, Value>,
    field: &str,
    id_repr: &str,
) -> Result<String, UnsupportedArtifactError> {
    match &map[field] {
        Value::String(s) => Ok(s.clone()),
        other => Err(invalid(id_repr, field, other)),
    }
}

fn opt_str_field(
    map: &Map<String, Value>,
    field: &str,
    id_repr: &str,
) -> Result<Option<String>, UnsupportedArtifactError> {
    match &map[field] {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        other => Err(invalid(id_repr, field, other)),
    }
}

fn req_num(
    map: &Map<String, Value>,
    field: &str,
    id_repr: &str,
) -> Result<Number, UnsupportedArtifactError> {
    match &map[field] {
        Value::Number(n) => Ok(n.clone()),
        other => Err(invalid(id_repr, field, other)),
    }
}

fn opt_str(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |s| Value::String(s.to_owned()))
}

fn repr_opt(value: Option<&Value>) -> String {
    value.map_or_else(|| "null".to_owned(), Value::to_string)
}

/// The last path component, ignoring empty and `.` components.
pub(crate) fn base_name(path: &str) -> &str {
    path.rsplit('/')
        .find(|part| !part.is_empty() && *part != ".")
        .unwrap_or("")
}

/// A bare file name's suffix: from the last dot, unless the part before it is empty or all dots
/// (`.bashrc`, `...` have no suffix; `a.` has suffix `.`).
pub(crate) fn suffix(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if !name[..i].trim_start_matches('.').is_empty() => &name[i..],
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Main's hand-written record (T-ART-01): compact, keys shuffled.
    const HAND_WRITTEN_A: &str = concat!(
        r#"{"history":[{"rev":0,"changes":null,"at":1.5,"session_id":"s1"},"#,
        r#"{"changes":"tweak","session_id":"user","rev":1,"at":2.0}],"group":null,"created_at":1.5,"#,
        r#""filename":"plan.md","title":null,"id":"a","artifact_schema_version":2}"#
    );

    /// Main's golden/art/01.txt, plus the two keys records written since artifact kinds carry.
    const GOLDEN_01: &str = "{\n  \"artifact_schema_version\": 2,\n  \"id\": \"a\",\n  \"title\": null,\n  \"filename\": \"plan.md\",\n  \"created_at\": 1.5,\n  \"group\": null,\n  \"history\": [\n    {\n      \"session_id\": \"s1\",\n      \"at\": 1.5,\n      \"rev\": 0,\n      \"changes\": null\n    },\n    {\n      \"session_id\": \"user\",\n      \"at\": 2.0,\n      \"rev\": 1,\n      \"changes\": \"tweak\"\n    }\n  ],\n  \"kind\": {\n    \"kind\": \"document\",\n    \"file\": \"plan.md\"\n  },\n  \"archived\": false\n}";

    fn err(data: Value) -> String {
        Artifact::from_value(&data).unwrap_err().to_string()
    }

    fn pretty(value: &Value) -> String {
        serde_json::to_string_pretty(value).unwrap()
    }

    fn record(id: &str, revs: &[u64]) -> Value {
        let history: Vec<Value> = revs
            .iter()
            .map(|rev| json!({"session_id": "s1", "at": 1.0, "rev": rev, "changes": null}))
            .collect();
        json!({
            "artifact_schema_version": 2, "id": id, "title": null, "filename": "plan.md",
            "created_at": 1.0, "group": null, "history": history,
        })
    }

    #[test]
    fn main_artifact_fixtures_round_trip() {
        let data: Value = serde_json::from_str(HAND_WRITTEN_A).unwrap();
        let artifact = Artifact::from_value(&data).unwrap();
        assert_eq!(
            artifact.kind,
            Kind::Document {
                file: "plan.md".into()
            },
            "a record without a kind is a document"
        );
        assert!(!artifact.archived);
        assert_eq!(pretty(&artifact.to_value()), GOLDEN_01);
        assert_eq!(artifact.updated_at().to_string(), "2.0");
        assert_eq!(artifact.latest_rev(), 1);
        assert_eq!(artifact.history()[1].changes.as_deref(), Some("tweak"));
        assert_eq!(artifact.history()[1].session_id, USER_ACTOR);
        let golden: Value = serde_json::from_str(GOLDEN_01).unwrap();
        assert_eq!(Artifact::from_value(&golden).unwrap(), artifact);
    }

    #[test]
    fn integer_timestamps_stay_integers() {
        let mut data = record("i", &[0]);
        data["created_at"] = json!(7);
        data["history"][0]["at"] = json!(7);
        let out = pretty(&Artifact::from_value(&data).unwrap().to_value());
        assert!(out.contains("\"created_at\": 7,"), "{out}");
        assert!(out.contains("\"at\": 7,"), "{out}");
    }

    #[test]
    fn version_guard_messages() {
        let tail = "is unsupported (expected 2); older artifact records are not migrated";
        assert_eq!(
            err(json!({"artifact_schema_version": 1, "id": "old", "foo": 1})),
            format!("record artifact_schema_version=1 {tail}")
        );
        assert_eq!(
            err(json!({"id": "nover"})),
            format!("record artifact_schema_version=null {tail}")
        );
        assert_eq!(
            err(json!({"artifact_schema_version": "2"})),
            format!("record artifact_schema_version=\"2\" {tail}")
        );
    }

    #[test]
    fn exact_key_set_messages() {
        let mut keys = record("keys", &[0]);
        keys.as_object_mut().unwrap().remove("group");
        keys["foo"] = json!(1);
        assert_eq!(
            err(keys),
            "artifact \"keys\" has an invalid key set: unexpected [foo]; missing [group]"
        );
        let mut touchkeys = record("touchkeys", &[0]);
        touchkeys["history"][0]["how"] = json!("create");
        assert_eq!(
            err(touchkeys),
            "a history entry has an invalid key set: unexpected [how]"
        );
        assert_eq!(
            err(json!({"artifact_schema_version": 2, "x": 1})),
            "artifact null has an invalid key set: unexpected [x]; missing \
             [created_at, filename, group, history, id, title]"
        );
        let mut extra = record("extra", &[0]);
        extra["kind"] = json!({"kind": "document", "file": "plan.md"});
        extra["archived"] = json!(false);
        assert!(
            Artifact::from_value(&extra).is_ok(),
            "kind and archived are optional"
        );
    }

    #[test]
    fn history_invariant_messages() {
        assert_eq!(
            err(record("empty", &[])),
            "artifact \"empty\" has an empty history (entry 0 must be the create)"
        );
        assert_eq!(
            err(record("gap02", &[0, 2])),
            "artifact \"gap02\" has non-contiguous rev numbers [0, 2] (expected 0..1, one per touch)"
        );
        assert_eq!(
            err(record("gap12", &[1, 2])),
            "artifact \"gap12\" has non-contiguous rev numbers [1, 2] (expected 0..1, one per touch)"
        );
    }

    #[test]
    fn field_types_are_checked() {
        let mut data = record("t", &[0]);
        data["title"] = json!(3);
        assert_eq!(err(data), "artifact \"t\" has an invalid title: 3");
        let mut data = record("k", &[0]);
        data["kind"] = json!({"file": "x"});
        assert_eq!(
            err(data),
            "artifact \"k\" has an invalid kind: {\"file\":\"x\"}"
        );
        let mut data = record("ar", &[0]);
        data["archived"] = json!("yes");
        assert_eq!(
            err(data),
            "artifact \"ar\" has an invalid archived: \"yes\""
        );
    }

    #[test]
    fn kinds_round_trip_through_the_record() {
        let kinds = [
            Kind::Document {
                file: "plan.md".into(),
            },
            Kind::Diff {
                worktree: "/w/tree".into(),
                base: "b28bae8af3042e3e".into(),
                repository: Some("/w/main/.git".into()),
            },
            Kind::Other {
                kind: "tour".into(),
                facts: json!({"worktree": "/w/tree", "stops": "stops.lua"})
                    .as_object()
                    .unwrap()
                    .clone(),
            },
        ];
        for kind in kinds {
            let mut artifact = Artifact::new(
                "id".into(),
                kind.clone(),
                None,
                "f".into(),
                Number::from(1),
                None,
                "s1".into(),
            );
            artifact.archived = true;
            let value = artifact.to_value();
            assert_eq!(value["kind"]["kind"], json!(kind.name()));
            assert_eq!(value["archived"], json!(true));
            let back = Artifact::from_value(&value).unwrap();
            assert_eq!(back, artifact, "{kind:?}");
        }
        let tour: Kind = serde_json::from_value(json!({"kind": "tour", "stops": "s.lua"})).unwrap();
        assert_eq!(tour.name(), "tour");
        assert_eq!(tour.describe(), "tour");
        assert_eq!(tour.worktree(), None);
        let diff = Kind::Diff {
            worktree: "/w".into(),
            base: "b28bae8af3042e3e".into(),
            repository: Some("/r/.git".into()),
        };
        assert_eq!(diff.describe(), "diff vs b28bae8");
        assert_eq!(diff.worktree(), Some(Path::new("/w")));
        assert!(diff.is_diff());
        assert_eq!(
            serde_json::to_value(&diff).unwrap(),
            json!({"kind": "diff", "worktree": "/w", "base": "b28bae8af3042e3e", "repository": "/r/.git"})
        );
        let early: Kind =
            serde_json::from_value(json!({"kind": "diff", "worktree": "/w", "base": "b"})).unwrap();
        assert_eq!(
            early,
            Kind::Diff {
                worktree: "/w".into(),
                base: "b".into(),
                repository: None
            },
            "a diff recorded before the repository was: no repository"
        );
    }

    #[test]
    fn record_touch_appends_next_rev() {
        let mut artifact = Artifact::new(
            "id".into(),
            Kind::Document {
                file: "plan.md".into(),
            },
            None,
            "plan.md".into(),
            Number::from(1),
            None,
            "s1".into(),
        );
        assert_eq!(artifact.next_rev(), 1);
        let rev = artifact.record_touch(USER_ACTOR.into(), Number::from(2), Some("c".into()));
        assert_eq!((rev, artifact.latest_rev()), (1, 1));
        assert_eq!(artifact.updated_at(), &Number::from(2));
        let back = Artifact::from_value(&artifact.to_value()).unwrap();
        assert_eq!(back, artifact);
    }

    #[test]
    fn extension_is_the_last_suffix() {
        let cases = [
            ("plan.md", ".md"),
            ("artifact", ""),
            ("a.", "."),
            (".bashrc", ""),
            ("a.tar.gz", ".gz"),
            ("..", ""),
            ("x/.md", ""),
            ("dir/plan.md", ".md"),
            ("a..b", ".b"),
            (".a.b", ".b"),
            ("...", ""),
            ("plan.md/", ".md"),
        ];
        for (name, expected) in cases {
            assert_eq!(suffix(base_name(name)), expected, "{name}");
        }
    }
}
