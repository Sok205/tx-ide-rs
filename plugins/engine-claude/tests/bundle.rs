//! The bundle says what the code does: `plugin.yaml` against what `ClaudePlugin` registers and
//! what `ClaudeAdapter` answers.

use std::path::Path;

use tx_core::{Agent, Input, LlmEngine};
use tx_engine_claude::{ClaudeAdapter, ClaudePlugin};

#[test]
fn the_manifest_matches_the_plugin() {
    tx_plugin::testing::check_bundle(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        vec![Box::new(ClaudePlugin)],
    );
}

/// Core stores the engine's data under the manifest's `ext_key` and answers `interrupt` from
/// the manifest, without starting the program: both must be what the adapter says, which is
/// what an agent of the engine compiled into `tx` gets.
#[test]
fn the_manifest_says_what_the_adapter_answers() {
    let manifest = tx_plugin::testing::check_manifest(Path::new(env!("CARGO_MANIFEST_DIR")));
    let engine = manifest.engine.unwrap();
    let adapter = ClaudeAdapter::new("/nowhere");
    assert_eq!(engine.ext_key, adapter.ext_key());
    let session = Agent::from_json(
        r#"{"version":2,"id":"a1","name":"worker","engine":"claude","working_directory":"/w","roles":[],"state":"working","created_at":1.0,"last_modified_at":1.0,"tags":[]}"#,
    )
    .unwrap();
    let fixed = engine.interrupt.unwrap();
    assert_eq!(
        adapter.interrupt(&session),
        Some((Input::Key(fixed.key), fixed.state))
    );
}
