//! Picker row rendering: port of main's `render.rs` picker feed and `palette.rs` tag colours.
//! Pure formatting; every function takes the records and `now`, nothing reads the clock.

use tx_core::Agent;

use crate::tmux::Location;
use crate::views::{View, ViewKind};

pub const WARN_ANSI: &str = "\x1b[38;2;224;175;104m";
pub const RESET_FG: &str = "\x1b[39m";
pub const ACCENT_ANSI: &str = "\x1b[38;2;122;162;247m";
pub const FG_ANSI: &str = "\x1b[38;2;192;202;245m";
pub const BOLD: &str = "\x1b[1m";
pub const RESET: &str = "\x1b[0m";

// Inlined from main's `crates/tx/src/palette.rs` (tx-client has no palette module).
pub const DIM_FG_ANSI: &str = "\x1b[38;2;169;177;214m";
pub const INFO_ANSI: &str = "\x1b[38;2;125;207;255m";
pub const ITALIC: &str = "\x1b[3m";
pub const SELECTION_BG: &str = "236";

/// LOCATION column width, shared with the picker header.
pub const LOCATION_W: usize = 25;
/// ROLE column width (the longest role, `shell` / `other`).
pub const ROLE_W: usize = 5;

const NAMEW_OVERHEAD: i64 = 68;
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

/// One picker row's subject: an agent (a core record) or a view (a tx-client tmux session).
#[derive(Clone, Copy, Debug)]
pub enum PickerEntry<'a> {
    Agent(&'a Agent),
    View(&'a View),
}

impl<'a> PickerEntry<'a> {
    /// The tmux session name, for both kinds.
    pub fn name(self) -> &'a str {
        match self {
            PickerEntry::Agent(session) => &session.name,
            PickerEntry::View(view) => &view.name,
        }
    }

    pub fn tags(self) -> &'a [String] {
        match self {
            PickerEntry::Agent(session) => &session.tags,
            PickerEntry::View(view) => &view.tags,
        }
    }

    pub fn created_at(self) -> f64 {
        match self {
            PickerEntry::Agent(session) => session.created_at,
            PickerEntry::View(view) => view.created_at,
        }
    }
}

