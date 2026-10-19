//! Picker row rendering: port of main's `render.rs` picker feed and `palette.rs` tag colours.
//! Pure formatting; every function takes the records and `now`, nothing reads the clock.

use serde_json::{Map, Value};
use tx_core::{Launch, Session};

use crate::Ext;
use crate::tmux::Location;

pub const WARN_ANSI: &str = "\x1b[38;2;224;175;104m";
pub const RESET_FG: &str = "\x1b[39m";
pub const ACCENT_ANSI: &str = "\x1b[38;2;122;162;247m";
pub const FG_ANSI: &str = "\x1b[38;2;169;177;214m";
pub const BOLD: &str = "\x1b[1m";
pub const RESET: &str = "\x1b[0m";

/// LOCATION column width, shared with the picker header.
pub const LOCATION_W: usize = 25;
/// ROLE column width (the longest role, `shell`).
pub const ROLE_W: usize = 5;

const NAMEW_OVERHEAD: i64 = 65;
const NAMEW_FLOOR: i64 = 12;
const NAMEW_CEILING: i64 = 60;
const HASH_MODULUS: u64 = 2_147_483_647;

pub const TAG_CUBE: [u8; 12] = [210, 167, 215, 149, 80, 73, 37, 117, 38, 141, 198, 140];

/// The tmux `colourN` names of [`TAG_CUBE`], same index: what `shared/palette.sh` `tag_hex()`
/// prints (`colour%s` of the cube value).
const TAG_HEX: [&str; 12] = [
    "colour210",
    "colour167",
    "colour215",
    "colour149",
    "colour80",
    "colour73",
    "colour37",
    "colour117",
    "colour38",
    "colour141",
    "colour198",
    "colour140",
];

/// Rolling hash over the Unicode code points, mod the 31-bit prime.
pub fn tag_hash(s: &str) -> u64 {
    s.chars()
        .fold(0, |acc, c| (acc * 31 + u64::from(c)) % HASH_MODULUS)
}

fn tag_index(s: &str) -> usize {
    (tag_hash(s) % TAG_CUBE.len() as u64) as usize
}

/// The SGR foreground escape colouring `s`'s chip (`\e[38;5;Nm`).
pub fn tag_ansi(s: &str) -> String {
    format!("\x1b[38;5;{}m", TAG_CUBE[tag_index(s)])
}

/// The tmux colour name for `s` (`colourN`).
pub fn tag_hex(s: &str) -> &'static str {
    TAG_HEX[tag_index(s)]
}

/// `s[:end]` with Python slice semantics (code points; a negative `end` counts from the back).
fn py_prefix(s: &str, end: i64) -> String {
    let len = s.chars().count() as i64;
    let end = if end < 0 { (len + end).max(0) } else { end };
    s.chars().take(end as usize).collect()
}

/// Compact relative age (`-`, `5s`, `3m`, `2h`, `4d`); `-` for an unset or zero epoch.
pub fn reltime(epoch: Option<f64>, now: f64) -> String {
    let Some(epoch) = epoch.filter(|e| *e != 0.0) else {
        return "-".into();
    };
    let delta = ((now - epoch) as i64).max(0);
    match delta {
        d if d < 60 => format!("{d}s"),
        d if d < 3600 => format!("{}m", d / 60),
        d if d < 86400 => format!("{}h", d / 3600),
        d => format!("{}d", d / 86400),
    }
}

/// The LOCATION cell: primary `window[pane]` + ` +N` overflow, `—` when attached nowhere. Only
/// the window name is clipped (with `…`) so the cell fits [`LOCATION_W`].
pub fn location_text(locations: &[Location]) -> String {
    let Some(primary) = locations.first() else {
        return "—".into();
    };
    let suffix = if locations.len() > 1 {
        format!(" +{}", locations.len() - 1)
    } else {
        String::new()
    };
    let tail = format!("[{}]{suffix}", primary.pane_index);
    let tail_len = tail.chars().count();
    let mut name = primary.window_name.clone();
    if name.chars().count() + tail_len > LOCATION_W {
        name = py_prefix(&name, LOCATION_W as i64 - tail_len as i64 - 1) + "…";
    }
    format!("{name}{tail}")
}

