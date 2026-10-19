//! tx-core plugin `menu`: the tx-ide start menu. A full-screen popup with the shimmering `TX`
//! logo, every useful tx-ide action one key away, and the recent past sessions. It opens on
//! prefix+m and, once per server, on the first client attach. Items run other plugins' verbs
//! only through `txc` and tmux (D3); an item whose plugin is disabled is hidden.
//!
//! State and keys: [`model`]; drawing: [`ui`].

pub mod model;
pub mod ui;
