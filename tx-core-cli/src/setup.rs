//! Opening the core for `tx` and `txi`: `tx_plugin::setup`, with no plugin hosted in the process.

use tx_core::TxCore;
pub use tx_plugin::setup::*;

use crate::args::Global;

/// The core of this installation: every active plugin is behind a proxy that starts its
/// program.
pub fn open(global: &Global) -> Result<TxCore, Failure> {
    Installation::read(global)?.open_hosted(global, Vec::new())
}
