//! Property-based tests for `tx_core::session` and `tx_core::config`.
//!
//! Tests marked `#[ignore = "BUG: …"]` document real failures; they stay in the file so the
//! reproduction survives, and the suite stays green.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;

use proptest::prelude::*;

fn sid(id: &str) -> tx_core::SessionId {
    id.parse().unwrap()
}
use serde_json::{Map, Value, json};
use tx_core::config::{MAX_SOCKET_PATH_BYTES, SOCKET_FILE};
use tx_core::session::{MAX_NAME_BYTES, RECORD_VERSION};
use tx_core::{
    CoreConfig, CoreError, Effort, Launch, Lifecycle, RecordError, Session, SessionId,
    validate_name,
};

// ---------------------------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------------------------

fn arb_lifecycle() -> impl Strategy<Value = Lifecycle> {
    prop::sample::select(Lifecycle::ALL.to_vec())
}

fn arb_effort() -> impl Strategy<Value = Effort> {
    prop::sample::select(Effort::ALL.to_vec())
}

/// Any string, including control characters, NUL and astral-plane unicode.
fn arb_text() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => any::<String>(),
        1 => "[\\x00-\\x1f\\x7f]{0,8}",
        1 => "\\PC{0,16}",
        1 => Just(String::new()),
    ]
}

fn arb_session_id() -> impl Strategy<Value = SessionId> {
    prop_oneof![
        2 => "[A-Za-z0-9_-]{1,64}".prop_map(|s| sid(s.as_str())),
        1 => Just(SessionId::mint()),
    ]
}

fn arb_launch() -> impl Strategy<Value = Launch> {
    prop_oneof![
        (
            arb_text(),
            prop::collection::vec(arb_text(), 0..4),
            prop::option::of(arb_text()),
            prop::option::of(arb_effort()),
        )
            .prop_map(|(engine, profile, model, effort)| Launch::Agent {
                engine,
                profile,
                model,
                effort,
                resume: None,
            }),
        (arb_text(), prop::collection::vec(arb_text(), 0..4))
            .prop_map(|(program, args)| Launch::Process { program, args }),
    ]
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
    ended_at: impl Strategy<Value = Option<f64>>,
) -> impl Strategy<Value = Session> {
    (
        arb_session_id(),
        arb_text(),
        prop::option::of(arb_session_id()),
        prop::collection::vec(arb_text(), 0..4),
        arb_text().prop_map(PathBuf::from),
        created_at,
        ended_at,
        arb_lifecycle(),
        arb_launch(),
    )
        .prop_map(
            |(id, name, parent, tags, cwd, created_at, ended_at, lifecycle, launch)| Session {
                id,
                name,
                parent,
                tags,
                cwd,
                created_at,
                ended_at,
                lifecycle,
                launch,
            },
        )
}

