//! Property-based and adversarial tests for `tx_core::events`.
//!
//! Tests marked `#[ignore = "BUG: …"]` document real failures; they stay in the file so the
//! reproduction survives, and the suite stays green.

use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use proptest::prelude::*;
use serde_json::Value;
use tx_core::events::{PIPE_BUF, encode, now};
use tx_core::{EventLog, EventType};

// ---------------------------------------------------------------------------------------------
// Helpers / strategies
// ---------------------------------------------------------------------------------------------

/// Any string: random unicode, dense control chars, quotes/backslashes, astral chars, long runs.
fn arb_text() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => any::<String>(),
        2 => "[\\x00-\\x1f\\x7f\"\\\\]{0,64}",
        2 => "[\\x00-\\x1f\"\\\\a\u{e9}\u{1F600}\u{10FFFF}]{0,700}",
        1 => "\\PC{0,200}",
        1 => any::<String>().prop_map(|s| s.repeat(64)),
        1 => "[a-z]{10000}",
        1 => Just(String::new()),
    ]
}

/// Short actor/type so only `msg` can push the line over budget.
fn arb_short() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => "[a-z0-9-]{0,16}",
        1 => "[\\x00-\\x1f\"\\\\\u{e9}\u{1F600}]{0,8}",
    ]
}

fn arb_ts() -> impl Strategy<Value = f64> {
    prop_oneof![
        4 => any::<f64>(),
        1 => Just(f64::NAN),
        1 => Just(f64::INFINITY),
        1 => Just(f64::NEG_INFINITY),
        1 => Just(-0.0),
        1 => Just(0.0),
        1 => Just(1e300),
        1 => Just(-1e300),
        1 => Just(f64::MIN_POSITIVE),
        1 => Just(5e-324),
        1 => Just(f64::MAX),
        1 => Just(f64::MIN),
        1 => Just(1_727_000_000.123_456_7),
    ]
}

fn parse(line: &[u8]) -> Value {
    serde_json::from_slice(line)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(line)))
}

/// Exact number of bytes `c` takes once serde_json has escaped it, measured, not tabulated.
fn real_escaped_len(c: char) -> usize {
    let s = serde_json::to_string(&c.to_string()).unwrap();
    s.len() - 2
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "tx-core-fuzz-events-{tag}-{}-{}",
            std::process::id(),
            now().to_bits()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------------------------
// 1. encode(): shape, bound, maximal prefix
// ---------------------------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]

    #[test]
    fn encode_line_shape_and_bound(ts in arb_ts(), actor in arb_short(), type_ in arb_short(), msg in arb_text()) {
        let line = encode(ts, &actor, &type_, "i", "n", &msg);
        prop_assert!(line.len() <= PIPE_BUF, "len {}", line.len());
        prop_assert_eq!(line.last(), Some(&b'\n'));
        prop_assert_eq!(line.iter().filter(|&&b| b == b'\n').count(), 1);
        let value = parse(&line);
        let obj = value.as_object().expect("object");
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        prop_assert_eq!(keys, vec!["ts", "actor", "type", "id", "name", "msg"]);
        prop_assert_eq!(obj["actor"].as_str(), Some(actor.as_str()));
        prop_assert_eq!(obj["type"].as_str(), Some(type_.as_str()));
        if ts.is_finite() {
            let back = obj["ts"].as_f64().expect("finite ts is a number");
            prop_assert_eq!(back.to_bits(), ts.to_bits(), "ts {:e} -> {:e}", ts, back);
        } else {
            prop_assert!(obj["ts"].is_null());
        }
        let kept = obj["msg"].as_str().expect("msg is a string");
        prop_assert!(msg.starts_with(kept), "msg not a prefix: {:?} of {:?}", kept, msg);
        prop_assert!(msg.is_char_boundary(kept.len()));
    }

    #[test]
    fn encode_trimmed_prefix_is_maximal(ts in arb_ts(), actor in arb_short(), type_ in arb_short(), msg in arb_text()) {
        let line = encode(ts, &actor, &type_, "i", "n", &msg);
        let kept = parse(&line)["msg"].as_str().unwrap().to_owned();
        if kept.len() == msg.len() {
            prop_assert_eq!(kept, msg);
            return Ok(());
        }
        prop_assert!(line.len() <= PIPE_BUF);
        let next = msg[kept.len()..].chars().next().unwrap();
        let one_more = &msg[..kept.len() + next.len_utf8()];
        let full = serde_json::json!({"ts": ts, "actor": actor, "type": type_, "id": "i", "name": "n", "msg": one_more}).to_string();
        // Exact-accounting check: the real line with one more char must overflow.
        prop_assert!(full.len() + 1 > PIPE_BUF, "not maximal: kept {} bytes, next {:?} would give {}", line.len(), next, full.len() + 1);
        // And the trimmed line's real length equals budget accounting (no slack from a wrong table).
        prop_assert_eq!(line.len() + real_escaped_len(next), full.len() + 1);
    }

    #[test]
    fn escaped_len_table_matches_serde_json(c in any::<char>()) {
        // Drive encode() so the private table is exercised: fill budget with this exact char.
        let skeleton = encode(0.0, "", "", "i", "n", "").len();
        let budget = PIPE_BUF - skeleton;
        let per = real_escaped_len(c);
        let n = budget / per + 1;
        let msg: String = std::iter::repeat_n(c, n).collect();
        let line = encode(0.0, "", "", "i", "n", &msg);
        let kept = parse(&line)["msg"].as_str().unwrap().chars().count();
        prop_assert_eq!(kept, budget / per, "char {:?} ({:#x}), real escaped len {}", c, c as u32, per);
        prop_assert_eq!(line.len(), skeleton + kept * per);
    }

    #[test]
    fn encode_is_deterministic(ts in arb_ts(), actor in arb_short(), type_ in arb_short(), msg in arb_text()) {
        prop_assert_eq!(encode(ts, &actor, &type_, "i", "n", &msg), encode(ts, &actor, &type_, "i", "n", &msg));
    }
}

