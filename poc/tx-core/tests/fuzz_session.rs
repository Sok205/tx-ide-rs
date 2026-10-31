//! Property-based tests for `tx_core::session` and `tx_core::config`.
//!
//! Tests marked `#[ignore = "BUG: …"]` document real failures; they stay in the file so the
//! reproduction survives, and the suite stays green.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;

use proptest::prelude::*;

fn sid(id: &str) -> tx_core::AgentId {
    id.parse().unwrap()
}
use serde_json::{Map, Value, json};
use tx_core::agent::{MAX_NAME_BYTES, RECORD_VERSION};
use tx_core::config::{MAX_SOCKET_PATH_BYTES, SOCKET_FILE};
use tx_core::{
    Agent, AgentId, CoreConfig, CoreError, Directories, EngineKind, LlmEngineState, RecordError,
    validate_name,
};

// ---------------------------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------------------------

/// Any string, including control characters, NUL and astral-plane unicode.
fn arb_text() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => any::<String>(),
        1 => "[\\x00-\\x1f\\x7f]{0,8}",
        1 => "\\PC{0,16}",
        1 => Just(String::new()),
    ]
}

fn arb_session_id() -> impl Strategy<Value = AgentId> {
    prop_oneof![
        2 => "[A-Za-z0-9_-]{1,64}".prop_map(|s| sid(s.as_str())),
        1 => Just(AgentId::mint()),
    ]
}

fn arb_engine() -> impl Strategy<Value = EngineKind> {
    prop::sample::select(EngineKind::ALL.to_vec())
}

fn arb_state() -> impl Strategy<Value = LlmEngineState> {
    prop::sample::select(LlmEngineState::ALL.to_vec())
}

/// Every f64 bit pattern serde_json can represent: finite, including -0.0 and subnormals.
fn arb_finite_f64() -> impl Strategy<Value = f64> {
    prop_oneof![
        4 => any::<f64>().prop_filter("finite", |f| f.is_finite()),
        1 => Just(-0.0),
        1 => Just(0.0),
        1 => Just(f64::MIN_POSITIVE),
        1 => Just(f64::from_bits(1)), // smallest subnormal
        1 => Just(-f64::from_bits(1)),
        1 => Just(f64::MAX),
        1 => Just(f64::MIN),
        1 => Just(1727000000.5),
    ]
}

fn arb_non_finite_f64() -> impl Strategy<Value = f64> {
    prop::sample::select(vec![f64::NAN, -f64::NAN, f64::INFINITY, f64::NEG_INFINITY])
}

fn arb_session_with(
    created_at: impl Strategy<Value = f64>,
    archived_at: impl Strategy<Value = Option<f64>>,
) -> impl Strategy<Value = Agent> {
    (
        arb_session_id(),
        arb_text(),
        arb_engine(),
        arb_text().prop_map(PathBuf::from),
        prop::collection::vec(arb_text(), 0..4),
        arb_state(),
        created_at,
        archived_at,
        prop::collection::vec(arb_text(), 0..4),
        prop::option::of(arb_session_id()),
        prop::collection::btree_map(arb_text(), arb_json(), 0..3),
    )
        .prop_map(
            |(
                id,
                name,
                engine,
                working_directory,
                roles,
                state,
                created_at,
                archived_at,
                tags,
                parent_id,
                ext,
            )| Agent {
                id,
                name,
                engine,
                working_directory,
                roles,
                state,
                created_at,
                last_modified_at: created_at,
                archived_at,
                tags,
                parent_id,
                ext,
                other: Map::new(),
            },
        )
}

fn arb_session() -> impl Strategy<Value = Agent> {
    arb_session_with(arb_finite_f64(), prop::option::of(arb_finite_f64()))
}

/// Random JSON trees, bounded in depth and width.
fn arb_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        arb_finite_f64().prop_map(Value::from),
        arb_text().prop_map(Value::String),
    ];
    leaf.prop_recursive(6, 64, 8, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..8).prop_map(Value::Array),
            prop::collection::btree_map(arb_text(), inner, 0..8)
                .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

fn session_value(session: &Agent) -> Map<String, Value> {
    match serde_json::from_str(&session.to_json().unwrap()).unwrap() {
        Value::Object(map) => map,
        other => panic!("to_json produced a non-object: {other}"),
    }
}

