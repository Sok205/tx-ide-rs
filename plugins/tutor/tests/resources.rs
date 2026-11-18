//! The tutor plugin's packs: compiled into the program, published as no resource.

use std::path::{Path, PathBuf};

use tx_core::{CoreConfig, PluginConfig, TxCore};
use tx_tutor::{TutorPlugin, bundle};

fn core(config: &PluginConfig) -> (tempfile::TempDir, TxCore) {
    let dir = tempfile::Builder::new()
        .prefix("tx")
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
fn the_program_carries_every_file_of_the_packs_tree() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("packs");
    let mut expected = Vec::new();
    files(&root, &root, &mut expected);
    // Lint config is not a pack file.
    expected.retain(|name| name != "tutor/.markdownlint-cli2.yaml");
    expected.sort();
    let mut names: Vec<String> = bundle::FILES
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();
    names.sort();
    assert_eq!(names, expected);
    for (name, bytes) in bundle::FILES {
        let on_disk = std::fs::read(root.join(name.strip_prefix("tutor/").unwrap())).unwrap();
        assert_eq!(*bytes, on_disk.as_slice(), "{name}");
    }
}

#[test]
fn the_lesson_lua_talks_to_the_cores_server() {
    let (name, bytes) = bundle::LESSON_LUA;
    assert_eq!(name, "tutor/lesson.lua");
    let text = std::str::from_utf8(bytes).unwrap();
    assert!(text.contains("vim.g.tx_tutor_tmux"), "{text}");
    assert!(!text.contains("{ \"tmux\", \"send-keys\""), "{text}");
}

#[test]
fn the_plugin_publishes_no_resource() {
    let (_dir, core) = core(&PluginConfig::default());
    assert!(core.plugin_resources("").is_empty());
    assert!(core.verb("tutor").is_some());
}

#[test]
fn a_disabled_plugin_has_no_verb() {
    let mut config = PluginConfig::default();
    config.disabled.insert("tutor".to_owned());
    let (_dir, core) = core(&config);
    assert!(core.plugin_resources("").is_empty());
    assert!(core.verb("tutor").is_none());
}
