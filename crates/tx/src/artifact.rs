//! The artifact record (`Artifact`): a durable deliverable of one of three kinds.
//!
//! The record is read through a strict boundary: `artifact_schema_version` must be 4, the key
//! set is the eight base keys plus exactly the one kind key named by `kind`, and that kind's
//! subfield has its own exact key set. No history, no revisions: `modified_at` is stamped by tx on
//! every tx-side content mutation; `archived_at` only by archive / unarchive.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Map, Number, Value};

use crate::pyjson::float_repr;

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
const DOCUMENT_KEYS: [&str; 1] = ["file"];
const DIFF_KEYS: [&str; 2] = ["worktree", "base"];
const TOUR_KEYS: [&str; 2] = ["worktree", "stops"];

/// A persisted record is not a current-version artifact record.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct UnsupportedArtifactError(String);

/// What an artifact is, with the structured facts its kind needs. tx derives the open and close
/// behaviour from this in code; the record carries no shell commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A hand-edited file under the artifact's directory.
    Document { file: String },
    /// `git diff <base>` of a repository checkout, pinned at `base`.
    Diff { worktree: PathBuf, base: String },
    /// A code tour: a stops file under the artifact's directory whose stops point into `worktree`.
    Tour { worktree: PathBuf, stops: String },
}

impl Kind {
    /// The kind's name: the `kind` value and the name of its subfield key.
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

    /// The subfield, with its exact key set in order.
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        match self {
            Self::Document { file } => {
                map.insert("file".into(), Value::String(file.clone()));
            }
            Self::Diff { worktree, base } => {
                map.insert("worktree".into(), path_value(worktree));
                map.insert("base".into(), Value::String(base.clone()));
            }
            Self::Tour { worktree, stops } => {
                map.insert("worktree".into(), path_value(worktree));
                map.insert("stops".into(), Value::String(stops.clone()));
            }
        }
        Value::Object(map)
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
        match name {
            "document" => {
                require_exact_keys(map, &DOCUMENT_KEYS, &what)?;
                Ok(Self::Document {
                    file: req_str(map, "file", id_repr)?,
                })
            }
            "diff" => {
                require_exact_keys(map, &DIFF_KEYS, &what)?;
                Ok(Self::Diff {
                    worktree: PathBuf::from(req_str(map, "worktree", id_repr)?),
                    base: req_str(map, "base", id_repr)?,
                })
            }
            "tour" => {
                require_exact_keys(map, &TOUR_KEYS, &what)?;
                Ok(Self::Tour {
                    worktree: PathBuf::from(req_str(map, "worktree", id_repr)?),
                    stops: req_str(map, "stops", id_repr)?,
                })
            }
            _ => unreachable!("kind names are validated before the subfield is read"),
        }
    }
}

const KIND_NAMES: [&str; 3] = ["document", "diff", "tour"];

