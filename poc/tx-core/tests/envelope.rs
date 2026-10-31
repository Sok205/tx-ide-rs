//! Seam 4: an engine may frame incoming messages its own way. Process sessions and agents
//! whose engine is not registered in this core get the default envelope.

mod common;

use std::time::Duration;

use common::{Harness, STUB_ID, agent, process, typed, wait_for};
use tx_core::{
    CreateOptions, EngineError, LaunchOptions, LaunchPlan, LaunchRequest, LlmEngine, SendOptions,
};

struct Shout;

impl LlmEngine for Shout {
    fn on_hook(
        &self,
        _: tx_core::LlmEngineState,
        _: &tx_core::LlmEngineHook,
    ) -> Option<tx_core::LlmEngineState> {
        None
    }

    fn id(&self) -> &str {
        "shout"
    }

    fn launch(&self, _: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
        Ok(LaunchPlan::new("sh").args(["-c", "env > env.txt; cat > input.txt"]))
    }

    fn envelope(&self, sender: Option<&str>, text: &str) -> String {
        format!("[{}] {}", sender.unwrap_or("USER"), text.to_uppercase())
    }
}

#[test]
fn the_recipients_engine_frames_the_message() {
    require_tmux!();
    let h = Harness::new();
    let mut core = h.open_with_engine();
    core.register_engine(Box::new(Shout)).unwrap();
    let cwd = h.workdir("s");
    let shouter = core
        .create_session(CreateOptions::new(
            "shouter",
            &cwd,
            LaunchOptions::agent("shout"),
        ))
        .unwrap();
    let plain_dir = h.workdir("p");
    let plain = core.create_session(process("plain", &plain_dir)).unwrap();
    let stub_dir = h.workdir("a");
    let stub = core.create_session(agent("stubbed", &stub_dir)).unwrap();
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
    core.send_message(&stub, "hi", SendOptions::default())
        .unwrap();
    assert!(wait_for(Duration::from_secs(5), || typed(&cwd)
        .lines()
        .count()
        == 2));
    assert_eq!(typed(&cwd), "[USER] HELLO\n[plain] FROM PLAIN\n");
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
        == 3));
    assert!(
        typed(&cwd).ends_with("<from-user>fallback</from-user>\n"),
        "{}",
        typed(&cwd)
    );
    let _ = STUB_ID;
}
