//! The data layer against a real core with no agents, no tmux server and no artifacts plugin.

use tx_client::tmux::Ui;
use tx_core::{CoreConfig, PluginConfig, TxCore};
use tx_home::HomePlugin;
use tx_home::data::{assistant_state, context};
use tx_home::detail::artifact_counts;
use tx_home::model::AssistantState;
use tx_home::project::ProjectCache;

fn core() -> (tempfile::TempDir, TxCore) {
    let dir = tempfile::Builder::new()
        .prefix("tx")
        .tempdir_in("/tmp")
        .unwrap();
    let mut core = TxCore::open(CoreConfig::new(tx_core::Directories::under(
        dir.path().join("state"),
    )))
    .unwrap();
    core.plug(vec![Box::new(HomePlugin)], &PluginConfig::default())
        .unwrap();
    (dir, core)
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
    assert_eq!(gathered.keys.len(), 1);
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