fn arb_session() -> impl Strategy<Value = Session> {
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

fn session_value(session: &Session) -> Map<String, Value> {
    match serde_json::from_str(&session.to_json().unwrap()).unwrap() {
        Value::Object(map) => map,
        other => panic!("to_json produced a non-object: {other}"),
    }
}

const TOP_KEYS: [&str; 10] = [
    "version",
    "id",
    "name",
    "parent",
    "tags",
    "cwd",
    "created_at",
    "ended_at",
    "lifecycle",
    "launch",
];
const LAUNCH_KEYS: [&str; 7] = [
    "kind", "engine", "profile", "model", "effort", "program", "args",
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
// 1. Session JSON round trip
// ---------------------------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn session_round_trips_through_json(session in arb_session()) {
        let text = session.to_json().unwrap();
        prop_assert!(text.ends_with("}\n"), "{text:?}");
        prop_assert!(text.starts_with("{\n  \"version\": 1,"), "{text:?}");
        let back = Session::from_json(&text).unwrap();
        prop_assert_eq!(&back, &session);
        // PartialEq treats -0.0 == 0.0; make sure the sign bit survives too.
        prop_assert_eq!(back.created_at.to_bits(), session.created_at.to_bits());
        prop_assert_eq!(back.ended_at.map(f64::to_bits), session.ended_at.map(f64::to_bits));
    }

    #[test]
    fn to_json_is_stable_and_version_first(session in arb_session()) {
        let value = session_value(&session);
        prop_assert_eq!(value.keys().next().map(String::as_str), Some("version"));
        prop_assert_eq!(value.get("version"), Some(&json!(RECORD_VERSION)));
        prop_assert_eq!(value.get("lifecycle"), Some(&json!(session.lifecycle.as_str())));
        let kind = match &session.launch {
            Launch::Agent { .. } => "agent",
            Launch::Process { .. } => "process",
        };
        prop_assert_eq!(value["launch"].get("kind"), Some(&json!(kind)));
        if let Launch::Agent { effort, .. } = &session.launch {
            prop_assert_eq!(value["launch"].get("effort"), Some(&effort.map_or(Value::Null, |e| json!(e.as_str()))));
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

    /// `Some(NaN)` ended_at silently becomes `None` after a round trip.
    #[test]
    fn non_finite_ended_at_is_refused(session in arb_session_with(Just(1.0), arb_non_finite_f64().prop_map(Some))) {
        let refused = matches!(session.to_json(), Err(RecordError::NonFiniteTime("ended_at")));
        prop_assert!(refused);
    }
}

/// serde's `PathBuf` serializer errors on non-UTF-8 paths, and `to_json` unwraps that with
/// `expect("a session is always serializable")`. On unix, `cwd` can hold arbitrary bytes.
#[cfg(unix)]
#[test]
fn to_json_does_not_panic_on_non_utf8_cwd() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let mut session = Session::from_json(&sample().to_json().unwrap()).unwrap();
    session.cwd = PathBuf::from(OsStr::from_bytes(b"/tmp/\xff\xfe"));
    let result = std::panic::catch_unwind(|| session.to_json());
    assert!(
        matches!(result, Ok(Err(RecordError::Json(_)))),
        "to_json must refuse, not panic"
    );
}

fn sample() -> Session {
    Session {
        id: sid("x"),
        name: "shell".into(),
        parent: None,
        tags: vec![],
        cwd: PathBuf::from("/"),
        created_at: 1.0,
        ended_at: Some(2.0),
        lifecycle: Lifecycle::Stopped,
        launch: Launch::Process {
            program: "sh".into(),
            args: vec![],
        },
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
        let back = Session::from_json(&session.to_json().unwrap()).unwrap();
        prop_assert_eq!(back.name, name);
    }
}

// ---------------------------------------------------------------------------------------------
// 3. SessionId
// ---------------------------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn session_id_from_str_accepts_exactly_the_documented_set(text in arb_text()) {
        let expected_ok = !text.is_empty()
            && text.len() <= 64
            && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        let parsed = SessionId::from_str(&text);
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
        let back: SessionId = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, id);
    }

    /// `SessionId` deserialization is transparent, so a record on disk may carry an id that
    /// `from_str` would refuse (`""`, `"a/b"`, `".."`). Core only writes minted ids, but
    /// `from_json` does not enforce that.
    #[test]
    fn from_json_rejects_ids_from_str_rejects(text in arb_text().prop_filter("valid", |t| SessionId::from_str(t).is_err())) {
        for key in ["id", "parent"] {
            let mut value = session_value(&sample());
            value.insert(key.into(), Value::String(text.clone()));
            let rejected = matches!(Session::from_json(&Value::Object(value).to_string()), Err(RecordError::Json(_)));
            prop_assert!(rejected, "{} = {:?} was accepted", key, text);
        }
    }
}

#[test]
fn minted_ids_are_hyphenated_lowercase_uuid_v4() {
    for _ in 0..256 {
        let id = SessionId::mint();
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
        assert_eq!(SessionId::from_str(s).unwrap(), id);
        assert_eq!(id.to_string(), s);
    }
}

// ---------------------------------------------------------------------------------------------
// 4. Lifecycle / Effort
// ---------------------------------------------------------------------------------------------

#[test]
fn lifecycle_as_str_from_str_bijection_and_serde_names() {
    let mut seen = std::collections::HashSet::new();
    for lifecycle in Lifecycle::ALL {
        let name = lifecycle.as_str();
        assert!(seen.insert(name), "duplicate name {name}");
        assert_eq!(Lifecycle::from_str(name).unwrap(), lifecycle);
        assert_eq!(serde_json::to_value(lifecycle).unwrap(), json!(name));
        assert_eq!(
            serde_json::from_value::<Lifecycle>(json!(name)).unwrap(),
            lifecycle
        );
    }
    assert_eq!(seen.len(), Lifecycle::ALL.len());
}