const TOP_KEYS: [&str; 13] = [
    "version",
    "id",
    "name",
    "engine",
    "working_directory",
    "roles",
    "state",
    "created_at",
    "last_modified_at",
    "archived_at",
    "tags",
    "parent_id",
    "ext",
];

fn arb_extra_keys(
    reserved: &'static [&'static str],
) -> impl Strategy<Value = BTreeMap<String, Value>> {
    prop::collection::btree_map(
        arb_text().prop_filter("not a schema key", move |k| !reserved.contains(&k.as_str())),
        arb_json(),
        0..5,
    )
}

fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

// ---------------------------------------------------------------------------------------------
// 1. Agent JSON round trip
// ---------------------------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn session_round_trips_through_json(session in arb_session()) {
        let text = session.to_json().unwrap();
        prop_assert!(text.ends_with("}\n"), "{text:?}");
        prop_assert!(text.starts_with("{\n  \"version\": 2,"), "{text:?}");
        let back = Agent::from_json(&text).unwrap();
        prop_assert_eq!(&back, &session);
        // PartialEq treats -0.0 == 0.0; make sure the sign bit survives too.
        prop_assert_eq!(back.created_at.to_bits(), session.created_at.to_bits());
        prop_assert_eq!(back.archived_at.map(f64::to_bits), session.archived_at.map(f64::to_bits));
    }

    #[test]
    fn to_json_is_stable_and_version_first(session in arb_session()) {
        let value = session_value(&session);
        prop_assert_eq!(value.keys().next().map(String::as_str), Some("version"));
        prop_assert_eq!(value.get("version"), Some(&json!(RECORD_VERSION)));
        prop_assert_eq!(value.get("state"), Some(&json!(session.state.as_str())));
        prop_assert_eq!(value.get("engine"), Some(&json!(session.engine.as_str())));
        for gone in ["lifecycle", "launch", "ended_at", "cwd", "parent"] {
            prop_assert!(value.get(gone).is_none(), "{gone}");
        }
        prop_assert_eq!(session.to_json().unwrap(), session.to_json().unwrap());
    }

    /// serde_json cannot encode NaN/±inf; `to_json` must not panic on them, whatever it emits.
    #[test]
    fn to_json_never_panics_on_non_finite_timestamps(
        session in arb_session_with(arb_non_finite_f64(), prop::option::of(arb_non_finite_f64()))
    ) {
        let refused = matches!(session.to_json(), Err(RecordError::NonFiniteTime(_)));
        prop_assert!(refused);
    }

    /// Round trip does not hold for non-finite `created_at`: it is written as `null` and
    /// `from_json` then refuses the record (f64 cannot be null), so a session with a NaN/inf
    /// `created_at` is written but can never be read back.
    #[test]
    fn non_finite_created_at_is_refused(session in arb_session_with(arb_non_finite_f64(), Just(None))) {
        let refused = matches!(session.to_json(), Err(RecordError::NonFiniteTime("created_at")));
        prop_assert!(refused);
    }

    /// `Some(NaN)` archived_at would silently become `None` after a round trip.
    #[test]
    fn non_finite_archived_at_is_refused(session in arb_session_with(Just(1.0), arb_non_finite_f64().prop_map(Some))) {
        let refused = matches!(session.to_json(), Err(RecordError::NonFiniteTime("archived_at")));
        prop_assert!(refused);
    }
}

/// serde's `PathBuf` serializer errors on non-UTF-8 paths, and `to_json` unwraps that with
/// `expect("a session is always serializable")`. On unix, a path can hold arbitrary bytes.
#[cfg(unix)]
#[test]
fn to_json_does_not_panic_on_non_utf8_working_directory() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let mut session = Agent::from_json(&sample().to_json().unwrap()).unwrap();
    session.working_directory = PathBuf::from(OsStr::from_bytes(b"/tmp/\xff\xfe"));
    let result = std::panic::catch_unwind(|| session.to_json());
    assert!(
        matches!(result, Ok(Err(RecordError::Json(_)))),
        "to_json must refuse, not panic"
    );
}

fn sample() -> Agent {
    Agent {
        id: sid("x"),
        name: "shell".into(),
        engine: EngineKind::Claude,
        working_directory: PathBuf::from("/"),
        roles: vec![],
        state: LlmEngineState::Ended,
        created_at: 1.0,
        last_modified_at: 1.0,
        archived_at: Some(2.0),
        tags: vec![],
        parent_id: None,
        ext: std::collections::BTreeMap::new(),
        other: Map::new(),
    }
}

