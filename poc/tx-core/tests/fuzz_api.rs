//! Adversarial / property tests for the public API surface: builder ⇔ field equivalence, chaining
//! order, append-vs-replace on repeated calls, compile-time auto traits, `EventType::ALL`, and
//! `CoreConfig` round trips with the socket-length boundary.
//!
//! Tests marked `#[ignore = "BUG: …"]` document real failures; none were needed in this suite.

use std::collections::BTreeSet;
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::path::PathBuf;

use proptest::prelude::*;
use tx_core::{
    CoreConfig, CoreError, CreateOptions, Directories, EngineKind, EventType, Launch,
    LaunchOptions, LaunchPlan, LaunchRequest, Lifecycle, ListFilter, LlmEngine, LlmEngineState,
    MAX_SOCKET_PATH_BYTES, Profile, ResolvedProfile, SendOptions, Sender, Session, SessionId,
    TxCore, WorkspaceAction,
};

// ---------------------------------------------------------------------------------------------
// Helpers and strategies
// ---------------------------------------------------------------------------------------------

fn assert_send_sync<T: Send + Sync>() {}
fn assert_copy<T: Copy>() {}
fn assert_static<T: 'static>() {}
fn assert_unwind_safe<T: UnwindSafe + RefUnwindSafe>() {}

/// The variant name only: `prop_assert!` messages are format strings.
fn kind(error: &CoreError) -> &'static str {
    match error {
        CoreError::NotFound(_) => "NotFound",
        CoreError::NameTaken(_) => "NameTaken",
        CoreError::InvalidName(_) => "InvalidName",
        CoreError::InvalidEnvKey(_) => "InvalidEnvKey",
        CoreError::NotADirectory(_) => "NotADirectory",
        CoreError::NotRunning(_) => "NotRunning",
        CoreError::EngineDisabled(_) => "EngineDisabled",
        CoreError::EngineTaken(_) => "EngineTaken",
        CoreError::SocketPathTooLong(_) => "SocketPathTooLong",
        CoreError::Profile(_) => "Profile",
        CoreError::Engine(_) => "Engine",
        CoreError::Store(_) => "Store",
        CoreError::Tmux(_) => "Tmux",
        CoreError::Lock(_) => "Lock",
        _ => "Other",
    }
}

fn kind() -> impl Strategy<Value = EngineKind> {
    prop::sample::select(EngineKind::ALL.to_vec())
}

fn bare_name() -> impl Strategy<Value = String> {
    "[A-Za-z0-9_-]{1,32}"
}

/// Arbitrary printable text, including empty, spaces, `=`, unicode.
fn text() -> impl Strategy<Value = String> {
    "\\PC{0,24}"
}

fn texts() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(text(), 0..4)
}

fn pairs() -> impl Strategy<Value = Vec<(String, String)>> {
    prop::collection::vec((text(), text()), 0..4)
}

fn lifecycle() -> impl Strategy<Value = Lifecycle> {
    prop::sample::select(Lifecycle::ALL.to_vec())
}

fn session_id() -> impl Strategy<Value = SessionId> {
    bare_name().prop_map(|s| s.parse().unwrap())
}

/// A safe absolute path of 1..=3 bare segments.
fn abs_path() -> impl Strategy<Value = PathBuf> {
    prop::collection::vec(bare_name(), 1..4).prop_map(|segs| {
        let mut p = PathBuf::from("/");
        for s in segs {
            p.push(s);
        }
        p
    })
}

fn paths() -> impl Strategy<Value = Vec<PathBuf>> {
    prop::collection::vec(abs_path(), 0..4)
}

fn workspace_action() -> impl Strategy<Value = WorkspaceAction> {
    prop_oneof![
        (bare_name(), text()).prop_map(|(p, c)| WorkspaceAction::Write {
            path: p.into(),
            content: c
        }),
        (abs_path(), bare_name()).prop_map(|(s, d)| WorkspaceAction::Symlink {
            src: s,
            dst: d.into()
        }),
    ]
}

