//! Property-based and adversarial tests for `AgentStore`: `<dir>/<id>/agent.json`.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use proptest::prelude::*;
use tx_core::store::RECORD_FILE;
use tx_core::{Agent, AgentId, EngineKind, LlmEngineState, ReadError, RecordError, StoreError};

fn store() -> (tempfile::TempDir, tx_core::AgentStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = tx_core::AgentStore::new(dir.path().join("agents"));
    (dir, store)
}

fn sid(id: &str) -> AgentId {
    id.parse().unwrap()
}

fn session(id: &str, created_at: f64) -> Agent {
    Agent {
        id: sid(id),
        name: format!("n-{id}"),
        engine: EngineKind::Claude,
        working_directory: PathBuf::from("/"),
        roles: vec![],
        state: LlmEngineState::Idle,
        created_at,
        last_modified_at: created_at,
        archived_at: None,
        tags: vec![],
        parent_id: None,
        ext: std::collections::BTreeMap::new(),
        other: serde_json::Map::new(),
    }
}

fn record_text(id: &str, created_at: f64) -> String {
    session(id, created_at).to_json().unwrap()
}

/// `<id>/agent.json`: how the store names a record in its errors.
fn record_name(id: &str) -> String {
    format!("{id}/{RECORD_FILE}")
}

/// Writes `text` where the record of `id` belongs, creating the agent's directory.
fn plant(store: &tx_core::AgentStore, id: &str, text: impl AsRef<[u8]>) {
    std::fs::create_dir_all(store.agent_dir(&sid(id))).unwrap();
    std::fs::write(store.path(&sid(id)), text).unwrap();
}

fn entries(dir: &Path) -> BTreeSet<String> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
        Err(error) => panic!("{error}"),
    }
}

fn ids(sessions: &[Agent]) -> Vec<String> {
    sessions.iter().map(|s| s.id.to_string()).collect()
}

fn is_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
}

// ---- strategies ------------------------------------------------------------------------------

fn finite_f64() -> impl Strategy<Value = f64> {
    use proptest::num::f64::{NEGATIVE, NORMAL, POSITIVE, SUBNORMAL, ZERO};
    prop_oneof![
        5 => POSITIVE | NEGATIVE | NORMAL | SUBNORMAL | ZERO,
        1 => Just(0.0),
        1 => Just(-0.0),
        1 => Just(f64::MIN_POSITIVE),
        1 => Just(f64::from_bits(1)),
        1 => Just(f64::MAX),
        1 => Just(f64::MIN),
    ]
}

fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => "[^\0]{0,32}",
        1 => any::<String>(),
        1 => Just(String::new()),
    ]
}

/// Lower-case ids only: APFS and NTFS fold case, so `A` and `a` would share one directory.
fn lower_id() -> impl Strategy<Value = String> {
    "[a-z0-9_\\-]{1,64}"
}

fn engine() -> impl Strategy<Value = EngineKind> {
    prop::sample::select(EngineKind::ALL.to_vec())
}

fn state() -> impl Strategy<Value = LlmEngineState> {
    prop::sample::select(LlmEngineState::ALL.to_vec())
}

fn arb_session(id: impl Strategy<Value = String>) -> impl Strategy<Value = Agent> {
    (
        id,
        text(),
        prop::option::of("[A-Za-z0-9_\\-]{1,64}"),
        prop::collection::vec(text(), 0..6),
        "[^\0]{0,64}",
        finite_f64(),
        prop::option::of(finite_f64()),
        engine(),
        prop::collection::vec(text(), 0..4),
        state(),
    )
        .prop_map(
            |(id, name, parent, tags, cwd, created_at, archived_at, engine, roles, state)| Agent {
                id: sid(&id),
                name,
                engine,
                working_directory: PathBuf::from(cwd),
                roles,
                state,
                created_at,
                last_modified_at: created_at,
                archived_at,
                tags,
                parent_id: parent.map(|p| sid(&p)),
                ext: std::collections::BTreeMap::new(),
                other: serde_json::Map::new(),
            },
        )
}

/// Bitwise equality so `-0.0` vs `0.0` and the exact subnormal are checked, not just `==`.
fn assert_bitwise_eq(a: &Agent, b: &Agent) {
    assert_eq!(a, b);
    assert_eq!(
        a.created_at.to_bits(),
        b.created_at.to_bits(),
        "created_at bits"
    );
    assert_eq!(
        a.archived_at.map(f64::to_bits),
        b.archived_at.map(f64::to_bits),
        "archived_at bits"
    );
}

