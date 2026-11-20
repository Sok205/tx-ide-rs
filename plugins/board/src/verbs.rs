//! `tx board` (the TUI in this pane, `--snapshot`), `_board-resident` (the TUI a popup
//! attaches to), `_board-open` (prefix+b: the popup on the client the key was pressed on),
//! `_board-attach` (inside the popup: start or attach the resident of the checkout's
//! repository, then show what a key asked for) and `_board-json` (the board as JSON).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use tx_client::show::Show;
use tx_client::tmux::{Ui, shell_quote};
use tx_client::views::{self, ViewKind, ViewRequest};
use tx_core::{TxCore, Verb, VerbError, VerbIo};

use crate::github::{self, Repository};
use crate::model::{Board, Query, load_agents};
use crate::{PLUGIN_ID, run};

pub const RESIDENT_VERB: &str = "_board-resident";
const ATTACH_VERB: &str = "_board-attach";
/// The tmux sessions (views, not records) hosting the resident boards: one per repository.
pub const POPUP_SESSION_PREFIX: &str = "tx-board-";
const POPUP_LOCK: &str = "popup.lock";
/// Session option on a resident: the build of the program that started it, so an attach after
/// an upgrade starts a fresh one instead of showing the old process.
const BUILD_OPTION: &str = "@tx-board-build";
const DEFAULT_SNAPSHOT_COLUMNS: u16 = 120;
const DEFAULT_SNAPSHOT_ROWS: u16 = 40;
/// prefix+b's popup, and its inside (less the border) that the resident's window is sized to.
const POPUP_COLUMNS: &str = "118";
const POPUP_ROWS: &str = "34";
const POPUP_INSIDE_COLUMNS: &str = "116";
const POPUP_INSIDE_ROWS: &str = "32";

const USAGE: &str = "\
usage: tx board [--repo OWNER/NAME] [--cwd DIR] [--snapshot [--cols N] [--rows N] [--issues FILE] [--query Q] [--now EPOCH]]
";
const JSON_USAGE: &str =
    "usage: tx _board-json [--repo OWNER/NAME] [--cwd DIR] [--issues FILE] [--query Q]\n";

/// `plugins.board.settings`: the repository to fall back on when the checkout has no GitHub
/// `origin`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub repository: Option<Repository>,
}

impl Settings {
    pub fn from_value(value: &serde_json::Value) -> Result<Self, String> {
        let repository = match value.get("repository") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(slug)) => Some(
                Repository::parse(slug)
                    .ok_or_else(|| format!("settings.repository: {slug:?} is not OWNER/NAME"))?,
            ),
            Some(other) => {
                return Err(format!(
                    "settings.repository: expected a string, got {other}"
                ));
            }
        };
        Ok(Self { repository })
    }
}

pub fn verbs(settings: Settings) -> Vec<Arc<dyn Verb>> {
    let settings = Arc::new(settings);
    vec![
        Arc::new(BoardVerb {
            settings: settings.clone(),
            resident: false,
        }),
        Arc::new(BoardVerb {
            settings: settings.clone(),
            resident: true,
        }),
        Arc::new(BoardOpen {
            settings: settings.clone(),
        }),
        Arc::new(BoardAttach {
            settings: settings.clone(),
        }),
        Arc::new(BoardJson { settings }),
    ]
}

fn failed(error: impl std::fmt::Display) -> VerbError {
    VerbError::Failed(error.to_string())
}

fn say(out: &mut dyn Write, text: &str) -> Result<(), VerbError> {
    out.write_all(text.as_bytes()).map_err(failed)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
    pub repository: Option<String>,
    pub cwd: Option<PathBuf>,
    pub snapshot: bool,
    pub columns: u16,
    pub rows: u16,
    /// A file of issues as `gh issue list --json` prints them, instead of asking GitHub.
    pub issues: Option<PathBuf>,
    /// A filter in GitHub's search language, as `/` takes it.
    pub query: Option<String>,
    /// Epoch seconds the view takes as now; the clock unless given (reproducible output).
    pub now: Option<f64>,
}