/// A tempdir under /tmp so the default socket path stays short.
fn short_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("txa")
        .tempdir_in("/tmp")
        .unwrap()
}

fn session_with(parent: Option<SessionId>, tags: Vec<String>, lifecycle: Lifecycle) -> Session {
    Session {
        id: SessionId::mint(),
        name: "s".into(),
        parent,
        tags,
        cwd: "/w".into(),
        created_at: 1.0,
        ended_at: None,
        archived_at: None,
        last_modified_at: 1.0,
        lifecycle,
        launch: Launch::Agent {
            engine: EngineKind::Claude,
            profile: vec![],
            model: None,
            effort: None,
            resume: None,
        },
        ext: std::collections::BTreeMap::new(),
        state: LlmEngineState::Idle,
    }
}

// ---------------------------------------------------------------------------------------------
// 1. Builder / field equivalence
// ---------------------------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Each `LaunchPlan` setter touches exactly its field; `program` replaces, the rest append;
    /// the setters commute with one another.
    #[test]
    fn launch_plan_builder_sets_named_field_only(
        prog1 in text(), prog2 in text(),
        args1 in texts(), args2 in texts(), single in text(),
        env in pairs(),
        ws in prop::collection::vec(workspace_action(), 0..3),
    ) {
        let mut expected = LaunchPlan::new(&prog2);
        expected.args = args1.iter().chain(&args2).cloned().collect();
        expected.args.push(single.clone());
        expected.env = env.clone();
        expected.workspace = ws.clone();

        let mut forward = LaunchPlan::new(&prog1).program(&prog2).args(args1.clone()).args(args2.clone()).arg(&single);
        for (k, v) in &env {
            forward = forward.env(k, v);
        }
        for a in &ws {
            forward = forward.workspace(a.clone());
        }
        prop_assert_eq!(&forward, &expected);

        // Order of unrelated setters does not matter; only within-field order is preserved.
        let mut reversed = LaunchPlan::default();
        for a in &ws {
            reversed = reversed.workspace(a.clone());
        }
        for (k, v) in &env {
            reversed = reversed.env(k, v);
        }
        reversed = reversed.args(args1.clone()).args(args2.clone()).arg(&single).program(&prog1).program(&prog2);
        prop_assert_eq!(&reversed, &expected);

        // `new` is `default().program(..)` and nothing else.
        let fresh = LaunchPlan::new(&prog1);
        prop_assert_eq!(&fresh.program, &prog1);
        prop_assert!(fresh.args.is_empty() && fresh.env.is_empty() && fresh.workspace.is_empty());
    }

    /// `LaunchRequest`: `hook_command` is the one setter; last call wins, nothing else moves.
    #[test]
    fn launch_request_builder_sets_named_field_only(w1 in text(), w in text()) {
        let dir = short_tempdir();
        let profile = Profile::write(
            ResolvedProfile { text: "t".into(), skill_dirs: vec![] },
            &dir.path().join("p.md"),
        ).unwrap();
        let session = session_with(None, vec![], Lifecycle::Running);
        let first = [w1.clone()];
        let last = [w.clone()];

        let a = LaunchRequest::new(&session, &profile).hook_command(&first).hook_command(&last);
        let b = LaunchRequest::new(&session, &profile).hook_command(&last);
        for req in [a, b] {
            prop_assert_eq!(&req.session.id, &session.id);
            prop_assert_eq!(req.profile.text(), "t");
            prop_assert_eq!(req.hook_command, &last[..]);
        }
        let bare = LaunchRequest::new(&session, &profile);
        prop_assert!(bare.hook_command.is_empty());
        // `Copy`: `a` is still usable after being moved into the loop above.
        prop_assert_eq!(a.hook_command, &last[..]);
    }

    /// `CreateOptions`: `parent` replaces, `tags` and `env` append, order of setters is irrelevant.
    #[test]
    fn create_options_builder_sets_named_field_only(
        name in text(), cwd in abs_path(),
        p1 in session_id(), p2 in session_id(),
        tags1 in texts(), tags2 in texts(),
        env in pairs(),
        engine in kind(),
    ) {
        let launch = LaunchOptions::agent(engine);
        let mut expected = CreateOptions::new(&name, &cwd, launch.clone());
        expected.parent = Some(p2.clone());
        expected.tags = tags1.iter().chain(&tags2).cloned().collect();
        expected.env = env.clone();

        let mut forward = CreateOptions::new(&name, &cwd, launch.clone()).parent(p1.clone()).parent(p2.clone()).tags(tags1.clone()).tags(tags2.clone());
        for (k, v) in &env {
            forward = forward.env(k, v);
        }
        prop_assert_eq!(&forward, &expected);

        let mut reversed = CreateOptions::new(&name, &cwd, launch.clone());
        for (k, v) in &env {
            reversed = reversed.env(k, v);
        }
        reversed = reversed.tags(tags1.clone()).tags(tags2.clone()).parent(p2.clone());
        prop_assert_eq!(&reversed, &expected);

        let fresh = CreateOptions::new(&name, &cwd, launch.clone());
        prop_assert_eq!(&fresh.name, &name);
        prop_assert_eq!(&fresh.cwd, &cwd);
        prop_assert_eq!(&fresh.launch, &launch);
        prop_assert!(fresh.parent.is_none() && fresh.tags.is_empty() && fresh.env.is_empty());
    }

    /// `LaunchOptions::Agent`: scalar setters replace, `profile` appends, `option` replaces its
    /// key, order is irrelevant.
    #[test]
    fn launch_options_agent_builder_sets_named_field_only(
        engine in kind(),
        roles1 in texts(), roles2 in texts(),
        m1 in text(), m2 in text(), e in text(), p in text(),
    ) {
        let a = LaunchOptions::agent(engine).profile(roles1.clone()).option("model", m1.as_str()).option("model", m2.as_str()).option("effort", e.as_str()).initial_prompt(&p).profile(roles2.clone());
        let b = LaunchOptions::agent(engine).initial_prompt(&p).option("effort", e.as_str()).option("model", m2.as_str()).profile(roles1.clone()).profile(roles2.clone());
        prop_assert_eq!(&a, &b);
        let LaunchOptions::Agent { engine: eng, profile, options, initial_prompt, .. } = &a else {
            prop_assert!(false, "agent() did not build an Agent");
            unreachable!()
        };
        let roles: Vec<String> = roles1.iter().chain(&roles2).cloned().collect();
        prop_assert_eq!(*eng, engine);
        prop_assert_eq!(profile, &roles);
        prop_assert_eq!(options, &serde_json::json!({"model": m2, "effort": e}));
        prop_assert_eq!(initial_prompt, &p);

        let bare = LaunchOptions::agent(engine);
        let LaunchOptions::Agent { profile, options, initial_prompt, .. } = &bare else {
            prop_assert!(false, "agent() did not build an Agent");
            unreachable!()
        };
        prop_assert!(profile.is_empty() && options.as_object().is_some_and(|o| o.is_empty()) && initial_prompt.is_empty());
    }

    /// `ListFilter`: each setter replaces its own field; `matches` is the conjunction.
    #[test]
    fn list_filter_builder_and_matches(
        p1 in session_id(), p2 in session_id(),
        t1 in text(), t2 in text(),
        l1 in lifecycle(), l2 in lifecycle(),
        s_parent in prop::option::of(session_id()), s_tags in texts(), s_life in lifecycle(),
    ) {
        let a = ListFilter::new().parent(p1.clone()).parent(p2.clone()).tag(&t1).tag(&t2).lifecycle(l1).lifecycle(l2);
        let b = ListFilter::new().lifecycle(l2).tag(&t2).parent(p2.clone());
        prop_assert_eq!(&a, &b);
        prop_assert_eq!(&a.parent, &Some(p2.clone()));
        prop_assert_eq!(a.tag.as_deref(), Some(t2.as_str()));
        prop_assert_eq!(a.lifecycle, Some(l2));
        let only_tag = ListFilter::new().tag(&t1);
        prop_assert!(only_tag.parent.is_none() && only_tag.lifecycle.is_none());

        let session = session_with(s_parent.clone(), s_tags.clone(), s_life);
        prop_assert!(ListFilter::new().matches(&session), "empty filter must match everything");
        let expected = s_parent.as_ref() == Some(&p2) && s_tags.contains(&t2) && s_life == l2;
        prop_assert_eq!(a.matches(&session), expected);
        prop_assert_eq!(ListFilter::new().parent(p2.clone()).matches(&session), s_parent.as_ref() == Some(&p2));
        prop_assert_eq!(ListFilter::new().tag(&t2).matches(&session), s_tags.contains(&t2));
        prop_assert_eq!(ListFilter::new().lifecycle(l2).matches(&session), s_life == l2);
    }

    #[test]
    fn send_options_from_agent(id in session_id()) {
        prop_assert_eq!(SendOptions::from_agent(id.clone()).sender, Sender::Agent { id });
        prop_assert_eq!(SendOptions::default().sender, Sender::User { via: None });
    }
}

