//! The data layer against a real core with no agents, no tmux server and no artifacts plugin.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tx_client::tmux::Ui;
use tx_core::{
    CoreConfig, Plugin, PluginConfig, PluginError, Registry, TxCore, Verb, VerbError, VerbIo,
};
use tx_home::HomePlugin;
use tx_home::data::{assistant_state, context};
use tx_home::detail::artifact_counts;
use tx_home::model::AssistantState;
use tx_home::project::ProjectCache;

fn core() -> (tempfile::TempDir, TxCore) {
    core_with(Vec::new())
}

/// A core with the home and `others` plugged in.
fn core_with(others: Vec<Box<dyn Plugin>>) -> (tempfile::TempDir, TxCore) {
    let dir = tempfile::Builder::new()
        .prefix("tx")
        .tempdir_in("/tmp")
        .unwrap();
    let mut core = TxCore::open(CoreConfig::new(tx_core::Directories::under(
        dir.path().join("state"),
    )))
    .unwrap();
    let mut plugins: Vec<Box<dyn Plugin>> = vec![Box::new(HomePlugin)];
    plugins.extend(others);
    core.plug(plugins, &PluginConfig::default()).unwrap();
    (dir, core)
}

/// A stand-in for the `layouts` plugin: its `layout` verb prints one name and counts its runs.
struct CountingLayouts(Arc<AtomicUsize>);

struct CountingLayout(Arc<AtomicUsize>);

impl Verb for CountingLayout {
    fn name(&self) -> &'static str {
        "layout"
    }

    fn usage(&self) -> &'static str {
        "layout ls"
    }

    fn run(&self, _: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        assert_eq!(argv, ["ls"]);
        let run = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        writeln!(io.stdout, "listing-{run}").unwrap();
        Ok(0)
    }
}

impl Plugin for CountingLayouts {
    fn id(&self) -> &'static str {
        "layouts"
    }

    fn register(&self, registry: &mut Registry) -> Result<(), PluginError> {
        registry.verb(Arc::new(CountingLayout(Arc::clone(&self.0))));
        Ok(())
    }
}

#[test]
fn an_empty_core_gives_an_empty_context_and_an_absent_assistant() {
    let (_dir, core) = core();
    let ui = Ui::from_core(&core);
    let mut cache = ProjectCache::new();
    let gathered = context(&core, &ui, &mut cache, None).unwrap();
    assert!(gathered.agents.is_empty());
    assert!(gathered.projects.is_empty());
    assert!(gathered.views.is_empty());
    assert_eq!(gathered.assistant, AssistantState::Absent);
    assert_eq!(gathered.plugins, [("home".to_owned(), true)]);
    assert!(gathered.keys.is_empty(), "nothing is published yet");
    assert_eq!(gathered.layouts, None, "no layout verb");
    assert!(gathered.now > 0.0);
    assert_eq!(assistant_state(&core), AssistantState::Absent);
}

#[test]
fn a_failed_warm_shows_its_reason() {
    let (_dir, core) = core();
    let directory = core.config().plugin_data_dir("assistant");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("warm.failed"), "no claude binary\n").unwrap();
    assert_eq!(
        assistant_state(&core),
        AssistantState::Failed("no claude binary".to_owned())
    );
}

#[test]
fn without_the_artifact_verb_there_are_no_counts() {
    let (_dir, core) = core();
    assert!(artifact_counts(&core, "anything").is_empty());
}

/// The cheat sheet is what `tx update` published under `<data>/keys`, for the active plugins
/// only: a fragment left by a plugin that is not there gives no hint.
#[test]
fn the_key_hints_are_read_from_the_published_fragments_of_the_active_plugins() {
    let (_dir, core) = core();
    let keys = core.config().directories.data.join("keys");
    std::fs::create_dir_all(&keys).unwrap();
    let fragment =
        "#!/usr/bin/env bash\n# keys: m\n# tx key plugin `home`: prefix+m opens the home.\n";
    std::fs::write(keys.join("home.tmux"), fragment).unwrap();
    std::fs::write(
        keys.join("gone.tmux"),
        fragment.replace("keys: m", "keys: g"),
    )
    .unwrap();
    let ui = Ui::from_core(&core);
    let gathered = context(&core, &ui, &mut ProjectCache::new(), None).unwrap();
    assert_eq!(gathered.keys.len(), 1, "{:?}", gathered.keys);
    assert_eq!(gathered.keys[0].plugin, "home");
    assert_eq!(gathered.keys[0].keys, ["m"]);
    assert_eq!(gathered.keys[0].text, "prefix+m opens the home");
}

/// A context is gathered every second and `layout ls` is another plugin's program: it runs for
/// the first context, and again only when `<config>/layouts` changed.
#[test]
fn the_layouts_are_listed_once_and_again_only_when_their_directory_changed() {
    let runs = Arc::new(AtomicUsize::new(0));
    let (_dir, core) = core_with(vec![Box::new(CountingLayouts(Arc::clone(&runs)))]);
    let ui = Ui::from_core(&core);
    let mut cache = ProjectCache::new();
    let listing = |number: usize| Some(vec![format!("listing-{number}")]);

    let first = context(&core, &ui, &mut cache, None).unwrap();
    assert_eq!(first.layouts, listing(1));
    let second = context(&core, &ui, &mut cache, Some(&first)).unwrap();
    assert_eq!(second.layouts, listing(1));
    assert_eq!(runs.load(Ordering::SeqCst), 1, "nothing changed");

    // The directory appears with its first layout.
    let directory = core.config().directories.config.join("layouts");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("pair.yaml"), "panes: []\n").unwrap();
    let third = context(&core, &ui, &mut cache, Some(&second)).unwrap();
    assert_eq!(third.layouts, listing(2));
    let fourth = context(&core, &ui, &mut cache, Some(&third)).unwrap();
    assert_eq!(fourth.layouts, listing(2));
    assert_eq!(runs.load(Ordering::SeqCst), 2, "one listing per change");

    // A context that starts from nothing (a new home) lists for itself.
    assert_eq!(
        context(&core, &ui, &mut cache, None).unwrap().layouts,
        listing(3)
    );
}
