//! `tx timeline` — swimlanes of session activity in the terminal, the artifact-browser's HTML
//! timeline as a resident TUI (ratatui). Data layer: [`model`] over [`rows`] + [`cache`];
//! state and input: [`app`]; drawing: [`ui`]; the terminal and the event loop: [`run`].

pub mod app;
pub mod cache;
pub mod clock;
pub mod metadata;
pub mod model;
pub mod rows;
pub mod run;
pub mod turns;
pub mod ui;
pub mod verbs;