// ---------------------------------------------------------------------------------------------
// 3. Compile-time surface
// ---------------------------------------------------------------------------------------------

#[test]
fn public_types_have_the_promised_auto_traits() {
    assert_send_sync::<TxCore>();
    assert_send_sync::<Box<dyn LlmEngine>>();
    assert_send_sync::<CoreConfig>();
    assert_send_sync::<CreateOptions>();
    assert_send_sync::<LaunchOptions>();
    assert_send_sync::<LaunchPlan>();
    assert_send_sync::<LaunchRequest<'static>>();
    assert_send_sync::<ListFilter>();
    assert_send_sync::<SendOptions>();
    assert_send_sync::<EventType>();
    assert_send_sync::<CoreError>();
    assert_static::<CoreError>();
    assert_copy::<LaunchRequest<'static>>();
    assert_copy::<EventType>();
    assert_copy::<Lifecycle>();

    // Value types are unwind-safe; a `TxCore` is not (it owns `Box<dyn LlmEngine>`), see report.
    assert_unwind_safe::<CoreConfig>();
    assert_unwind_safe::<CreateOptions>();
    assert_unwind_safe::<LaunchPlan>();
    assert_unwind_safe::<ListFilter>();
    assert_unwind_safe::<SendOptions>();
    // `CoreError` is not unwind-safe: it wraps `io::Error`, which std marks `!RefUnwindSafe`.
}

