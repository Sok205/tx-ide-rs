//! Property-based and adversarial tests for `SessionStore`.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use proptest::prelude::*;
use tx_core::{Effort, Launch, Lifecycle, ReadError, RecordError, Session, SessionId, StoreError};

fn store() -> (tempfile::TempDir, tx_core::SessionStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = tx_core::SessionStore::new(dir.path().join("sessions"));
    (dir, store)
}

fn sid(id: &str) -> SessionId {
    id.parse().unwrap()
}

fn session(id: &str, created_at: f64) -> Session {
    Session {
        id: sid(id),
        name: format!("n-{id}"),
        parent: None,
        tags: vec![],
        cwd: PathBuf::from("/"),
        created_at,
        ended_at: None,
        last_modified_at: created_at,
        lifecycle: Lifecycle::Running,
        launch: Launch::Process {
            program: "sh".into(),
            args: vec![],
        },
        ext: std::collections::BTreeMap::new(),
        state: None,
    }
}

fn record_text(id: &str, created_at: f64) -> String {
    session(id, created_at).to_json().unwrap()
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

fn ids(sessions: &[Session]) -> Vec<String> {
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
        1 => Just(-0.0),
        1 => Just(0.0),
        1 => Just(f64::MIN_POSITIVE / 4.0),
        1 => Just(f64::MAX),
        1 => Just(1_727_000_000.123_456_7),
    ]
}

/// Ids inside the grammar, lowercase so two ids never collide on a case-insensitive filesystem.
fn lower_id() -> impl Strategy<Value = String> {
    "[a-z0-9_\\-]{1,64}"
}

fn text() -> impl Strategy<Value = String> {
    "\\PC{0,48}"
}

fn launch() -> impl Strategy<Value = Launch> {
    prop_oneof![
        (text(), prop::collection::vec(text(), 0..4))
            .prop_map(|(program, args)| { Launch::Process { program, args } }),
        (
            text(),
            prop::collection::vec(text(), 0..4),
            prop::option::of(text()),
            prop::option::of(prop::sample::select(Effort::ALL.to_vec())),
        )
            .prop_map(|(engine, profile, model, effort)| Launch::Agent {
                engine,
                profile,
                model,
                effort,
                resume: None,
            }),
    ]
}

fn arb_session(id: impl Strategy<Value = String>) -> impl Strategy<Value = Session> {
    (
        id,
        text(),
        prop::option::of("[A-Za-z0-9_\\-]{1,64}"),
        prop::collection::vec(text(), 0..6),
        "[^\0]{0,64}",
        finite_f64(),
        prop::option::of(finite_f64()),
        prop::sample::select(Lifecycle::ALL.to_vec()),
        launch(),
    )
        .prop_map(
            |(id, name, parent, tags, cwd, created_at, ended_at, lifecycle, launch)| Session {
                id: sid(&id),
                name,
                parent: parent.map(|p| sid(&p)),
                tags,
                cwd: PathBuf::from(cwd),
                created_at,
                ended_at,
                last_modified_at: created_at,
                lifecycle,
                launch,
                ext: std::collections::BTreeMap::new(),
                state: None,
            },
        )
}

/// Bitwise equality so `-0.0` vs `0.0` and the exact subnormal are checked, not just `==`.
fn assert_bitwise_eq(a: &Session, b: &Session) {
    assert_eq!(a, b);
    assert_eq!(
        a.created_at.to_bits(),
        b.created_at.to_bits(),
        "created_at bits"
    );
    assert_eq!(
        a.ended_at.map(f64::to_bits),
        b.ended_at.map(f64::to_bits),
        "ended_at bits"
    );
}

