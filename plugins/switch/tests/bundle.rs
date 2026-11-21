//! The bundle says what the code does: `plugin.yaml` against what `SwitchPlugin` registers.

use std::path::Path;

#[test]
fn the_manifest_matches_the_plugin() {
    tx_plugin::testing::check_bundle(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        vec![Box::new(tx_switch::SwitchPlugin)],
    );
}