#[test]
fn core_error_boxes_into_a_threadsafe_error_and_crosses_threads() {
    let error = CoreError::InvalidName("x".into());
    let boxed: Box<dyn std::error::Error + Send + Sync + 'static> = Box::new(error);
    let text = std::thread::spawn(move || boxed.to_string())
        .join()
        .unwrap();
    assert!(text.contains("invalid session name"), "{text}");
}

/// The trait object bound is on the trait itself: a non-`Send` adapter does not compile, so a
/// `Send + Sync` one must be accepted here through a generic bound only.
#[test]
fn engine_adapter_object_is_usable_from_another_thread() {
    struct Stub;
    impl LlmEngine for Stub {
        fn on_hook(
            &self,
            _: &tx_core::Session,
            _: &tx_core::LlmEngineHook,
        ) -> tx_core::LlmEngineChange {
            tx_core::LlmEngineChange::NONE
        }

        fn kind(&self) -> EngineKind {
            EngineKind::Claude
        }

        fn ext_key(&self) -> &'static str {
            "stub"
        }

        fn configure(
            &self,
            _: &tx_core::Session,
            options: &serde_json::Value,
        ) -> Result<serde_json::Value, tx_core::EngineError> {
            Ok(options.clone())
        }

        fn start(
            &self,
            _: &LaunchRequest<'_>,
            _: &str,
        ) -> Result<LaunchPlan, tx_core::EngineError> {
            Ok(LaunchPlan::new("true"))
        }

        fn resume(&self, _: &LaunchRequest<'_>) -> Result<LaunchPlan, tx_core::EngineError> {
            Ok(LaunchPlan::new("true"))
        }

        fn fork(
            &self,
            _: &LaunchRequest<'_>,
            _: &tx_core::Session,
            _: &str,
        ) -> Result<LaunchPlan, tx_core::EngineError> {
            Ok(LaunchPlan::new("true"))
        }
    }
    let adapter: Box<dyn LlmEngine> = Box::new(Stub);
    let id = std::thread::spawn(move || adapter.id().to_owned())
        .join()
        .unwrap();
    assert_eq!(id, "stub");
}

