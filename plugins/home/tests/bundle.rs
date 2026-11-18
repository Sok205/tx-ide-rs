//! The bundle says what the code does: `plugin.yaml` against what the plugins `tx-home` hosts
//! register. `home-themes` registers no verb, so the verbs are the home's.

use std::path::Path;

#[test]
fn the_manifest_matches_the_plugin() {
    tx_plugin::testing::check_bundle(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        vec![
            Box::new(tx_home::HomePlugin),
            Box::new(tx_home_themes::HomeThemesPlugin),
        ],
    );
}
