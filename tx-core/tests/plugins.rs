//! The plugin host: one trait, three slots, a config that switches plugins off.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use common::{Harness, STUB_KIND, ShellStub, agent};
use serde_json::json;
use tx_core::plugin::{Activation, Entry, ExecContext, Index};
use tx_core::updates::Outcome;
use tx_core::{
    Event, EventSink, Plugin, PluginConfig, PluginError, PluginStatus, Registry, Resource, TxCore,
    Updater, Verb, VerbError, VerbIo,
};

struct Echo;
impl Verb for Echo {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn usage(&self) -> &'static str {
        "echo WORD...   prints its arguments"
    }
    fn run(&self, _core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        writeln!(io.stdout, "{}", argv.join(" ")).unwrap();
        Ok(0)
    }
}

#[derive(Default)]
struct Seen(Mutex<Vec<String>>);
impl EventSink for Seen {
    fn on_event(&self, event: &Event) {
        self.0.lock().unwrap().push(event.name.clone());
    }
}

/// Fills every slot and remembers its settings.
struct Full {
    seen: Arc<Seen>,
    settings: Arc<Mutex<serde_json::Value>>,
}
impl Plugin for Full {
    fn id(&self) -> &'static str {
        "full"
    }
    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        *self.settings.lock().unwrap() = r.settings().clone();
        r.engine(Box::new(ShellStub));
        r.sink(Box::new(Arc::clone(&self.seen)));
        r.verb(Arc::new(Echo));
        Ok(())
    }
}

/// (plugin id, verb name)
struct Named(&'static str, &'static str);
struct NamedVerb(&'static str);
impl Verb for NamedVerb {
    fn name(&self) -> &'static str {
        self.0
    }
    fn usage(&self) -> &'static str {
        ""
    }
    fn run(&self, _: &TxCore, _: &mut VerbIo<'_>, _: &[String]) -> Result<i32, VerbError> {
        Ok(0)
    }
}
impl Plugin for Named {
    fn id(&self) -> &'static str {
        self.0
    }
    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.verb(Arc::new(NamedVerb(self.1)));
        Ok(())
    }
}

struct EngineOnly(&'static str);
impl Plugin for EngineOnly {
    fn id(&self) -> &'static str {
        self.0
    }
    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.engine(Box::new(ShellStub));
        Ok(())
    }
}

struct Broken;
impl Plugin for Broken {
    fn id(&self) -> &'static str {
        "broken"
    }
    fn register(&self, _: &mut Registry) -> Result<(), PluginError> {
        Err(PluginError::Register {
            plugin: "broken".into(),
            reason: "no".into(),
        })
    }
}

fn full() -> (Box<dyn Plugin>, Arc<Seen>, Arc<Mutex<serde_json::Value>>) {
    let seen = Arc::new(Seen::default());
    let settings = Arc::new(Mutex::new(serde_json::Value::Null));
    (
        Box::new(Full {
            seen: Arc::clone(&seen),
            settings: Arc::clone(&settings),
        }),
        seen,
        settings,
    )
}

#[test]
fn a_plugin_fills_all_three_slots() {
    require_tmux!();
    let h = Harness::new();
    let mut core = h.open();
    let (plugin, seen, settings) = full();
    let cfg = PluginConfig {
        settings: BTreeMap::from([("full".into(), json!({"k": 1}))]),
        ..Default::default()
    };
    core.plug(vec![plugin], &cfg).unwrap();
    assert_eq!(core.engine_kinds(), vec![STUB_KIND]);
    assert_eq!(*settings.lock().unwrap(), json!({"k": 1}));
    core.create_agent(agent("alice", &h.repository("a")))
        .unwrap();
    assert_eq!(*seen.0.lock().unwrap(), vec!["alice".to_owned()]);
    let verb = core.verb("echo").expect("verb registered");
    let mut out = Vec::new();
    let (mut stdin, mut err) = (std::io::empty(), Vec::new());
    let mut io = VerbIo {
        stdin: &mut stdin,
        stdout: &mut out,
        stderr: &mut err,
    };
    assert_eq!(
        verb.run(&core, &mut io, &["a".into(), "b".into()]).unwrap(),
        0
    );
    assert_eq!(String::from_utf8(out).unwrap(), "a b\n");
    assert_eq!(
        core.plugin_status(),
        &[("full".to_owned(), PluginStatus::Active)]
    );
}

