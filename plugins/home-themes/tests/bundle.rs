//! The manifest alone: `home-themes` has no program of its own (`tx-home` hosts it), no verb
//! and no resource.

use std::path::Path;

#[test]
fn the_manifest_reads() {
    let manifest = tx_plugin::testing::check_manifest(Path::new(env!("CARGO_MANIFEST_DIR")));
    assert_eq!(manifest.id, tx_home_themes::PLUGIN_ID);
    assert!(manifest.verbs.is_empty());
    assert!(manifest.resources.is_empty());
}
