//! Adversarial / property tests for `tx_core::messaging` (`build_envelope`, `DELIVERY_PAUSE`)
//! and the delivery path it rides on (`Tmux::send_keys_literal` + `Tmux::send_enter`), against
//! a PRIVATE tmux server under /tmp. The user's default server is never touched.
//!
//! Pane-content checks go through a `cat > FILE` pane in cooked mode, so every message stays
//! well under the 1024-byte tty line limit and carries no C0 control except `\n` / `\t`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use proptest::prelude::*;
use tx_core::{DELIVERY_PAUSE, Live, SessionId, Tmux, TmuxError, build_envelope, validate_name};

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
            .prefix("txm")
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

    /// A pane running `cat > <file>`; `raw` switches the tty to raw mode first (no line limit,
    /// no ICRNL, so `Enter` lands as `\r`).
    fn cat_pane(&self, name: &str, raw: bool) -> (Live, PathBuf) {
        let out = self.dir.path().join(format!("{name}.typed"));
        let stty = if raw { "stty raw; " } else { "" };
        self.tmux
            .new_session(
                name,
                Path::new("/"),
                "sh",
                &[
                    "-c".into(),
                    format!("{stty}cat > {}; sleep 60", out.display()),
                ],
                &[],
            )
            .unwrap();
        let id = sid(name);
        self.tmux.set_tx_id(name, &id).unwrap();
        let live = self.tmux.is_live(&id).unwrap().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        (live, out)
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

fn sid(s: &str) -> SessionId {
    s.parse().unwrap()
}

fn wait_for(path: &Path, pred: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let bytes = std::fs::read(path).unwrap_or_default();
        if pred(&bytes) || Instant::now() > deadline {
            return bytes;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Types `text` + Enter into the pane and returns the bytes that arrived after `already`.
fn deliver_and_read(
    server: &Server,
    live: &Live,
    out: &Path,
    already: usize,
    text: &str,
) -> Vec<u8> {
    server
        .tmux
        .send_keys_literal(&live.pane_target, text)
        .unwrap();
    server.tmux.send_enter(&live.pane_target).unwrap();
    let want = already + text.len() + 1;
    let bytes = wait_for(out, |b| b.len() >= want);
    bytes[already.min(bytes.len())..].to_vec()
}

// ---------------------------------------------------------------------------------------------
// 1. build_envelope — pure shape
// ---------------------------------------------------------------------------------------------

fn arb_name() -> impl Strategy<Value = String> {
    "[A-Za-z0-9_-]{1,64}"
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn user_envelope_wraps_text_verbatim(text in any::<String>()) {
        let env = build_envelope(None, &text);
        prop_assert_eq!(&env, &format!("<from-user>{}</from-user>", text));
        let inner = &env["<from-user>".len()..env.len() - "</from-user>".len()];
        prop_assert_eq!(inner, text.as_str());
    }

    #[test]
    fn agent_envelope_names_sender_and_wraps_text(name in arb_name(), text in any::<String>()) {
        let env = build_envelope(Some(&name), &text);
        prop_assert_eq!(
            &env,
            &format!("<from-agent session=\"{}\">{}</from-agent>", name, text)
        );
        prop_assert!(validate_name(&name).is_ok());
    }

    #[test]
    fn envelope_is_exact_length_no_escaping(name in arb_name(), text in any::<String>()) {
        let user = build_envelope(None, &text);
        prop_assert_eq!(user.len(), text.len() + "<from-user></from-user>".len());
        let agent = build_envelope(Some(&name), &text);
        prop_assert_eq!(
            agent.len(),
            text.len() + name.len() + "<from-agent session=\"\"></from-agent>".len()
        );
    }
}

/// SHARP EDGE: the sender name is interpolated raw; `"`, `>` or whitespace yield a tag that
/// does not parse as the documented shape. Unreachable through core (`validate_name`), but
/// `build_envelope` itself does not defend.
#[test]
fn sharp_edge_unvalidated_sender_name_breaks_tag_shape() {
    for name in ["a\"b", "a>b", "a b", "a\n", "</from-agent>", ""] {
        let env = build_envelope(Some(name), "x");
        assert_eq!(
            env,
            format!("<from-agent session=\"{name}\">x</from-agent>")
        );
        assert!(validate_name(name).is_err(), "core must reject {name:?}");
    }
}

#[test]
fn delivery_pause_is_a_sane_bound() {
    assert_eq!(DELIVERY_PAUSE, Duration::from_millis(300));
    assert!(DELIVERY_PAUSE < Duration::from_secs(2));
}

#[test]
fn text_that_looks_like_an_envelope_is_not_escaped() {
    let text = "<from-user>nested</from-user><from-agent session=\"x\">";
    assert_eq!(
        build_envelope(None, text),
        format!("<from-user>{text}</from-user>")
    );
}

// ---------------------------------------------------------------------------------------------
// 2. End-to-end verbatim delivery through a cooked-mode pane
// ---------------------------------------------------------------------------------------------

/// Hostile-but-tty-safe text: printable ASCII, unicode, `;`, `\`, `#{...}`, leading `-`,
/// embedded `\n` / `\t`, and a trailing `;` / `\` / `\;` / `;;` suffix. No `\r`, no other C0.
fn arb_text() -> impl Strategy<Value = String> {
    let piece = prop_oneof![
        4 => "[ -~]{0,12}",
        2 => "[żółćĄŁ日本🦀é]{1,4}",
        2 => Just(";".to_owned()),
        2 => Just("\\".to_owned()),
        1 => Just("\\;".to_owned()),
        1 => Just("-n -l -- ".to_owned()),
        1 => Just("#{session_id} #[fg=red]".to_owned()),
        1 => Just("\n".to_owned()),
        1 => Just("\t".to_owned()),
        1 => Just("$HOME `id` $(id) ' \"".to_owned()),
    ];
    let suffix = prop_oneof![
        Just(""),
        Just(";"),
        Just("\\"),
        Just("\\;"),
        Just(";;"),
        Just(";;;"),
        Just("\\\\;"),
        Just("; "),
    ];
    (prop::collection::vec(piece, 0..12), suffix).prop_map(|(pieces, suffix)| {
        let mut s: String = pieces.concat();
        s.push_str(suffix);
        // cooked tty: keep every line and the whole message under the 1024-byte line limit
        while s.len() > 700 {
            s.pop();
        }
        s
    })
}

fn arb_sender() -> impl Strategy<Value = Option<String>> {
    prop_oneof![2 => Just(None), 1 => arb_name().prop_map(Some)]
}

#[test]
fn envelopes_arrive_verbatim_and_in_order() {
    let server = server!();
    let (live, out) = server.cat_pane("e2e", false);
    let already = AtomicUsize::new(0);
    let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig {
        cases: 40,
        max_shrink_iters: 0,
        ..ProptestConfig::default()
    });
    let strategy = (arb_sender(), arb_text());
    let result = runner.run(&strategy, |(sender, text)| {
        let envelope = build_envelope(sender.as_deref(), &text);
        let before = already.load(Ordering::SeqCst);
        let got = deliver_and_read(&server, &live, &out, before, &envelope);
        already.store(before + envelope.len() + 1, Ordering::SeqCst);
        let want = format!("{envelope}\n");
        prop_assert_eq!(
            String::from_utf8_lossy(&got).into_owned(),
            want,
            "text = {:?}",
            text
        );
        Ok(())
    });
    if let Err(e) = result {
        panic!("{e}");
    }
}

#[test]
fn sequence_of_envelopes_is_whole_and_ordered() {
    let server = server!();
    let (live, out) = server.cat_pane("seq", false);
    let messages: Vec<String> = (0..10)
        .map(|i| {
            build_envelope(
                if i % 2 == 0 { None } else { Some("worker-1") },
                &format!("msg {i}; żółć #{{x}} \\; tail;"),
            )
        })
        .collect();
    let mut expected = String::new();
    for m in &messages {
        server.tmux.send_keys_literal(&live.pane_target, m).unwrap();
        server.tmux.send_enter(&live.pane_target).unwrap();
        expected.push_str(m);
        expected.push('\n');
    }
    let want = expected.len();
    let got = wait_for(&out, |b| b.len() >= want);
    assert_eq!(String::from_utf8_lossy(&got), expected);
}

// ---------------------------------------------------------------------------------------------
// 3. The trailing-semicolon fix
// ---------------------------------------------------------------------------------------------

#[test]
fn trailing_semicolon_forms_arrive_verbatim() {
    let server = server!();
    let (live, out) = server.cat_pane("semi", false);
    let cases = [
        "a;", ";", ";;", "a\\;", "\\;", "a\\\\;", "a;\\", "a; ", ";;;;", "a\\;;", "\\;\\;", "a ;",
        "x\n;", ";\n;",
    ];
    let mut already = 0;
    for text in cases {
        let envelope = build_envelope(None, text);
        let got = deliver_and_read(&server, &live, &out, already, &envelope);
        already += envelope.len() + 1;
        assert_eq!(
            String::from_utf8_lossy(&got),
            format!("{envelope}\n"),
            "text {text:?}"
        );
    }
    // the raw text too (no envelope), so the fix is not hidden behind `</from-user>`
    for text in cases {
        let got = deliver_and_read(&server, &live, &out, already, text);
        already += text.len() + 1;
        assert_eq!(
            String::from_utf8_lossy(&got),
            format!("{text}\n"),
            "raw {text:?}"
        );
    }
}

#[test]
fn empty_text_sends_nothing_and_enter_still_submits() {
    let server = server!();
    let (live, out) = server.cat_pane("empty", false);
    server
        .tmux
        .send_keys_literal(&live.pane_target, "")
        .unwrap();
    server.tmux.send_enter(&live.pane_target).unwrap();
    let got = wait_for(&out, |b| !b.is_empty());
    assert_eq!(got, b"\n");
    let env = build_envelope(None, "");
    assert_eq!(env, "<from-user></from-user>");
    let got = deliver_and_read(&server, &live, &out, 1, &env);
    assert_eq!(String::from_utf8_lossy(&got), "<from-user></from-user>\n");
}

// ---------------------------------------------------------------------------------------------
// 4. Concurrency on one pane without a lock: tearing is possible
// ---------------------------------------------------------------------------------------------

#[test]
fn unserialized_senders_can_tear_lines() {
    let server = server!();
    // raw mode: no 1024-byte line limit even if every line merges; Enter lands as `\r`
    let (live, out) = server.cat_pane("race", true);
    const N: usize = 20;
    let envelopes = |tag: char| -> Vec<String> {
        (0..N)
            .map(|i| build_envelope(None, &format!("{tag}{i:02} {}", tag.to_string().repeat(24))))
            .collect()
    };
    let a = envelopes('A');
    let b = envelopes('B');
    let total: usize = a.iter().chain(b.iter()).map(|m| m.len() + 1).sum();

    let send = |msgs: Vec<String>| {
        let tmux = server.tmux.clone();
        let pane = live.pane_target.clone();
        std::thread::spawn(move || {
            for m in msgs {
                tmux.send_keys_literal(&pane, &m)?;
                tmux.send_enter(&pane)?;
            }
            Ok::<(), TmuxError>(())
        })
    };
    let (ta, tb) = (send(a.clone()), send(b.clone()));
    ta.join().unwrap().expect("thread A: no error");
    tb.join().unwrap().expect("thread B: no error");

    let got = wait_for(&out, |bytes| bytes.len() >= total);
    assert_eq!(got.len(), total, "every byte arrives, torn or not");
    let text = String::from_utf8_lossy(&got);
    let lines: Vec<&str> = text.split('\r').filter(|l| !l.is_empty()).collect();
    let whole = lines
        .iter()
        .filter(|l| a.iter().chain(b.iter()).any(|m| m == *l))
        .count();
    let torn = lines.len().saturating_sub(whole) + (2 * N).saturating_sub(lines.len());
    eprintln!(
        "unserialized senders: {} of {} messages involved in torn lines ({} lines observed)",
        torn,
        2 * N,
        lines.len()
    );
    // only "no panic / no error / nothing lost" is asserted; tearing is recorded, not required
    for m in a.iter().chain(b.iter()) {
        let head = &m[..12];
        assert!(text.contains(head), "message prefix {head:?} vanished");
    }
}

// ---------------------------------------------------------------------------------------------
// 5. Dead / missing panes: Failed, never a hang
// ---------------------------------------------------------------------------------------------

fn is_failed(e: &TmuxError) -> bool {
    matches!(e, TmuxError::Failed { .. })
}

#[test]
fn sending_to_a_dead_pane_is_failed_not_a_hang() {
    let server = server!();
    let (live, _) = server.cat_pane("dead", false);
    // keep the server alive after the pane dies
    server
        .tmux
        .new_session("keep", Path::new("/"), "sleep", &["60".into()], &[])
        .unwrap();
    assert!(server.tmux.kill_session(&live.session_target).unwrap());

    let start = Instant::now();
    let e1 = server
        .tmux
        .send_keys_literal(&live.pane_target, "x;")
        .unwrap_err();
    let e2 = server.tmux.send_enter(&live.pane_target).unwrap_err();
    let e3 = server.tmux.send_keys_literal("%4242", "x").unwrap_err();
    let e4 = server.tmux.send_enter("%4242").unwrap_err();
    assert!(start.elapsed() < Duration::from_secs(5));
    for e in [&e1, &e2, &e3, &e4] {
        assert!(is_failed(e), "{e}");
    }
    assert!(e1.to_string().contains("can't find pane"), "{e1}");
}

#[test]
fn sending_with_no_server_is_failed_not_a_hang() {
    let server = server!();
    let start = Instant::now();
    let e1 = server.tmux.send_keys_literal("%0", "x").unwrap_err();
    let e2 = server.tmux.send_enter("%0").unwrap_err();
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(is_failed(&e1), "{e1}");
    assert!(is_failed(&e2), "{e2}");
    // empty text to no server: nothing is sent, so nothing can fail
    assert!(server.tmux.send_keys_literal("%0", "").is_ok());
}

#[test]
fn trailing_semicolon_chunks_fail_atomically_on_dead_pane() {
    let server = server!();
    let e = server.tmux.send_keys_literal("%4242", ";;;").unwrap_err();
    assert!(is_failed(&e), "{e}");
}