#[test]
fn effort_as_str_from_str_bijection_and_serde_names() {
    let mut seen = std::collections::HashSet::new();
    for effort in Effort::ALL {
        let name = effort.as_str();
        assert!(seen.insert(name), "duplicate name {name}");
        assert_eq!(Effort::from_str(name).unwrap(), effort);
        assert_eq!(serde_json::to_value(effort).unwrap(), json!(name));
        assert_eq!(
            serde_json::from_value::<Effort>(json!(name)).unwrap(),
            effort
        );
    }
    assert_eq!(seen.len(), Effort::ALL.len());
    // ALL is in ascending order, matching the derived Ord.
    assert!(Effort::ALL.windows(2).all(|w| w[0] < w[1]));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn lifecycle_from_str_rejects_everything_else(text in arb_text()) {
        let known = Lifecycle::ALL.iter().any(|l| l.as_str() == text);
        prop_assert_eq!(Lifecycle::from_str(&text).is_ok(), known, "{:?}", text);
        prop_assert_eq!(serde_json::from_value::<Lifecycle>(Value::String(text.clone())).is_ok(), known, "{:?}", text);
    }

    #[test]
    fn effort_from_str_rejects_everything_else(text in arb_text()) {
        let known = Effort::ALL.iter().any(|e| e.as_str() == text);
        prop_assert_eq!(Effort::from_str(&text).is_ok(), known, "{:?}", text);
        prop_assert_eq!(serde_json::from_value::<Effort>(Value::String(text.clone())).is_ok(), known, "{:?}", text);
    }

    /// Case variants ("Running", "HIGH") are not accepted anywhere.
    #[test]
    fn enum_names_are_case_sensitive(lifecycle in arb_lifecycle(), effort in arb_effort()) {
        let upper = lifecycle.as_str().to_ascii_uppercase();
        prop_assert!(Lifecycle::from_str(&upper).is_err());
        let upper = effort.as_str().to_ascii_uppercase();
        prop_assert!(Effort::from_str(&upper).is_err());
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
        let mut config = CoreConfig::new(state_dir.clone());
        config.tmux_socket = tmux_socket.clone().map(PathBuf::from);
        let expected = match &tmux_socket {
            Some(s) => PathBuf::from(s),
            None => PathBuf::from(&state_dir).join(SOCKET_FILE),
        };
        prop_assert_eq!(config.socket_path(), expected);
        prop_assert_eq!(config.tmux_conf_path(), PathBuf::from(&state_dir).join("tmux.conf"));
    }

    #[test]
    fn check_socket_path_is_ok_iff_at_most_100_bytes(
        state_dir in prop_oneof![arb_text(), "/[a-z/]{0,120}", "/[\\PC]{0,60}"],
        tmux_socket in prop::option::of(prop_oneof![arb_text(), "/[a-z/]{0,120}", "/[\\PC]{0,60}"]),
    ) {
        let mut config = CoreConfig::new(state_dir);
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
        let mut config = CoreConfig::new("/x");
        if use_override {
            config.tmux_socket = Some(PathBuf::from(format!("/{}", "s".repeat(len - 1))));
        } else {
            let dir_len = len - 1 - SOCKET_FILE.len(); // "/" + dir + "/" + "tmux.sock"
            config = CoreConfig::new(format!("/{}", "d".repeat(dir_len - 1)));
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
        let _ = Session::from_json(&text);
    }

    #[test]
    fn from_json_never_panics_on_arbitrary_json(value in arb_json()) {
        let _ = Session::from_json(&value.to_string());
        let _ = Session::from_json(&serde_json::to_string_pretty(&value).unwrap());
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
        let result = Session::from_json(&Value::Object(value).to_string());
        match version.as_u64() {
            Some(RECORD_VERSION) => prop_assert!(result.is_ok(), "{result:?}"),
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
        let _ = Session::from_json(&Value::Object(value).to_string());
    }

    #[test]
    fn from_json_with_one_launch_field_replaced_never_panics(
        session in arb_session(),
        key in prop::sample::select(LAUNCH_KEYS.to_vec()),
        replacement in arb_json(),
    ) {
        let mut value = session_value(&session);
        value["launch"][key] = replacement;
        let _ = Session::from_json(&Value::Object(value).to_string());
    }
}

#[test]
fn version_zero_is_reported_as_newer() {
    // Sharp edge: the message says "newer than this build supports" for version 0 too.
    let mut value = session_value(&sample());
    value.insert("version".into(), json!(0));
    let err = Session::from_json(&Value::Object(value).to_string()).unwrap_err();
    assert!(matches!(err, RecordError::UnsupportedVersion(0)), "{err:?}");
    assert!(err.to_string().contains("not supported"), "{err}");
}

#[test]
fn from_json_requires_all_mandatory_fields() {
    for key in TOP_KEYS {
        let mut value = session_value(&sample());
        value.remove(key);
        let result = Session::from_json(&Value::Object(value).to_string());
        match key {
            "version" => assert!(
                matches!(result, Err(RecordError::MissingVersion)),
                "{key}: {result:?}"
            ),
            // A missing Option field reads as None, same as an explicit null.
            "parent" => assert_eq!(result.unwrap().parent, None),
            "ended_at" => assert_eq!(result.unwrap().ended_at, None),
            _ => assert!(
                matches!(result, Err(RecordError::Json(_))),
                "{key}: {result:?}"
            ),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// 7. Forward compatibility
// ---------------------------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn extra_keys_are_ignored(
        session in arb_session(),
        top_extra in arb_extra_keys(&TOP_KEYS),
        launch_extra in arb_extra_keys(&LAUNCH_KEYS),
    ) {
        let mut value = session_value(&session);
        for (k, v) in top_extra {
            value.insert(k, v);
        }
        let Some(Value::Object(launch)) = value.get_mut("launch") else {
            panic!("launch is not an object");
        };
        for (k, v) in launch_extra {
            launch.insert(k, v);
        }
        let text = Value::Object(value).to_string();
        let back = Session::from_json(&text).unwrap();
        prop_assert_eq!(back, session);
    }

    /// Keys from the *other* launch variant are extra keys too and must not confuse the tag.
    #[test]
    fn other_variant_keys_inside_launch_are_ignored(session in arb_session(), filler in arb_json()) {
        let mut value = session_value(&session);
        let foreign: &[&str] = match session.launch {
            Launch::Agent { .. } => &["program", "args"],
            Launch::Process { .. } => &["engine", "profile", "model", "effort"],
        };
        for key in foreign {
            value["launch"][*key] = filler.clone();
        }
        let back = Session::from_json(&Value::Object(value).to_string()).unwrap();
        prop_assert_eq!(back, session);
    }
}