/// A durable deliverable (the record half; files live under `artifacts/<id>/`).
#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    pub id: String,
    pub kind: Kind,
    pub title: String,
    /// The session that created the artifact (or [`USER_ACTOR`]).
    pub owner: String,
    pub created_at: Number,
    pub modified_at: Number,
    /// When the artifact was archived — hidden from pickers and default listings, content kept.
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

    /// The on-disk shape, in canonical key order: the base keys, then the one kind key.
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert(
            "artifact_schema_version".into(),
            Value::from(ARTIFACT_SCHEMA_VERSION),
        );
        map.insert("id".into(), Value::String(self.id.clone()));
        map.insert("kind".into(), Value::String(self.kind.name().to_owned()));
        map.insert("title".into(), Value::String(self.title.clone()));
        map.insert("owner".into(), Value::String(self.owner.clone()));
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
            return Err(unsupported(format!(
                "record is not a JSON object: {}",
                py_repr(data)
            )));
        };
        let version = map.get("artifact_schema_version");
        if version.and_then(Value::as_u64) != Some(ARTIFACT_SCHEMA_VERSION) {
            return Err(unsupported(format!(
                "record artifact_schema_version={} is unsupported (expected \
                 {ARTIFACT_SCHEMA_VERSION}); run `tx migrate` to upgrade older artifact records",
                py_repr_opt(version)
            )));
        }
        let id_repr = py_repr_opt(map.get("id"));
        let kind_name = match map.get("kind") {
            Some(Value::String(name)) if KIND_NAMES.contains(&name.as_str()) => name.as_str(),
            other => {
                return Err(unsupported(format!(
                    "artifact {id_repr} has an invalid kind: {} (expected one of {})",
                    py_repr_opt(other),
                    py_str_list(&KIND_NAMES)
                )));
            }
        };
        let mut expected: Vec<&str> = BASE_KEYS.to_vec();
        expected.push(kind_name);
        require_exact_keys(map, &expected, &format!("artifact {id_repr}"))?;
        let kind = Kind::from_value(kind_name, &map[kind_name], &id_repr)?;
        let title = req_str(map, "title", &id_repr)?;
        if title.is_empty() {
            return Err(unsupported(format!(
                "artifact {id_repr} has an empty title"
            )));
        }
        Ok(Self {
            id: req_str(map, "id", &id_repr)?,
            kind,
            title,
            owner: req_str(map, "owner", &id_repr)?,
            created_at: req_num(map, "created_at", &id_repr)?,
            modified_at: req_num(map, "modified_at", &id_repr)?,
            archived_at: opt_num(map, "archived_at", &id_repr)?,
        })
    }
}

fn path_value(path: &Path) -> Value {
    Value::String(path.to_string_lossy().into_owned())
}

fn unsupported(message: String) -> UnsupportedArtifactError {
    UnsupportedArtifactError(message)
}