/// The picker NAME column width: fit the longest name, capped so the other columns fit `cols`.
pub fn picker_namew(cols: i64, longest: i64) -> i64 {
    let ceiling = NAMEW_FLOOR.max(NAMEW_CEILING.min(cols - NAMEW_OVERHEAD));
    NAMEW_FLOOR.max(longest).min(ceiling)
}

pub fn chips(tags: &[String]) -> String {
    tags.iter().map(|tag| format!(" [{tag}]")).collect()
}

/// `ext["hooks"].since`; a missing or malformed record (anything but an object) reads as absent.
fn hooks_since(session: &Session) -> Option<f64> {
    session
        .ext::<Map<String, Value>>("hooks")
        .ok()
        .flatten()
        .and_then(|hooks| hooks.get("since")?.as_f64())
}

fn views_kind(session: &Session) -> Option<String> {
    session
        .ext::<Ext>("views")
        .ok()
        .flatten()
        .and_then(|views| views.kind)
}

pub fn role_of(session: &Session) -> &'static str {
    match session.launch {
        Launch::Agent { .. } => "llm",
        Launch::Process { .. } => match views_kind(session).as_deref() {
            Some("nvim") => "nvim",
            Some("view") => "view",
            _ => "shell",
        },
    }
}

pub fn activity_at(session: &Session) -> f64 {
    hooks_since(session).unwrap_or(session.created_at)
}

/// IDLE: the last-turn age for an agent session, `—` for a process (no activity signal).
pub fn idle_cell(session: &Session, now: f64) -> String {
    match session.launch {
        Launch::Agent { .. } => reltime(hooks_since(session), now),
        Launch::Process { .. } => "—".into(),
    }
}

pub struct Row<'a> {
    pub session: &'a Session,
    /// The tmux `$N` session id, rendered verbatim as field 4.
    pub target: &'a str,
    pub attached: &'a [Location],
}

/// Truncate to `width` code points with a trailing `…`.
fn trunc(text: &str, width: i64) -> String {
    if text.chars().count() as i64 > width {
        py_prefix(text, width - 1) + "…"
    } else {
        text.to_owned()
    }
}

/// One fzf row: `name \t plain_chips \t L \t target \t visual`.
fn picker_row(row: &Row<'_>, namew: i64, now: f64) -> String {
    let session = row.session;
    let role = role_of(session);
    let name_disp = trunc(&session.name, namew);
    let pad = " ".repeat((namew - name_disp.chars().count() as i64).max(0) as usize);
    let colored_chips: String = session
        .tags
        .iter()
        .map(|tag| format!(" {}[{tag}]{RESET_FG}", tag_ansi(tag)))
        .collect();
    let started = reltime(Some(session.created_at), now);
    let idle = idle_cell(session, now);
    let role_cell = format!("{}{role:<ROLE_W$}{RESET_FG}", tag_ansi(role));
    let location = location_text(row.attached);
    let visual = format!(
        "{name_disp}{pad}   {location:<LOCATION_W$} {started:<7} {WARN_ANSI}{idle:<6}{RESET_FG} {role_cell}{colored_chips}"
    );
    [
        session.name.as_str(),
        &chips(&session.tags),
        "L",
        row.target,
        &visual,
    ]
    .join("\t")
}

