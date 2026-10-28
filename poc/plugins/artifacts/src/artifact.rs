//! The artifact record (`Artifact`): a durable deliverable of one of three kinds, ported from
//! main's v4 `artifact.rs`.
//!
//! The record is read through a strict boundary: `artifact_schema_version` must be 4, the key set
//! is the eight base keys plus exactly the one kind key named by `kind`, and that kind's subfield
//! has its own exact key set. No history, no revisions: `modified_at` is stamped on every
//! content mutation, `archived_at` only by archive / unarchive. Timestamps stay
//! `serde_json::Number`, so an integer written by hand stays an integer.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Map, Number, Value, json};

pub const ARTIFACT_SCHEMA_VERSION: u64 = 4;

/// Owner of an artifact created outside any tx session (a plain-terminal call).
pub const USER_ACTOR: &str = "user";

const BASE_KEYS: [&str; 8] = [
    "artifact_schema_version",
    "id",
    "kind",
    "title",
    "owner",
    "created_at",
    "modified_at",
    "archived_at",
];
pub const KIND_NAMES: [&str; 3] = ["document", "diff", "tour"];

/// A persisted record is not a current-version artifact record.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct UnsupportedArtifactError(String);

/// What an artifact is, with the structured facts its kind needs. The open and close behaviour
/// is derived from this in code (`kinds`); the record carries no commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A hand-edited file under the artifact's directory.
    Document { file: String },
    /// `git diff <base>` of a repository checkout, pinned at `base`.
    Diff { worktree: PathBuf, base: String },
    /// A code tour: a stops file under the artifact's directory pointing into `worktree`.
    Tour { worktree: PathBuf, stops: String },
}

impl Kind {
    /// The `kind` value, which also names the subfield key.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Document { .. } => "document",
            Self::Diff { .. } => "diff",
            Self::Tour { .. } => "tour",
        }
    }

    pub fn is_diff(&self) -> bool {
        matches!(self, Self::Diff { .. })
    }

    /// The checkout the kind points into (`None` for a document).
    pub fn worktree(&self) -> Option<&Path> {
        match self {
            Self::Document { .. } => None,
            Self::Diff { worktree, .. } | Self::Tour { worktree, .. } => Some(worktree),
        }
    }

    /// The subfield, its keys in order (`serde_json` keeps insertion order here).
    fn to_value(&self) -> Value {
        match self {
            Self::Document { file } => json!({ "file": file }),
            Self::Diff { worktree, base } => {
                json!({ "worktree": path_value(worktree), "base": base })
            }
            Self::Tour { worktree, stops } => {
                json!({ "worktree": path_value(worktree), "stops": stops })
            }
        }
    }

    fn from_value(
        name: &str,
        data: &Value,
        id_repr: &str,
    ) -> Result<Self, UnsupportedArtifactError> {
        let Value::Object(map) = data else {
            return Err(invalid(id_repr, name, data));
        };
        let what = format!("artifact {id_repr} {name}");
        let field = |key: &str| req_str(map, key, id_repr);
        match name {
            "document" => {
                require_exact_keys(map, &["file"], &what)?;
                Ok(Self::Document {
                    file: field("file")?,
                })
            }
            "diff" => {
                require_exact_keys(map, &["worktree", "base"], &what)?;
                Ok(Self::Diff {
                    worktree: field("worktree")?.into(),
                    base: field("base")?,
                })
            }
            "tour" => {
                require_exact_keys(map, &["worktree", "stops"], &what)?;
                Ok(Self::Tour {
                    worktree: field("worktree")?.into(),
                    stops: field("stops")?,
                })
            }
            _ => unreachable!("kind names are validated before the subfield is read"),
        }
    }
}

/// A durable deliverable (the record half; files live under `<data_dir>/<id>/`).
#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    pub id: String,
    pub kind: Kind,
    pub title: String,
    /// The session that created the artifact, or [`USER_ACTOR`].
    pub owner: String,
    pub created_at: Number,
    pub modified_at: Number,
    /// When it was archived: hidden from prefix+a and default listings, content kept.
    pub archived_at: Option<Number>,
}