/// The flags of `board` / `_board-resident` / `_board-json`; `Err` is the message.
pub fn parse_options(argv: &[String], allowed: &[&str]) -> Result<Options, String> {
    let mut options = Options {
        columns: DEFAULT_SNAPSHOT_COLUMNS,
        rows: DEFAULT_SNAPSHOT_ROWS,
        ..Options::default()
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
            "--repo" => {
                Repository::parse(value).ok_or_else(|| bad("expected OWNER/NAME"))?;
                options.repository = Some(value.clone());
            }
            "--cwd" => options.cwd = Some(PathBuf::from(value)),
            "--issues" => options.issues = Some(PathBuf::from(value)),
            "--query" => options.query = Some(value.clone()),
            "--cols" | "--rows" => {
                let number: i64 = value.parse().map_err(|_| bad("expected an integer"))?;
                if flag == "--cols" {
                    options.columns = number.clamp(20, 1000) as u16;
                } else {
                    options.rows = number.clamp(6, 1000) as u16;
                }
            }
            "--now" => {
                options.now = Some(
                    value
                        .parse::<f64>()
                        .ok()
                        .filter(|now| now.is_finite() && *now > 0.0)
                        .ok_or_else(|| bad("expected epoch seconds"))?,
                );
            }
            _ => unreachable!("allowed flags are matched above"),
        }
    }
    Ok(options)
}

/// The board's place: the checkout holding `directory` (if any) and the repository: `--repo`,
/// else the checkout's `git config tx.board.repository` (a checkout whose issues live
/// elsewhere), else its GitHub `origin`, else the setting.
pub struct Place {
    pub repository: Repository,
    pub checkout: Option<PathBuf>,
}

/// The git configuration key naming a checkout's issue repository.
pub const GIT_CONFIG_KEY: &str = "tx.board.repository";

pub fn place(
    settings: &Settings,
    repository: Option<&str>,
    directory: &Path,
) -> Result<Place, String> {
    let checkout = github::checkout_of(directory);
    let configured = checkout
        .as_deref()
        .and_then(|checkout| github::git(checkout, &["config", "--get", GIT_CONFIG_KEY]));
    let repository = match (repository, configured) {
        (Some(slug), _) => {
            Repository::parse(slug).ok_or_else(|| format!("{slug}: not OWNER/NAME"))?
        }
        (None, Some(slug)) => Repository::parse(&slug)
            .ok_or_else(|| format!("git config {GIT_CONFIG_KEY}: {slug:?} is not OWNER/NAME"))?,
        (None, None) => checkout
            .as_deref()
            .and_then(Repository::of_checkout)
            .or_else(|| settings.repository.clone())
            .ok_or_else(|| {
                format!(
                    "{}: not inside a checkout with a GitHub origin; pass --repo OWNER/NAME, set git config {GIT_CONFIG_KEY}, or set plugins.board.settings.repository",
                    directory.display()
                )
            })?,
    };
    Ok(Place {
        repository,
        checkout,
    })
}

fn current_directory() -> Result<PathBuf, VerbError> {
    std::env::current_dir().map_err(|error| failed(format!("current directory: {error}")))
}

/// The issues: the `--issues` file, else GitHub.
fn issues(options: &Options, repository: &Repository) -> Result<Vec<github::Issue>, VerbError> {
    match &options.issues {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|error| failed(format!("{}: {error}", path.display())))?;
            github::parse_issues(&text).map_err(failed)
        }
        None => github::fetch_issues(repository).map_err(failed),
    }
}

struct BoardVerb {
    settings: Arc<Settings>,
    resident: bool,
}

impl Verb for BoardVerb {
    fn name(&self) -> &'static str {
        if self.resident {
            RESIDENT_VERB
        } else {
            "board"
        }
    }

    fn usage(&self) -> &'static str {
        if self.resident {
            "_board-resident --repo OWNER/NAME [--cwd DIR]   the popup's resident board; q detaches (internal)"
        } else {
            "board [--repo OWNER/NAME] [--cwd DIR] [--snapshot]   the GitHub issue board of a checkout (q quits)"
        }
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let allowed = [
            "--repo",
            "--cwd",
            "--snapshot",
            "--cols",
            "--rows",
            "--issues",
            "--query",
            "--now",
        ];
        let options = match parse_options(argv, &allowed) {
            Ok(options) => options,
            Err(message) => {
                say(
                    io.stderr,
                    &format!("tx {}: {message}\n{USAGE}", self.name()),
                )?;
                return Ok(2);
            }
        };
        let directory = match &options.cwd {
            Some(directory) => directory.clone(),
            None => current_directory()?,
        };
        let place = match place(&self.settings, options.repository.as_deref(), &directory) {
            Ok(place) => place,
            Err(message) => {
                say(io.stderr, &format!("tx {}: {message}\n", self.name()))?;
                return Ok(1);
            }
        };
        if options.snapshot {
            let now = options.now.unwrap_or_else(tx_core::events::now);
            let issues = issues(&options, &place.repository)?;
            let query = Query::parse(options.query.as_deref().unwrap_or(""));
            let board = Board::build(place.repository, issues, &load_agents(core)?, &query);
            let text = run::snapshot(board, query, now, now, options.columns, options.rows);
            writeln!(io.stdout, "{text}").map_err(failed)?;
            return Ok(0);
        }
        let resident = self.resident.then(|| resident_session(&place.repository));
        run::run(run::Context {
            core,
            repository: place.repository,
            checkout: place.checkout,
            resident: resident.as_deref(),
        })
        .map_err(failed)?;
        Ok(0)
    }
}

