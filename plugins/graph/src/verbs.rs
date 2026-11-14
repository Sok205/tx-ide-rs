//! `tx graph`: the agent graph in the terminal, or one frame of it as text (`--snapshot`);
//! `_graph-resident`, the graph a popup attaches to; `_graph-attach` (prefix+g), which starts
//! or attaches the resident.

use std::fs::OpenOptions;
use std::process::Stdio;
use std::sync::Arc;

use tx_client::tmux::Ui;
use tx_client::views::{self, ViewKind, ViewRequest};
use tx_core::{TxCore, Verb, VerbError, VerbIo};

use crate::app::App;
use crate::model::Filter;
use crate::{PLUGIN_ID, run};

pub const RESIDENT_VERB: &str = "_graph-resident";
/// The tmux session (a view, not a record) hosting the resident graph.
pub const POPUP_SESSION: &str = "tx-graph";
const POPUP_LOCK: &str = "popup.lock";

pub const DEFAULT_WINDOW: f64 = 3600.0;
const DEFAULT_SNAPSHOT_COLUMNS: u16 = 140;
const DEFAULT_SNAPSHOT_ROWS: u16 = 40;

const USAGE: &str = "\
usage: tx graph [--tag T] [--group G] [--last DURATION] [--snapshot [--cols N] [--rows N] [--now EPOCH]]
";

pub fn verbs() -> Vec<Arc<dyn Verb>> {
    vec![
        Arc::new(GraphVerb { resident: false }),
        Arc::new(GraphVerb { resident: true }),
        Arc::new(GraphAttach),
    ]
}

fn failed(error: impl std::fmt::Display) -> VerbError {
    VerbError::Failed(error.to_string())
}

/// `--last` as seconds: `30m`, `2h`, `90s`, `1d`, `1.5h`, or bare minutes.
pub fn parse_duration(raw: &str) -> Option<f64> {
    let raw = raw.trim();
    let (number, unit) = match raw.chars().last()? {
        'd' => (&raw[..raw.len() - 1], 86400.0),
        'h' => (&raw[..raw.len() - 1], 3600.0),
        'm' => (&raw[..raw.len() - 1], 60.0),
        's' => (&raw[..raw.len() - 1], 1.0),
        _ => (raw, 60.0),
    };
    let value: f64 = number.trim().parse().ok()?;
    (value.is_finite() && value > 0.0).then_some(value * unit)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub filter: Filter,
    /// Seconds.
    pub window: f64,
    pub snapshot: bool,
    pub columns: u16,
    pub rows: u16,
    /// Epoch seconds the snapshot takes as now; the clock unless given.
    pub now: Option<f64>,
}

/// `Err` is the message.
pub fn parse_options(argv: &[String]) -> Result<Options, String> {
    let mut options = Options {
        filter: Filter::default(),
        window: DEFAULT_WINDOW,
        snapshot: false,
        columns: DEFAULT_SNAPSHOT_COLUMNS,
        rows: DEFAULT_SNAPSHOT_ROWS,
        now: None,
    };
    let mut argv = argv.iter();
    while let Some(flag) = argv.next() {
        if flag == "--snapshot" {
            options.snapshot = true;
            continue;
        }
        if !["--tag", "--group", "--last", "--cols", "--rows", "--now"].contains(&flag.as_str()) {
            return Err(format!("unknown argument '{flag}'"));
        }
        let value = argv
            .next()
            .ok_or_else(|| format!("argument {flag}: expected a value"))?;
        let bad = |what: &str| format!("argument {flag}: {what}, got '{value}'");
        match flag.as_str() {
            "--tag" | "--group" if value.is_empty() => return Err(bad("expected a name")),
            "--tag" => options.filter.tag = Some(value.clone()),
            "--group" => options.filter.group = Some(value.clone()),
            "--last" => {
                options.window = parse_duration(value)
                    .ok_or_else(|| bad("expected a duration like 30m, 2h or 90s"))?;
            }
            "--cols" | "--rows" => {
                let number: i64 = value.parse().map_err(|_| bad("expected an integer"))?;
                if flag == "--cols" {
                    options.columns = number.clamp(20, 1000) as u16;
                } else {
                    options.rows = number.clamp(6, 1000) as u16;
                }
            }
            _ => {
                let now = value
                    .parse::<f64>()
                    .ok()
                    .filter(|now| now.is_finite() && *now > 0.0)
                    .ok_or_else(|| bad("expected epoch seconds"))?;
                options.now = Some(now);
            }
        }
    }
    if !options.snapshot && options.now.is_some() {
        return Err("--now needs --snapshot".to_owned());
    }
    Ok(options)
}