/// The fzf picker feed, newest activity first (stable for ties).
pub fn picker_rows(rows: &[Row<'_>], namew: i64, now: f64) -> String {
    let mut ordered: Vec<&Row<'_>> = rows.iter().collect();
    ordered.sort_by(|a, b| activity_at(b.session).total_cmp(&activity_at(a.session)));
    ordered
        .into_iter()
        .map(|row| picker_row(row, namew, now))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;
    use tx_core::{Lifecycle, SessionId};

    use super::*;

    const NOW: f64 = 1_000_000.0;

    fn location(window: &str, pane: &str) -> Location {
        Location {
            host: "Views".into(),
            window_index: "1".into(),
            window_name: window.into(),
            pane_id: "%1".into(),
            pane_index: pane.into(),
        }
    }

    fn agent(name: &str, created_at: f64) -> Session {
        Session {
            id: "s1".parse::<SessionId>().unwrap(),
            name: name.into(),
            parent: None,
            tags: vec![],
            cwd: PathBuf::from("/r"),
            created_at,
            ended_at: None,
            lifecycle: Lifecycle::Running,
            launch: Launch::Agent {
                engine: "claude".into(),
                profile: vec![],
                model: None,
                effort: None,
                resume: None,
            },
            ext: Default::default(),
        }
    }

    fn process(name: &str, created_at: f64) -> Session {
        Session {
            launch: Launch::Process {
                program: "nvim".into(),
                args: vec![],
            },
            ..agent(name, created_at)
        }
    }

    #[test]
    fn reltime_boundaries() {
        assert_eq!(reltime(None, NOW), "-");
        assert_eq!(reltime(Some(0.0), NOW), "-");
        assert_eq!(reltime(Some(NOW + 5.0), NOW), "0s");
        assert_eq!(reltime(Some(NOW - 59.9), NOW), "59s");
        assert_eq!(reltime(Some(NOW - 59.0), NOW), "59s");
        assert_eq!(reltime(Some(NOW - 60.0), NOW), "1m");
        assert_eq!(reltime(Some(NOW - 3599.0), NOW), "59m");
        assert_eq!(reltime(Some(NOW - 3600.0), NOW), "1h");
        assert_eq!(reltime(Some(NOW - 86399.0), NOW), "23h");
        assert_eq!(reltime(Some(NOW - 86400.0), NOW), "1d");
    }

    #[test]
    fn location_text_cells() {
        let a = |n: usize| "a".repeat(n);
        assert_eq!(location_text(&[]), "—");
        assert_eq!(location_text(&[location("main", "0")]), "main[0]");
        let three = [
            location("main", "0"),
            location("x", "1"),
            location("y", "1"),
        ];
        assert_eq!(location_text(&three), "main[0] +2");
        assert_eq!(location_text(&[location(&a(22), "1")]), a(22) + "[1]");
        assert_eq!(location_text(&[location(&a(23), "1")]), a(21) + "…[1]");
        assert_eq!(
            location_text(&[location(&a(23), "1")]).chars().count(),
            LOCATION_W
        );
        let two = [location(&a(30), "1"), location("b", "1")];
        assert_eq!(location_text(&two), a(18) + "…[1] +1");
        let pane = "9".repeat(30);
        assert_eq!(
            location_text(&[location("abcdef", &pane)]),
            format!("…[{pane}]")
        );
        assert_eq!(
            location_text(&[location("abcdefghijkl", &pane)]),
            format!("abcd…[{pane}]")
        );
    }

    #[test]
    fn picker_namew_table() {
        let table: [(i64, i64, i64); 55] = [
            (0, -1, 12),
            (0, 5, 12),
            (0, 12, 12),
            (0, 53, 12),
            (0, 59, 12),
            (0, 60, 12),
            (0, 80, 12),
            (60, -1, 12),
            (60, 5, 12),
            (60, 12, 12),
            (60, 53, 12),
            (60, 59, 12),
            (60, 60, 12),
            (60, 80, 12),
            (76, -1, 12),
            (76, 5, 12),
            (76, 12, 12),
            (76, 53, 12),
            (76, 59, 12),
            (76, 60, 12),
            (76, 80, 12),
            (77, -1, 12),
            (77, 5, 12),
            (77, 12, 12),
            (77, 53, 12),
            (77, 59, 12),
            (77, 60, 12),
            (77, 80, 12),
            (118, -1, 12),
            (118, 5, 12),
            (118, 12, 12),
            (118, 53, 53),
            (118, 59, 53),
            (118, 60, 53),
            (118, 80, 53),
            (125, -1, 12),
            (125, 5, 12),
            (125, 12, 12),
            (125, 53, 53),
            (125, 59, 59),
            (125, 60, 60),
            (125, 80, 60),
            (126, -1, 12),
            (126, 5, 12),
            (126, 12, 12),
            (126, 53, 53),
            (126, 59, 59),
            (126, 60, 60),
            (126, 80, 60),
            (200, -1, 12),
            (200, 5, 12),
            (200, 12, 12),
            (200, 53, 53),
            (200, 59, 59),
            (200, 60, 60),
        ];
        for (cols, longest, want) in table {
            assert_eq!(picker_namew(cols, longest), want, "{cols} {longest}");
        }
    }

    #[test]
    fn tag_hash_matches_python() {
        assert_eq!(tag_hash("ab"), 3105);
        assert_eq!(tag_hash("日本語"), 25_921_943);
        assert_eq!(tag_hash(""), 0);
        assert_eq!(tag_ansi("a"), "\x1b[38;5;167m");
        assert_eq!(tag_ansi("é"), "\x1b[38;5;73m");
        assert_eq!(tag_ansi("日本語"), "\x1b[38;5;140m");
        assert_eq!(tag_hex("nvim"), "colour80");
        assert_eq!(tag_hex("日本語"), "colour140");
    }

    #[test]
    fn row_layout_is_byte_exact() {
        let mut session = agent("worker-1", NOW - 90.0);
        session.tags = vec!["a".into()];
        session.ext.insert(
            "hooks".into(),
            json!({"state": "working", "since": NOW - 5.0}),
        );
        let attached = [location("main", "1")];
        let rows = [Row {
            session: &session,
            target: "$3",
            attached: &attached,
        }];
        let want = concat!(
            "worker-1\t",
            " [a]\t",
            "L\t",
            "$3\t",
            "worker-1",
            "    ",
            "   ",
            "main[1]                  ",
            " ",
            "1m     ",
            " ",
            "\x1b[38;2;224;175;104m",
            "5s    ",
            "\x1b[39m",
            " ",
            "\x1b[38;5;167m",
            "llm  ",
            "\x1b[39m",
            " ",
            "\x1b[38;5;167m",
            "[a]",
            "\x1b[39m",
        );
        assert_eq!(picker_rows(&rows, 12, NOW), want);
    }

    #[test]
    fn long_names_truncate_and_rows_sort_by_activity() {
        let old = agent("o", NOW - 500.0);
        let long = process("a-very-long-session-name", NOW - 100.0);
        let mut fresh = agent("f", NOW - 900.0);
        fresh.ext.insert(
            "hooks".into(),
            json!({"state": "idle", "since": NOW - 10.0}),
        );
        let rows = [&old, &long, &fresh].map(|session| Row {
            session,
            target: "$1",
            attached: &[],
        });
        let out = picker_rows(&rows, 12, NOW);
        let names: Vec<&str> = out
            .lines()
            .map(|line| line.split('\t').next().unwrap())
            .collect();
        assert_eq!(names, ["f", "a-very-long-session-name", "o"]);
        let visual = out.lines().nth(1).unwrap().split('\t').nth(4).unwrap();
        assert!(visual.starts_with("a-very-long…   —"), "{visual:?}");
        assert!(visual.contains(" —      "), "{visual:?}");
    }

    #[test]
    fn roles_and_idle_cells() {
        let mut view = process("v", NOW - 100.0);
        assert_eq!(role_of(&view), "shell");
        view.ext.insert("views".into(), json!({"kind": "nvim"}));
        assert_eq!(role_of(&view), "nvim");
        view.ext.insert("views".into(), json!({"kind": "view"}));
        assert_eq!(role_of(&view), "view");
        view.ext.insert("views".into(), json!({"kind": "other"}));
        assert_eq!(role_of(&view), "shell");
        assert_eq!(role_of(&agent("a", 1.0)), "llm");
        assert_eq!(idle_cell(&view, NOW), "—");
        assert_eq!(idle_cell(&agent("a", 1.0), NOW), "-");
    }

    #[test]
    fn malformed_hooks_ext_is_tolerated() {
        for bad in [json!("oops"), json!(7), json!([1]), json!({"since": "x"})] {
            let mut session = agent("w", NOW - 90.0);
            session.ext.insert("hooks".into(), bad);
            session.ext.insert("views".into(), json!("junk"));
            assert_eq!(activity_at(&session), NOW - 90.0);
            assert_eq!(idle_cell(&session, NOW), "-");
            assert_eq!(role_of(&session), "llm");
            let rows = [Row {
                session: &session,
                target: "$1",
                attached: &[],
            }];
            let out = picker_rows(&rows, 12, NOW);
            assert!(out.contains("1m"), "{out:?}");
        }
        let mut process = process("p", 5.0);
        process.ext.insert("views".into(), json!(42));
        assert_eq!(role_of(&process), "shell");
    }
}