#[test]
fn disabled_plugin_registers_nothing() {
    let h = Harness::new();
    let mut core = h.open();
    let (plugin, _, settings) = full();
    let cfg = PluginConfig {
        disabled: BTreeSet::from(["full".to_owned()]),
        ..Default::default()
    };
    core.plug(vec![plugin], &cfg).unwrap();
    assert!(core.engine_kinds().is_empty());
    assert!(core.verb("echo").is_none());
    assert!(settings.lock().unwrap().is_null(), "register never ran");
    assert_eq!(
        core.plugin_status(),
        &[("full".to_owned(), PluginStatus::Disabled)]
    );
}

#[test]
fn settings_default_to_null() {
    let h = Harness::new();
    let mut core = h.open();
    let (plugin, _, settings) = full();
    core.plug(vec![plugin], &PluginConfig::default()).unwrap();
    assert!(settings.lock().unwrap().is_null());
}

#[test]
fn unknown_ids_in_config_are_errors() {
    let h = Harness::new();
    for cfg in [
        PluginConfig {
            disabled: BTreeSet::from(["nope".to_owned()]),
            ..Default::default()
        },
        PluginConfig {
            settings: BTreeMap::from([("nope".to_owned(), json!(1))]),
            ..Default::default()
        },
    ] {
        let mut core = h.open();
        let error = core
            .plug(vec![Box::new(Named("a", "x"))], &cfg)
            .unwrap_err();
        assert!(
            matches!(error, PluginError::Unknown(ref id) if id == "nope"),
            "{error}"
        );
    }
}

#[test]
fn duplicate_verb_names_the_two_plugins() {
    let h = Harness::new();
    let mut core = h.open();
    let error = core
        .plug(
            vec![Box::new(Named("a", "x")), Box::new(Named("b", "x"))],
            &PluginConfig::default(),
        )
        .unwrap_err();
    match error {
        PluginError::DuplicateVerb {
            verb,
            first,
            second,
        } => {
            assert_eq!(
                (verb.as_str(), first.as_str(), second.as_str()),
                ("x", "a", "b")
            );
        }
        other => panic!("{other}"),
    }
}

#[test]
fn reserved_verb_is_refused() {
    let h = Harness::new();
    let mut core = h.open();
    let cfg = PluginConfig {
        reserved: BTreeSet::from(["ls".to_owned()]),
        ..Default::default()
    };
    let error = core
        .plug(vec![Box::new(Named("a", "ls"))], &cfg)
        .unwrap_err();
    assert!(
        matches!(error, PluginError::ReservedVerb { ref verb, ref plugin } if verb == "ls" && plugin == "a")
    );
}

#[test]
fn duplicate_engine_kind_fails_with_plugin_name() {
    let h = Harness::new();
    let mut core = h.open();
    let error = core
        .plug(
            vec![Box::new(EngineOnly("e1")), Box::new(EngineOnly("e2"))],
            &PluginConfig::default(),
        )
        .unwrap_err();
    assert!(
        matches!(error, PluginError::Engine { ref plugin, .. } if plugin == "e2"),
        "{error}"
    );
}

#[test]
fn bad_and_duplicate_plugin_ids() {
    let h = Harness::new();
    let mut core = h.open();
    let error = core
        .plug(
            vec![Box::new(Named("Bad_Id", "x"))],
            &PluginConfig::default(),
        )
        .unwrap_err();
    assert!(matches!(error, PluginError::InvalidId(_)), "{error}");
    let mut core = h.open();
    let error = core
        .plug(
            vec![Box::new(Named("a", "x")), Box::new(Named("a", "y"))],
            &PluginConfig::default(),
        )
        .unwrap_err();
    assert!(
        matches!(error, PluginError::DuplicatePlugin(ref id) if id == "a"),
        "{error}"
    );
}

#[test]
fn a_plugins_own_error_surfaces() {
    let h = Harness::new();
    let mut core = h.open();
    let error = core
        .plug(vec![Box::new(Broken)], &PluginConfig::default())
        .unwrap_err();
    assert_eq!(error.to_string(), "plugin \"broken\": no");
}

#[test]
fn verbs_are_listed_in_name_order() {
    let h = Harness::new();
    let mut core = h.open();
    core.plug(
        vec![Box::new(Named("a", "zeta")), Box::new(Named("b", "alpha"))],
        &PluginConfig::default(),
    )
    .unwrap();
    let verbs = core.verbs();
    let names: Vec<_> = verbs.iter().map(|v| v.name()).collect();
    assert_eq!(names, ["alpha", "zeta"]);
}

