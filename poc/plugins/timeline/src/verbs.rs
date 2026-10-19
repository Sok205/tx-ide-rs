//! `txc timeline` (the swimlane TUI, `--snapshot`, `health`), `_timeline-resident` (the TUI a
//! popup attaches to), `_timeline-attach` (prefix+T: start or attach the resident) and
//! `_timeline-json` (the payload as the artifact-browser's `/api/timeline` JSON, for parity
//! diffs against main's `tx _timeline-json`).

use std::fs::OpenOptions;
use std::io::Write;
use std::process::Stdio;
use std::sync::Arc;

use tx_core::{TxCore, Verb, VerbError, VerbIo};
use tx_ui::tmux::{Ui, shell_quote};

use crate::app::DEFAULT_WINDOW_MIN;
use crate::model::{Builder, DEFAULT_DAYS, chat_sources, is_agent, load_sessions};
use crate::{PLUGIN_ID, run};
use tx_chat::data_dir;

pub const RESIDENT_VERB: &str = "_timeline-resident";
/// The tmux session (a view, not a record) hosting the resident timeline.
pub const POPUP_SESSION: &str = "tx-timeline";
const POPUP_LOCK: &str = "popup.lock";
const DEFAULT_SNAPSHOT_COLUMNS: u16 = 120;
const DEFAULT_SNAPSHOT_ROWS: u16 = 40;
const RESIDENT_WINDOW: &str = "30m";

const USAGE: &str = "\
usage: txc timeline [--days N] [--last DURATION] [--snapshot [--cols N] [--rows N]] [--now EPOCH]
       txc timeline health
";
const JSON_USAGE: &str = "usage: txc _timeline-json [--days N] [--now EPOCH]\n";

pub fn verbs() -> Vec<Arc<dyn Verb>> {
    vec![
        Arc::new(Timeline { resident: false }),
        Arc::new(Timeline { resident: true }),
        Arc::new(TimelineAttach),
        Arc::new(TimelineJson),
    ]
}

fn failed(error: impl std::fmt::Display) -> VerbError {
    VerbError::Failed(error.to_string())
}

fn say(out: &mut dyn Write, text: &str) -> Result<(), VerbError> {
    out.write_all(text.as_bytes()).map_err(failed)
}

/// `--last` as minutes: `30m`, `2h`, `90s`, `1d`, `1.5h`, or bare minutes.
pub fn parse_duration_minutes(raw: &str) -> Option<f64> {
    let raw = raw.trim();
    let (number, unit) = match raw.chars().last()? {
        'd' => (&raw[..raw.len() - 1], 1440.0),
        'h' => (&raw[..raw.len() - 1], 60.0),
        'm' => (&raw[..raw.len() - 1], 1.0),
        's' => (&raw[..raw.len() - 1], 1.0 / 60.0),
        _ => (raw, 1.0),
    };
    let value: f64 = number.trim().parse().ok()?;
    (value.is_finite() && value > 0.0).then_some(value * unit)
}

fn positive(raw: &str) -> Option<f64> {
    raw.parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value > 0.0)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub days: f64,
    pub window: f64,
    pub snapshot: bool,
    pub columns: u16,
    pub rows: u16,
    /// Epoch seconds the view takes as now; the clock unless given (reproducible output).
    pub now: Option<f64>,
}