/// `tx-board-<owner>-<name>`, in the characters a tmux session name takes.
pub fn resident_session(repository: &Repository) -> String {
    let clean = |text: &str| -> String {
        text.chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                    character.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect()
    };
    format!(
        "{POPUP_SESSION_PREFIX}{}-{}",
        clean(&repository.owner),
        clean(&repository.name)
    )
}

/// `<tx> <cli_arguments> <arguments>`, as argv.
fn tx_argv(core: &TxCore, arguments: &[&str]) -> Vec<String> {
    let mut words = vec![tx_plugin::sdk::tx_bin().to_string_lossy().into_owned()];
    words.extend(core.config().cli_arguments().iter().cloned());
    words.extend(arguments.iter().map(|word| (*word).to_owned()));
    words
}

/// `<tx> <cli_arguments> <arguments>`, as one shell command.
fn tx_command(core: &TxCore, arguments: &[&str]) -> String {
    tx_argv(core, arguments)
        .iter()
        .map(|word| shell_quote(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// This program's build, as a resident records it: the modification time of its binary.
fn build_stamp() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| std::fs::metadata(exe).ok())
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|age| age.as_secs().to_string())
        .unwrap_or_default()
}

/// Starts the resident view of `place` unless it is running, and sizes its window to the
/// popup's inside on every attach. A resident left by an older build is closed and started
/// again, so an upgrade shows on the next prefix+b. Callers hold the popup lock.
fn ensure_resident(core: &TxCore, ui: &Ui, place: &Place) -> Result<String, VerbError> {
    let session = resident_session(&place.repository);
    let stamp = build_stamp();
    if ui.has_session(&session) && ui.session_option(&session, BUILD_OPTION) != Some(stamp.clone())
    {
        views::close(ui, &session).map_err(failed)?;
    }
    if !ui.has_session(&session) {
        let slug = place.repository.slug();
        let mut arguments = vec![RESIDENT_VERB, "--repo", &slug];
        let checkout = place
            .checkout
            .as_ref()
            .map(|checkout| checkout.to_string_lossy().into_owned());
        if let Some(checkout) = &checkout {
            arguments.extend(["--cwd", checkout]);
        }
        let argv = tx_argv(core, &arguments);
        let working_directory = place
            .checkout
            .clone()
            .unwrap_or_else(|| core.config().directories.data.clone());
        views::open(
            ui,
            &ViewRequest {
                name: &session,
                kind: ViewKind::Board,
                program: &argv[0],
                arguments: &argv[1..],
                working_directory: &working_directory,
                tags: &[],
                parent_id: None,
                bound: None,
            },
        )
        .map_err(failed)?;
        for (option, value) in [("status", "off"), (BUILD_OPTION, stamp.as_str())] {
            ui.set_session_option(&session, option, value)
                .map_err(failed)?;
        }
    }
    ui.run(&[
        "resize-window",
        "-t",
        &format!("={session}:"),
        "-x",
        POPUP_INSIDE_COLUMNS,
        "-y",
        POPUP_INSIDE_ROWS,
    ])
    .map_err(failed)?;
    Ok(session)
}

/// `_board-open <pane> <client> <path>`: prefix+b. The popup cannot expand formats in its
/// command, so the bind runs this through `run-shell`, which hands over the pane the key was
/// pressed in, the client it was pressed on and the pane's directory.
struct BoardOpen {
    settings: Arc<Settings>,
}