// ---------------------------------------------------------------------------------------------
// 2. validate_name
// ---------------------------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn validate_name_matches_grammar_on_arbitrary_strings(name in arb_text()) {
        let result = validate_name(&name);
        prop_assert_eq!(result.is_ok(), is_valid_name(&name), "{:?} -> {:?}", name, result);
        if let Err(err) = result {
            prop_assert!(matches!(&err, CoreError::InvalidName(n) if *n == name), "{err:?}");
        }
    }

    #[test]
    fn validate_name_accepts_every_grammar_string(name in "[A-Za-z0-9_-]{1,128}") {
        prop_assert!(validate_name(&name).is_ok(), "{name:?}");
    }

    #[test]
    fn validate_name_rejects_too_long(name in "[A-Za-z0-9_-]{129,400}") {
        prop_assert!(validate_name(&name).is_err(), "{}", name.len());
    }

    /// One bad byte anywhere in an otherwise valid name is enough to reject it.
    #[test]
    fn validate_name_rejects_single_foreign_char(
        prefix in "[A-Za-z0-9_-]{0,60}",
        bad in any::<char>().prop_filter("outside grammar", |c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '-')),
        suffix in "[A-Za-z0-9_-]{0,60}",
    ) {
        let name = format!("{prefix}{bad}{suffix}");
        prop_assert!(validate_name(&name).is_err(), "{name:?}");
    }

    /// Names arriving from disk are not validated: `from_json` accepts any string as `name`.
    #[test]
    fn from_json_accepts_names_outside_the_grammar(name in arb_text().prop_filter("invalid", |n| !is_valid_name(n))) {
        let mut session = sample();
        session.name = name.clone();
        let back = Agent::from_json(&session.to_json().unwrap()).unwrap();
        prop_assert_eq!(back.name, name);
    }
}

// ---------------------------------------------------------------------------------------------
// 3. AgentId
// ---------------------------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn session_id_from_str_accepts_exactly_the_documented_set(text in arb_text()) {
        let expected_ok = !text.is_empty()
            && text.len() <= 64
            && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        let parsed = AgentId::from_str(&text);
        prop_assert_eq!(parsed.is_ok(), expected_ok, "{:?} -> {:?}", text, parsed);
        if let Ok(id) = parsed {
            prop_assert_eq!(id.as_str(), text.as_str());
            prop_assert_eq!(id.to_string(), text.clone());
            prop_assert_eq!(id, sid(text.as_str()));
        }
    }

    #[test]
    fn session_id_serde_round_trip_is_transparent(text in "[A-Za-z0-9_-]{1,64}") {
        let id = sid(text.as_str());
        let json = serde_json::to_string(&id).unwrap();
        prop_assert_eq!(&json, &serde_json::to_string(&text).unwrap());
        let back: AgentId = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, id);
    }

    /// `AgentId` deserialization is transparent, so a record on disk may carry an id that
    /// `from_str` would refuse (`""`, `"a/b"`, `".."`). Core only writes minted ids, but
    /// `from_json` does not enforce that.
    #[test]
    fn from_json_rejects_ids_from_str_rejects(text in arb_text().prop_filter("valid", |t| AgentId::from_str(t).is_err())) {
        for key in ["id", "parent_id"] {
            let mut value = session_value(&sample());
            value.insert(key.into(), Value::String(text.clone()));
            let rejected = matches!(Agent::from_json(&Value::Object(value).to_string()), Err(RecordError::Json(_)));
            prop_assert!(rejected, "{} = {:?} was accepted", key, text);
        }
    }
}

#[test]
fn minted_ids_are_hyphenated_lowercase_uuid_v4() {
    for _ in 0..256 {
        let id = AgentId::mint();
        let s = id.as_str();
        assert_eq!(s.len(), 36, "{s}");
        let bytes = s.as_bytes();
        for (i, b) in bytes.iter().enumerate() {
            if [8, 13, 18, 23].contains(&i) {
                assert_eq!(*b, b'-', "{s}");
            } else {
                assert!(b.is_ascii_hexdigit() && !b.is_ascii_uppercase(), "{s}");
            }
        }
        assert_eq!(bytes[14], b'4', "version nibble in {s}");
        assert!(
            matches!(bytes[19], b'8' | b'9' | b'a' | b'b'),
            "variant nibble in {s}"
        );
        assert_eq!(AgentId::from_str(s).unwrap(), id);
        assert_eq!(id.to_string(), s);
    }
}