fn expected_order(sessions: &BTreeMap<String, Agent>) -> Vec<String> {
    let mut v: Vec<&Agent> = sessions.values().collect();
    v.sort_by(|a, b| {
        a.created_at
            .total_cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    v.iter().map(|s| s.id.to_string()).collect()
}

// ---- 1. round trip ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn save_load_round_trips_bitwise(session in arb_session(lower_id())) {
        let (_dir, store) = store();
        store.save(&session).unwrap();
        let loaded = store.load(&session.id).unwrap().expect("record exists");
        assert_bitwise_eq(&session, &loaded);
        let all = store.all().unwrap();
        prop_assert_eq!(all.len(), 1);
        assert_bitwise_eq(&session, &all[0]);
        prop_assert_eq!(entries(store.dir()), BTreeSet::from([session.id.to_string()]));
        prop_assert_eq!(entries(&store.agent_dir(&session.id)), BTreeSet::from([RECORD_FILE.to_owned()]));
    }

    #[test]
    fn overwrite_replaces_whole_record(a in arb_session(Just("same".to_owned())),
                                       b in arb_session(Just("same".to_owned()))) {
        let (_dir, store) = store();
        store.save(&a).unwrap();
        store.save(&b).unwrap();
        assert_bitwise_eq(&b, &store.load(&a.id).unwrap().unwrap());
        prop_assert_eq!(entries(store.dir()), BTreeSet::from(["same".to_owned()]));
        prop_assert_eq!(entries(&store.agent_dir(&a.id)), BTreeSet::from([RECORD_FILE.to_owned()]));
    }
}