/// The flags of `timeline` / `_timeline-resident` / `_timeline-json`; `Err` is the message.
pub fn parse_options(argv: &[String], allowed: &[&str]) -> Result<Options, String> {
    let mut options = Options {
        days: DEFAULT_DAYS,
        window: DEFAULT_WINDOW_MIN,
        snapshot: false,
        columns: DEFAULT_SNAPSHOT_COLUMNS,
        rows: DEFAULT_SNAPSHOT_ROWS,
        now: None,
    };
    let mut argv = argv.iter();
    while let Some(flag) = argv.next() {
        if !allowed.contains(&flag.as_str()) {
            return Err(format!("unknown argument '{flag}'"));
        }
        if flag == "--snapshot" {
            options.snapshot = true;
            continue;
        }
        let value = argv
            .next()
            .ok_or_else(|| format!("argument {flag}: expected a value"))?;
        let bad = |what: &str| format!("argument {flag}: {what}, got '{value}'");
        match flag.as_str() {
            "--days" => {
                options.days = positive(value).ok_or_else(|| bad("invalid positive number"))?
            }
            "--last" => {
                options.window = parse_duration_minutes(value)
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
            "--now" => {
                options.now = Some(positive(value).ok_or_else(|| bad("expected epoch seconds"))?)
            }
            _ => unreachable!("allowed flags are matched above"),
        }
    }
    Ok(options)
}

pub fn builder(core: &TxCore) -> Builder {
    let state = &core.config().state_dir;
    Builder::new(
        tx_chat::data_dir(state, tx_chat::HISTORY_ID),
        data_dir(state, PLUGIN_ID),
    )
}

struct Timeline {
    resident: bool,
}

impl Verb for Timeline {
    fn name(&self) -> &'static str {
        if self.resident {
            RESIDENT_VERB
        } else {
            "timeline"
        }
    }

    fn usage(&self) -> &'static str {
        if self.resident {
            "_timeline-resident [--last DURATION]   the popup's resident timeline; q detaches (internal)"
        } else {
            "timeline [--last DURATION] [--snapshot] | health   swimlanes of session activity (q quits)"
        }
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        if !self.resident && argv.first().map(String::as_str) == Some("health") {
            if argv.len() > 1 {
                say(io.stderr, USAGE)?;
                return Ok(2);
            }
            return health(core, io);
        }
        let allowed = [
            "--days",
            "--last",
            "--snapshot",
            "--cols",
            "--rows",
            "--now",
        ];
        let options = match parse_options(argv, &allowed) {
            Ok(options) => options,
            Err(message) => {
                say(
                    io.stderr,
                    &format!("txc {}: {message}\n{USAGE}", self.name()),
                )?;
                return Ok(2);
            }
        };
        let mut builder = builder(core);
        let days = options.days.max(options.window / 1440.0);
        if options.snapshot {
            let now = options.now.unwrap_or_else(tx_core::events::now);
            let payload = run::payload(core, &mut builder, days, now).map_err(failed)?;
            let text = run::snapshot(payload, now, options.window, options.columns, options.rows);
            writeln!(io.stdout, "{text}").map_err(failed)?;
            return Ok(0);
        }
        let resident = self.resident.then_some(POPUP_SESSION);
        run::run(core, builder, days, options.window, resident).map_err(failed)?;
        Ok(0)
    }
}

/// `<txc> --state-dir <state> [--socket <socket>] _timeline-resident --last 30m`, shell-quoted.
fn resident_command(core: &TxCore) -> Result<String, VerbError> {
    let exe = std::env::current_exe().map_err(|error| failed(format!("current exe: {error}")))?;
    let config = core.config();
    let mut words = vec![
        shell_quote(&exe.to_string_lossy()),
        "--state-dir".to_owned(),
        shell_quote(&config.state_dir.to_string_lossy()),
    ];
    if let Some(socket) = &config.tmux_socket {
        words.push("--socket".to_owned());
        words.push(shell_quote(&socket.to_string_lossy()));
    }
    words.extend([
        RESIDENT_VERB.to_owned(),
        "--last".to_owned(),
        RESIDENT_WINDOW.to_owned(),
    ]);
    Ok(words.join(" "))
}

/// Starts the resident view unless it is running. Callers hold the popup lock.
fn ensure_resident(core: &TxCore, ui: &Ui) -> Result<(), VerbError> {
    if ui.has_session(POPUP_SESSION) {
        return Ok(());
    }
    let command = resident_command(core)?;
    // `-c` is format-expanded by tmux, so a literal `#` must be doubled.
    let cwd = core.config().state_dir.to_string_lossy().replace('#', "##");
    ui.run(&[
        "new-session",
        "-d",
        "-s",
        POPUP_SESSION,
        "-x",
        "118",
        "-y",
        "30",
        "-c",
        &cwd,
        &command,
    ])
    .map_err(failed)?;
    for (option, value) in [("@tx-view", "1"), ("status", "off")] {
        ui.set_session_option(POPUP_SESSION, option, value)
            .map_err(failed)?;
    }
    Ok(())
}

/// The popup is a client of a persistent resident timeline, so closing it keeps the cache warm.
struct TimelineAttach;

impl Verb for TimelineAttach {
    fn name(&self) -> &'static str {
        "_timeline-attach"
    }

    fn usage(&self) -> &'static str {
        "_timeline-attach   start or attach the resident timeline; q detaches (prefix+T, internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        if !argv.is_empty() {
            say(io.stderr, "usage: txc _timeline-attach\n")?;
            return Ok(2);
        }
        let ui = Ui::from_core(core);
        let dir = data_dir(&core.config().state_dir, PLUGIN_ID);
        tx_chat::ensure_dir(&dir).map_err(|error| failed(format!("{}: {error}", dir.display())))?;
        // Two clients opening at once must reuse the same session.
        let lock_path = dir.join(POPUP_LOCK);
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
        // Enter in the resident left the target behind and detached us; this process runs on
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
                ui.display_status(&client, &format!("timeline: {target}: {reason}"));
            }
        }
        Ok(status.code().unwrap_or(1))
    }
}