impl Verb for BoardOpen {
    fn name(&self) -> &'static str {
        "_board-open"
    }

    fn usage(&self) -> &'static str {
        "_board-open <pane> <client> <path>   prefix+b: the resident board of a checkout in a popup (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [pane, client, path] = argv else {
            say(io.stderr, "usage: tx _board-open <pane> <client> <path>\n")?;
            return Ok(2);
        };
        let ui = Ui::from_core(core);
        let place = match place(&self.settings, None, Path::new(path)) {
            Ok(place) => place,
            Err(message) => {
                ui.display_status(client, &format!("board: {message}"));
                return Ok(1);
            }
        };
        let command = tx_command(core, &[ATTACH_VERB, pane, client, path]);
        ui.run(&[
            "display-popup",
            "-c",
            client,
            "-E",
            "-w",
            POPUP_COLUMNS,
            "-h",
            POPUP_ROWS,
            "-x",
            "C",
            "-y",
            "1",
            "-T",
            &format!(" board {} ", place.repository.slug()),
            &command,
        ])
        .map_err(failed)?;
        Ok(0)
    }
}

/// The popup is a client of a persistent resident board, so closing it keeps it warm. A show
/// in the resident (Enter / a / p) is done once the attach returns, through tx-client
/// ([`Ui::show_session`], safe from inside this popup). `<pane> <client>` are where prefix+b
/// was pressed; `<path>` the pane's directory, which names the repository.
struct BoardAttach {
    settings: Arc<Settings>,
}

impl Verb for BoardAttach {
    fn name(&self) -> &'static str {
        ATTACH_VERB
    }

    fn usage(&self) -> &'static str {
        "_board-attach <pane> <client> <path>   start or attach the resident board; q detaches (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [pane, client, path] = argv else {
            say(
                io.stderr,
                "usage: tx _board-attach <pane> <client> <path>\n",
            )?;
            return Ok(2);
        };
        let place = place(&self.settings, None, Path::new(path)).map_err(failed)?;
        let ui = Ui::from_core(core);
        let dir = core.config().plugin_data_dir(PLUGIN_ID);
        std::fs::create_dir_all(&dir)
            .map_err(|error| failed(format!("{}: {error}", dir.display())))?;
        // Two clients opening at once must reuse the same session.
        let lock_path = dir.join(POPUP_LOCK);
        let lock = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| failed(format!("{}: {error}", lock_path.display())))?;
        lock.lock().map_err(failed)?;
        let session = ensure_resident(core, &ui, &place)?;
        drop(lock);
        let status = ui
            .command(&["attach-session", "-t", &format!("={session}")])
            .stdin(Stdio::inherit())
            .env_remove("TMUX")
            .status()
            .map_err(failed)?;
        // A show in the resident left `<mode> <name>` behind and detached us.
        if let Some(pick) = ui.session_option(&session, run::GOTO_OPTION) {
            ui.quiet(&[
                "set-option",
                "-u",
                "-t",
                &format!("={session}:"),
                run::GOTO_OPTION,
            ]);
            let (show, name) = pick
                .split_once(' ')
                .ok_or_else(|| failed(format!("{}: {pick:?}", run::GOTO_OPTION)))?;
            let show = Show::parse(show)
                .ok_or_else(|| failed(format!("{}: {pick:?}", run::GOTO_OPTION)))?;
            if let Err(reason) = ui.show_session(client, show, name, Some(pane)) {
                ui.display_status(client, &format!("board: {name}: {reason}"));
            }
        }
        Ok(status.code().unwrap_or(1))
    }
}

struct BoardJson {
    settings: Arc<Settings>,
}

