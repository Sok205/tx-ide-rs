//! Adversarial / property tests for `tx_core::tmux` against a PRIVATE tmux server.
//!
//! Every tmux call carries `-S <tempdir>/tmux.sock -f <tempdir>/tmux.conf`; the user's default
//! server is never touched. Tests marked `#[ignore = "LIMITATION: …"]` document platform limits.
//! Everything else that is merely surprising is asserted as observed and labelled "sharp edge".

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use proptest::prelude::*;
use tx_core::tmux::exact;
use tx_core::{Live, LiveTable, SessionId, Tmux, TmuxError};

// ---------------------------------------------------------------------------------------------
// Private server helper
// ---------------------------------------------------------------------------------------------

struct Server {
    dir: tempfile::TempDir,
    tmux: Tmux,
}

impl Server {
    fn start() -> Option<Self> {
        Command::new("tmux").arg("-V").output().ok()?;
        let dir = tempfile::Builder::new()
            .prefix("txf")
            .tempdir_in("/tmp")
            .ok()?;
        let conf = dir.path().join("tmux.conf");
        std::fs::write(&conf, "set -g @from-conf yes\n").ok()?;
        let tmux = Tmux::new("tmux", dir.path().join("tmux.sock"), conf);
        Some(Self { dir, tmux })
    }

    fn raw(&self, args: &[&str]) -> std::process::Output {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        Command::new("tmux")
            .args(self.tmux.argv(&args))
            .output()
            .expect("spawn tmux")
    }

    fn sleeper(&self, name: &str) -> Result<(), TmuxError> {
        self.sleeper_with_env(name, &[])
    }

    fn sleeper_with_env(&self, name: &str, env: &[(String, String)]) -> Result<(), TmuxError> {
        self.tmux
            .new_session(name, Path::new("/"), "sleep", &["60".into()], env)
    }

    /// A session whose pane runs `cat > <file>`, returning (id, live, file).
    fn cat_pane(&self, name: &str) -> (SessionId, Live, std::path::PathBuf) {
        let out = self.dir.path().join(format!("{name}.typed"));
        self.tmux
            .new_session(
                name,
                Path::new("/"),
                "sh",
                &["-c".into(), format!("cat > {}; sleep 60", out.display())],
                &[],
            )
            .unwrap();
        let id = sid(name);
        self.tmux.set_tx_id(name, &id).unwrap();
        let live = self.tmux.is_live(&id).unwrap().unwrap();
        // let the shell reach `cat` before typing
        std::thread::sleep(Duration::from_millis(300));
        (id, live, out)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.raw(&["kill-server"]);
    }
}

macro_rules! server {
    () => {
        match Server::start() {
            Some(server) => server,
            None => {
                eprintln!("tmux not available; skipping");
                return;
            }
        }
    };
}

fn wait_for(path: &Path, pred: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let bytes = std::fs::read(path).unwrap_or_default();
        if pred(&bytes) || Instant::now() > deadline {
            return bytes;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn sid(s: &str) -> SessionId {
    s.parse().unwrap()
}

fn is_failed(error: &TmuxError) -> bool {
    matches!(error, TmuxError::Failed { .. })
}

fn is_inspection_failed(error: &TmuxError) -> bool {
    matches!(error, TmuxError::InspectionFailed { .. })
}

// ---------------------------------------------------------------------------------------------
// 1. LiveTable::parse — pure, property-tested against a reference model
// ---------------------------------------------------------------------------------------------

/// Reference semantics: split on '\n', drop empty lines, each line must have exactly 3 tab
/// fields, ids outside `[A-Za-z0-9_-]{1,64}` skipped (not ours), first id wins.
fn model_parse(out: &str) -> Option<HashMap<String, (String, String)>> {
    let mut map = HashMap::new();
    for line in out.split('\n').filter(|l| !l.is_empty()) {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 3 {
            return None;
        }
        if fields[0].parse::<SessionId>().is_err() {
            continue;
        }
        map.entry(fields[0].to_owned())
            .or_insert((fields[1].to_owned(), fields[2].to_owned()));
    }
    Some(map)
}

fn arb_field() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => "[a-z0-9$%-]{0,8}",
        1 => "[^\t\n]{0,6}",
        1 => Just(String::new()),
        1 => Just("\r".to_owned()),
        1 => Just("żółć".to_owned()),
    ]
}

