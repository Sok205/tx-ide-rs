//! The tutor plugin's resources: the repo's `tutor/` packs, listed through the core.

use std::path::{Path, PathBuf};

use tx_core::{CoreConfig, PluginConfig, TxCore};
use tx_tutor::TutorPlugin;

fn core(config: &PluginConfig) -> (tempfile::TempDir, TxCore) {
    let dir = tempfile::Builder::new()
        .prefix("txc")
        .tempdir_in("/tmp")
        .unwrap();
    let mut core = TxCore::open(CoreConfig::new(tx_core::Directories::under(
        dir.path().join("state"),
    )))
    .unwrap();
    core.plug(vec![Box::new(TutorPlugin)], config).unwrap();
    (dir, core)
}

fn files(dir: &Path, root: &Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(&path, root, out);
        } else {
            let relative = path.strip_prefix(root).unwrap().to_string_lossy();
            out.push(format!("tutor/{relative}"));
        }
    }
}

#[test]
fn ships_every_file_of_the_repos_tutor_tree() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tutor");
    let mut expected = Vec::new();
    files(&root, &root, &mut expected);
    // Lint config is not a pack file.
    expected.retain(|name| name != "tutor/.markdownlint-cli2.yaml");
    expected.sort();
    let (_dir, core) = core(&PluginConfig::default());
    let resources = core.resources("tutor/");
    let mut names: Vec<String> = resources.iter().map(|r| r.name.clone()).collect();
    names.sort();
    assert_eq!(names, expected);
    assert_eq!(
        core.resources("").len(),
        names.len(),
        "only tutor/ resources"
    );
    for resource in &resources {
        if resource.name == "tutor/lesson.lua" {
            continue;
        }
        let on_disk =
            std::fs::read(root.join(resource.name.strip_prefix("tutor/").unwrap())).unwrap();
        assert_eq!(resource.bytes, on_disk.as_slice(), "{}", resource.name);
    }
}

#[test]
fn the_lesson_lua_talks_to_the_cores_server() {
    let (_dir, core) = core(&PluginConfig::default());
    let lua = core.resources("tutor/lesson.lua");
    let text = std::str::from_utf8(lua[0].bytes).unwrap();
    assert!(text.contains("vim.g.tx_tutor_tmux"), "{text}");
    assert!(!text.contains("{ \"tmux\", \"send-keys\""), "{text}");
}

#[test]
fn a_disabled_plugin_publishes_nothing() {
    let mut config = PluginConfig::default();
    config.disabled.insert("tutor".to_owned());
    let (_dir, core) = core(&config);
    assert!(core.resources("").is_empty());
    assert!(core.verb("tutor").is_none());
}