fn invalid(id_repr: &str, field: &str, value: &Value) -> UnsupportedArtifactError {
    unsupported(format!(
        "artifact {id_repr} has an invalid {field}: {}",
        py_repr(value)
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
    let mut parts = Vec::new();
    let unexpected: Vec<&str> = keys.difference(&expected).copied().collect();
    if !unexpected.is_empty() {
        parts.push(format!("unexpected {}", py_str_list(&unexpected)));
    }
    let missing: Vec<&str> = expected.difference(&keys).copied().collect();
    if !missing.is_empty() {
        parts.push(format!("missing {}", py_str_list(&missing)));
    }
    Err(unsupported(format!(
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

fn opt_num(
    map: &Map<String, Value>,
    field: &str,
    id_repr: &str,
) -> Result<Option<Number>, UnsupportedArtifactError> {
    match &map[field] {
        Value::Null => Ok(None),
        Value::Number(n) => Ok(Some(n.clone())),
        other => Err(invalid(id_repr, field, other)),
    }
}

fn py_repr_opt(value: Option<&Value>) -> String {
    value.map_or_else(|| "None".to_owned(), py_repr)
}

fn py_str_list(items: &[&str]) -> String {
    let parts: Vec<String> = items.iter().map(|s| py_str_repr(s)).collect();
    format!("[{}]", parts.join(", "))
}

/// Python `repr()` of a `json.load`ed value — the texts `doctor` and `ls` print for a bad record.
pub(crate) fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => match (n.as_u64(), n.as_i64()) {
            (Some(u), _) => u.to_string(),
            (None, Some(i)) => i.to_string(),
            _ => float_repr(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => py_str_repr(s),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(py_repr).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", py_str_repr(k), py_repr(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

/// Python `str.__repr__`. Non-printable detection covers the control, format and non-space
/// separator code points likely in practice, not the full Unicode table.
fn py_str_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !py_printable(c) => {
                let code = u32::from(c);
                if code < 0x100 {
                    out.push_str(&format!("\\x{code:02x}"));
                } else if code < 0x10000 {
                    out.push_str(&format!("\\u{code:04x}"));
                } else {
                    out.push_str(&format!("\\U{code:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn py_printable(c: char) -> bool {
    !(c.is_control()
        || matches!(
            c,
            '\u{a0}'
                | '\u{ad}'
                | '\u{1680}'
                | '\u{2000}'..='\u{200f}'
                | '\u{2028}'..='\u{202f}'
                | '\u{205f}'..='\u{2064}'
                | '\u{2066}'..='\u{206f}'
                | '\u{3000}'
                | '\u{e000}'..='\u{f8ff}'
                | '\u{feff}'
                | '\u{fff9}'..='\u{fffb}'
        ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyjson::dumps_pretty;
    use serde_json::json;

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
            dumps_pretty(&artifact.to_value()),
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
            "is unsupported (expected 4); run `tx migrate` to upgrade older artifact records";
        assert_eq!(
            err(json!({"artifact_schema_version": 3, "id": "old", "foo": 1})),
            format!("record artifact_schema_version=3 {tail}")
        );
        assert_eq!(
            err(json!({"id": "nover"})),
            format!("record artifact_schema_version=None {tail}")
        );
        assert_eq!(
            err(json!({"artifact_schema_version": 4, "id": "k", "kind": "site"})),
            "artifact 'k' has an invalid kind: 'site' (expected one of ['document', 'diff', 'tour'])"
        );
        assert_eq!(
            err(json!({"artifact_schema_version": 4, "id": "k"})),
            "artifact 'k' has an invalid kind: None (expected one of ['document', 'diff', 'tour'])"
        );
    }

    #[test]
    fn key_set_messages() {
        let mut wrong_kind_key = document("a");
        wrong_kind_key["kind"] = json!("diff");
        assert_eq!(
            err(wrong_kind_key),
            "artifact 'a' has an invalid key set: unexpected ['document']; missing ['diff']"
        );
        let mut extra = document("b");
        extra["group"] = json!(null);
        assert_eq!(
            err(extra),
            "artifact 'b' has an invalid key set: unexpected ['group']"
        );
        let mut subfield = document("c");
        subfield["document"] = json!({"file": "x", "path": "y"});
        assert_eq!(
            err(subfield),
            "artifact 'c' document has an invalid key set: unexpected ['path']"
        );
        let mut missing_sub = document("d");
        missing_sub["document"] = json!({});
        assert_eq!(
            err(missing_sub),
            "artifact 'd' document has an invalid key set: missing ['file']"
        );
        let mut not_object = document("e");
        not_object["document"] = json!("plan.md");
        assert_eq!(
            err(not_object),
            "artifact 'e' has an invalid document: 'plan.md'"
        );
    }

    #[test]
    fn field_type_and_title_messages() {
        let mut empty_title = document("t");
        empty_title["title"] = json!("");
        assert_eq!(err(empty_title), "artifact 't' has an empty title");
        let mut null_title = document("n");
        null_title["title"] = json!(null);
        assert_eq!(err(null_title), "artifact 'n' has an invalid title: None");
        let mut bad_time = document("m");
        bad_time["modified_at"] = json!("soon");
        assert_eq!(
            err(bad_time),
            "artifact 'm' has an invalid modified_at: 'soon'"
        );
        let mut bad_archive = document("a");
        bad_archive["archived_at"] = json!("yesterday");
        assert_eq!(
            err(bad_archive),
            "artifact 'a' has an invalid archived_at: 'yesterday'"
        );
        let mut bad_file = document("f");
        bad_file["document"]["file"] = json!(1);
        assert_eq!(err(bad_file), "artifact 'f' has an invalid file: 1");
    }

    #[test]
    fn integer_timestamps_stay_integers() {
        let mut data = document("i");
        data["created_at"] = json!(7);
        data["modified_at"] = json!(8);
        let out = dumps_pretty(&Artifact::from_value(&data).unwrap().to_value());
        assert!(out.contains("\"created_at\": 7,"), "{out}");
        assert!(out.contains("\"modified_at\": 8,"), "{out}");
        data["archived_at"] = json!(9);
        let archived = Artifact::from_value(&data).unwrap();
        assert!(archived.is_archived());
        let out = dumps_pretty(&archived.to_value());
        assert!(out.contains("\"archived_at\": 9,"), "{out}");
    }

    #[test]
    fn str_repr_matches_python() {
        assert_eq!(py_str_repr("it's"), "\"it's\"");
        assert_eq!(py_str_repr("a\"b"), "'a\"b'");
        assert_eq!(py_str_repr("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(py_str_repr("t\u{1}é\u{200b}\n"), "'t\\x01é\\u200b\\n'");
    }
}
