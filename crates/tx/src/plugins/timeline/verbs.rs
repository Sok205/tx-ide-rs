//! `tx timeline` — the swimlane TUI — and `tx _timeline-json`, its payload as the artifact-browser
//! prototype's `/api/timeline` JSON (for parity diffs against `timeline.py`).

use std::rc::Rc;

use cordis::{BoxError, Component, Ctx};

use crate::app::{COMMANDS, Command, HOME, STORE, Visibility};
use crate::argparse::{Arg, Matches, ParseExit, Parser};
use crate::plugins::timeline::app::DEFAULT_WINDOW_MIN;
use crate::plugins::timeline::model::Builder;
use crate::plugins::timeline::run;
use crate::pyjson::dumps_pretty;
use crate::storage::Home;
use crate::store::SessionStore;

const DEFAULT_SNAPSHOT_COLUMNS: i64 = 120;
const DEFAULT_SNAPSHOT_ROWS: i64 = 40;

struct Deps {
    home: Rc<Home>,
    store: Rc<SessionStore>,
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

/// The initial viewport length from `--last`, argparse-style error on garbage.
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
        &["commands", "home", "store"]
    }
    fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        let deps = Rc::new(Deps {
            home: ctx.get(HOME)?,
            store: ctx.get(STORE)?,
        });
        let table = ctx.get(COMMANDS)?;
        table.register(ctx, Visibility::Public, Timeline(Rc::clone(&deps)));
        table.register(ctx, Visibility::Hidden, TimelineJson(deps));
        Ok(())
    }
}

struct Timeline(Rc<Deps>);

impl Command for Timeline {
    fn name(&self) -> &'static str {
        "timeline"
    }
    fn summary(&self) -> &'static str {
        "Swimlanes of session activity in the terminal (a resident TUI; q quits)."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = self
            .0
            .parser(self.name(), self.summary())
            .arg(Arg::option("--last").metavar("DURATION").help(
                "the initial viewport: how much time before now to fit on screen, e.g. 30m, 2h, \
                 90s (default 4.5h); zoom levels go from 15 s to 8 h per column",
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
        let builder = self.0.builder();
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
        run::run(builder, days, window)?;
        Ok(0)
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
