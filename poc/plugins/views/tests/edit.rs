//! `apply_edit` against a real core: what a saved form writes, and what it must leave alone.

use std::path::PathBuf;
use std::process::Command;

use tx_core::{CoreConfig, CoreError, CreateOptions, LaunchOptions, SessionId, TxCore};
use tx_views::Ext;
use tx_views::edit::apply_edit;

struct Harness {
    dir: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        Self {
            dir: tempfile::Builder::new()
                .prefix("txv")
                .tempdir_in("/tmp")
                .unwrap(),
        }
    }

    fn state_dir(&self) -> PathBuf {
        self.dir.path().join("state")
    }

    fn core(&self) -> TxCore {
        TxCore::open(CoreConfig::new(self.state_dir())).unwrap()
    }

    fn create(&self, core: &TxCore, name: &str) -> SessionId {
        let cwd = self.dir.path().join("work").join(name);
        std::fs::create_dir_all(&cwd).unwrap();
        core.create_session(CreateOptions::new(
            name,
            cwd,
            LaunchOptions::process("sleep", ["60"]),
        ))
        .unwrap()
    }

    fn record(&self, id: &SessionId) -> PathBuf {
        self.state_dir().join(format!("sessions/{id}.json"))
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(self.state_dir().join("tmux.sock"))
            .arg("kill-server")
            .output();
    }
}

macro_rules! require_tmux {
    () => {
        if Command::new("tmux").arg("-V").output().is_err() {
            eprintln!("tmux not available; skipping");
            return;
        }
    };
}

fn edited(name: &str, tags: &str, group: &str) -> (String, String, String) {
    (name.to_owned(), tags.to_owned(), group.to_owned())
}

#[test]
fn apply_edit_renames_tags_and_groups() {
    require_tmux!();
    let harness = Harness::new();
    let core = harness.core();
    let id = harness.create(&core, "a");
    let before = core.get_session(&id).unwrap();
    apply_edit(&core, &id, &before, edited("b", "x,,y,", " g ")).unwrap();
    let after = core.get_session(&id).unwrap();
    assert_eq!(after.name, "b");
    assert_eq!(after.tags, ["x", "y"]);
    assert_eq!(
        after.ext::<Ext>("views").unwrap().unwrap().group.as_deref(),
        Some("g")
    );
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(harness.record(&id)).unwrap()).unwrap();
    assert_eq!(json["name"], "b");
    assert_eq!(json["ext"]["views"]["group"], "g");
}

#[test]
fn apply_edit_keeps_other_ext_fields() {
    require_tmux!();
    let harness = Harness::new();
    let core = harness.core();
    let id = harness.create(&core, "a");
    core.update_ext(&id, "views", |v| {
        *v = serde_json::json!({"kind": "nvim", "group": "old"});
    })
    .unwrap();
    let before = core.get_session(&id).unwrap();
    apply_edit(&core, &id, &before, edited("a", "", "new")).unwrap();
    let ext = core
        .get_session(&id)
        .unwrap()
        .ext::<Ext>("views")
        .unwrap()
        .unwrap();
    assert_eq!(ext.kind.as_deref(), Some("nvim"));
    assert_eq!(ext.group.as_deref(), Some("new"));
}

#[test]
fn apply_edit_noop_writes_nothing() {
    require_tmux!();
    let harness = Harness::new();
    let core = harness.core();
    let id = harness.create(&core, "a");
    core.set_session_tags(&id, vec!["x".into(), "y".into()])
        .unwrap();
    let before = core.get_session(&id).unwrap();
    let path = harness.record(&id);
    let bytes = std::fs::read(&path).unwrap();
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    apply_edit(&core, &id, &before, edited("a", "x,y", "")).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);
}

#[test]
fn apply_edit_name_clash_leaves_record() {
    require_tmux!();
    let harness = Harness::new();
    let core = harness.core();
    let id = harness.create(&core, "a");
    harness.create(&core, "taken");
    let before = core.get_session(&id).unwrap();
    let bytes = std::fs::read(harness.record(&id)).unwrap();
    let error = apply_edit(&core, &id, &before, edited("taken", "t", "g")).unwrap_err();
    assert!(matches!(error, CoreError::NameTaken(_)), "{error:?}");
    assert_eq!(std::fs::read(harness.record(&id)).unwrap(), bytes);
}
