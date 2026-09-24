//! Port of lib/tx/artifact.py — the artifact record (`Artifact`) and its history entries (`Touch`).
//!
//! The record is read through a strict boundary: `artifact_schema_version` must be 2, the key sets
//! must be exact, the history non-empty with revs contiguous from 0. `updated_at` is derived from
//! the last touch, never stored.

use std::collections::BTreeSet;

use serde_json::{Map, Number, Value};

use crate::pyjson::float_repr;

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
const TOUCH_KEYS: [&str; 4] = ["session_id", "at", "rev", "changes"];

/// A persisted record is not a current-version artifact record. The message is the Python text.
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

/// A durable, versioned deliverable (the record half; files live under `artifacts/<id>/`).
///
/// `history` is private so it stays non-empty with revs `0..len`; extend it with `record_touch`.
#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    pub id: String,
    pub title: Option<String>,
    pub filename: String,
    pub created_at: Number,
    pub group: Option<String>,
    history: Vec<Touch>,
}

impl Artifact {
    /// A fresh artifact whose history is the single create touch (rev 0) at `created_at`.
    pub fn new(
        id: String,
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
            title,
            filename,
            created_at,
            group,
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

    /// `Path(filename).suffix` (Python 3.14 semantics): reused for every rev and `current`.
    pub fn extension(&self) -> &str {
        py_suffix(py_name(&self.filename))
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
        Value::Object(map)
    }

    /// Deserialize at the strict boundary, in the reference's check order: version, record key
    /// set, each entry's key set, non-empty history, contiguous revs. Field types are checked last
    /// (the reference does not check them at all).
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
                 {ARTIFACT_SCHEMA_VERSION}); tx-ide does not back-migrate older artifact records",
                py_repr_opt(version)
            )));
        }
        let id_repr = py_repr_opt(map.get("id"));
        require_exact_keys(map, &ARTIFACT_KEYS, &format!("artifact {id_repr}"))?;

        let Value::Array(entries) = &map["history"] else {
            return Err(invalid(&id_repr, "history", &map["history"]));
        };
        let mut touch_maps = Vec::with_capacity(entries.len());
        for entry in entries {
            let Value::Object(touch) = entry else {
                return Err(unsupported(format!(
                    "a history entry is not an object: {}",
                    py_repr(entry)
                )));
            };
            require_exact_keys(touch, &TOUCH_KEYS, "a history entry")?;
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
        Ok(Self {
            id: req_str(map, "id", &id_repr)?,
            title: opt_str_field(map, "title", &id_repr)?,
            filename: req_str(map, "filename", &id_repr)?,
            created_at: req_num(map, "created_at", &id_repr)?,
            group: opt_str_field(map, "group", &id_repr)?,
            history,
        })
    }
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
        let revs: Vec<String> = touches.iter().map(|touch| py_repr(&touch["rev"])).collect();
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

/// `PurePosixPath(path).name`: the last component, ignoring empty and `.` components.
pub(crate) fn py_name(path: &str) -> &str {
    path.rsplit('/')
        .find(|part| !part.is_empty() && *part != ".")
        .unwrap_or("")
}

/// Python 3.14 `PurePath.suffix` of a bare file name: from the last dot, unless the part before
/// it is empty or all dots (`.bashrc`, `...` have no suffix; `a.` has suffix `.`).
pub(crate) fn py_suffix(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if !name[..i].trim_start_matches('.').is_empty() => &name[i..],
        _ => "",
    }
}

fn py_repr_opt(value: Option<&Value>) -> String {
    value.map_or_else(|| "None".to_owned(), py_repr)
}

fn py_str_list(items: &[&str]) -> String {
    let parts: Vec<String> = items.iter().map(|s| py_str_repr(s)).collect();
    format!("[{}]", parts.join(", "))
}