// ---------------------------------------------------------------------------------------------
// 4. EventType::ALL
// ---------------------------------------------------------------------------------------------

#[test]
fn event_type_all_is_twelve_unique_lowercase_kebab_names() {
    let all: &[EventType] = EventType::ALL;
    assert_eq!(all.len(), 12);
    let variants: BTreeSet<&str> = all
        .iter()
        .map(|t| format!("{t:?}").leak() as &str)
        .collect();
    assert_eq!(variants.len(), 12, "duplicate variant in ALL");
    let names: BTreeSet<&'static str> = all.iter().map(|t| t.as_str()).collect();
    assert_eq!(names.len(), 12, "duplicate as_str in ALL");
    for name in names {
        assert!(!name.is_empty());
        assert!(
            !name.starts_with('-') && !name.ends_with('-') && !name.contains("--"),
            "{name}"
        );
        assert!(
            name.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'),
            "{name}"
        );
    }
    // `as_str` is a function of the variant, so equal variants give equal names and vice versa.
    for (i, a) in all.iter().enumerate() {
        for (j, b) in all.iter().enumerate() {
            assert_eq!(i == j, a == b);
            assert_eq!(a == b, a.as_str() == b.as_str());
        }
    }
}

// ---------------------------------------------------------------------------------------------
// 5. CoreConfig round trip and the socket-length boundary
// ---------------------------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn core_config_builder_round_trip(
        state in abs_path(), sock1 in abs_path(), sock2 in abs_path(),
        env in pairs(), pd1 in paths(), pd2 in paths(), sd in paths(),
    ) {
        let mut expected = CoreConfig::new(Directories::under(&state));
        expected.tmux_socket = Some(sock2.clone());
        expected.env = env.clone();
        expected.profile_dirs = pd1.iter().chain(&pd2).cloned().collect();
        expected.skill_dirs = sd.clone();

        let mut built = CoreConfig::new(Directories::under(&state)).tmux_socket(&sock1).tmux_socket(&sock2).profile_dirs(pd1.clone()).profile_dirs(pd2.clone()).skill_dirs(sd.clone());
        for (k, v) in &env {
            built = built.env(k, v);
        }
        prop_assert_eq!(&built, &expected);
        prop_assert_eq!(built.socket_path(), sock2.clone());
        prop_assert_eq!(built.tmux_conf_path(), state.join("data/tmux.conf"));

        let mut reversed = CoreConfig::new(Directories::under(&state)).skill_dirs(sd.clone());
        for (k, v) in &env {
            reversed = reversed.env(k, v);
        }
        reversed = reversed.profile_dirs(pd1.clone()).profile_dirs(pd2.clone()).tmux_socket(&sock2);
        prop_assert_eq!(&reversed, &expected);

        let bare = CoreConfig::new(Directories::under(&state));
        prop_assert_eq!(bare.socket_path(), state.join("run/tmux.sock"));
        prop_assert_eq!(bare.tmux_conf_path(), state.join("data/tmux.conf"));
        prop_assert!(bare.tmux_socket.is_none() && bare.env.is_empty() && bare.profile_dirs.is_empty() && bare.skill_dirs.is_empty());
        // Only `tmux_socket` affects `socket_path`; the data dir alone decides the conf path.
        prop_assert_eq!(CoreConfig::new(Directories::under(&state)).tmux_socket(&sock1).tmux_conf_path(), state.join("data/tmux.conf"));
        prop_assert_eq!(CoreConfig::new(Directories::under(&state)).env("A", "b").profile_dirs(pd1.clone()).socket_path(), state.join("run/tmux.sock"));
    }

    /// `check_socket_path` is a pure byte-length test on `socket_path()`: ≤ 100 ok, > 100 error
    /// naming that path; a builder-set socket is what is measured, not the run dir.
    #[test]
    fn check_socket_path_boundary_with_builder_socket(len in 1usize..=2 * MAX_SOCKET_PATH_BYTES, state_len in 1usize..=200) {
        let socket = PathBuf::from(format!("/{}", "s".repeat(len - 1)));
        prop_assert_eq!(socket.as_os_str().len(), len);
        let state = PathBuf::from(format!("/{}", "d".repeat(state_len - 1)));
        let config = CoreConfig::new(Directories::under(&state)).tmux_socket(&socket);
        let result = config.check_socket_path();
        if len <= MAX_SOCKET_PATH_BYTES {
            prop_assert!(result.is_ok(), "{} bytes must pass", len);
        } else {
            let error = result.unwrap_err();
            let names_socket = matches!(&error, CoreError::SocketPathTooLong(p) if *p == socket);
            prop_assert!(names_socket, "expected SocketPathTooLong(socket), got {}", kind(&error));
            let text = error.to_string();
            prop_assert!(text.contains(&format!("{len} bytes")), "{}", text);
            prop_assert!(text.contains(&format!("{MAX_SOCKET_PATH_BYTES} allowed")), "{}", text);
        }
        // Without a builder socket, the run dir decides.
        let default_len = state.join("run/tmux.sock").as_os_str().len();
        prop_assert_eq!(CoreConfig::new(Directories::under(&state)).check_socket_path().is_ok(), default_len <= MAX_SOCKET_PATH_BYTES);
    }
}