// ---------------------------------------------------------------------------------------------
// 4. LlmEngineState names
// ---------------------------------------------------------------------------------------------

#[test]
fn state_as_str_and_serde_names_agree() {
    let mut seen = std::collections::HashSet::new();
    for state in LlmEngineState::ALL {
        let name = state.as_str();
        assert!(seen.insert(name), "duplicate name {name}");
        assert_eq!(serde_json::to_value(state).unwrap(), json!(name));
        assert_eq!(
            serde_json::from_value::<LlmEngineState>(json!(name)).unwrap(),
            state
        );
    }
    assert_eq!(seen.len(), LlmEngineState::ALL.len());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn state_names_reject_everything_else(text in arb_text()) {
        let known = LlmEngineState::ALL.iter().any(|state| state.as_str() == text);
        prop_assert_eq!(serde_json::from_value::<LlmEngineState>(Value::String(text.clone())).is_ok(), known, "{:?}", text);
    }

    /// Case variants ("Idle") are not accepted anywhere.
    #[test]
    fn state_names_are_case_sensitive(state in arb_state()) {
        let upper = state.as_str().to_ascii_uppercase();
        prop_assert!(serde_json::from_value::<LlmEngineState>(Value::String(upper)).is_err());
    }
}

// ---------------------------------------------------------------------------------------------
// 5. CoreConfig socket path
// ---------------------------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn socket_path_is_override_or_state_dir_join(
        state_dir in arb_text(),
        tmux_socket in prop::option::of(arb_text()),
    ) {
        let mut config = CoreConfig::new(Directories::under(&state_dir));
        config.tmux_socket = tmux_socket.clone().map(PathBuf::from);
        let expected = match &tmux_socket {
            Some(s) => PathBuf::from(s),
            None => PathBuf::from(&state_dir).join("run").join(SOCKET_FILE),
        };
        prop_assert_eq!(config.socket_path(), expected);
        prop_assert_eq!(config.tmux_conf_path(), PathBuf::from(&state_dir).join("data").join("tmux.conf"));
    }

    #[test]
    fn check_socket_path_is_ok_iff_at_most_100_bytes(
        state_dir in prop_oneof![arb_text(), "/[a-z/]{0,120}", "/[\\PC]{0,60}"],
        tmux_socket in prop::option::of(prop_oneof![arb_text(), "/[a-z/]{0,120}", "/[\\PC]{0,60}"]),
    ) {
        let mut config = CoreConfig::new(Directories::under(state_dir));
        config.tmux_socket = tmux_socket.map(PathBuf::from);
        let socket = config.socket_path();
        let len = socket.as_os_str().len();
        let result = config.check_socket_path();
        let expected_ok = len <= MAX_SOCKET_PATH_BYTES && socket.is_absolute();
        prop_assert_eq!(result.is_ok(), expected_ok, "{:?} ({} bytes) -> {:?}", socket, len, result);
        if let Err(err) = result {
            if len > MAX_SOCKET_PATH_BYTES {
                prop_assert!(matches!(&err, CoreError::SocketPathTooLong(p) if *p == socket), "{err:?}");
                prop_assert!(err.to_string().contains(&format!("{len} bytes")), "{err}");
            } else {
                prop_assert!(matches!(&err, CoreError::SocketPathNotAbsolute(p) if *p == socket), "{err:?}");
            }
        }
    }

    /// Exactly at the boundary, for every way of reaching it.
    #[test]
    fn check_socket_path_boundary(extra in 0usize..3, use_override in any::<bool>()) {
        let len = MAX_SOCKET_PATH_BYTES - 1 + extra; // 99, 100, 101
        let mut config = CoreConfig::new(Directories::under("/x"));
        if use_override {
            config.tmux_socket = Some(PathBuf::from(format!("/{}", "s".repeat(len - 1))));
        } else {
            let dir_len = len - "/run/".len() - SOCKET_FILE.len(); // "/" + dir + "/run/" + "tmux.sock"
            config = CoreConfig::new(Directories::under(format!("/{}", "d".repeat(dir_len - 1))));
        }
        prop_assert_eq!(config.socket_path().as_os_str().len(), len);
        prop_assert_eq!(config.check_socket_path().is_ok(), len <= MAX_SOCKET_PATH_BYTES);
    }
}