impl Verb for BoardJson {
    fn name(&self) -> &'static str {
        "_board-json"
    }

    fn usage(&self) -> &'static str {
        "_board-json [--repo OWNER/NAME] [--cwd DIR] [--query Q]   the board as JSON (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let options = match parse_options(argv, &["--repo", "--cwd", "--issues", "--query"]) {
            Ok(options) => options,
            Err(message) => {
                say(
                    io.stderr,
                    &format!("tx _board-json: {message}\n{JSON_USAGE}"),
                )?;
                return Ok(2);
            }
        };
        let directory = match &options.cwd {
            Some(directory) => directory.clone(),
            None => current_directory()?,
        };
        let place =
            place(&self.settings, options.repository.as_deref(), &directory).map_err(failed)?;
        let issues = issues(&options, &place.repository)?;
        let query = Query::parse(options.query.as_deref().unwrap_or(""));
        let board = Board::build(place.repository, issues, &load_agents(core)?, &query);
        let text = serde_json::to_string_pretty(&board.to_json()).map_err(failed)?;
        writeln!(io.stdout, "{text}").map_err(failed)?;
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    const ALL: [&str; 8] = [
        "--repo",
        "--cwd",
        "--snapshot",
        "--cols",
        "--rows",
        "--issues",
        "--query",
        "--now",
    ];

    #[test]
    fn options_default_and_parse() {
        let options = parse_options(&[], &ALL).unwrap();
        assert_eq!(
            options,
            Options {
                columns: 120,
                rows: 40,
                ..Options::default()
            }
        );
        let options = parse_options(
            &argv(&[
                "--repo",
                "o/n",
                "--snapshot",
                "--cols",
                "5",
                "--now",
                "9",
                "--issues",
                "f",
                "--query",
                "label:bug",
            ]),
            &ALL,
        )
        .unwrap();
        assert_eq!(options.repository.as_deref(), Some("o/n"));
        assert!(options.snapshot);
        assert_eq!((options.columns, options.now), (20, Some(9.0)));
        assert_eq!(options.issues, Some(PathBuf::from("f")));
        assert_eq!(options.query.as_deref(), Some("label:bug"));
    }

    #[test]
    fn options_reject_garbage() {
        for bad in [
            &["--repo", "x"][..],
            &["--cols"],
            &["--now", "soon"],
            &["--bogus"],
        ] {
            assert!(parse_options(&argv(bad), &ALL).is_err(), "{bad:?}");
        }
        assert!(parse_options(&argv(&["--snapshot"]), &["--repo"]).is_err());
    }

    #[test]
    fn settings_name_a_repository_or_nothing() {
        assert_eq!(
            Settings::from_value(&serde_json::Value::Null).unwrap(),
            Settings::default()
        );
        assert_eq!(
            Settings::from_value(&serde_json::json!({"repository": "o/n"})).unwrap(),
            Settings {
                repository: Repository::parse("o/n")
            }
        );
        assert!(Settings::from_value(&serde_json::json!({"repository": "nope"})).is_err());
        assert!(Settings::from_value(&serde_json::json!({"repository": 3})).is_err());
    }

    #[test]
    fn resident_sessions_are_named_after_the_repository() {
        let repository = Repository::parse("Tx-Devs/tx.ide").unwrap();
        assert_eq!(resident_session(&repository), "tx-board-tx-devs-tx-ide");
    }

    #[test]
    fn the_place_of_a_directory_outside_any_checkout_is_the_setting() {
        let scratch = tempfile::tempdir().unwrap();
        let settings = Settings {
            repository: Repository::parse("o/n"),
        };
        let place = place(&settings, None, scratch.path()).unwrap();
        assert_eq!(place.repository.slug(), "o/n");
        assert_eq!(place.checkout, None);
        assert!(place_fails(&Settings::default(), scratch.path()));
        let place = super::place(&Settings::default(), Some("a/b"), scratch.path()).unwrap();
        assert_eq!(place.repository.slug(), "a/b");
    }

    fn place_fails(settings: &Settings, directory: &Path) -> bool {
        super::place(settings, None, directory).is_err()
    }

    fn run_git(directory: &Path, arguments: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(status.status.success(), "git {arguments:?}");
    }

    #[test]
    fn a_checkout_names_its_repository_by_git_config_before_its_origin() {
        let scratch = tempfile::tempdir().unwrap();
        let checkout = scratch.path().join("c");
        std::fs::create_dir(&checkout).unwrap();
        run_git(&checkout, &["init", "-q"]);
        let inside = checkout.join("src");
        std::fs::create_dir(&inside).unwrap();
        assert!(place_fails(&Settings::default(), &inside));
        run_git(
            &checkout,
            &["remote", "add", "origin", "git@github.com:me/fork.git"],
        );
        let found = super::place(&Settings::default(), None, &inside).unwrap();
        assert_eq!(found.repository.slug(), "me/fork");
        assert_eq!(
            found.checkout.as_deref().and_then(|path| path.file_name()),
            checkout.file_name()
        );
        run_git(&checkout, &["config", GIT_CONFIG_KEY, "them/upstream"]);
        let found = super::place(&Settings::default(), None, &inside).unwrap();
        assert_eq!(found.repository.slug(), "them/upstream");
        let found = super::place(&Settings::default(), Some("a/b"), &inside).unwrap();
        assert_eq!(found.repository.slug(), "a/b");
    }
}