struct GraphVerb {
    resident: bool,
}

impl Verb for GraphVerb {
    fn name(&self) -> &'static str {
        if self.resident {
            RESIDENT_VERB
        } else {
            "graph"
        }
    }

    fn usage(&self) -> &'static str {
        if self.resident {
            "_graph-resident   the popup's resident graph; q detaches (internal)"
        } else {
            "graph [--tag T] [--group G] [--last DURATION] [--snapshot]   the agents as a graph of lineage and messages (q quits)"
        }
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let options = match parse_options(argv) {
            Ok(options) => options,
            Err(message) => {
                write!(io.stderr, "tx {}: {message}\n{USAGE}", self.name()).map_err(failed)?;
                return Ok(2);
            }
        };
        if options.snapshot {
            let now = options.now.unwrap_or_else(tx_core::events::now);
            let source = run::source(core, now, options.window).map_err(failed)?;
            let mut app = App::new(source, options.filter, now, options.window);
            let text = run::snapshot(&mut app, options.columns, options.rows);
            writeln!(io.stdout, "{text}").map_err(failed)?;
            return Ok(0);
        }
        let resident = self.resident.then_some(POPUP_SESSION);
        run::run(core, options.filter, options.window, resident).map_err(failed)?;
        Ok(0)
    }
}

/// Starts the resident graph unless it is running. Callers hold the popup lock.
fn ensure_resident(core: &TxCore, ui: &Ui) -> Result<(), VerbError> {
    if ui.has_session(POPUP_SESSION) {
        return Ok(());
    }
    let program =
        std::env::current_exe().map_err(|error| failed(format!("current exe: {error}")))?;
    let mut arguments = core.config().cli_arguments().to_vec();
    arguments.push(RESIDENT_VERB.to_owned());
    views::open(
        ui,
        &ViewRequest {
            name: POPUP_SESSION,
            kind: ViewKind::Graph,
            program: &program.to_string_lossy(),
            arguments: &arguments,
            working_directory: &core.config().directories.data,
            tags: &[],
            parent_id: None,
            bound: None,
        },
    )
    .map_err(failed)?;
    ui.set_session_option(POPUP_SESSION, "status", "off")
        .map_err(failed)?;
    Ok(())
}

/// The popup is a client of a persistent resident graph, so closing it keeps the graph's log
/// position and transcript reads warm.
struct GraphAttach;

impl Verb for GraphAttach {
    fn name(&self) -> &'static str {
        "_graph-attach"
    }

    fn usage(&self) -> &'static str {
        "_graph-attach   start or attach the resident graph; q detaches (prefix+g, internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        if !argv.is_empty() {
            writeln!(io.stderr, "usage: tx _graph-attach").map_err(failed)?;
            return Ok(2);
        }
        let ui = Ui::from_core(core);
        let directory = core.config().plugin_data_dir(PLUGIN_ID);
        tx_chat::ensure_dir(&directory)
            .map_err(|error| failed(format!("{}: {error}", directory.display())))?;
        // Two clients opening at once must reuse the same session.
        let lock_path = directory.join(POPUP_LOCK);
        let lock = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| failed(format!("{}: {error}", lock_path.display())))?;
        lock.lock().map_err(failed)?;
        ensure_resident(core, &ui)?;
        drop(lock);
        let status = ui
            .command(&["attach-session", "-t", &format!("={POPUP_SESSION}")])
            .stdin(Stdio::inherit())
            .env_remove("TMUX")
            .status()
            .map_err(failed)?;
        // Enter in the resident left the agent behind and detached us; this process runs on
        // the outer client, so the focusing happens here.
        if let Some(target) = ui.session_option(POPUP_SESSION, run::GOTO_OPTION) {
            ui.quiet(&[
                "set-option",
                "-u",
                "-t",
                &format!("={POPUP_SESSION}:"),
                run::GOTO_OPTION,
            ]);
            if let Err(reason) = run::focus_session_pane(&ui, &target)
                && let Some(client) = ui.display_message("#{client_name}", None)
            {
                ui.display_status(&client, &format!("graph: {target}: {reason}"));
            }
        }
        Ok(status.code().unwrap_or(1))
    }
}