#[test]
fn preview_lists_verbs_without_opening_a_core() {
    let h = Harness::new();
    let cfg = PluginConfig {
        disabled: BTreeSet::from(["b".to_owned()]),
        ..Default::default()
    };
    let plugins: Vec<Box<dyn Plugin>> =
        vec![Box::new(Named("a", "zeta")), Box::new(Named("b", "alpha"))];
    let verbs = tx_core::plugin::preview_verbs(&plugins, &cfg, &h.config()).unwrap();
    let names: Vec<_> = verbs.iter().map(|v| v.name()).collect();
    assert_eq!(names, ["zeta"], "disabled plugin's verb is absent");
    assert!(!h.home().exists(), "preview never touches the directories");
    let error = tx_core::plugin::preview_verbs(
        &[Box::new(Named("a", "ls"))],
        &PluginConfig {
            reserved: BTreeSet::from(["ls".to_owned()]),
            ..Default::default()
        },
        &h.config(),
    )
    .err()
    .expect("reserved verb is refused");
    assert!(matches!(error, PluginError::ReservedVerb { .. }));
}

/// (plugin id, resources it publishes in this order)
struct Publisher(&'static str, &'static [(&'static str, &'static [u8])]);
impl Plugin for Publisher {
    fn id(&self) -> &'static str {
        self.0
    }
    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        for (name, bytes) in self.1 {
            r.resource(name, bytes)?;
        }
        Ok(())
    }
}

fn pairs(found: &[Resource]) -> Vec<(&str, &str)> {
    found
        .iter()
        .map(|r| (r.plugin.as_str(), r.name.as_str()))
        .collect()
}

#[test]
fn resources_are_listed_by_prefix_in_name_order() {
    let h = Harness::new();
    let mut core = h.open();
    core.plug(
        vec![Box::new(Publisher(
            "a",
            &[("keys/a.tmux", b"# a"), ("bin/a", b"#!/bin/sh")],
        ))],
        &PluginConfig::default(),
    )
    .unwrap();
    let keys = core.resources("keys/");
    assert_eq!(pairs(&keys), [("a", "keys/a.tmux")]);
    assert_eq!(keys[0].bytes, b"# a");
    assert_eq!(
        pairs(&core.plugin_resources("")),
        [("a", "bin/a"), ("a", "keys/a.tmux")]
    );
    assert!(core.resources("conf/").is_empty());
}

#[test]
fn resources_follow_manifest_order_then_name_order() {
    let h = Harness::new();
    let mut core = h.open();
    core.plug(
        vec![
            Box::new(Publisher("z", &[("keys/z", b"z"), ("keys/b", b"b")])),
            Box::new(Publisher("a", &[("keys/a", b"a")])),
        ],
        &PluginConfig::default(),
    )
    .unwrap();
    assert_eq!(
        pairs(&core.resources("keys/")),
        [("z", "keys/b"), ("z", "keys/z"), ("a", "keys/a")]
    );
}

#[test]
fn duplicate_resource_across_plugins_fails_plug() {
    let h = Harness::new();
    let mut core = h.open();
    let error = core
        .plug(
            vec![
                Box::new(Publisher("a", &[("keys/x", b"1")])),
                Box::new(Publisher("b", &[("keys/x", b"2")])),
            ],
            &PluginConfig::default(),
        )
        .unwrap_err();
    match error {
        PluginError::DuplicateResource(name) => assert_eq!(name, "keys/x"),
        other => panic!("{other}"),
    }
}

#[test]
fn duplicate_resource_inside_one_plugin_is_refused() {
    let h = Harness::new();
    let mut core = h.open();
    let error = core
        .plug(
            vec![Box::new(Publisher(
                "a",
                &[("keys/x", b"1"), ("keys/x", b"2")],
            ))],
            &PluginConfig::default(),
        )
        .unwrap_err();
    assert!(matches!(error, PluginError::DuplicateResource(n) if n == "keys/x"));
}

/// Probes `Registry::resource` directly: bad names are refused, good ones accepted.
struct NameProbe;
impl Plugin for NameProbe {
    fn id(&self) -> &'static str {
        "probe"
    }
    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        for bad in [
            "../x", "/x", "", "a/../b", "a/..", "..", "a b", "a\\b", "é", "a\n",
        ] {
            match r.resource(bad, b"") {
                Err(PluginError::InvalidResourceName(name)) => assert_eq!(name, bad),
                other => panic!("{bad:?} accepted: {other:?}"),
            }
        }
        for good in ["x", "keys/kill.tmux", "a_b-c.d/e", "..x", "x..", "a/.b"] {
            r.resource(good, b"").unwrap();
        }
        Ok(())
    }
}