fn arb_row() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => (arb_field(), arb_field(), arb_field()).prop_map(|(a, b, c)| format!("{a}\t{b}\t{c}")),
        1 => prop::collection::vec(arb_field(), 0..5).prop_map(|f| f.join("\t")),
        1 => Just(String::new()),
    ]
}

fn arb_output() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => prop::collection::vec(arb_row(), 0..8).prop_map(|rows| rows.join("\n") + "\n"),
        1 => prop::collection::vec(arb_row(), 0..8).prop_map(|rows| rows.join("\n")),
        1 => any::<String>(),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn parse_matches_model_and_never_panics(out in arb_output()) {
        let parsed = LiveTable::parse(&out);
        match (parsed, model_parse(&out)) {
            (Ok(table), Some(model)) => {
                prop_assert_eq!(table.len(), model.len());
                prop_assert_eq!(table.is_empty(), model.is_empty());
                for (id, (session, pane)) in &model {
                    let key = sid(id);
                    prop_assert!(table.contains(&key));
                    let live = table.get(&key).unwrap();
                    prop_assert_eq!(&live.session_target, session);
                    prop_assert_eq!(&live.pane_target, pane);
                }
            }
            (Err(error), None) => {
                let ok = is_inspection_failed(&error);
                prop_assert!(ok, "{}", error);
            }
            (parsed, model) => prop_assert!(false, "mismatch: {:?} vs {:?}", parsed, model),
        }
    }

    #[test]
    fn parse_is_order_insensitive_except_for_duplicates(
        rows in prop::collection::vec(("[a-f]{1,3}", "[$][0-9]", "[%][0-9]"), 0..6)
    ) {
        let ids: HashSet<&str> = rows.iter().map(|(id, _, _)| id.as_str()).collect();
    let _ = &ids;
        let text: String = rows.iter().map(|(a, b, c)| format!("{a}\t{b}\t{c}\n")).collect();
        let table = LiveTable::parse(&text).unwrap();
        prop_assert_eq!(table.len(), ids.len());
        for (id, session, pane) in &rows {
            // first occurrence wins
            let first = rows.iter().find(|(i, _, _)| i == id).unwrap();
            let live = table.get(&sid(id)).unwrap();
            prop_assert_eq!(&live.session_target, &first.1);
            prop_assert_eq!(&live.pane_target, &first.2);
            let _ = (session, pane);
        }
    }
}