#[test]
fn del_0x7f_is_one_byte_not_escaped() {
    assert_eq!(real_escaped_len('\u{7f}'), 1);
    let skeleton = encode(0.0, "", "", "i", "n", "").len();
    let msg = "\u{7f}".repeat(PIPE_BUF);
    let line = encode(0.0, "", "", "i", "n", &msg);
    assert_eq!(line.len(), PIPE_BUF);
    assert_eq!(
        parse(&line)["msg"].as_str().unwrap().len(),
        PIPE_BUF - skeleton
    );
}

#[test]
fn every_control_and_short_escape_boundary() {
    let skeleton = encode(0.0, "", "", "i", "n", "").len();
    for code in 0u32..0x80 {
        let c = char::from_u32(code).unwrap();
        let per = real_escaped_len(c);
        let budget = PIPE_BUF - skeleton;
        let msg: String = std::iter::repeat_n(c, budget / per + 1).collect();
        let line = encode(0.0, "", "", "i", "n", &msg);
        assert_eq!(
            line.len(),
            skeleton + (budget / per) * per,
            "char {code:#x}"
        );
    }
}

#[test]
fn timestamp_edge_cases_roundtrip_or_null() {
    for ts in [
        -0.0,
        0.0,
        1e300,
        -1e300,
        f64::MIN_POSITIVE,
        5e-324,
        f64::MAX,
        f64::MIN,
    ] {
        let v = parse(&encode(ts, "a", "t", "i", "n", "m"));
        assert_eq!(v["ts"].as_f64().unwrap().to_bits(), ts.to_bits(), "{ts:e}");
    }
    for ts in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(parse(&encode(ts, "a", "t", "i", "n", "m"))["ts"].is_null());
    }
}

// ---------------------------------------------------------------------------------------------
// 2. Long actor / type_ blow the budget: measure, do not assert fine
// ---------------------------------------------------------------------------------------------

#[test]
fn long_actor_or_type_exceeds_pipe_buf_measured() {
    let actor = "a".repeat(600);
    let line = encode(0.0, &actor, "t", "i", "n", "hello");
    let kept = parse(&line)["msg"].as_str().unwrap().to_owned();
    eprintln!(
        "actor=600 bytes: line {} bytes (PIPE_BUF {PIPE_BUF}), msg kept {kept:?}",
        line.len()
    );
    // Documented observation: only msg is trimmed, so the line is > PIPE_BUF and msg is emptied.
    assert!(line.len() > PIPE_BUF);
    assert_eq!(kept, "");

    let type_ = "\u{1F600}".repeat(200);
    let line = encode(0.0, "s", &type_, "i", "n", "x".repeat(100).as_str());
    eprintln!("type_=800 bytes: line {} bytes", line.len());
    assert!(line.len() > PIPE_BUF);

    // Skeleton exactly at the boundary: msg has zero budget, line == PIPE_BUF.
    let skeleton = encode(0.0, "", "t", "i", "n", "").len();
    let actor = "a".repeat(PIPE_BUF - skeleton);
    let line = encode(0.0, &actor, "t", "i", "n", "z");
    assert_eq!(line.len(), PIPE_BUF);
    assert_eq!(parse(&line)["msg"], "");
}

// ---------------------------------------------------------------------------------------------
// 3. Concurrency
// ---------------------------------------------------------------------------------------------

