//! Seam 4: an engine may frame incoming messages its own way (`receive_message`). Agents whose
//! engine frames nothing, or whose engine is not registered in this core, get the default
//! envelope, submitted.

mod common;

use std::time::Duration;

use common::{Harness, agent, cwd_of, typed, wait_for};
use tx_core::{
    CreateOptions, EngineError, EngineKind, EnvelopeSender, Input, LaunchOptions, LaunchPlan,
    LaunchRequest, LlmEngine, SendOptions, Sender,
};

struct Shout;

impl LlmEngine for Shout {
    fn on_hook(
        &self,
        _: &tx_core::Session,
        _: &tx_core::LlmEngineHook,
    ) -> tx_core::LlmEngineChange {
        tx_core::LlmEngineChange::NONE
    }

    /// Codex: the harness's stub holds Claude.
    fn kind(&self) -> EngineKind {
        EngineKind::Codex
    }

    fn ext_key(&self) -> &'static str {
        "shout"
    }

    fn configure(
        &self,
        _: &tx_core::Session,
        options: &serde_json::Value,
    ) -> Result<serde_json::Value, EngineError> {
        Ok(options.clone())
    }

    fn start(&self, _: &LaunchRequest<'_>, _: &str) -> Result<LaunchPlan, EngineError> {
        Ok(LaunchPlan::new("sh").args(["-c", "env > env.txt; cat > input.txt"]))
    }

    fn resume(&self, request: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
        self.start(request, "")
    }

    fn fork(
        &self,
        request: &LaunchRequest<'_>,
        _: &tx_core::Session,
        _: &str,
    ) -> Result<LaunchPlan, EngineError> {
        self.start(request, "")
    }

    fn receive_message(&self, sender: &EnvelopeSender<'_>, text: &str) -> Input {
        let from = match sender {
            EnvelopeSender::User { via } => via.unwrap_or("USER"),
            EnvelopeSender::Agent { name } => name,
        };
        Input::line(format!("[{from}] {}", text.to_uppercase()))
    }
}

#[test]
fn the_recipients_engine_frames_the_message() {
    require_tmux!();
    let h = Harness::new();
    let mut core = h.open_with_engine();
    core.register_engine(Box::new(Shout)).unwrap();
    let shouter = core
        .create_session(CreateOptions::new(
            "shouter",
            h.repository("s"),
            LaunchOptions::agent(EngineKind::Codex).initial_prompt("start"),
        ))
        .unwrap();
    let cwd = cwd_of(&core, &shouter);
    let plain = core
        .create_session(agent("plain", &h.repository("p")))
        .unwrap();
    let plain_dir = cwd_of(&core, &plain);
    let stub = core
        .create_session(agent("stubbed", &h.repository("a")))
        .unwrap();
    let stub_dir = cwd_of(&core, &stub);
    for dir in [&cwd, &plain_dir, &stub_dir] {
        assert!(wait_for(Duration::from_secs(5), || dir
            .join("env.txt")
            .exists()));
    }
    core.send_message(&shouter, "hello", SendOptions::default())
        .unwrap();
    core.send_message(
        &shouter,
        "from plain",
        SendOptions::from_agent(plain.clone()),
    )
    .unwrap();
    core.send_message(&plain, "hi", SendOptions::from_agent(shouter.clone()))
        .unwrap();
    core.send_message(
        &shouter,
        "from notes",
        SendOptions::from(Sender::User {
            via: Some("notes".into()),
        }),
    )
    .unwrap();
    core.send_message(&stub, "hi", SendOptions::default())
        .unwrap();
    assert!(wait_for(Duration::from_secs(5), || typed(&cwd)
        .lines()
        .count()
        == 3));
    assert_eq!(
        typed(&cwd),
        "[USER] HELLO\n[plain] FROM PLAIN\n[notes] FROM NOTES\n"
    );
    assert!(wait_for(Duration::from_secs(5), || !typed(&plain_dir).is_empty()));
    assert_eq!(
        typed(&plain_dir),
        "<from-agent session=\"shouter\">hi</from-agent>\n"
    );
    assert!(wait_for(Duration::from_secs(5), || !typed(&stub_dir).is_empty()));
    assert_eq!(
        typed(&stub_dir),
        "<from-user>hi</from-user>\n",
        "the stub keeps the default"
    );

    // A core without the engine registered falls back to the default envelope.
    let other = h.open();
    other
        .send_message(&shouter, "fallback", SendOptions::default())
        .unwrap();
    assert!(wait_for(Duration::from_secs(5), || typed(&cwd)
        .lines()
        .count()
        == 4));
    assert!(
        typed(&cwd).ends_with("<from-user>fallback</from-user>\n"),
        "{}",
        typed(&cwd)
    );
}