/// Python `repr()` of a `json.load`ed value.
fn py_repr(value: &Value) -> String {
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

    /// T-ART-01's hand-written record: compact, keys shuffled.
    const HAND_WRITTEN_A: &str = concat!(
        r#"{"history":[{"rev":0,"changes":null,"at":1.5,"session_id":"s1"},"#,
        r#"{"changes":"tweak","session_id":"user","rev":1,"at":2.0}],"group":null,"created_at":1.5,"#,
        r#""filename":"plan.md","title":null,"id":"a","artifact_schema_version":2}"#
    );

    /// golden/art/01.txt
    const GOLDEN_01: &str = "{\n  \"artifact_schema_version\": 2,\n  \"id\": \"a\",\n  \"title\": null,\n  \"filename\": \"plan.md\",\n  \"created_at\": 1.5,\n  \"group\": null,\n  \"history\": [\n    {\n      \"session_id\": \"s1\",\n      \"at\": 1.5,\n      \"rev\": 0,\n      \"changes\": null\n    },\n    {\n      \"session_id\": \"user\",\n      \"at\": 2.0,\n      \"rev\": 1,\n      \"changes\": \"tweak\"\n    }\n  ]\n}";

    fn err(data: Value) -> String {
        Artifact::from_value(&data).unwrap_err().to_string()
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
    fn round_trip_is_canonical_golden() {
        let data: Value = serde_json::from_str(HAND_WRITTEN_A).unwrap();
        let artifact = Artifact::from_value(&data).unwrap();
        assert_eq!(dumps_pretty(&artifact.to_value()), GOLDEN_01);
        assert_eq!(artifact.updated_at().to_string(), "2.0");
        assert_eq!(artifact.latest_rev(), 1);
        assert_eq!(artifact.history()[1].changes.as_deref(), Some("tweak"));
        assert_eq!(artifact.history()[1].session_id, USER_ACTOR);
    }

    #[test]
    fn integer_timestamps_stay_integers() {
        let mut data = record("i", &[0]);
        data["created_at"] = json!(7);
        data["history"][0]["at"] = json!(7);
        let out = dumps_pretty(&Artifact::from_value(&data).unwrap().to_value());
        assert!(out.contains("\"created_at\": 7,"), "{out}");
        assert!(out.contains("\"at\": 7,"), "{out}");
    }

    #[test]
    fn version_guard_messages() {
        let tail =
            "is unsupported (expected 2); tx-ide does not back-migrate older artifact records";
        assert_eq!(
            err(json!({"artifact_schema_version": 1, "id": "old", "foo": 1})),
            format!("record artifact_schema_version=1 {tail}")
        );
        assert_eq!(
            err(json!({"id": "nover"})),
            format!("record artifact_schema_version=None {tail}")
        );
        assert_eq!(
            err(json!({"artifact_schema_version": "2"})),
            format!("record artifact_schema_version='2' {tail}")
        );
    }

    #[test]
    fn exact_key_set_messages() {
        let mut keys = record("keys", &[0]);
        keys.as_object_mut().unwrap().remove("group");
        keys["foo"] = json!(1);
        assert_eq!(
            err(keys),
            "artifact 'keys' has an invalid key set: unexpected ['foo']; missing ['group']"
        );
        let mut touchkeys = record("touchkeys", &[0]);
        touchkeys["history"][0]["how"] = json!("create");
        assert_eq!(
            err(touchkeys),
            "a history entry has an invalid key set: unexpected ['how']"
        );
        assert_eq!(
            err(json!({"artifact_schema_version": 2, "id": "it's", "z": 1, "a": 2})),
            "artifact \"it's\" has an invalid key set: unexpected ['a', 'z']; missing \
             ['created_at', 'filename', 'group', 'history', 'title']"
        );
        assert_eq!(
            err(json!({"artifact_schema_version": 2, "x": 1})),
            "artifact None has an invalid key set: unexpected ['x']; missing \
             ['created_at', 'filename', 'group', 'history', 'id', 'title']"
        );
    }

    #[test]
    fn history_invariant_messages() {
        assert_eq!(
            err(record("empty", &[])),
            "artifact 'empty' has an empty history (entry 0 must be the create)"
        );
        assert_eq!(
            err(record("gap02", &[0, 2])),
            "artifact 'gap02' has non-contiguous rev numbers [0, 2] (expected 0..1, one per touch)"
        );
        assert_eq!(
            err(record("gap12", &[1, 2])),
            "artifact 'gap12' has non-contiguous rev numbers [1, 2] (expected 0..1, one per touch)"
        );
    }

    #[test]
    fn record_touch_appends_next_rev() {
        let mut artifact = Artifact::new(
            "id".into(),
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
    fn extension_matches_python_suffix() {
        // Expected values from python3.14 `Path(n).suffix`.
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
            ("a. b", ". b"),
            ("plan.md/", ".md"),
        ];
        for (name, suffix) in cases {
            assert_eq!(py_suffix(py_name(name)), suffix, "{name}");
        }
    }

    #[test]
    fn str_repr_matches_python() {
        assert_eq!(py_str_repr("it's"), "\"it's\"");
        assert_eq!(py_str_repr("a\"b"), "'a\"b'");
        assert_eq!(py_str_repr("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(py_str_repr("t\u{1}é\u{200b}\n"), "'t\\x01é\\u200b\\n'");
    }
}