#[test]
fn invalid_resource_names_are_refused() {
    let h = Harness::new();
    let mut core = h.open();
    core.plug(vec![Box::new(NameProbe)], &PluginConfig::default())
        .unwrap();
    assert_eq!(
        core.plugin_resources("").len(),
        6,
        "only the valid names landed"
    );
}

#[test]
fn invalid_resource_name_fails_plug_through_the_plugin() {
    let h = Harness::new();
    let mut core = h.open();
    let error = core
        .plug(
            vec![Box::new(Publisher("a", &[("../x", b"")]))],
            &PluginConfig::default(),
        )
        .unwrap_err();
    assert!(matches!(error, PluginError::InvalidResourceName(n) if n == "../x"));
}

#[test]
fn disabled_plugin_publishes_no_resources() {
    let h = Harness::new();
    let mut core = h.open();
    let cfg = PluginConfig {
        disabled: BTreeSet::from(["a".to_owned()]),
        ..Default::default()
    };
    core.plug(
        vec![
            Box::new(Publisher("a", &[("keys/x", b"1")])),
            Box::new(Publisher("b", &[("keys/x", b"2"), ("keys/y", b"3")])),
        ],
        &cfg,
    )
    .unwrap();
    assert_eq!(
        pairs(&core.plugin_resources("")),
        [("b", "keys/x"), ("b", "keys/y")]
    );
}