impl Artifact {
    /// A fresh artifact, modified when created.
    pub fn new(id: String, kind: Kind, title: String, owner: String, created_at: Number) -> Self {
        Self {
            id,
            kind,
            title,
            owner,
            modified_at: created_at.clone(),
            created_at,
            archived_at: None,
        }
    }

    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }

    pub fn modified_secs(&self) -> f64 {
        self.modified_at.as_f64().unwrap_or(0.0)
    }

    /// The on-disk shape, in canonical key order: the base keys, then the one kind key.
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert(
            "artifact_schema_version".into(),
            ARTIFACT_SCHEMA_VERSION.into(),
        );
        map.insert("id".into(), self.id.as_str().into());
        map.insert("kind".into(), self.kind.name().into());
        map.insert("title".into(), self.title.as_str().into());
        map.insert("owner".into(), self.owner.as_str().into());
        map.insert("created_at".into(), Value::Number(self.created_at.clone()));
        map.insert(
            "modified_at".into(),
            Value::Number(self.modified_at.clone()),
        );
        map.insert(
            "archived_at".into(),
            self.archived_at.clone().map_or(Value::Null, Value::Number),
        );
        map.insert(self.kind.name().into(), self.kind.to_value());
        Value::Object(map)
    }

    /// Deserialize at the strict boundary: version, the kind name, the record key set (base keys
    /// plus the kind key), the subfield's key set, then field types.
    pub fn from_value(data: &Value) -> Result<Self, UnsupportedArtifactError> {
        let Value::Object(map) = data else {
            return Err(UnsupportedArtifactError(format!(
                "record is not a JSON object: {data}"
            )));
        };
        let version = map.get("artifact_schema_version");
        if version.and_then(Value::as_u64) != Some(ARTIFACT_SCHEMA_VERSION) {
            return Err(UnsupportedArtifactError(format!(
                "record artifact_schema_version={} is unsupported (expected \
                 {ARTIFACT_SCHEMA_VERSION}); run `txc artifact migrate` to upgrade older records",
                repr_opt(version)
            )));
        }
        let id_repr = repr_opt(map.get("id"));
        let kind_name = match map.get("kind") {
            Some(Value::String(name)) if KIND_NAMES.contains(&name.as_str()) => name.as_str(),
            other => {
                return Err(UnsupportedArtifactError(format!(
                    "artifact {id_repr} has an invalid kind: {} (expected one of [{}])",
                    repr_opt(other),
                    KIND_NAMES.join(", ")
                )));
            }
        };
        let mut expected = BASE_KEYS.to_vec();
        expected.push(kind_name);
        require_exact_keys(map, &expected, &format!("artifact {id_repr}"))?;
        let kind = Kind::from_value(kind_name, &map[kind_name], &id_repr)?;
        let title = req_str(map, "title", &id_repr)?;
        if title.is_empty() {
            return Err(UnsupportedArtifactError(format!(
                "artifact {id_repr} has an empty title"
            )));
        }
        let archived_at = match &map["archived_at"] {
            Value::Null => None,
            Value::Number(at) => Some(at.clone()),
            other => return Err(invalid(&id_repr, "archived_at", other)),
        };
        Ok(Self {
            id: req_str(map, "id", &id_repr)?,
            kind,
            title,
            owner: req_str(map, "owner", &id_repr)?,
            created_at: req_num(map, "created_at", &id_repr)?,
            modified_at: req_num(map, "modified_at", &id_repr)?,
            archived_at,
        })
    }
}

fn path_value(path: &Path) -> Value {
    path.to_string_lossy().into_owned().into()
}

fn invalid(id_repr: &str, field: &str, value: &Value) -> UnsupportedArtifactError {
    UnsupportedArtifactError(format!(
        "artifact {id_repr} has an invalid {field}: {value}"
    ))
}