#[test]
fn non_finite_timestamps_are_refused_by_save_without_touching_disk() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let (_dir, store) = store();
        let s = session("nf", value);
        let error = store.save(&s).unwrap_err();
        assert!(
            matches!(&error, StoreError::Encode { id, source: RecordError::NonFiniteTime("created_at") } if id.as_str() == "nf"),
            "{error:?}"
        );
        let mut s = session("nf", 1.0);
        s.archived_at = Some(value);
        let error = store.save(&s).unwrap_err();
        assert!(
            matches!(
                &error,
                StoreError::Encode {
                    source: RecordError::NonFiniteTime("archived_at"),
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(
            !store.dir().exists(),
            "save must not even create the dir on Encode"
        );
        assert_eq!(store.load(&s.id).unwrap(), None);
        assert!(store.all().unwrap().is_empty());
    }
}

#[test]
fn non_utf8_working_directory_is_refused_by_save() {
    use std::os::unix::ffi::OsStrExt;
    let (_dir, store) = store();
    let mut s = session("cwd", 1.0);
    s.working_directory = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/\xff"));
    let error = store.save(&s).unwrap_err();
    assert!(
        matches!(
            &error,
            StoreError::Encode {
                source: RecordError::Json(_),
                ..
            }
        ),
        "{error:?}"
    );
    assert!(!store.dir().exists());
}

#[test]
fn null_created_at_on_disk_is_unreadable() {
    let (_dir, store) = store();
    let text = record_text("x", 1.0).replace("\"created_at\": 1.0", "\"created_at\": null");
    assert!(text.contains("\"created_at\": null"), "{text}");
    plant(&store, "x", text);
    let error = store.load(&sid("x")).unwrap_err();
    assert!(
        matches!(
            &error,
            StoreError::Unreadable {
                source: ReadError::Record(RecordError::Json(_)),
                ..
            }
        ),
        "{error:?}"
    );
}

// ---- 2. all(): set semantics + order ---------------------------------------------------------

#[derive(Clone, Debug)]
enum Op {
    Save(usize, f64),
    Delete(usize),
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    let at = prop_oneof![Just(1.0), Just(2.0), Just(-0.0), Just(0.0), finite_f64()];
    prop::collection::vec(
        prop_oneof![
            3 => (0..12usize, at).prop_map(|(i, at)| Op::Save(i, at)),
            1 => (0..12usize).prop_map(Op::Delete),
        ],
        0..40,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// The agent directory outlives its record (the engine's archive lives in it), so the set
    /// of directories only grows while the set of records is exactly what was saved and not
    /// deleted.
    #[test]
    fn all_is_the_saved_set_in_total_order(ops in ops()) {
        let (_dir, store) = store();
        let mut model: BTreeMap<String, Agent> = BTreeMap::new();
        let mut touched: BTreeSet<String> = BTreeSet::new();
        for op in ops {
            match op {
                Op::Save(i, at) => {
                    let s = session(&format!("s{i}"), at);
                    store.save(&s).unwrap();
                    touched.insert(s.id.to_string());
                    model.insert(s.id.to_string(), s);
                }
                Op::Delete(i) => {
                    let id = sid(&format!("s{i}"));
                    let existed = store.delete(&id).unwrap();
                    prop_assert_eq!(existed, model.remove(id.as_str()).is_some());
                }
            }
            let all = store.all().unwrap();
            prop_assert_eq!(ids(&all), expected_order(&model));
            for s in &all {
                assert_bitwise_eq(s, &model[s.id.as_str()]);
            }
            prop_assert_eq!(entries(store.dir()), touched.clone());
        }
    }

    #[test]
    fn duplicate_created_at_ties_break_on_id(n in 1..10usize, at in finite_f64()) {
        let (_dir, store) = store();
        let mut want = Vec::new();
        for i in (0..n).rev() {
            let s = session(&format!("id{i}"), at);
            store.save(&s).unwrap();
            want.push(s.id.to_string());
        }
        want.sort();
        prop_assert_eq!(ids(&store.all().unwrap()), want);
    }
}

#[test]
fn negative_zero_sorts_before_positive_zero() {
    let (_dir, store) = store();
    store.save(&session("a", 0.0)).unwrap();
    store.save(&session("b", -0.0)).unwrap();
    assert_eq!(ids(&store.all().unwrap()), ["b", "a"]);
}

#[test]
fn ids_sort_bytewise() {
    let (_dir, store) = store();
    for id in ["b", "Z", "_", "-", "0"] {
        store.save(&session(id, 1.0)).unwrap();
    }
    assert_eq!(ids(&store.all().unwrap()), ["-", "0", "Z", "_", "b"]);
}

// ---- 3. concurrency --------------------------------------------------------------------------

#[test]
fn concurrent_writers_never_expose_partial_or_temp_records() {
    const WRITERS: usize = 8;
    const ROUNDS: usize = 60;
    let (_dir, store) = store();
    let store = Arc::new(store);
    let done = Arc::new(AtomicBool::new(false));
    let big_tags: Vec<String> = (0..64)
        .map(|i| format!("tag-{i}-{}", "x".repeat(64)))
        .collect();

    let readers: Vec<_> = (0..2)
        .map(|_| {
            let store = Arc::clone(&store);
            let done = Arc::clone(&done);
            let big_tags = big_tags.clone();
            std::thread::spawn(move || {
                let mut seen = 0usize;
                while !done.load(Ordering::Relaxed) {
                    let all = store.all().expect("all() during writes");
                    for s in &all {
                        assert_eq!(s.tags, big_tags, "partial record {}", s.id);
                    }
                    seen = seen.max(all.len());
                    let shared = store.load(&sid("shared")).expect("load() during writes");
                    if let Some(s) = shared {
                        assert_eq!(s.tags, big_tags);
                    }
                }
                seen
            })
        })
        .collect();

    let writers: Vec<_> = (0..WRITERS)
        .map(|w| {
            let store = Arc::clone(&store);
            let big_tags = big_tags.clone();
            std::thread::spawn(move || {
                for r in 0..ROUNDS {
                    let mut s = session(&format!("w{w}-{r}"), r as f64);
                    s.tags = big_tags.clone();
                    store.save(&s).unwrap();
                    let mut shared = session("shared", (w * ROUNDS + r) as f64);
                    shared.tags = big_tags.clone();
                    store.save(&shared).unwrap();
                }
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }
    done.store(true, Ordering::Relaxed);
    for r in readers {
        r.join().unwrap();
    }

    let mut want: BTreeSet<String> = (0..WRITERS)
        .flat_map(|w| (0..ROUNDS).map(move |r| format!("w{w}-{r}")))
        .collect();
    want.insert("shared".into());
    assert_eq!(entries(store.dir()), want);
    assert_eq!(
        entries(&store.agent_dir(&sid("shared"))),
        BTreeSet::from([RECORD_FILE.to_owned()]),
        "no temp file left behind"
    );
    assert_eq!(store.all().unwrap().len(), WRITERS * ROUNDS + 1);
}

#[test]
fn concurrent_save_and_delete_of_one_id_leave_no_temp_files() {
    let (_dir, store) = store();
    let store = Arc::new(store);
    let handles: Vec<_> = (0..6)
        .map(|t| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                for i in 0..100 {
                    if (t + i) % 3 == 0 {
                        store.delete(&sid("x")).unwrap();
                    } else {
                        store.save(&session("x", i as f64)).unwrap();
                    }
                    let _ = store.all().unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let left = entries(&store.agent_dir(&sid("x")));
    assert!(
        left.is_subset(&BTreeSet::from([RECORD_FILE.to_owned()])),
        "{left:?}"
    );
}

// ---- 4. weird ids ----------------------------------------------------------------------------

#[test]
fn ids_outside_the_grammar_cannot_be_built() {
    let bad = [
        "",
        ".",
        "..",
        "a/b",
        "../x",
        "./../x",
        "/abs",
        ".hidden",
        "a b",
        "a\nb",
        "a\tb",
        "a.json",
        "a.b",
        "a:b",
        "a\\b",
        "ä",
        "😀",
        "a\0b",
        &"a".repeat(65),
        &"a".repeat(300),
    ];
    for id in bad {
        assert!(id.parse::<AgentId>().is_err(), "{id:?} should be rejected");
        assert!(
            AgentId::try_from(id.to_owned()).is_err(),
            "{id:?} should be rejected"
        );
    }
    for ok in ["a", "-", "_", "A-b_0", &"z".repeat(64)] {
        assert_eq!(ok.parse::<AgentId>().unwrap().as_str(), ok);
    }
}

proptest! {
    #[test]
    fn grammar_is_exactly_safe_fragment(id in "[^\0]{0,70}") {
        let valid = !id.is_empty()
            && id.len() <= 64
            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        prop_assert_eq!(id.parse::<AgentId>().is_ok(), valid, "{:?}", id);
    }
}

#[test]
fn longest_id_saves_and_lists_with_room_for_the_temp_name() {
    let (_dir, store) = store();
    let id = "a".repeat(64);
    store.save(&session(&id, 1.0)).unwrap();
    assert_eq!(ids(&store.all().unwrap()), std::slice::from_ref(&id));
    assert_eq!(entries(store.dir()), BTreeSet::from([id]));
}

#[test]
fn record_with_out_of_grammar_id_on_disk_is_unreadable() {
    let (_dir, store) = store();
    for bad in ["a/b", "..", "", "a b", ".h"] {
        let text = record_text("x", 1.0).replace(
            "\"id\": \"x\"",
            &format!("\"id\": {}", serde_json::json!(bad)),
        );
        plant(&store, "x", text);
        let error = store.load(&sid("x")).unwrap_err();
        assert!(
            matches!(&error, StoreError::Unreadable { file, source: ReadError::Record(RecordError::Json(_)) } if *file == record_name("x")),
            "{bad:?}: {error:?}"
        );
        let error = store.all().unwrap_err();
        assert!(
            matches!(&error, StoreError::Unreadable { file, .. } if *file == record_name("x")),
            "{bad:?}: {error:?}"
        );
    }
}

#[test]
fn case_variant_ids_may_collide_on_case_insensitive_fs() {
    let (_dir, store) = store();
    store.save(&session("CaseId", 1.0)).unwrap();
    store.save(&session("caseid", 2.0)).unwrap();
    let insensitive = entries(store.dir()).len() == 1;
    if insensitive {
        // Sharp edge: the grammar allows both cases, so on APFS/NTFS the second save overwrote
        // the first through the same directory entry; the store reports the corruption instead
        // of answering with the wrong record.
        let err = store.all().unwrap_err();
        assert!(
            matches!(&err, StoreError::Unreadable { file, source: ReadError::IdMismatch { found } } if *file == record_name("CaseId") && found == "caseid"),
            "{err:?}"
        );
        assert!(store.load(&sid("CaseId")).is_err());
        assert_eq!(store.load(&sid("caseid")).unwrap().unwrap().created_at, 2.0);
    } else {
        assert_eq!(store.all().unwrap().len(), 2);
    }
}

// ---- 5. corrupt / foreign files --------------------------------------------------------------

fn unreadable_named(result: Result<Vec<Agent>, StoreError>, name: &str) -> ReadError {
    match result {
        Err(StoreError::Unreadable { file, source }) if file == name => source,
        other => panic!("expected Unreadable {name}, got {other:?}"),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn random_bytes_record_is_unreadable_by_name(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let (_dir, store) = store();
        store.save(&session("ok", 1.0)).unwrap();
        plant(&store, "bad", &bytes);
        let source = unreadable_named(store.all(), &record_name("bad"));
        let kind_ok = matches!(source, ReadError::Utf8(_) | ReadError::Record(_));
        prop_assert!(kind_ok, "{:?}", source);
        let error = store.load(&sid("bad")).unwrap_err();
        let named = matches!(error, StoreError::Unreadable { ref file, .. } if *file == record_name("bad"));
        prop_assert!(named, "load: {:?}", error);
        prop_assert_eq!(store.load(&sid("ok")).unwrap(), Some(session("ok", 1.0)));
    }

    /// Wrong shapes, version-1 records included: all a `RecordError`, never a panic or a skip.
    #[test]
    fn wrong_shape_json_is_unreadable(text in prop::sample::select(vec![
        "null",
        "[]",
        "{}",
        "{\"version\": 2}",
        "{\"version\": \"2\", \"id\": \"x\"}",
        "{\"version\": 2, \"id\": 5}",
        "{\"version\": -1}",
        "{\"version\": 1.5}",
        "{\"version\": 18446744073709551616}",
        "{\"version\": 1, \"id\": \"bad\", \"name\": \"n\", \"parent\": null, \"tags\": [], \"cwd\": \"/\", \"created_at\": 1, \"ended_at\": null, \"lifecycle\": \"running\", \"launch\": {\"kind\": \"agent\", \"engine\": \"claude\", \"profile\": []}}",
        "{\"version\": 1, \"id\": \"bad\", \"name\": \"n\", \"parent\": null, \"tags\": [], \"cwd\": \"/\", \"created_at\": 1, \"ended_at\": null, \"lifecycle\": \"running\", \"launch\": {\"kind\": \"process\", \"program\": \"sh\"}}",
        "{\"version\": 2, \"id\": \"bad\", \"name\": \"n\", \"engine\": \"claude\", \"working_directory\": \"/\", \"roles\": [], \"state\": \"idle\", \"created_at\": \"1\", \"last_modified_at\": 1, \"tags\": []}",
        "{\"version\": 2, \"id\": \"bad\", \"name\": \"n\", \"engine\": \"claude\", \"working_directory\": \"/\", \"roles\": [], \"state\": \"Idle\", \"created_at\": 1, \"last_modified_at\": 1, \"tags\": []}",
        "{\"version\": 2, \"id\": \"bad\", \"name\": \"n\", \"engine\": 7, \"working_directory\": \"/\", \"roles\": [], \"state\": \"idle\", \"created_at\": 1, \"last_modified_at\": 1, \"tags\": []}",
        "{\"version\": 2, \"id\": \"bad\", \"name\": \"n\", \"engine\": \"claude\", \"working_directory\": \"/\", \"roles\": [], \"state\": \"idle\", \"created_at\": 1, \"last_modified_at\": 1, \"tags\": [], \"parent_id\": \"p/q\"}",
        "{\"version\": 2, \"id\": \"bad\", \"name\": \"n\", \"engine\": \"claude\", \"working_directory\": \"/\", \"roles\": [], \"state\": \"idle\", \"created_at\": 1, \"last_modified_at\": 1, \"tags\": []} trailing",
    ])) {
        let (_dir, store) = store();
        plant(&store, "bad", text);
        let source = unreadable_named(store.all(), &record_name("bad"));
        let kind_ok = matches!(source, ReadError::Record(_));
        prop_assert!(kind_ok, "{:?}", source);
    }
}

/// A version-1 record where a version-2 one belongs is refused with the pointer to
/// `txc migrate`, whatever else it says, and never skipped.
#[test]
fn version_one_records_need_migration() {
    let (_dir, store) = store();
    store.save(&session("a", 1.0)).unwrap();
    let agent = "{\"version\": 1, \"id\": \"p\", \"name\": \"n\", \"parent\": null, \"tags\": [], \"cwd\": \"/\", \"created_at\": 1, \"ended_at\": null, \"lifecycle\": \"running\", \"launch\": {\"kind\": \"agent\", \"engine\": \"claude\", \"profile\": []}}";
    plant(&store, "p", agent);
    let odd = "{\"version\": 1, \"id\": \"q\", \"name\": \"n\", \"created_at\": \"1\", \"lifecycle\": \"Running\", \"launch\": {\"kind\": \"process\"}}";
    plant(&store, "q", odd);
    for id in ["p", "q"] {
        let error = store.load(&sid(id)).unwrap_err();
        assert!(
            matches!(&error, StoreError::Unreadable { file, source: ReadError::Record(RecordError::NeedsMigration(1)) } if *file == record_name(id)),
            "{error:?}"
        );
    }
    assert!(store.all().is_err());
    assert!(store.skipped().is_err());
}

/// A record naming an engine this build lacks is skipped and named, not an error.
#[test]
fn unknown_engine_records_are_skipped_and_named() {
    let (_dir, store) = store();
    store.save(&session("a", 1.0)).unwrap();
    plant(
        &store,
        "k",
        record_text("k", 2.0).replace("\"claude\"", "\"kodex\""),
    );
    assert_eq!(ids(&store.all().unwrap()), ["a"]);
    let skipped: Vec<_> = store
        .skipped()
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(skipped, [sid("k")]);
    assert_eq!(store.load(&sid("k")).unwrap(), None);
}

#[test]
fn integer_created_at_is_accepted() {
    let (_dir, store) = store();
    let text = record_text("x", 1.0).replace("\"created_at\": 1.0", "\"created_at\": 1");
    assert!(text.contains("\"created_at\": 1,"), "{text}");
    plant(&store, "x", text);
    assert_eq!(store.load(&sid("x")).unwrap().unwrap().created_at, 1.0);
}

#[test]
fn empty_file_is_unreadable_by_name() {
    let (_dir, store) = store();
    plant(&store, "empty", b"");
    let source = unreadable_named(store.all(), &record_name("empty"));
    assert!(
        matches!(source, ReadError::Record(RecordError::Json(_))),
        "{source:?}"
    );
}

#[test]
fn record_that_is_a_directory_is_skipped() {
    let (_dir, store) = store();
    store.save(&session("a", 1.0)).unwrap();
    std::fs::create_dir_all(store.path(&sid("link"))).unwrap();
    assert_eq!(ids(&store.all().unwrap()), ["a"]);
    assert!(store.load(&sid("link")).is_err());
}

#[test]
fn symlink_alias_of_an_agent_directory_is_unreadable_by_its_own_name() {
    let (_dir, store) = store();
    store.save(&session("a", 1.0)).unwrap();
    std::os::unix::fs::symlink(store.agent_dir(&sid("a")), store.dir().join("alias")).unwrap();
    let err = store.all().unwrap_err();
    assert!(
        matches!(&err, StoreError::Unreadable { file, .. } if *file == record_name("alias")),
        "{err:?}"
    );
}

#[test]
fn record_whose_id_disagrees_with_its_directory_is_unreadable() {
    let (_dir, store) = store();
    plant(&store, "file", record_text("inner", 1.0));
    let err = store.load(&sid("file")).unwrap_err();
    assert!(
        matches!(&err, StoreError::Unreadable { file, .. } if *file == record_name("file")),
        "{err:?}"
    );
    assert!(store.all().is_err());
    assert_eq!(store.load(&sid("inner")).unwrap(), None);
}

#[test]
fn unreadable_permissions_are_reported_by_name() {
    if is_root() {
        return;
    }
    let (_dir, store) = store();
    store.save(&session("a", 1.0)).unwrap();
    plant(&store, "locked", record_text("locked", 1.0));
    let locked = store.path(&sid("locked"));
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let source = unreadable_named(store.all(), &record_name("locked"));
    assert!(
        matches!(&source, ReadError::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied),
        "{source:?}"
    );
    let error = store.load(&sid("locked")).unwrap_err();
    assert!(
        matches!(&error, StoreError::Unreadable { file, .. } if *file == record_name("locked"))
    );
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(ids(&store.all().unwrap()), ["a", "locked"]);
}

#[test]
fn fifo_record_is_skipped_without_blocking() {
    let (_dir, store) = store();
    store.save(&session("a", 1.0)).unwrap();
    std::fs::create_dir_all(store.agent_dir(&sid("pipe"))).unwrap();
    let fifo = store.path(&sid("pipe"));
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let (tx, rx) = mpsc::channel();
    let scanner = store.clone();
    std::thread::spawn(move || {
        tx.send((
            ids(&scanner.all().unwrap()),
            scanner.load(&sid("pipe")).is_err(),
        ))
        .unwrap()
    });
    let (listed, load_failed) = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("all() hung on the FIFO");
    assert_eq!(listed, ["a"]);
    assert!(load_failed);
}

#[test]
fn non_utf8_directory_name_is_skipped() {
    use std::os::unix::ffi::OsStrExt;
    let (_dir, store) = store();
    store.save(&session("a", 1.0)).unwrap();
    let name = std::ffi::OsStr::from_bytes(b"\xff\xfe");
    if std::fs::create_dir(store.dir().join(name)).is_err() {
        return; // APFS refuses invalid UTF-8 names (EILSEQ); nothing to scan.
    }
    std::fs::write(store.dir().join(name).join(RECORD_FILE), "garbage").unwrap();
    assert_eq!(ids(&store.all().unwrap()), ["a"]);
}

#[test]
fn files_hidden_entries_and_foreign_dirs_are_ignored() {
    let (_dir, store) = store();
    store.save(&session("a", 1.0)).unwrap();
    std::fs::write(store.dir().join("a.json"), "x").unwrap();
    std::fs::write(store.dir().join("b"), "x").unwrap();
    std::fs::write(store.dir().join(".hidden"), "x").unwrap();
    std::fs::create_dir(store.dir().join(".hidden-dir")).unwrap();
    std::fs::write(store.dir().join(".hidden-dir").join(RECORD_FILE), "x").unwrap();
    std::fs::create_dir(store.dir().join("not an id")).unwrap();
    std::fs::write(store.dir().join("not an id").join(RECORD_FILE), "x").unwrap();
    std::fs::create_dir(store.dir().join("empty")).unwrap();
    std::fs::write(store.agent_dir(&sid("a")).join(".agent.deadbeef.tmp"), "x").unwrap();
    assert_eq!(ids(&store.all().unwrap()), ["a"]);
}

// ---- 6. unwritable directory -----------------------------------------------------------------

#[test]
fn unwritable_dir_fails_save_with_io_and_leaves_nothing() {
    if is_root() {
        return;
    }
    let (_dir, store) = store();
    store.save(&session("existing", 1.0)).unwrap();
    std::fs::set_permissions(store.dir(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let error = store.save(&session("a", 1.0)).unwrap_err();
    assert!(
        matches!(&error, StoreError::Io { path, .. } if *path == store.agent_dir(&sid("a"))),
        "{error:?}"
    );
    let existing = store.agent_dir(&sid("existing"));
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o500)).unwrap();
    let overwrite = store.save(&session("existing", 2.0)).unwrap_err();
    assert!(
        matches!(&overwrite, StoreError::Io { path, .. } if *path == existing),
        "{overwrite:?}"
    );
    let error = store.delete(&sid("existing")).unwrap_err();
    assert!(
        matches!(&error, StoreError::Io { path, .. } if *path == store.path(&sid("existing"))),
        "{error:?}"
    );
    assert_eq!(
        entries(store.dir()),
        BTreeSet::from(["existing".to_owned()])
    );
    assert_eq!(entries(&existing), BTreeSet::from([RECORD_FILE.to_owned()]));
    assert_eq!(store.all().unwrap()[0].created_at, 1.0);
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(store.dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn unlistable_dir_fails_all_with_io_naming_the_dir() {
    if is_root() {
        return;
    }
    let (_dir, store) = store();
    store.save(&session("a", 1.0)).unwrap();
    std::fs::set_permissions(store.dir(), std::fs::Permissions::from_mode(0o000)).unwrap();
    let error = store.all().unwrap_err();
    assert!(
        matches!(&error, StoreError::Io { path, .. } if path == store.dir()),
        "{error:?}"
    );
    let error = store.load(&sid("a")).unwrap_err();
    assert!(
        matches!(&error, StoreError::Unreadable { file, source: ReadError::Io(_) } if *file == record_name("a")),
        "{error:?}"
    );
    std::fs::set_permissions(store.dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn save_creates_missing_parent_directories() {
    let dir = tempfile::tempdir().unwrap();
    let store = tx_core::AgentStore::new(dir.path().join("a/b/c"));
    assert!(store.all().unwrap().is_empty());
    assert_eq!(store.load(&sid("x")).unwrap(), None);
    assert!(!store.delete(&sid("x")).unwrap());
    store.save(&session("x", 1.0)).unwrap();
    assert_eq!(ids(&store.all().unwrap()), ["x"]);
}