/// ROLE, one of the four roles of `tx attach`: `llm` for an agent; `nvim`, `shell` or `other`
/// for a view by its kind.
pub fn role_of(entry: PickerEntry<'_>) -> &'static str {
    match entry {
        PickerEntry::Agent(_) => "llm",
        PickerEntry::View(view) => match view.kind {
            ViewKind::Editor | ViewKind::Diff | ViewKind::Tour => "nvim",
            ViewKind::Shell => "shell",
            ViewKind::Home | ViewKind::Timeline | ViewKind::Tutor | ViewKind::Mockup => "other",
        },
    }
}

/// Whether the picker lists `entry`: every agent and every view but the home base, which is
/// where the picker is opened from.
pub fn listed(entry: PickerEntry<'_>) -> bool {
    !matches!(entry, PickerEntry::View(view) if view.kind == ViewKind::Home)
}

/// The picker's sort key: when an agent last did something (its last record write for a record
/// that has not moved state since `last_activity_at` arrived); when a view was created (a view
/// has no activity signal).
pub fn activity_at(entry: PickerEntry<'_>) -> f64 {
    match entry {
        PickerEntry::Agent(session) => session.last_activity_at.unwrap_or(session.last_modified_at),
        PickerEntry::View(view) => view.created_at,
    }
}

/// IDLE: the last-turn age for an agent, `—` for a view (no activity signal).
pub fn idle_cell(entry: PickerEntry<'_>, now: f64) -> String {
    match entry {
        PickerEntry::Agent(_) => reltime(Some(activity_at(entry)), now),
        PickerEntry::View(_) => "—".into(),
    }
}

pub struct Row<'a> {
    pub entry: PickerEntry<'a>,
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

/// One fzf row: `name \t plain_chips \t L \t target \t visual`; the binds read fields 1 to 4.
fn picker_row(row: &Row<'_>, namew: i64, now: f64) -> String {
    let entry = row.entry;
    let role = role_of(entry);
    let name_disp = trunc(entry.name(), namew);
    let pad = " ".repeat((namew - name_disp.chars().count() as i64).max(0) as usize);
    let colored_chips: String = entry
        .tags()
        .iter()
        .map(|tag| format!(" {}[{tag}]{RESET_FG}", tag_ansi(tag)))
        .collect();
    let started = reltime(Some(entry.created_at()), now);
    let idle = idle_cell(entry, now);
    let role_cell = format!("{}{role:<ROLE_W$}{RESET_FG}", tag_ansi(role));
    let location = location_text(row.attached);
    let visual = format!(
        "{name_disp}{pad}   {location:<LOCATION_W$} {started:<7} {WARN_ANSI}{idle:<6}{RESET_FG} {role_cell}{colored_chips}"
    );
    [entry.name(), &chips(entry.tags()), "L", row.target, &visual].join("\t")
}

/// The fzf picker feed: one flat list, newest activity first (stable for ties).
pub fn picker_rows(rows: &[Row<'_>], namew: i64, now: f64) -> String {
    let mut sorted: Vec<&Row<'_>> = rows.iter().collect();
    sorted.sort_by(|a, b| activity_at(b.entry).total_cmp(&activity_at(a.entry)));
    sorted
        .iter()
        .map(|row| picker_row(row, namew, now))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tx_core::AgentId;

    use super::*;
    use crate::views::ViewKind;

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

    fn agent(name: &str, created_at: f64) -> Agent {
        Agent {
            id: format!("id-{name}").parse::<AgentId>().unwrap(),
            name: name.into(),
            engine: tx_core::EngineKind::Claude,
            working_directory: PathBuf::from("/r"),
            roles: vec![],
            state: tx_core::LlmEngineState::Idle,
            created_at,
            last_modified_at: created_at,
            last_activity_at: None,
            archived_at: None,
            tags: vec![],
            parent_id: None,
            ext: Default::default(),
            other: Default::default(),
        }
    }

    fn view(name: &str, kind: ViewKind, parent: Option<&Agent>, created_at: f64) -> View {
        View {
            name: name.into(),
            kind,
            working_directory: PathBuf::from("/r"),
            tags: vec![],
            parent_id: parent.map(|session| session.id.clone()),
            bound: None,
            created_at,
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
            (118, 53, 50),
            (118, 59, 50),
            (118, 60, 50),
            (118, 80, 50),
            (125, -1, 12),
            (125, 5, 12),
            (125, 12, 12),
            (125, 53, 53),
            (125, 59, 57),
            (125, 60, 57),
            (125, 80, 57),
            (126, -1, 12),
            (126, 5, 12),
            (126, 12, 12),
            (126, 53, 53),
            (126, 59, 58),
            (126, 60, 58),
            (126, 80, 58),
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
        session.last_modified_at = NOW - 5.0;
        let attached = [location("main", "1")];
        let rows = [Row {
            entry: PickerEntry::Agent(&session),
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

    fn names(feed: &str) -> Vec<&str> {
        feed.lines()
            .map(|line| line.split('\t').next().unwrap())
            .collect()
    }

    fn visual<'a>(feed: &'a str, name: &str) -> &'a str {
        feed.lines()
            .find(|line| line.split('\t').next() == Some(name))
            .unwrap()
            .split('\t')
            .nth(4)
            .unwrap()
    }

    #[test]
    fn long_names_truncate_and_agents_sort_by_activity() {
        let old = agent("o", NOW - 500.0);
        let long = agent("a-very-long-session-name", NOW - 100.0);
        let mut fresh = agent("f", NOW - 900.0);
        fresh.last_modified_at = NOW - 10.0;
        let rows = [&old, &long, &fresh].map(|session| Row {
            entry: PickerEntry::Agent(session),
            target: "$1",
            attached: &[],
        });
        let out = picker_rows(&rows, 12, NOW);
        assert_eq!(names(&out), ["f", "a-very-long-session-name", "o"]);
        let shown = visual(&out, "a-very-long-session-name");
        assert!(shown.starts_with("a-very-long…   —"), "{shown:?}");
    }

    #[test]
    fn agents_and_views_form_one_flat_list_by_activity() {
        let first = agent("first", NOW - 900.0);
        let mut second = agent("second", NOW - 800.0);
        second.last_modified_at = NOW - 5.0;
        let editor = view("plan-editor", ViewKind::Editor, Some(&first), NOW - 60.0);
        let diff = view("plan-diff", ViewKind::Diff, Some(&first), NOW - 1200.0);
        let shell = view("sh", ViewKind::Shell, Some(&second), NOW - 30.0);
        let targets = ["$1", "$2", "$3", "$4", "$5"];
        let entries = [
            PickerEntry::View(&editor),
            PickerEntry::Agent(&first),
            PickerEntry::View(&shell),
            PickerEntry::Agent(&second),
            PickerEntry::View(&diff),
        ];
        let rows: Vec<Row<'_>> = entries
            .iter()
            .zip(targets)
            .map(|(entry, target)| Row {
                entry: *entry,
                target,
                attached: &[],
            })
            .collect();
        let out = picker_rows(&rows, 16, NOW);
        assert_eq!(
            names(&out),
            ["second", "sh", "plan-editor", "first", "plan-diff"]
        );
        // Fields 1 to 4 stay plain: the binds read the name and the tmux target by position.
        let editor_line = out
            .lines()
            .find(|line| line.starts_with("plan-editor\t"))
            .unwrap();
        let fields: Vec<&str> = editor_line.split('\t').collect();
        assert_eq!(fields[..4], ["plan-editor", "", "L", "$1"]);
        for name in names(&out) {
            assert!(
                visual(&out, name).starts_with(&format!("{name} ")),
                "{name}"
            );
        }
        assert!(!out.contains('└'), "{out:?}");
    }

    #[test]
    fn roles_and_idle_cells() {
        let parent = agent("a", NOW - 30.0);
        assert_eq!(role_of(PickerEntry::Agent(&parent)), "llm");
        assert_eq!(idle_cell(PickerEntry::Agent(&parent), NOW), "30s");
        let roles = [
            (ViewKind::Home, "other"),
            (ViewKind::Editor, "nvim"),
            (ViewKind::Diff, "nvim"),
            (ViewKind::Tour, "nvim"),
            (ViewKind::Shell, "shell"),
            (ViewKind::Timeline, "other"),
            (ViewKind::Tutor, "other"),
            (ViewKind::Mockup, "other"),
        ];
        assert_eq!(roles.map(|(kind, _)| kind), ViewKind::ALL);
        for (kind, role) in roles {
            let shown = view("v", kind, Some(&parent), NOW - 100.0);
            assert_eq!(role_of(PickerEntry::View(&shown)), role, "{kind}");
            assert!(role.len() <= ROLE_W, "{kind}");
            assert_eq!(idle_cell(PickerEntry::View(&shown), NOW), "—");
        }
    }

    #[test]
    fn only_the_home_view_is_not_listed() {
        let parent = agent("a", NOW);
        assert!(listed(PickerEntry::Agent(&parent)));
        for kind in ViewKind::ALL {
            let shown = view("v", kind, None, NOW);
            assert_eq!(
                listed(PickerEntry::View(&shown)),
                kind != ViewKind::Home,
                "{kind}"
            );
        }
    }

    #[test]
    fn a_view_row_shows_its_role_age_and_tags() {
        let mut shown = view("notes", ViewKind::Timeline, None, NOW - 120.0);
        shown.tags = vec!["qa".into()];
        let rows = [Row {
            entry: PickerEntry::View(&shown),
            target: "$9",
            attached: &[],
        }];
        let out = picker_rows(&rows, 12, NOW);
        let fields: Vec<&str> = out.split('\t').collect();
        assert_eq!(fields[..4], ["notes", " [qa]", "L", "$9"]);
        assert!(fields[4].contains("2m     "), "{:?}", fields[4]);
        assert!(fields[4].contains("other"), "{:?}", fields[4]);
    }

    #[test]
    fn an_untouched_agent_reads_its_creation() {
        let session = agent("w", NOW - 90.0);
        assert_eq!(activity_at(PickerEntry::Agent(&session)), NOW - 90.0);
        assert_eq!(idle_cell(PickerEntry::Agent(&session), NOW), "1m");
    }
}
