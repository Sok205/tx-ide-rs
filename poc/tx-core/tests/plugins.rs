//! The plugin host: one trait, three slots, a config that switches plugins off.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use common::{Harness, STUB_KIND, ShellStub, agent};
use serde_json::json;
use tx_core::{
    Event, EventSink, Plugin, PluginConfig, PluginError, PluginStatus, Registry, Resource, TxCore,
    Verb, VerbError, VerbIo,
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
    let names: Vec<_> = core.verbs().iter().map(|v| v.name()).collect();
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
        pairs(&core.resources("")),
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
    assert_eq!(core.resources("").len(), 6, "only the valid names landed");
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
        pairs(&core.resources("")),
        [("b", "keys/x"), ("b", "keys/y")]
    );
}