#[test]
fn eight_threads_five_hundred_appends_untorn() {
    let dir = TempDir::new("concurrent");
    let path = dir.0.join("log.jsonl");
    let handles: Vec<_> = (0..8)
        .map(|worker: u8| {
            let log = EventLog::new(&path);
            std::thread::spawn(move || {
                let msg = char::from(b'a' + worker).to_string().repeat(430);
                for _ in 0..500 {
                    log.append_at(f64::from(worker), "t", "i", "n", &msg, "s")
                        .unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.last(), Some(&b'\n'));
    let lines: Vec<&[u8]> = bytes
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(lines.len(), 8 * 500);
    let mut counts = [0usize; 8];
    for line in &lines {
        assert!(line.len() <= PIPE_BUF);
        let v = parse(line);
        let msg = v["msg"].as_str().unwrap();
        assert_eq!(msg.len(), 430);
        let first = msg.as_bytes()[0];
        assert!(msg.bytes().all(|b| b == first), "torn: {msg}");
        let worker = usize::from(first - b'a');
        assert_eq!(v["ts"].as_f64().unwrap() as usize, worker);
        counts[worker] += 1;
    }
    assert!(counts.iter().all(|&c| c == 500), "{counts:?}");
}

// ---------------------------------------------------------------------------------------------
// 4. append() error paths: Err, no panic
// ---------------------------------------------------------------------------------------------

#[test]
fn append_parent_is_a_file_errs() {
    let dir = TempDir::new("parent-file");
    let blocker = dir.0.join("blocker");
    std::fs::write(&blocker, b"x").unwrap();
    let log = EventLog::new(blocker.join("log.jsonl"));
    assert!(log.append_at(0.0, "t", "i", "n", "m", "a").is_err());
    assert_eq!(std::fs::read(&blocker).unwrap(), b"x");
}

#[test]
fn append_path_is_a_directory_errs() {
    let dir = TempDir::new("is-dir");
    let log = EventLog::new(&dir.0);
    assert!(log.append_at(0.0, "t", "i", "n", "m", "a").is_err());
    assert!(dir.0.is_dir());
}

#[test]
fn append_in_read_only_dir_errs() {
    let dir = TempDir::new("ro");
    let ro = dir.0.join("ro");
    std::fs::create_dir(&ro).unwrap();
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o500)).unwrap();
    if std::fs::write(ro.join("probe"), b"").is_ok() {
        eprintln!("skipped: permissions not enforced (root?)");
        return;
    }
    let log = EventLog::new(ro.join("log.jsonl"));
    assert!(log.append_at(0.0, "t", "i", "n", "m", "a").is_err());
    let nested = EventLog::new(ro.join("sub").join("log.jsonl"));
    assert!(nested.append_at(0.0, "t", "i", "n", "m", "a").is_err());
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn append_empty_path_errs() {
    let log = EventLog::new("");
    assert!(log.append_at(0.0, "t", "i", "n", "m", "a").is_err());
}

#[test]
fn append_with_nul_in_path_errs() {
    let dir = TempDir::new("nul");
    let log = EventLog::new(dir.0.join("log\0.jsonl"));
    assert!(log.append_at(0.0, "t", "i", "n", "m", "a").is_err());
}

#[test]
fn append_at_arbitrary_inputs_writes_bounded_lines() {
    let dir = TempDir::new("arb");
    let log = EventLog::new(dir.0.join("log.jsonl"));
    let inputs = [
        (f64::NAN, "\u{0}", "\"\\", "\n".repeat(1000)),
        (-0.0, "", "", String::new()),
        (1e300, "\u{1F600}", "é", "\u{10FFFF}".repeat(300)),
    ];
    for (ts, type_, actor, msg) in &inputs {
        log.append_at(*ts, type_, "i", "n", msg, actor).unwrap();
    }
    let text = std::fs::read_to_string(log.path()).unwrap();
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), inputs.len());
    for (line, (_, type_, actor, msg)) in lines.iter().zip(&inputs) {
        assert!(line.len() < PIPE_BUF);
        let v: Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["type"], *type_);
        assert_eq!(v["actor"], *actor);
        assert!(msg.starts_with(v["msg"].as_str().unwrap()));
    }
}

// ---------------------------------------------------------------------------------------------
// 5. EventType
// ---------------------------------------------------------------------------------------------

#[test]
fn event_type_names_unique_and_stable() {
    let names: Vec<&'static str> = EventType::ALL.iter().map(|t| t.as_str()).collect();
    let unique: HashSet<&str> = names.iter().copied().collect();
    assert_eq!(unique.len(), EventType::ALL.len());
    assert_eq!(
        names,
        [
            "created",
            "renamed",
            "tagged",
            "message-sent",
            "state-changed",
            "engine-data-changed",
            "exited",
            "stopped",
            "forgotten"
        ]
    );
    let variants: HashSet<EventType> = EventType::ALL.iter().copied().collect();
    assert_eq!(variants.len(), EventType::ALL.len());
    for t in EventType::ALL {
        let s = t.as_str();
        assert!(!s.is_empty());
        assert!(
            s.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'),
            "{s}"
        );
        assert_eq!(t.as_str(), s, "as_str not stable");
        assert_eq!(
            serde_json::to_string(s).unwrap().len(),
            s.len() + 2,
            "needs escaping: {s}"
        );
    }
}

#[test]
fn now_is_monotone_enough_and_finite() {
    let a = now();
    let b = now();
    assert!(a.is_finite() && b.is_finite());
    assert!(b >= a);
    assert!(a > 1.7e9 && a < 4e9);
}