fn require_exact_keys(
    data: &Map<String, Value>,
    expected: &[&str],
    what: &str,
) -> Result<(), UnsupportedArtifactError> {
    let keys: BTreeSet<&str> = data.keys().map(String::as_str).collect();
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
    Err(UnsupportedArtifactError(format!(
        "{what} has an invalid key set: {}",
        parts.join("; ")
    )))
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

fn repr_opt(value: Option<&Value>) -> String {
    value.map_or_else(|| "null".to_owned(), Value::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(data: Value) -> String {
        Artifact::from_value(&data).unwrap_err().to_string()
    }

    fn document(id: &str) -> Value {
        json!({
            "artifact_schema_version": 4, "id": id, "kind": "document", "title": "T",
            "owner": "s1", "created_at": 1.5, "modified_at": 2.0, "archived_at": null,
            "document": {"file": "plan.md"},
        })
    }

    fn pretty(value: &Value) -> String {
        serde_json::to_string_pretty(value).unwrap()
    }

    #[test]
    fn document_round_trip_is_canonical() {
        let shuffled: Value = serde_json::from_str(
            r#"{"document":{"file":"plan.md"},"archived_at":null,"modified_at":2.0,"owner":"user",
                "created_at":1.5,"title":"Plan","kind":"document","id":"a","artifact_schema_version":4}"#,
        )
        .unwrap();
        let artifact = Artifact::from_value(&shuffled).unwrap();
        assert_eq!(
            artifact.kind,
            Kind::Document {
                file: "plan.md".into()
            }
        );
        assert_eq!(artifact.owner, USER_ACTOR);
        assert_eq!(
            pretty(&artifact.to_value()),
            "{\n  \"artifact_schema_version\": 4,\n  \"id\": \"a\",\n  \"kind\": \"document\",\n  \
             \"title\": \"Plan\",\n  \"owner\": \"user\",\n  \"created_at\": 1.5,\n  \
             \"modified_at\": 2.0,\n  \"archived_at\": null,\n  \"document\": {\n    \"file\": \"plan.md\"\n  }\n}"
        );
    }

    #[test]
    fn diff_and_tour_round_trip() {
        let diff = Artifact::new(
            "d".into(),
            Kind::Diff {
                worktree: PathBuf::from("/w"),
                base: "abc".into(),
            },
            "review".into(),
            "s1".into(),
            Number::from(7),
        );
        assert_eq!(diff.modified_at, Number::from(7));
        let back = Artifact::from_value(&diff.to_value()).unwrap();
        assert_eq!(back, diff);
        assert_eq!(back.kind.worktree(), Some(Path::new("/w")));
        assert!(back.kind.is_diff());
        let tour = Artifact::new(
            "t".into(),
            Kind::Tour {
                worktree: PathBuf::from("/w"),
                stops: "stops.lua".into(),
            },
            "tour".into(),
            "s1".into(),
            Number::from_f64(1.5).unwrap(),
        );
        let value = tour.to_value();
        assert_eq!(
            value["tour"],
            json!({"worktree": "/w", "stops": "stops.lua"})
        );
        assert_eq!(Artifact::from_value(&value).unwrap(), tour);
    }

    #[test]
    fn version_and_kind_guards() {
        let tail =
            "is unsupported (expected 4); run `txc artifact migrate` to upgrade older records";
        assert_eq!(
            err(json!({"artifact_schema_version": 2, "id": "old", "foo": 1})),
            format!("record artifact_schema_version=2 {tail}")
        );
        assert_eq!(
            err(json!({"id": "nover"})),
            format!("record artifact_schema_version=null {tail}")
        );
        assert_eq!(
            err(json!({"artifact_schema_version": 4, "id": "k", "kind": "site"})),
            "artifact \"k\" has an invalid kind: \"site\" (expected one of [document, diff, tour])"
        );
    }

    #[test]
    fn key_set_messages() {
        let mut wrong_kind_key = document("a");
        wrong_kind_key["kind"] = json!("diff");
        assert_eq!(
            err(wrong_kind_key),
            "artifact \"a\" has an invalid key set: unexpected [document]; missing [diff]"
        );
        let mut extra = document("b");
        extra["group"] = json!(null);
        assert_eq!(
            err(extra),
            "artifact \"b\" has an invalid key set: unexpected [group]"
        );
        let mut subfield = document("c");
        subfield["document"] = json!({"file": "x", "path": "y"});
        assert_eq!(
            err(subfield),
            "artifact \"c\" document has an invalid key set: unexpected [path]"
        );
        let mut not_object = document("e");
        not_object["document"] = json!("plan.md");
        assert_eq!(
            err(not_object),
            "artifact \"e\" has an invalid document: \"plan.md\""
        );
    }

    #[test]
    fn field_type_and_title_messages() {
        let mut empty_title = document("t");
        empty_title["title"] = json!("");
        assert_eq!(err(empty_title), "artifact \"t\" has an empty title");
        let mut bad_time = document("m");
        bad_time["modified_at"] = json!("soon");
        assert_eq!(
            err(bad_time),
            "artifact \"m\" has an invalid modified_at: \"soon\""
        );
        let mut bad_archive = document("a");
        bad_archive["archived_at"] = json!("yesterday");
        assert_eq!(
            err(bad_archive),
            "artifact \"a\" has an invalid archived_at: \"yesterday\""
        );
    }

    #[test]
    fn integer_timestamps_stay_integers() {
        let mut data = document("i");
        data["created_at"] = json!(7);
        data["archived_at"] = json!(9);
        let archived = Artifact::from_value(&data).unwrap();
        assert!(archived.is_archived());
        let out = pretty(&archived.to_value());
        assert!(out.contains("\"created_at\": 7,"), "{out}");
        assert!(out.contains("\"archived_at\": 9,"), "{out}");
    }
}
