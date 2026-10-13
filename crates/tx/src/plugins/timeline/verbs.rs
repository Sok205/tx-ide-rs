//! `tx timeline` — the swimlane TUI — and `tx _timeline-json`, its payload as the artifact-browser
//! prototype's `/api/timeline` JSON (for parity diffs against `timeline.py`).

use std::fs::OpenOptions;
use std::process::{Command as Process, Stdio};
use std::rc::Rc;

use cordis::{BoxError, Component, Ctx};

use crate::app::{COMMANDS, Command, ENV, Env, HOME, STORE, TMUX, Visibility};
use crate::argparse::{Arg, Matches, ParseExit, Parser};
use crate::plugins::timeline::app::DEFAULT_WINDOW_MIN;
use crate::plugins::timeline::model::Builder;
use crate::plugins::timeline::run;
use crate::pyjson::dumps_pretty;
use crate::shlex::shlex_join;
use crate::storage::Home;
use crate::store::SessionStore;
use crate::tmux::Tmux;
use crate::verbs::common::bin_tx;

const DEFAULT_SNAPSHOT_COLUMNS: i64 = 120;
const DEFAULT_SNAPSHOT_ROWS: i64 = 40;
const POPUP_SESSION: &str = "tx-timeline";

struct Deps {
    home: Rc<Home>,
    store: Rc<SessionStore>,
    env: Rc<Env>,
    tmux: Rc<Tmux>,
}

impl Deps {
    fn builder(&self) -> Builder {
        Builder::new(Rc::clone(&self.home), Rc::clone(&self.store))
    }

    fn parser(&self, name: &str, summary: &str) -> Parser {
        Parser::new(&format!("tx {name}")).description(summary).arg(
            Arg::option("--days")
                .default("14")
                .help("window length in days back from today (local midnights; default 14)"),
        )
    }
}

/// `--days` as a float, argparse-style error on garbage.
fn days(parser: &Parser, matches: &Matches) -> Result<f64, i32> {
    let raw = matches.get_one("days").unwrap_or("14");
    raw.parse::<f64>()
        .ok()
        .filter(|days| days.is_finite() && *days > 0.0)
        .ok_or_else(|| {
            parser
                .error(&format!(
                    "argument --days: invalid positive number: '{raw}'"
                ))
                .emit()
        })
}

/// `--last` as minutes: `30m`, `2h`, `90s`, `1d`, `1.5h`, or bare minutes.
fn parse_duration_minutes(raw: &str) -> Option<f64> {
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

/// The rolling observation window from `--last`, argparse-style error on garbage.
fn window(parser: &Parser, matches: &Matches) -> Result<f64, i32> {
    let Some(raw) = matches.get_one("last") else {
        return Ok(DEFAULT_WINDOW_MIN);
    };
    parse_duration_minutes(raw).ok_or_else(|| {
        parser
            .error(&format!(
                "argument --last: expected a duration like 30m, 2h or 90s, got '{raw}'"
            ))
            .emit()
    })
}

fn parse(parser: &Parser, argv: &[String]) -> Result<Matches, i32> {
    parser.parse(argv).map_err(|exit: ParseExit| exit.emit())
}

pub struct TimelineVerbs;

impl Component for TimelineVerbs {
    fn name(&self) -> &str {
        "verbs.timeline"
    }
    fn inject(&self) -> &[&'static str] {
        &["commands", "home", "store", "env", "tmux"]
    }
    fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        let deps = Rc::new(Deps {
            home: ctx.get(HOME)?,
            store: ctx.get(STORE)?,
            env: ctx.get(ENV)?,
            tmux: ctx.get(TMUX)?,
        });
        let table = ctx.get(COMMANDS)?;
        table.register(ctx, Visibility::Public, Timeline {
            deps: Rc::clone(&deps),
            name: "timeline",
        });
        table.register(ctx, Visibility::Hidden, Timeline {
            deps: Rc::clone(&deps),
            name: "_timeline-resident",
        });
        table.register(ctx, Visibility::Hidden, TimelineAttach(Rc::clone(&deps)));
        table.register(ctx, Visibility::Hidden, TimelineJson(deps));
        Ok(())
    }
}

struct Timeline {
    deps: Rc<Deps>,
    name: &'static str,
}