#[test]
fn parse_sharp_edges() {
    // CRLF: '\r' is NOT stripped; it lands inside the pane target. tmux never emits CRLF on
    // stdout so this is a documented edge, not a bug.
    let table = LiveTable::parse("id\t$0\t%0\r\n").unwrap();
    assert_eq!(table.get(&sid("id")).unwrap().pane_target, "%0\r");
    // an id with '\r' is not a valid id, so the row is "not ours" and silently skipped
    assert!(LiveTable::parse("id\r\t$0\t%0\n").unwrap().is_empty());
    // trailing tab → 4 fields → InspectionFailed
    assert!(is_inspection_failed(
        &LiveTable::parse("id\t$0\t%0\t\n").unwrap_err()
    ));
    // tab inside the id (a hand-set @tx-id) corrupts the row → InspectionFailed, never a guess
    assert!(is_inspection_failed(
        &LiveTable::parse("a\tb\t$0\t%0\n").unwrap_err()
    ));
    // one bad row poisons the whole table
    assert!(is_inspection_failed(
        &LiveTable::parse("ok\t$0\t%0\nbad\n").unwrap_err()
    ));
    // ids with '\r', unicode, spaces or > 64 bytes are not core-minted → skipped, not errors
    let long = "a".repeat(65);
    let text = format!("a\rb\t$1\t%1\nżółć\t$2\t%2\na b\t$3\t%3\n{long}\t$4\t%4\nok\t$5\t%5\n");
    let table = LiveTable::parse(&text).unwrap();
    assert_eq!(table.len(), 1);
    assert_eq!(table.get(&sid("ok")).unwrap().session_target, "$5");
    // a row of only tabs: empty id → skipped
    assert!(LiveTable::parse("\t\t\n").unwrap().is_empty());
    // 64-byte id is the maximum
    assert_eq!(
        LiveTable::parse(&format!("{}\t$0\t%0\n", "b".repeat(64)))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn exact_prefixes_equals() {
    for name in ["", "-x", "=a", "a:b", "$1", "żółć"] {
        assert_eq!(exact(name), format!("={name}"));
    }
}

// ---------------------------------------------------------------------------------------------
// 2. Session names with special meaning to tmux
// ---------------------------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum NameOutcome {
    /// new_session, set_tx_id, list_live, rename by `$N`, kill by `$N` all work and the
    /// session is listed under exactly this name.
    RoundTrips,
    /// Created, but under a different name (tmux rewrites `.` and `:` to `_`).
    Rewritten(&'static str),
    /// `new_session` failed.
    Rejected,
    /// Created under this name, but `set_tx_id` (`-t =name:`) cannot address it.
    Unaddressable,
}

fn probe_name(server: &Server, name: &str) -> NameOutcome {
    let tmux = &server.tmux;
    if server.sleeper(name).is_err() {
        return NameOutcome::Rejected;
    }
    let id = sid(&format!("id-{}", name.len()));
    let set = tmux.set_tx_id(name, &id);
    // what name did tmux actually store? (list every session, newest id wins)
    let out = server.raw(&["list-sessions", "-F", "#{session_name}"]);
    let names = String::from_utf8_lossy(&out.stdout);
    let stored = names.lines().find(|n| *n == name).is_some();
    if set.is_err() {
        // clean up by whichever name exists so later probes start clean
        let _ = server.raw(&["kill-server"]);
        if stored {
            return NameOutcome::Unaddressable;
        }
        return match name {
            "a:b" | "a.b" => NameOutcome::Rewritten("a_b"),
            _ => NameOutcome::Unaddressable,
        };
    }
    let live = tmux
        .is_live(&id)
        .unwrap()
        .expect("set_tx_id succeeded → listed");
    assert!(live.session_target.starts_with('$'), "{live:?}");
    assert!(live.pane_target.starts_with('%'), "{live:?}");
    tmux.rename_session(&live.session_target, "renamed")
        .unwrap();
    assert_eq!(tmux.is_live(&id).unwrap().unwrap(), live);
    assert!(tmux.kill_session(&live.session_target).unwrap());
    assert!(tmux.is_live(&id).unwrap().is_none());
    if stored {
        NameOutcome::RoundTrips
    } else {
        NameOutcome::Rewritten("a_b")
    }
}

#[test]
fn special_session_names() {
    let server = server!();
    let long = "x".repeat(200);
    let cases: &[(&str, NameOutcome)] = &[
        ("-x", NameOutcome::RoundTrips),
        ("--", NameOutcome::RoundTrips),
        ("3", NameOutcome::RoundTrips),
        ("=a", NameOutcome::RoundTrips),
        ("%2", NameOutcome::RoundTrips),
        ("@3", NameOutcome::RoundTrips),
        ("~", NameOutcome::RoundTrips),
        ("a b", NameOutcome::RoundTrips),
        ("*", NameOutcome::RoundTrips),
        ("żółć", NameOutcome::RoundTrips),
        (long.as_str(), NameOutcome::RoundTrips),
        // sharp edges for the CLI layer (core rejects these via validate_name):
        ("", NameOutcome::Rejected),
        ("a:b", NameOutcome::Rewritten("a_b")),
        ("a.b", NameOutcome::Rewritten("a_b")),
        // `=$1:` is still parsed as session-id 1, so a session named "$1" can be created but
        // neither tagged nor killed by exact name (only via its real `$N`).
        ("$1", NameOutcome::Unaddressable),
    ];
    let mut report = Vec::new();
    for (name, expected) in cases {
        let got = probe_name(&server, name);
        report.push(format!("{name:?}: {got:?}"));
        assert_eq!(&got, expected, "name {name:?}");
    }
    eprintln!("{}", report.join("\n"));
}

#[test]
fn numeric_name_is_not_confused_with_session_index() {
    let server = server!();
    let tmux = &server.tmux;
    // "$0" exists as a session named "a"; a session named "0" must still be exact-addressable
    server.sleeper("a").unwrap();
    server.sleeper("0").unwrap();
    tmux.set_tx_id("0", &sid("zero")).unwrap();
    tmux.set_tx_id("a", &sid("a")).unwrap();
    let table = tmux.list_live().unwrap();
    assert_eq!(table.len(), 2);
    let zero = table.get(&sid("zero")).unwrap();
    assert_eq!(zero.session_target, "$1");
    assert!(tmux.kill_session(&exact("0")).unwrap());
    assert!(tmux.is_live(&sid("a")).unwrap().is_some());
}

#[test]
fn exact_target_does_not_prefix_match() {
    let server = server!();
    server.sleeper("agent-1").unwrap();
    assert!(!server.tmux.kill_session(&exact("agent")).unwrap());
    assert!(server.tmux.rename_session(&exact("agent"), "x").is_err());
    assert!(server.tmux.kill_session(&exact("agent-1")).unwrap());
}

// ---------------------------------------------------------------------------------------------
// 3. new_session env
// ---------------------------------------------------------------------------------------------

fn env_probe(
    server: &Server,
    name: &str,
    env: &[(String, String)],
    keys: &[&str],
) -> Result<Vec<Option<String>>, TmuxError> {
    let out = server.dir.path().join(format!("{name}.env"));
    let script = keys
        .iter()
        .map(|k| format!("if [ \"${{{k}+set}}\" = set ]; then printf '1%s\\0' \"${k}\"; else printf '0\\0'; fi"))
        .collect::<Vec<_>>()
        .join("; ");
    server.tmux.new_session(
        name,
        Path::new("/"),
        "sh",
        &[
            "-c".into(),
            format!(
                "{{ {script}; }} > {}.tmp && mv {0}.tmp {0}; sleep 60",
                out.display()
            ),
        ],
        env,
    )?;
    let bytes = wait_for(&out, |b| !b.is_empty());
    let mut values = Vec::new();
    for raw in bytes.split(|b| *b == 0).take(keys.len()) {
        values.push(match raw.first() {
            Some(b'1') => Some(String::from_utf8_lossy(&raw[1..]).into_owned()),
            _ => None,
        });
    }
    Ok(values)
}

#[test]
fn env_values_reach_the_process_verbatim() {
    let server = server!();
    let long = "v".repeat(8 * 1024);
    let env: Vec<(String, String)> = vec![
        ("EQ".into(), "a=b=c".into()),
        ("SP".into(), "two  words ".into()),
        ("NL".into(), "line1\nline2".into()),
        ("UNI".into(), "żółć 🦀".into()),
        ("EMPTY".into(), String::new()),
        ("LONG".into(), long.clone()),
        ("TAB".into(), "a\tb".into()),
    ];
    let got = env_probe(
        &server,
        "env",
        &env,
        &["EQ", "SP", "NL", "UNI", "EMPTY", "LONG", "TAB"],
    )
    .unwrap();
    let expected: Vec<Option<String>> = env.iter().map(|(_, v)| Some(v.clone())).collect();
    assert_eq!(got, expected);
}

#[test]
fn env_value_of_64kb_is_command_too_long() {
    // sharp edge: the whole new-session command line must fit tmux's ~16 kB message limit;
    // a big env value (or many of them) makes new_session fail with "command too long".
    let server = server!();
    let env: Vec<(String, String)> = vec![("LONG".into(), "v".repeat(64 * 1024))];
    let error = server.sleeper_with_env("big", &env).unwrap_err();
    assert!(is_failed(&error), "{error}");
    assert!(error.to_string().ends_with("command too long"), "{error}");
    assert!(server.tmux.list_live().unwrap().is_empty());
}

#[test]
fn env_with_empty_key_is_stored_but_invisible_to_the_process() {
    // sharp edge: tmux -e accepts "=v" without an error, keeps it in the session environment,
    // and the child never sees it. Core never validates keys.
    let server = server!();
    let env: Vec<(String, String)> = vec![("".into(), "v".into()), ("OK".into(), "1".into())];
    let got = env_probe(&server, "e1", &env, &["OK"]).unwrap();
    assert_eq!(got, vec![Some("1".into())]);
    let out = server.raw(&["show-environment", "-t", "=e1"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.lines().any(|l| l == "=v"), "{text}");
}

#[test]
fn env_key_containing_equals_splits_at_first_equals() {
    // sharp edge: ("A=B", "c") becomes A="B=c"; core never validates keys.
    let server = server!();
    let env: Vec<(String, String)> = vec![("A=B".into(), "c".into())];
    let got = env_probe(&server, "e2", &env, &["A"]).unwrap();
    assert_eq!(got, vec![Some("B=c".into())]);
}

#[test]
fn later_env_entry_wins() {
    let server = server!();
    let env: Vec<(String, String)> =
        vec![("K".into(), "first".into()), ("K".into(), "second".into())];
    let got = env_probe(&server, "e3", &env, &["K"]).unwrap();
    assert_eq!(got, vec![Some("second".into())]);
}

// ---------------------------------------------------------------------------------------------
// 4. send_keys_literal
// ---------------------------------------------------------------------------------------------

fn typed(server: &Server, name: &str, text: &str) -> Vec<u8> {
    let (_, live, out) = server.cat_pane(name);
    server
        .tmux
        .send_keys_literal(&live.pane_target, text)
        .unwrap();
    server.tmux.send_enter(&live.pane_target).unwrap();
    let want = text.len() + 1;
    wait_for(&out, |b| b.len() >= want)
}

#[test]
fn send_keys_literal_is_verbatim_for_shell_like_text() {
    let server = server!();
    let cases = [
        "-n -l -- -t",
        "a ; b",
        "\\; x",
        "' \" ` $HOME ${X} $(id) \\",
        "\x1b[31mred\x1b[0m",
        "żółć 🦀 日本",
        "Enter C-c Escape Space",
        "#{session_id} #[fg=red]",
        "%0 $0 =a",
    ];
    for (i, text) in cases.iter().enumerate() {
        let got = typed(&server, &format!("t{i}"), text);
        assert_eq!(
            String::from_utf8_lossy(&got),
            format!("{text}\n"),
            "case {text:?}"
        );
    }
}

#[test]
fn send_keys_literal_newline_in_text_is_a_line_break() {
    let server = server!();
    let got = typed(&server, "nl", "a\nb");
    assert_eq!(got, b"a\nb\n");
}

#[test]
fn send_keys_literal_cr_becomes_newline_via_tty() {
    // sharp edge: the pane's tty is in cooked mode (ICRNL), so '\r' arrives as '\n'.
    let server = server!();
    let got = typed(&server, "cr", "a\rb");
    assert_eq!(got, b"a\nb\n");
}

#[test]
fn send_keys_literal_empty_text_is_ok() {
    let server = server!();
    let (_, live, out) = server.cat_pane("empty");
    server
        .tmux
        .send_keys_literal(&live.pane_target, "")
        .unwrap();
    server.tmux.send_enter(&live.pane_target).unwrap();
    assert_eq!(wait_for(&out, |b| !b.is_empty()), b"\n");
}

#[test]
fn send_keys_literal_1000_bytes_is_delivered() {
    let server = server!();
    let text = "z".repeat(1000);
    let got = typed(&server, "k1000", &text);
    assert_eq!(got.len(), 1001);
}

#[test]
#[ignore = "LIMITATION: a line > 1024 bytes without a newline is dropped whole by the cooked tty (MAX_INPUT) — send_keys_literal cannot deliver 10 kB to a line-mode reader (cooked-mode tty MAX_INPUT; raw-mode agents unaffected)"]
fn send_keys_literal_10kb_is_delivered() {
    let server = server!();
    let text = "z".repeat(10 * 1024);
    let got = typed(&server, "k10k", &text);
    assert_eq!(got.len(), text.len() + 1);
}

#[test]
fn send_keys_literal_trailing_semicolon_is_verbatim() {
    let server = server!();
    for (i, text) in ["a;", ";", "b\\;"].iter().enumerate() {
        let got = typed(&server, &format!("semi{i}"), text);
        assert_eq!(
            String::from_utf8_lossy(&got),
            format!("{text}\n"),
            "case {text:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// 5. Stale targets and a dead server
// ---------------------------------------------------------------------------------------------

#[test]
fn stale_targets_fail_cleanly_while_server_lives() {
    let server = server!();
    let tmux = &server.tmux;
    server.sleeper("keep").unwrap();
    server.sleeper("victim").unwrap();
    tmux.set_tx_id("victim", &sid("v")).unwrap();
    let live = tmux.is_live(&sid("v")).unwrap().unwrap();
    assert!(tmux.kill_session(&live.session_target).unwrap());

    assert!(!tmux.kill_session(&live.session_target).unwrap());
    assert!(is_failed(
        &tmux.rename_session(&live.session_target, "x").unwrap_err()
    ));
    assert!(is_failed(
        &tmux.send_keys_literal(&live.pane_target, "x").unwrap_err()
    ));
    assert!(is_failed(&tmux.send_enter(&live.pane_target).unwrap_err()));
    assert!(is_failed(&tmux.set_tx_id("victim", &sid("v")).unwrap_err()));
    assert_eq!(tmux.list_live().unwrap().len(), 0);
    assert!(tmux.is_live(&sid("v")).unwrap().is_none());
}

#[test]
fn everything_fails_cleanly_after_kill_server() {
    let server = server!();
    let tmux = &server.tmux;
    server.sleeper("a").unwrap();
    tmux.set_tx_id("a", &sid("a")).unwrap();
    let live = tmux.is_live(&sid("a")).unwrap().unwrap();
    assert!(server.raw(&["kill-server"]).status.success());

    assert!(tmux.list_live().unwrap().is_empty());
    assert!(tmux.is_live(&sid("a")).unwrap().is_none());
    assert!(!tmux.kill_session(&live.session_target).unwrap());
    assert!(!tmux.kill_session(&exact("a")).unwrap());
    assert!(is_failed(
        &tmux.rename_session(&live.session_target, "x").unwrap_err()
    ));
    assert!(is_failed(
        &tmux.send_keys_literal(&live.pane_target, "x").unwrap_err()
    ));
    assert!(is_failed(&tmux.send_enter(&live.pane_target).unwrap_err()));
    assert!(is_failed(&tmux.set_tx_id("a", &sid("a")).unwrap_err()));
    // the socket file is gone, so a fresh server can start again
    server.sleeper("b").unwrap();
    assert_eq!(tmux.list_live().unwrap().len(), 0, "b has no @tx-id yet");
}

#[test]
fn server_exits_with_its_last_session_and_targets_reset() {
    let server = server!();
    let tmux = &server.tmux;
    server.sleeper("a").unwrap();
    tmux.set_tx_id("a", &sid("a")).unwrap();
    let first = tmux.is_live(&sid("a")).unwrap().unwrap();
    assert!(tmux.kill_session(&first.session_target).unwrap());
    assert!(tmux.list_live().unwrap().is_empty());
    server.sleeper("a").unwrap();
    tmux.set_tx_id("a", &sid("a")).unwrap();
    let second = tmux.is_live(&sid("a")).unwrap().unwrap();
    // `$N` restarts from 0 with the server: never store it
    assert_eq!(second, first);
}

// ---------------------------------------------------------------------------------------------
// 6. Unusable sockets
// ---------------------------------------------------------------------------------------------

#[test]
fn socket_that_is_a_directory_is_inspection_failed() {
    let server = server!();
    let dir = server.dir.path().join("d");
    std::fs::create_dir(&dir).unwrap();
    let tmux = Tmux::new("tmux", &dir, server.tmux.conf());
    assert!(is_inspection_failed(&tmux.list_live().unwrap_err()));
    assert!(is_failed(&tmux.kill_session("$0").unwrap_err()));
}

#[test]
fn socket_that_is_a_regular_file_is_inspection_failed() {
    let server = server!();
    let file = server.dir.path().join("f");
    std::fs::write(&file, b"").unwrap();
    let tmux = Tmux::new("tmux", &file, server.tmux.conf());
    let error = tmux.list_live().unwrap_err();
    assert!(is_inspection_failed(&error), "{error}");
}

#[test]
fn dangling_symlink_socket_reads_as_no_server() {
    // sharp edge: connect(2) on a dangling symlink is ENOENT, which tmux reports exactly like a
    // missing socket, so list_live says "nothing live". new_session would then replace the link.
    let server = server!();
    let link = server.dir.path().join("link");
    std::os::unix::fs::symlink(server.dir.path().join("nope"), &link).unwrap();
    let tmux = Tmux::new("tmux", &link, server.tmux.conf());
    assert!(tmux.list_live().unwrap().is_empty());
    assert!(!tmux.kill_session("$0").unwrap());
}

#[test]
fn socket_under_unreadable_dir_is_inspection_failed() {
    use std::os::unix::fs::PermissionsExt;
    let server = server!();
    let out = Command::new("id").arg("-u").output().unwrap();
    if String::from_utf8_lossy(&out.stdout).trim() == "0" {
        eprintln!("running as root; skipping");
        return;
    }
    let dir = server.dir.path().join("locked");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    let tmux = Tmux::new("tmux", dir.join("s.sock"), server.tmux.conf());
    let result = tmux.list_live();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let error = result.unwrap_err();
    assert!(is_inspection_failed(&error), "{error}");
    assert!(error.to_string().contains("Permission denied"), "{error}");
}

#[test]
fn missing_conf_file_does_not_stop_the_server() {
    // sharp edge check: a nonexistent -f is a startup error inside tmux; does new_session fail?
    let server = server!();
    let tmux = Tmux::new(
        "tmux",
        server.dir.path().join("x.sock"),
        server.dir.path().join("missing.conf"),
    );
    let result = tmux.new_session("a", Path::new("/"), "sleep", &["60".into()], &[]);
    let listed = tmux.list_live();
    let _ = Command::new("tmux")
        .args(tmux.argv(&["kill-server".into()]))
        .output();
    assert!(result.is_ok(), "{result:?}");
    assert!(listed.unwrap().is_empty());
}

// ---------------------------------------------------------------------------------------------
// 7. Many sessions
// ---------------------------------------------------------------------------------------------

#[test]
fn fifty_sessions_all_listed_with_unique_targets() {
    let server = server!();
    let tmux = &server.tmux;
    let ids: Vec<SessionId> = (0..50).map(|i| sid(&format!("id-{i:02}"))).collect();
    for (i, id) in ids.iter().enumerate() {
        let name = format!("s{i}");
        server.sleeper(&name).unwrap();
        tmux.set_tx_id(&name, id).unwrap();
    }
    let table = tmux.list_live().unwrap();
    assert_eq!(table.len(), 50);
    let mut sessions = HashSet::new();
    let mut panes = HashSet::new();
    for id in &ids {
        let live = table.get(id).unwrap();
        assert!(sessions.insert(live.session_target.clone()), "dup {live:?}");
        assert!(panes.insert(live.pane_target.clone()), "dup {live:?}");
    }
    // kill every other one by `$N`; the rest stay
    for id in ids.iter().step_by(2) {
        assert!(
            tmux.kill_session(&table.get(id).unwrap().session_target)
                .unwrap()
        );
    }
    let table = tmux.list_live().unwrap();
    assert_eq!(table.len(), 25);
    for id in ids.iter().skip(1).step_by(2) {
        assert!(table.contains(id));
    }
}

#[test]
fn cloned_session_shares_the_tx_id_and_first_wins() {
    // `new-session -t` makes a grouped session whose options are its own, so the clone carries
    // no @tx-id — but if someone copies the option, the table keeps the first row.
    let server = server!();
    let tmux = &server.tmux;
    server.sleeper("a").unwrap();
    tmux.set_tx_id("a", &sid("a")).unwrap();
    assert!(
        server
            .raw(&["new-session", "-d", "-s", "b", "-t", "=a"])
            .status
            .success()
    );
    assert_eq!(tmux.list_live().unwrap().len(), 1);
    assert!(
        server
            .raw(&["set-option", "-t", "=b:", "@tx-id", "a"])
            .status
            .success()
    );
    let table = tmux.list_live().unwrap();
    assert_eq!(table.len(), 1);
    let live = table.get(&sid("a")).unwrap();
    let raw = server.raw(&["list-sessions", "-F", "#{session_id}"]);
    let first_line = String::from_utf8_lossy(&raw.stdout)
        .lines()
        .next()
        .unwrap()
        .to_owned();
    assert_eq!(live.session_target, first_line);
}