fn expected_order(sessions: &BTreeMap<String, Session>) -> Vec<String> {
    let mut v: Vec<&Session> = sessions.values().collect();
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
        prop_assert_eq!(entries(store.dir()), BTreeSet::from([format!("{}.json", session.id)]));
    }

    #[test]
    fn overwrite_replaces_whole_record(a in arb_session(Just("same".to_owned())),
                                       b in arb_session(Just("same".to_owned()))) {
        let (_dir, store) = store();
        store.save(&a).unwrap();
        store.save(&b).unwrap();
        assert_bitwise_eq(&b, &store.load(&a.id).unwrap().unwrap());
        prop_assert_eq!(entries(store.dir()), BTreeSet::from(["same.json".to_owned()]));
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
        s.ended_at = Some(value);
        let error = store.save(&s).unwrap_err();
        assert!(
            matches!(
                &error,
                StoreError::Encode {
                    source: RecordError::NonFiniteTime("ended_at"),
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
fn non_utf8_cwd_is_refused_by_save() {
    use std::os::unix::ffi::OsStrExt;
    let (_dir, store) = store();
    let mut s = session("cwd", 1.0);
    s.cwd = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/\xff"));
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
    std::fs::create_dir_all(store.dir()).unwrap();
    let text = record_text("x", 1.0).replace("\"created_at\": 1.0", "\"created_at\": null");
    assert!(text.contains("\"created_at\": null"), "{text}");
    std::fs::write(store.path(&sid("x")), text).unwrap();
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

    #[test]
    fn all_is_the_saved_set_in_total_order(ops in ops()) {
        let (_dir, store) = store();
        let mut model: BTreeMap<String, Session> = BTreeMap::new();
        for op in ops {
            match op {
                Op::Save(i, at) => {
                    let s = session(&format!("s{i}"), at);
                    store.save(&s).unwrap();
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
            let want: BTreeSet<String> = model.keys().map(|k| format!("{k}.json")).collect();
            prop_assert_eq!(entries(store.dir()), want);
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
        .flat_map(|w| (0..ROUNDS).map(move |r| format!("w{w}-{r}.json")))
        .collect();
    want.insert("shared.json".into());
    assert_eq!(entries(store.dir()), want);
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
    let left = entries(store.dir());
    assert!(
        left.is_subset(&BTreeSet::from(["x.json".to_owned()])),
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
        assert!(
            id.parse::<SessionId>().is_err(),
            "{id:?} should be rejected"
        );
        assert!(
            SessionId::try_from(id.to_owned()).is_err(),
            "{id:?} should be rejected"
        );
    }
    for ok in ["a", "-", "_", "A-b_0", &"z".repeat(64)] {
        assert_eq!(ok.parse::<SessionId>().unwrap().as_str(), ok);
    }
}

proptest! {
    #[test]
    fn grammar_is_exactly_safe_fragment(id in "[^\0]{0,70}") {
        let valid = !id.is_empty()
            && id.len() <= 64
            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        prop_assert_eq!(id.parse::<SessionId>().is_ok(), valid, "{:?}", id);
    }
}

#[test]
fn longest_id_saves_and_lists_with_room_for_the_temp_name() {
    let (_dir, store) = store();
    let id = "a".repeat(64);
    store.save(&session(&id, 1.0)).unwrap();
    assert_eq!(ids(&store.all().unwrap()), std::slice::from_ref(&id));
    assert_eq!(entries(store.dir()), BTreeSet::from([format!("{id}.json")]));
}

#[test]
fn record_with_out_of_grammar_id_on_disk_is_unreadable() {
    let (_dir, store) = store();
    std::fs::create_dir_all(store.dir()).unwrap();
    for bad in ["a/b", "..", "", "a b", ".h"] {
        let text = record_text("x", 1.0).replace(
            "\"id\": \"x\"",
            &format!("\"id\": {}", serde_json::json!(bad)),
        );
        std::fs::write(store.path(&sid("x")), text).unwrap();
        let error = store.load(&sid("x")).unwrap_err();
        assert!(
            matches!(&error, StoreError::Unreadable { file, source: ReadError::Record(RecordError::Json(_)) } if file == "x.json"),
            "{bad:?}: {error:?}"
        );
        let error = store.all().unwrap_err();
        assert!(
            matches!(&error, StoreError::Unreadable { file, .. } if file == "x.json"),
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
            matches!(&err, StoreError::Unreadable { file, source: ReadError::IdMismatch { found } } if file == "CaseId.json" && found == "caseid"),
            "{err:?}"
        );
        assert!(store.load(&sid("CaseId")).is_err());
        assert_eq!(store.load(&sid("caseid")).unwrap().unwrap().created_at, 2.0);
    } else {
        assert_eq!(store.all().unwrap().len(), 2);
    }
}

// ---- 5. corrupt / foreign files --------------------------------------------------------------

fn unreadable_named(result: Result<Vec<Session>, StoreError>, name: &str) -> ReadError {
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
        std::fs::write(store.dir().join("bad.json"), &bytes).unwrap();
        let source = unreadable_named(store.all(), "bad.json");
        let kind_ok = matches!(source, ReadError::Utf8(_) | ReadError::Record(_));
        prop_assert!(kind_ok, "{:?}", source);
        let error = store.load(&sid("bad")).unwrap_err();
        let named = matches!(error, StoreError::Unreadable { ref file, .. } if file == "bad.json");
        prop_assert!(named, "load: {:?}", error);
        prop_assert_eq!(store.load(&sid("ok")).unwrap(), Some(session("ok", 1.0)));
    }

    #[test]
    fn wrong_shape_json_is_unreadable(text in prop::sample::select(vec![
        "null",
        "[]",
        "{}",
        "{\"version\": 1}",
        "{\"version\": \"1\", \"id\": \"x\"}",
        "{\"version\": 1, \"id\": 5}",
        "{\"version\": -1}",
        "{\"version\": 1.5}",
        "{\"version\": 18446744073709551616}",
        "{\"version\": 1, \"id\": \"x\", \"name\": \"n\", \"parent\": null, \"tags\": [], \"cwd\": \"/\", \"created_at\": \"1\", \"ended_at\": null, \"lifecycle\": \"running\", \"launch\": {\"kind\": \"process\", \"program\": \"sh\", \"args\": []}}",
        "{\"version\": 1, \"id\": \"x\", \"name\": \"n\", \"parent\": null, \"tags\": [], \"cwd\": \"/\", \"created_at\": 1, \"ended_at\": null, \"lifecycle\": \"Running\", \"launch\": {\"kind\": \"process\", \"program\": \"sh\", \"args\": []}}",
        "{\"version\": 1, \"id\": \"x\", \"name\": \"n\", \"parent\": null, \"tags\": [], \"cwd\": \"/\", \"created_at\": 1, \"ended_at\": null, \"lifecycle\": \"running\", \"launch\": {\"kind\": \"shell\"}}",
        "{\"version\": 1, \"id\": \"x\", \"name\": \"n\", \"parent\": null, \"tags\": [], \"cwd\": \"/\", \"created_at\": 1, \"ended_at\": null, \"lifecycle\": \"running\", \"launch\": {\"kind\": \"agent\", \"engine\": \"e\", \"profile\": [], \"model\": null, \"effort\": \"ultra\"}}",
        "{\"version\": 1, \"id\": \"x\", \"name\": \"n\", \"parent\": \"p/q\", \"tags\": [], \"cwd\": \"/\", \"created_at\": 1, \"ended_at\": null, \"lifecycle\": \"running\", \"launch\": {\"kind\": \"process\", \"program\": \"sh\", \"args\": []}}",
        "{\"version\": 1, \"id\": \"x\", \"name\": \"n\", \"parent\": null, \"tags\": [], \"cwd\": \"/\", \"created_at\": 1, \"ended_at\": null, \"lifecycle\": \"running\", \"launch\": {\"kind\": \"process\", \"program\": \"sh\", \"args\": []}} trailing",
    ])) {
        let (_dir, store) = store();
        std::fs::create_dir_all(store.dir()).unwrap();
        std::fs::write(store.dir().join("bad.json"), text).unwrap();
        let source = unreadable_named(store.all(), "bad.json");
        let kind_ok = matches!(source, ReadError::Record(_));
        prop_assert!(kind_ok, "{:?}", source);
    }
}

#[test]
fn integer_created_at_is_accepted() {
    let (_dir, store) = store();
    std::fs::create_dir_all(store.dir()).unwrap();
    let text = record_text("x", 1.0).replace("\"created_at\": 1.0", "\"created_at\": 1");
    assert!(text.contains("\"created_at\": 1,"), "{text}");
    std::fs::write(store.path(&sid("x")), text).unwrap();
    assert_eq!(store.load(&sid("x")).unwrap().unwrap().created_at, 1.0);
}

#[test]
fn empty_file_is_unreadable_by_name() {
    let (_dir, store) = store();
    std::fs::create_dir_all(store.dir()).unwrap();
    std::fs::write(store.dir().join("empty.json"), b"").unwrap();
    let source = unreadable_named(store.all(), "empty.json");
    assert!(
        matches!(source, ReadError::Record(RecordError::Json(_))),
        "{source:?}"
    );
}

#[test]
fn symlink_to_dir_is_skipped() {
    let (_dir, store) = store();
    store.save(&session("a", 1.0)).unwrap();
    std::fs::create_dir(store.dir().join("real_dir")).unwrap();
    std::os::unix::fs::symlink(store.dir().join("real_dir"), store.dir().join("link.json"))
        .unwrap();
    assert_eq!(ids(&store.all().unwrap()), ["a"]);
}

#[test]
fn symlink_alias_of_a_record_is_unreadable_by_its_own_name() {
    let (_dir, store) = store();
    store.save(&session("a", 1.0)).unwrap();
    std::os::unix::fs::symlink(store.path(&sid("a")), store.dir().join("alias.json")).unwrap();
    let err = store.all().unwrap_err();
    assert!(
        matches!(&err, StoreError::Unreadable { file, .. } if file == "alias.json"),
        "{err:?}"
    );
}

#[test]
fn record_whose_id_disagrees_with_file_name_is_unreadable() {
    let (_dir, store) = store();
    std::fs::create_dir_all(store.dir()).unwrap();
    std::fs::write(store.path(&sid("file")), record_text("inner", 1.0)).unwrap();
    let err = store.load(&sid("file")).unwrap_err();
    assert!(
        matches!(&err, StoreError::Unreadable { file, .. } if file == "file.json"),
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
    let locked = store.dir().join("locked.json");
    std::fs::write(&locked, record_text("locked", 1.0)).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let source = unreadable_named(store.all(), "locked.json");
    assert!(
        matches!(&source, ReadError::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied),
        "{source:?}"
    );
    let error = store.load(&sid("locked")).unwrap_err();
    assert!(matches!(&error, StoreError::Unreadable { file, .. } if file == "locked.json"));
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(ids(&store.all().unwrap()), ["a", "locked"]);
}

#[test]
fn fifo_record_is_skipped_without_blocking() {
    let (_dir, store) = store();
    store.save(&session("a", 1.0)).unwrap();
    let fifo = store.dir().join("pipe.json");
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
fn non_utf8_file_name_is_skipped() {
    use std::os::unix::ffi::OsStrExt;
    let (_dir, store) = store();
    store.save(&session("a", 1.0)).unwrap();
    let name = std::ffi::OsStr::from_bytes(b"\xff\xfe.json");
    if std::fs::write(store.dir().join(name), "garbage").is_err() {
        return; // APFS refuses invalid UTF-8 names (EILSEQ); nothing to scan.
    }
    assert_eq!(ids(&store.all().unwrap()), ["a"]);
}

#[test]
fn files_without_json_suffix_hidden_files_and_dirs_are_ignored() {
    let (_dir, store) = store();
    store.save(&session("a", 1.0)).unwrap();
    std::fs::write(store.dir().join("a.json.bak"), "x").unwrap();
    std::fs::write(store.dir().join("b.JSON"), "x").unwrap();
    std::fs::write(store.dir().join("json"), "x").unwrap();
    std::fs::write(store.dir().join(".a.deadbeef.tmp"), "x").unwrap();
    std::fs::write(store.dir().join(".hidden.json"), "x").unwrap();
    std::fs::create_dir(store.dir().join("d.json")).unwrap();
    std::fs::create_dir(store.dir().join("plain")).unwrap();
    assert_eq!(ids(&store.all().unwrap()), ["a"]);
}

// ---- 6. unwritable directory -----------------------------------------------------------------

#[test]
fn unwritable_dir_fails_save_with_io_naming_the_dir_and_leaves_nothing() {
    if is_root() {
        return;
    }
    let (_dir, store) = store();
    store.save(&session("existing", 1.0)).unwrap();
    std::fs::set_permissions(store.dir(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let error = store.save(&session("a", 1.0)).unwrap_err();
    assert!(
        matches!(&error, StoreError::Io { path, .. } if path == store.dir()),
        "{error:?}"
    );
    let overwrite = store.save(&session("existing", 2.0)).unwrap_err();
    assert!(
        matches!(&overwrite, StoreError::Io { path, .. } if path == store.dir()),
        "{overwrite:?}"
    );
    let error = store.delete(&sid("existing")).unwrap_err();
    assert!(
        matches!(&error, StoreError::Io { path, .. } if *path == store.path(&sid("existing"))),
        "{error:?}"
    );
    assert_eq!(
        entries(store.dir()),
        BTreeSet::from(["existing.json".to_owned()])
    );
    assert_eq!(store.all().unwrap()[0].created_at, 1.0);
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
        matches!(&error, StoreError::Unreadable { file, source: ReadError::Io(_) } if file == "a.json"),
        "{error:?}"
    );
    std::fs::set_permissions(store.dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn save_creates_missing_parent_directories() {
    let dir = tempfile::tempdir().unwrap();
    let store = tx_core::SessionStore::new(dir.path().join("a/b/c"));
    assert!(store.all().unwrap().is_empty());
    assert_eq!(store.load(&sid("x")).unwrap(), None);
    assert!(!store.delete(&sid("x")).unwrap());
    store.save(&session("x", 1.0)).unwrap();
    assert_eq!(ids(&store.all().unwrap()), ["x"]);
}