impl Command for Timeline {
    fn name(&self) -> &'static str {
        self.name
    }
    fn summary(&self) -> &'static str {
        "Swimlanes of session activity in the terminal (a resident TUI; q quits)."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = self
            .deps
            .parser(self.name(), self.summary())
            .arg(Arg::option("--last").metavar("DURATION").help(
                "rolling activity window, e.g. 30m, 2h, 90s (default 4.5h); sessions and \
                 messages outside this range disappear; also sets the initial viewport",
            ))
            .arg(
                Arg::option("--snapshot")
                    .flag()
                    .help("print one frame as plain text and exit (no terminal needed)"),
            )
            .arg(
                Arg::option("--cols")
                    .int()
                    .help("snapshot width (default 120)"),
            )
            .arg(
                Arg::option("--rows")
                    .int()
                    .help("snapshot height (default 40)"),
            );
        let matches = match parse(&parser, argv) {
            Ok(matches) => matches,
            Err(code) => return Ok(code),
        };
        let days = match days(&parser, &matches) {
            Ok(days) => days,
            Err(code) => return Ok(code),
        };
        let window = match window(&parser, &matches) {
            Ok(window) => window,
            Err(code) => return Ok(code),
        };
        let builder = self.deps.builder();
        if matches.get_flag("snapshot") {
            let columns = matches.get_int("cols").unwrap_or(DEFAULT_SNAPSHOT_COLUMNS);
            let rows = matches.get_int("rows").unwrap_or(DEFAULT_SNAPSHOT_ROWS);
            run::print_snapshot(
                builder,
                days,
                window,
                columns.clamp(20, 1000) as u16,
                rows.clamp(6, 1000) as u16,
            )?;
            return Ok(0);
        }
        let resident = (self.name == "_timeline-resident").then_some(self.deps.env.actor());
        run::run(builder, days, window, &self.deps.tmux, resident)?;
        Ok(0)
    }
}

/// The popup is a client of a persistent, tx-managed timeline, so closing it keeps the cache warm.
struct TimelineAttach(Rc<Deps>);

impl Command for TimelineAttach {
    fn name(&self) -> &'static str {
        "_timeline-attach"
    }
    fn summary(&self) -> &'static str {
        "Internal: start or attach the resident timeline; q detaches."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = Parser::new("tx _timeline-attach").description(self.summary());
        if let Err(code) = parse(&parser, argv) {
            return Ok(code);
        }
        // Two clients opening at once must reuse the same session.
        let lock = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(self.0.home.root().join("timeline-popup.lock"))?;
        lock.lock()?;
        let session = self.0.store
            .find_by_name(POPUP_SESSION)
            .filter(|session| self.0.tmux.has_session(&session.id));
        let session = match session {
            Some(session) => session,
            None => {
                let binary = bin_tx(&self.0.env);
                let command = shlex_join(&[
                    binary.to_string_lossy().as_ref(),
                    "_timeline-resident", "--last", "30m",
                ]);
                let status = Process::new(&binary)
                    .args(["spawn", POPUP_SESSION, "--tag", "tx-system", "--cwd"])
                    .arg(self.0.home.root())
                    .args(["--cmd", &command])
                    .args(["--env", &format!("TX_IDE_HOME={}", self.0.home.root().display())])
                    .env("TX_IDE_HOME", self.0.home.root())
                    .stdout(Stdio::null())
                    .status()?;
                if !status.success() {
                    return Ok(status.code().unwrap_or(1));
                }
                self.0.store.find_by_name(POPUP_SESSION).expect("spawn saved the session")
            }
        };
        drop(lock);
        let tmux = &self.0.tmux;
        let code = tmux.attach_session(&session.id)?;
        // Enter in the resident: it left the target session id behind and detached us; this
        // process runs on the outer client, so the focusing happens here.
        if let Some(target) = tmux.session_option(&session.id, run::GOTO_OPTION) {
            tmux.unset_session_option(&session.id, run::GOTO_OPTION);
            if let Err(reason) = run::focus_session_pane(tmux, &target)
                && let Some(client) = tmux.display_message("#{client_name}", None)
            {
                tmux.display_status(&client, &format!("timeline: {target}: {reason}"));
            }
        }
        Ok(code)
    }
}

struct TimelineJson(Rc<Deps>);

impl Command for TimelineJson {
    fn name(&self) -> &'static str {
        "_timeline-json"
    }
    fn summary(&self) -> &'static str {
        "Internal: the timeline payload as the artifact-browser's /api/timeline JSON."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = self.0.parser(self.name(), self.summary());
        let matches = match parse(&parser, argv) {
            Ok(matches) => matches,
            Err(code) => return Ok(code),
        };
        let days = match days(&parser, &matches) {
            Ok(days) => days,
            Err(code) => return Ok(code),
        };
        let mut builder = self.0.builder();
        let payload = builder.build(days, crate::plugins::timeline::clock::now());
        println!("{}", dumps_pretty(&payload.to_json()));
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn days_rejects_garbage() {
        let parser = Parser::new("tx timeline").arg(Arg::option("--days").default("14"));
        let matches = parser
            .parse(&["--days".to_owned(), "2.5".to_owned()])
            .unwrap();
        assert_eq!(days(&parser, &matches), Ok(2.5));
        let matches = parser.parse(&[]).unwrap();
        assert_eq!(days(&parser, &matches), Ok(14.0));
    }
}