#[test]
fn check_socket_path_counts_bytes_not_chars() {
    // 'é' is 2 bytes: 1 + 49 * 2 = 99, + 1 more char = 101.
    let ok = CoreConfig::new(Directories::under("/s")).tmux_socket(format!("/{}", "é".repeat(49)));
    assert_eq!(ok.socket_path().as_os_str().len(), 99);
    assert!(ok.check_socket_path().is_ok());
    let too_long =
        CoreConfig::new(Directories::under("/s")).tmux_socket(format!("/{}", "é".repeat(50)));
    assert_eq!(too_long.socket_path().as_os_str().len(), 101);
    assert!(matches!(
        too_long.check_socket_path(),
        Err(CoreError::SocketPathTooLong(_))
    ));
    let exact =
        CoreConfig::new(Directories::under("/s")).tmux_socket(format!("/{}x", "é".repeat(49)));
    assert_eq!(exact.socket_path().as_os_str().len(), MAX_SOCKET_PATH_BYTES);
    assert!(exact.check_socket_path().is_ok());
}

/// Observation, not a failure: the only check on a builder-set socket is its length. An empty
/// or relative socket path sails through `check_socket_path` and `TxCore::open`, and is only
/// discovered when tmux is first invoked with `-S ""`.
#[test]
fn empty_or_relative_socket_is_refused_before_open() {
    let dir = tempfile::Builder::new()
        .prefix("txcf")
        .tempdir_in("/tmp")
        .unwrap();
    for bad in ["", "rel.sock", "."] {
        let config = CoreConfig::new(Directories::under(dir.path().join("state"))).tmux_socket(bad);
        let refused = matches!(
            config.check_socket_path(),
            Err(CoreError::SocketPathNotAbsolute(_))
        );
        assert!(refused, "{bad:?}");
        assert!(TxCore::open(config).is_err());
        assert!(!dir.path().join("state").exists());
    }
}

#[test]
fn open_keeps_the_config_verbatim() {
    let dir = short_tempdir();
    let config = CoreConfig::new(Directories::under(dir.path().join("state")))
        .tmux_socket(dir.path().join("k.sock"))
        .env("A", "1")
        .profile_dirs(["/p"])
        .skill_dirs(["/s"]);
    let core = TxCore::open(config.clone()).unwrap();
    assert_eq!(core.config(), &config);
    assert_eq!(core.socket_path(), dir.path().join("k.sock"));
    assert!(core.engine_kinds().is_empty());
}