struct TimelineJson;

impl Verb for TimelineJson {
    fn name(&self) -> &'static str {
        "_timeline-json"
    }

    fn usage(&self) -> &'static str {
        "_timeline-json [--days N]   the timeline payload as /api/timeline JSON (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let options = match parse_options(argv, &["--days", "--now"]) {
            Ok(options) => options,
            Err(message) => {
                say(
                    io.stderr,
                    &format!("txc _timeline-json: {message}\n{JSON_USAGE}"),
                )?;
                return Ok(2);
            }
        };
        let now = options.now.unwrap_or_else(tx_core::events::now);
        let payload = run::payload(core, &mut builder(core), options.days, now).map_err(failed)?;
        let text = serde_json::to_string_pretty(&payload.to_json()).map_err(failed)?;
        writeln!(io.stdout, "{text}").map_err(failed)?;
        Ok(0)
    }
}

/// One line per agent record: what the timeline can read for it. A malformed `ext.chat` or
/// `ext.history` is reported (exit 1) but never stops the view.
fn health(core: &TxCore, io: &mut VerbIo<'_>) -> Result<i32, VerbError> {
    let sessions = load_sessions(core)?;
    let builder = builder(core);
    let mut problems = 0;
    for session in sessions.iter().filter(|session| is_agent(session)) {
        let mut notes = Vec::new();
        for (plugin, result) in [
            ("chat", tx_chat::ext::chat_ext(session).map(drop)),
            ("history", tx_chat::ext::history_ext(session).map(drop)),
        ] {
            if let Err(error) = result {
                problems += 1;
                notes.push(format!("ext.{plugin} malformed: {error}"));
            }
        }
        if session.ext.get("hooks").is_some_and(|hooks| {
            !hooks.is_object() || hooks.get("state").is_some_and(|state| !state.is_string())
        }) {
            problems += 1;
            notes.push("ext.hooks malformed".to_owned());
        }
        let chats = chat_sources(session).len();
        let found = builder.transcript_paths(session).len();
        if found < chats {
            notes.push(format!("{} transcript(s) missing", chats - found));
        }
        let notes = if notes.is_empty() {
            "ok".to_owned()
        } else {
            notes.join("; ")
        };
        writeln!(
            io.stdout,
            "{}\t{}\t{found}/{chats} transcripts\t{notes}",
            session.name,
            session.lifecycle.as_str()
        )
        .map_err(failed)?;
    }
    Ok(i32::from(problems > 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    const ALL: [&str; 6] = [
        "--days",
        "--last",
        "--snapshot",
        "--cols",
        "--rows",
        "--now",
    ];

    #[test]
    fn durations() {
        assert_eq!(parse_duration_minutes("30m"), Some(30.0));
        assert_eq!(parse_duration_minutes("2h"), Some(120.0));
        assert_eq!(parse_duration_minutes("90s"), Some(1.5));
        assert_eq!(parse_duration_minutes("1d"), Some(1440.0));
        assert_eq!(parse_duration_minutes("45"), Some(45.0));
        assert_eq!(parse_duration_minutes("1.5h"), Some(90.0));
        assert_eq!(parse_duration_minutes("soon"), None);
        assert_eq!(parse_duration_minutes("-5m"), None);
    }

    #[test]
    fn options_default_and_parse() {
        let options = parse_options(&[], &ALL).unwrap();
        assert_eq!(options.days, 14.0);
        assert_eq!(options.window, DEFAULT_WINDOW_MIN);
        assert!(!options.snapshot);
        let options = parse_options(
            &argv(&[
                "--days",
                "2.5",
                "--last",
                "2h",
                "--snapshot",
                "--cols",
                "5",
                "--now",
                "9",
            ]),
            &ALL,
        )
        .unwrap();
        assert_eq!(
            (
                options.days,
                options.window,
                options.snapshot,
                options.columns,
                options.now
            ),
            (2.5, 120.0, true, 20, Some(9.0))
        );
    }

    #[test]
    fn options_reject_garbage() {
        for bad in [
            &["--days", "x"][..],
            &["--days", "-1"],
            &["--last", "soon"],
            &["--cols"],
            &["--bogus"],
        ] {
            assert!(parse_options(&argv(bad), &ALL).is_err(), "{bad:?}");
        }
        assert!(parse_options(&argv(&["--snapshot"]), &["--days"]).is_err());
    }
}
