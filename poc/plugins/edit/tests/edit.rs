//! `apply_edit` against a real core: what a saved form writes, and what it must leave alone.

use std::path::{Path, PathBuf};
use std::process::Command;

use tx_core::{
    CoreConfig, CoreError, CreateOptions, EngineError, LaunchOptions, LaunchPlan, LaunchRequest,
    LlmEngine, LlmEngineChange, LlmEngineHook, Session, SessionId, TxCore,
};
use tx_edit::{Ext, apply_edit};

/// A test engine: an agent that just stays alive.
struct SleepStub;

impl LlmEngine for SleepStub {
    fn id(&self) -> &str {
        "stub"
    }

    fn ext_key(&self) -> &'static str {
        "stub"
    }

    fn configure(
        &self,
        _: &Session,
        options: &serde_json::Value,
    ) -> Result<serde_json::Value, EngineError> {
        Ok(options.clone())
    }

    fn launch(&self, _: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
        Ok(LaunchPlan::new("sleep").args(["60".to_owned()]))
    }

    fn on_hook(&self, _: &Session, _: &LlmEngineHook) -> LlmEngineChange {
        LlmEngineChange::NONE
    }
}

/// A git repository with one commit: an agent's starting checkout, isolated from the host's
/// git config.
fn repository(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("README.md"), "hello\n").unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "README.md"],
        &["commit", "-q", "-m", "init"],
    ] {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

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

    fn directories(&self) -> tx_core::Directories {
        tx_core::Directories::under(self.dir.path().join("home"))
    }

    fn core(&self) -> TxCore {
        let mut core = TxCore::open(CoreConfig::new(self.directories())).unwrap();
        core.register_engine(Box::new(SleepStub)).unwrap();
        core
    }

    fn create(&self, core: &TxCore, name: &str) -> SessionId {
        let cwd = self.dir.path().join("work").join(name);
        repository(&cwd);
        core.create_session(CreateOptions::new(
            name,
            cwd,
            LaunchOptions::agent("stub").initial_prompt("start"),
        ))
        .unwrap()
    }

    fn record(&self, id: &SessionId) -> PathBuf {
        self.directories().data.join(format!("sessions/{id}.json"))
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(self.directories().run.join("tmux.sock"))
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
        after.ext::<Ext>("edit").unwrap().unwrap().group.as_deref(),
        Some("g")
    );
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(harness.record(&id)).unwrap()).unwrap();
    assert_eq!(json["name"], "b");
    assert_eq!(json["ext"]["edit"]["group"], "g");
}

#[test]
fn apply_edit_keeps_other_ext_fields() {
    require_tmux!();
    let harness = Harness::new();
    let core = harness.core();
    let id = harness.create(&core, "a");
    core.update_ext(&id, "edit", |v| {
        *v = serde_json::json!({"kind": "nvim", "group": "old", "attached_to": "x"});
    })
    .unwrap();
    let before = core.get_session(&id).unwrap();
    apply_edit(&core, &id, &before, edited("a", "", "new")).unwrap();
    let ext = core
        .get_session(&id)
        .unwrap()
        .ext::<Ext>("edit")
        .unwrap()
        .unwrap();
    assert_eq!(ext.group.as_deref(), Some("new"));
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(harness.record(&id)).unwrap()).unwrap();
    assert_eq!(json["ext"]["edit"]["kind"], "nvim");
    assert_eq!(json["ext"]["edit"]["attached_to"], "x");
}

#[test]
fn apply_edit_replaces_a_non_object_ext() {
    require_tmux!();
    let harness = Harness::new();
    let core = harness.core();
    let id = harness.create(&core, "a");
    core.update_ext(&id, "edit", |v| *v = serde_json::json!(5))
        .unwrap();
    let before = core.get_session(&id).unwrap();
    apply_edit(&core, &id, &before, edited("a", "", "g")).unwrap();
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(harness.record(&id)).unwrap()).unwrap();
    assert_eq!(json["ext"]["edit"], serde_json::json!({"group": "g"}));
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