/// A plugin on disk, `<root>/<id>`: its manifest declares `verbs`, each run by `bin/<id>` with
/// the verb's name before the caller's arguments; `script` is that program, when it has one.
fn install(root: &Path, id: &str, verbs: &[&str], script: Option<&str>) -> PathBuf {
    let directory = root.join(id);
    std::fs::create_dir_all(directory.join("bin")).unwrap();
    let mut manifest = format!("api: 1\nid: {id}\ndescription: The {id} plugin.\n");
    if !verbs.is_empty() {
        manifest.push_str("verbs:\n");
    }
    for verb in verbs {
        manifest.push_str(&format!(
            "  - name: {verb}\n    usage: \"{verb} WORD...   a verb of {id}\"\n    bin: {id}\n    args: [{verb}]\n"
        ));
    }
    std::fs::write(directory.join("plugin.yaml"), manifest).unwrap();
    if let Some(script) = script {
        let program = directory.join("bin").join(id);
        std::fs::write(&program, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(
            &program,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
    }
    directory
}

/// A core on `h` whose plugin root is `<dir>/plugins-root`, with the plugins found there
/// plugged in as external ones: every one enabled unless `entries` says otherwise.
fn external_core(
    h: &Harness,
    entries: &BTreeMap<String, Entry>,
    context: ExecContext,
    linked: Vec<Box<dyn Plugin>>,
) -> TxCore {
    let config = h.config().plugin_dirs([root_of(h)]);
    let index = Index::load(&config, "0.1.0");
    let activation = Activation::new(&index, entries, &["ls"]);
    let hosted: Vec<&'static str> = linked.iter().map(|plugin| plugin.id()).collect();
    let mut core = TxCore::open(config).unwrap();
    core.plug(linked, &PluginConfig::default()).unwrap();
    core.plug_external(&index, &activation, context, &hosted)
        .unwrap();
    core
}

fn root_of(h: &Harness) -> PathBuf {
    h.dir.path().join("plugins-root")
}

/// Runs `verb` with `argv`, nothing on stdin: its exit code, output and error.
fn run_verb(core: &TxCore, verb: &str, argv: &[&str]) -> (Result<i32, VerbError>, String, String) {
    let verb = core.verb(verb).expect("the verb is registered");
    let argv: Vec<String> = argv.iter().map(|word| (*word).to_owned()).collect();
    let (mut stdin, mut out, mut err) = (std::io::empty(), Vec::new(), Vec::new());
    let result = verb.run(
        core,
        &mut VerbIo {
            stdin: &mut stdin,
            stdout: &mut out,
            stderr: &mut err,
        },
        &argv,
    );
    (
        result,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

const HELLO: &str = r#"echo "args: $*"
echo "api: $TX_PLUGIN_API id: $TX_PLUGIN_ID"
echo "dir: $TX_PLUGIN_DIR"
echo "data: $TX_PLUGIN_DATA_DIR"
echo "settings: $TX_PLUGIN_SETTINGS"
echo "tx: $TX_BIN app: $TX_APPNAME"
echo "stdin: [$(cat)]"
echo "to stderr" >&2
[ "$2" = fail ] && exit 7
[ "$2" = die ] && kill -TERM $$
exit 0"#;

#[test]
fn a_proxy_verb_starts_the_plugins_program_and_hands_back_what_it_said() {
    let h = Harness::new();
    let directory = install(&root_of(&h), "hello", &["hello"], Some(HELLO));
    let trace = h.dir.path().join("trace");
    let entries = BTreeMap::from([(
        "hello".to_owned(),
        Entry {
            enabled: true,
            settings: json!({"greeting": "hi"}),
        },
    )]);
    let context = ExecContext {
        environment: vec![
            ("TX_BIN".to_owned(), "/x/tx".to_owned()),
            ("TX_APPNAME".to_owned(), "tx-test".to_owned()),
        ],
        trace: Some(trace.clone()),
    };
    let core = external_core(&h, &entries, context, Vec::new());
    let verb = core
        .verb("hello")
        .expect("the manifest's verb is a verb of the core");
    assert_eq!(verb.name(), "hello");
    assert_eq!(verb.usage(), "hello WORD...   a verb of hello");
    assert!(!trace.exists(), "nothing is traced before a program starts");

    let (result, out, err) = run_verb(&core, "hello", &["a", "b c"]);
    assert_eq!(result.unwrap(), 0);
    let program = directory.join("bin/hello");
    assert_eq!(
        out,
        format!(
            "args: hello a b c\napi: 1 id: hello\ndir: {}\ndata: {}\n\
             settings: {{\"greeting\":\"hi\"}}\ntx: /x/tx app: tx-test\nstdin: []\n",
            directory.display(),
            h.data_dir().join("plugins/hello").display()
        )
    );
    assert_eq!(err, "to stderr\n");
    assert!(
        !h.data_dir().join("plugins/hello").exists(),
        "the data directory is named, not created"
    );

    let (result, out, _) = run_verb(&core, "hello", &["fail"]);
    assert_eq!(result.unwrap(), 7);
    assert!(out.starts_with("args: hello fail\n"), "{out}");
    let (result, _, _) = run_verb(&core, "hello", &["die"]);
    assert_eq!(result.unwrap(), 128 + 15, "killed by SIGTERM");

    let line = format!("verb\thello\t{}\n", program.display());
    assert_eq!(std::fs::read_to_string(&trace).unwrap(), line.repeat(3));
}

#[test]
fn a_plugin_without_settings_gets_null_and_no_trace_is_written_unasked() {
    let h = Harness::new();
    install(&root_of(&h), "hello", &["hello"], Some(HELLO));
    let core = external_core(&h, &BTreeMap::new(), ExecContext::default(), Vec::new());
    let (result, out, _) = run_verb(&core, "hello", &[]);
    assert_eq!(result.unwrap(), 0);
    assert!(out.contains("\nsettings: null\n"), "{out}");
    assert!(out.starts_with("args: hello\n"), "{out}");
}

#[test]
fn a_program_that_cannot_start_is_a_failed_verb() {
    let h = Harness::new();
    let directory = install(&root_of(&h), "hello", &["hello"], None);
    let core = external_core(&h, &BTreeMap::new(), ExecContext::default(), Vec::new());
    let (result, out, err) = run_verb(&core, "hello", &[]);
    let error = result.unwrap_err();
    assert!(matches!(error, VerbError::Failed(_)), "{error:?}");
    assert_eq!(
        error.to_string(),
        format!(
            "cannot run {}: No such file or directory (os error 2)",
            directory.join("bin/hello").display()
        )
    );
    assert!(out.is_empty() && err.is_empty());
}

#[test]
fn installed_plugins_join_the_status_listing_and_hosted_ones_keep_their_verbs() {
    let h = Harness::new();
    let root = root_of(&h);
    // `a` is installed and also linked into this process: it is hosted, not proxied.
    install(&root, "a", &["alpha"], Some("echo proxied"));
    install(&root, "b", &["beta"], Some("echo beta ran"));
    install(&root, "c", &["gamma"], Some("echo gamma ran"));
    std::fs::create_dir_all(root.join("d")).unwrap();
    std::fs::write(root.join("d/plugin.yaml"), "api: 2\n").unwrap();
    let entries = BTreeMap::from([
        (
            "c".to_owned(),
            Entry {
                enabled: false,
                settings: json!(null),
            },
        ),
        (
            "nowhere".to_owned(),
            Entry {
                enabled: true,
                settings: json!(null),
            },
        ),
    ]);
    let core = external_core(
        &h,
        &entries,
        ExecContext::default(),
        vec![Box::new(Named("a", "alpha"))],
    );
    assert_eq!(
        core.plugin_status(),
        &[
            ("a".to_owned(), PluginStatus::Active),
            ("b".to_owned(), PluginStatus::Active),
            ("c".to_owned(), PluginStatus::Disabled),
            (
                "d".to_owned(),
                PluginStatus::Error("api 2 is not supported (this tx: 1)".to_owned())
            ),
        ],
        "an entry that names no plugin is not a plugin"
    );
    let verbs = core.verbs();
    let names: Vec<_> = verbs.iter().map(|verb| verb.name()).collect();
    assert_eq!(names, ["alpha", "beta"], "a disabled plugin has no verb");
    let (result, out, _) = run_verb(&core, "alpha", &[]);
    assert_eq!(result.unwrap(), 0);
    assert_eq!(out, "", "the hosted verb ran in-process");
    assert_eq!(run_verb(&core, "beta", &[]).1, "beta ran\n");
}

#[test]
fn the_activations_owner_answers_for_a_name_also_registered_in_process() {
    let h = Harness::new();
    install(
        &root_of(&h),
        "hello",
        &["hello"],
        Some("echo the installed one"),
    );
    let core = external_core(
        &h,
        &BTreeMap::new(),
        ExecContext::default(),
        vec![Box::new(Named("linked", "hello"))],
    );
    assert_eq!(run_verb(&core, "hello", &[]).1, "the installed one\n");
    assert_eq!(core.verbs().len(), 1);
}

#[test]
fn the_second_directory_of_an_id_is_listed_once() {
    let h = Harness::new();
    let first = h.dir.path().join("first-root");
    let second = h.dir.path().join("second-root");
    install(&first, "hello", &["hello"], Some("echo first"));
    install(&second, "hello", &["hello"], Some("echo second"));
    let config = h.config().plugin_dirs([first, second]);
    let index = Index::load(&config, "0.1.0");
    assert_eq!(index.plugins.len(), 2);
    let activation = Activation::new(&index, &BTreeMap::new(), &[]);
    let mut core = TxCore::open(config).unwrap();
    core.plug_external(&index, &activation, ExecContext::default(), &[])
        .unwrap();
    assert_eq!(
        core.plugin_status(),
        &[("hello".to_owned(), PluginStatus::Active)]
    );
    assert_eq!(run_verb(&core, "hello", &[]).1, "first\n");
}

#[test]
fn a_registry_filled_outside_a_core_shows_what_the_plugin_registers() {
    let h = Harness::new();
    let (plugin, _seen, settings) = full();
    let mut registry = Registry::new(json!({"k": 1}), h.config());
    assert!(registry.verbs().is_empty() && registry.engines().is_empty());
    plugin.register(&mut registry).unwrap();
    assert_eq!(*settings.lock().unwrap(), json!({"k": 1}));
    let verbs: Vec<(&str, &str)> = registry
        .verbs()
        .iter()
        .map(|verb| (verb.name(), verb.usage()))
        .collect();
    assert_eq!(verbs, [("echo", "echo WORD...   prints its arguments")]);
    assert_eq!(registry.engines().len(), 1);
    assert_eq!(registry.engines()[0].kind(), STUB_KIND);
    assert_eq!(registry.sinks().len(), 1);
    assert!(registry.updaters().is_empty());
    assert!(!h.home().exists(), "no core was opened");
}

struct OnlyUpdates;
impl Updater for OnlyUpdates {
    fn update(&self, _: &TxCore, io: &mut VerbIo<'_>) -> Result<Outcome, VerbError> {
        writeln!(io.stdout, "updated").unwrap();
        Ok(Outcome {
            respawn_agents: true,
        })
    }
}

#[test]
fn an_updater_has_nothing_to_retire_unless_it_says_so() {
    let h = Harness::new();
    let core = h.open();
    let (mut stdin, mut out, mut err) = (std::io::empty(), Vec::new(), Vec::new());
    let mut io = VerbIo {
        stdin: &mut stdin,
        stdout: &mut out,
        stderr: &mut err,
    };
    assert_eq!(
        OnlyUpdates.retire(&core, &mut io).unwrap(),
        Outcome::default()
    );
    assert!(OnlyUpdates.update(&core, &mut io).unwrap().respawn_agents);
    assert_eq!(out, b"updated\n", "retiring wrote nothing");
}