// ---------------------------------------------------------------------------------------------
// 6. from_json never panics
// ---------------------------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn from_json_never_panics_on_arbitrary_text(text in arb_text()) {
        let _ = Agent::from_json(&text);
    }

    #[test]
    fn from_json_never_panics_on_arbitrary_json(value in arb_json()) {
        let _ = Agent::from_json(&value.to_string());
        let _ = Agent::from_json(&serde_json::to_string_pretty(&value).unwrap());
    }

    #[test]
    fn from_json_handles_weird_version_values(
        version in prop_oneof![
            any::<i64>().prop_map(Value::from),
            any::<u64>().prop_map(Value::from),
            arb_finite_f64().prop_map(Value::from),
            Just(json!(1.0)),
            Just(json!("1")),
            Just(json!(true)),
            Just(Value::Null),
            Just(json!([1])),
            Just(json!({"v": 1})),
            Just(json!(-1)),
            Just(json!(0)),
            Just(json!(u64::MAX)),
        ]
    ) {
        let mut value = session_value(&sample());
        value.insert("version".into(), version.clone());
        let result = Agent::from_json(&Value::Object(value).to_string());
        match version.as_u64() {
            Some(RECORD_VERSION) => prop_assert!(result.is_ok(), "{result:?}"),
            Some(1) => prop_assert!(matches!(result, Err(RecordError::NeedsMigration(1))), "{result:?}"),
            Some(other) => prop_assert!(matches!(result, Err(RecordError::UnsupportedVersion(v)) if v == other), "{result:?}"),
            None => {
                let expected = version.to_string();
                let invalid = matches!(&result, Err(RecordError::InvalidVersion(found)) if *found == expected);
                prop_assert!(invalid, "{} -> {:?}", version, result);
            }
        }
    }

    /// A random JSON value replacing one field: must not panic; only an exact-type value for
    /// that field (or null for the optional ones) may be accepted.
    #[test]
    fn from_json_with_one_field_replaced_never_panics(
        key in prop::sample::select(TOP_KEYS.to_vec()),
        replacement in arb_json(),
    ) {
        let mut value = session_value(&sample());
        value.insert(key.to_owned(), replacement);
        let _ = Agent::from_json(&Value::Object(value).to_string());
    }

}

#[test]
fn version_zero_is_reported_as_newer() {
    // Sharp edge: the message says "newer than this build supports" for version 0 too.
    let mut value = session_value(&sample());
    value.insert("version".into(), json!(0));
    let err = Agent::from_json(&Value::Object(value).to_string()).unwrap_err();
    assert!(matches!(err, RecordError::UnsupportedVersion(0)), "{err:?}");
    assert!(err.to_string().contains("not supported"), "{err}");
}

#[test]
fn from_json_requires_all_mandatory_fields() {
    for key in TOP_KEYS {
        let mut value = session_value(&sample());
        value.remove(key);
        let result = Agent::from_json(&Value::Object(value).to_string());
        match key {
            "version" => assert!(
                matches!(result, Err(RecordError::MissingVersion)),
                "{key}: {result:?}"
            ),
            // A missing Option field reads as None, same as an explicit null.
            "parent_id" => assert_eq!(result.unwrap().parent_id, None),
            "archived_at" => assert_eq!(result.unwrap().archived_at, None),
            "ext" => assert!(result.unwrap().ext.is_empty()),
            _ => assert!(
                matches!(result, Err(RecordError::Json(_))),
                "{key}: {result:?}"
            ),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// 7. Forward compatibility: unknown keys survive in `other`, and are written back
// ---------------------------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn extra_keys_are_kept_in_other_and_rewritten(
        session in arb_session(),
        top_extra in arb_extra_keys(&TOP_KEYS),
    ) {
        let mut value = session_value(&session);
        for (k, v) in &top_extra {
            value.insert(k.clone(), v.clone());
        }
        let text = Value::Object(value).to_string();
        let back = Agent::from_json(&text).unwrap();
        let mut expected = session.clone();
        expected.other = top_extra.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        prop_assert_eq!(&back, &expected);
        let rewritten = session_value(&back);
        for (k, v) in &top_extra {
            prop_assert_eq!(rewritten.get(k), Some(v), "{:?} lost on rewrite", k);
        }
        prop_assert_eq!(Agent::from_json(&back.to_json().unwrap()).unwrap(), back);
    }
}
