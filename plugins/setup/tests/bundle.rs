//! The bundle is sound: `plugin.yaml` reads and its `bin` is this package's program. There is no
//! `Plugin` whose registration the verb could be compared with.

use std::path::Path;

#[test]
fn the_manifest_reads_and_names_the_program() {
    let manifest = tx_plugin::testing::check_manifest(Path::new(env!("CARGO_MANIFEST_DIR")));
    assert_eq!(manifest.id, "setup");
    assert_eq!(manifest.bins(), ["tx-setup"]);
}
