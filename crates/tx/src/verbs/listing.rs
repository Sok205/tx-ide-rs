//! The read / browse verbs and the picker (cli.py): `ls` (Q11 FIX: `--json`), `_list`, `show`,
//! `history`, `chat ls`, `start`, `attach` (the fzf picker), `_edit-session`, `_edit-tag`.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, ExitStatus, Stdio};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cordis::{BoxError, Component, Ctx};
use serde_json::Value;

use crate::app::{COMMANDS, Command, ENGINES, ENV, Env, HOME, SERVICE, TMUX, Visibility};
use crate::argparse::{Arg, Matches, ParseExit, Parser};
use crate::engines::EngineRegistry;
use crate::palette;
use crate::pyjson::dumps_pretty;
use crate::render::{
    LOCATION_W, ROLE_W, picker_display_rows, picker_namew, render_chats, render_history, render_ls,
};
use crate::service::SessionService;
use crate::session::{Session, State};
use crate::session_editor::edit_session;
use crate::shlex::shlex_quote;
use crate::spawn::{SHELL_COMMANDS, SpawnSpec};
use crate::storage::Home;
use crate::tmux::Tmux;
use crate::verbs::common::{default_shell, detect_term_cols, env_namew, split_tags};

/// The pause before the picker re-runs after a vanished session / a failed jump.
const RETRY_PAUSE: Duration = Duration::from_millis(1200);

/// The `ServiceError`s cli.py raises itself (printed `tx <verb>: <text>`, exit 1).
#[derive(Debug, thiserror::Error)]
enum ListingError {
    #[error("no tx session in this pane (view homes cannot be edited)")]
    NoSessionInPane,
    #[error("the session in this pane is not tx-managed")]
    NotTxManaged,
    #[error("could not run {program}: {source}")]
    Run {
        program: String,
        source: std::io::Error,
    },
}

fn run_error(program: impl AsRef<Path>) -> impl FnOnce(std::io::Error) -> ListingError {
    let program = program.as_ref().display().to_string();
    move |source| ListingError::Run { program, source }
}

/// Everything the listing verbs share.
struct Deps {
    service: Rc<SessionService>,
    tmux: Rc<Tmux>,
    env: Rc<Env>,
    home: Rc<Home>,
    engines: Rc<RefCell<EngineRegistry>>,
}

impl Deps {
    fn parser(&self, name: &str, summary: &str) -> Parser {
        Parser::new(&format!("tx {name}")).description(summary)
    }

    /// This binary, canonical (the reference's `<repo>/bin/tx`, resolved).
    fn bin_tx(&self) -> PathBuf {
        std::fs::canonicalize(&self.env.exe).unwrap_or_else(|_| self.env.exe.clone())
    }

    /// `_repo_root()`: the checkout holding `bin/tx-assistant` — the first ancestor of this binary
    /// that has one (a cargo build lives under `<repo>/target/<profile>/`).
    fn repo_root(&self) -> PathBuf {
        let exe = self.bin_tx();
        exe.ancestors()
            .skip(1)
            .find(|dir| dir.join("bin").join("tx-assistant").is_file())
            .map(Path::to_path_buf)
            .or_else(|| exe.parent().and_then(Path::parent).map(Path::to_path_buf))
            .unwrap_or_default()
    }

    fn inside_tmux(&self) -> bool {
        self.env.var("TMUX").is_some_and(|value| !value.is_empty())
    }
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

/// Parse or emit argparse's exit (help on stdout / usage error on stderr).
fn parse(parser: &Parser, argv: &[String]) -> Result<Matches, i32> {
    parser.parse(argv).map_err(|exit: ParseExit| exit.emit())
}

pub struct ListingVerbs;

impl Component for ListingVerbs {
    fn name(&self) -> &str {
        "verbs.listing"
    }
    fn inject(&self) -> &[&'static str] {
        &["commands", "service", "tmux", "env", "home", "engines"]
    }
    fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        let deps = Rc::new(Deps {
            service: ctx.get(SERVICE)?,
            tmux: ctx.get(TMUX)?,
            env: ctx.get(ENV)?,
            home: ctx.get(HOME)?,
            engines: ctx.get(ENGINES)?,
        });
        let table = ctx.get(COMMANDS)?;
        let d = || Rc::clone(&deps);
        table.register(ctx, Visibility::Public, Start(d()));
        table.register(ctx, Visibility::Public, Attach(d()));
        table.register(ctx, Visibility::Public, Ls(d()));
        table.register(ctx, Visibility::Public, Show(d()));
        table.register(ctx, Visibility::Public, History(d()));
        table.register(ctx, Visibility::Public, Chat(d()));
        table.register(ctx, Visibility::Hidden, List(d()));
        table.register(ctx, Visibility::Hidden, EditSession(d()));
        table.register(ctx, Visibility::Hidden, EditTag(d()));
        Ok(())
    }
}

// ----- ls / _list / show ---------------------------------------------------------------------

struct Ls(Rc<Deps>);

impl Command for Ls {
    fn name(&self) -> &'static str {
        "ls"
    }
    fn summary(&self) -> &'static str {
        "List current (live) sessions (a single PROCESSES listing; views live in tmux)."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        // Q11 FIX: `--json` prints the live records (the same set as the table) as a JSON array.
        let parser = self.0.parser(self.name(), self.summary()).arg(
            Arg::option("--json")
                .flag()
                .help("print the live session records as a JSON array"),
        );
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let live = self.0.service.live_sessions()?;
        if args.get_flag("json") {
            let rows = Value::Array(live.iter().map(Session::to_value).collect());
            println!("{}", dumps_pretty(&rows));
        } else {
            println!("{}", render_ls(&live, now()));
        }
        Ok(0)
    }
}

struct List(Rc<Deps>);

impl Command for List {
    fn name(&self) -> &'static str {
        "_list"
    }
    fn summary(&self) -> &'static str {
        "Internal: ANSI fzf picker feed (the initial paint + each reload-sync of `tx attach`)."
    }
    fn run(&self, _argv: &[String]) -> Result<i32, BoxError> {
        // Lenient on argv: the picker is the only caller.
        let live = self.0.service.live_sessions()?;
        println!(
            "{}",
            picker_display_rows(&live, env_namew(&self.0.env), now())
        );
        Ok(0)
    }
}

struct Show(Rc<Deps>);

impl Command for Show {
    fn name(&self) -> &'static str {
        "show"
    }
    fn summary(&self) -> &'static str {
        "Print a session record as JSON (by id or name)."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = self
            .0
            .parser(self.name(), self.summary())
            .arg(Arg::positional("target"));
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let target = args.get_one("target").unwrap_or_default();
        let Some(mut session) = self.0.service.get(target)? else {
            eprintln!("tx show: no record for '{target}'");
            return Ok(1);
        };
        // Compute-on-read: a live record's stored snapshot is stale (display only, never saved).
        if session.is_alive() {
            session.attached_to = self.0.tmux.attached_to(session.tmux_name());
        }
        println!("{}", dumps_pretty(&session.to_value()));
        Ok(0)
    }
}

// ----- history / chat ls ---------------------------------------------------------------------

/// `datetime.strptime(raw, "%Y-%m-%d")` → `(year, month, day)`, `None` where Python raises.
fn parse_ymd(raw: &str) -> Option<(i32, u32, u32)> {
    fn digits(part: &str, min: usize, max: usize) -> Option<u32> {
        (part.len() >= min && part.len() <= max && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse().ok())
            .flatten()
    }
    let mut parts = raw.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let year = digits(year, 4, 4)?;
    let month = digits(month, 1, 2).filter(|m| (1..=12).contains(m))?;
    // `%d` also takes a space-padded single digit (` 5`).
    let day = match day.strip_prefix(' ') {
        Some(rest) => digits(rest, 1, 1).filter(|d| *d >= 1)?,
        None => digits(day, 1, 2)?,
    };
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days_in_month = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (year >= 1 && (1..=days_in_month).contains(&day)).then_some((year as i32, month, day))
}

/// Local-time epoch seconds (naive `datetime.timestamp()`).
fn local_epoch(year: i32, month: u32, day: u32, hour: i32, minute: i32, second: i32) -> f64 {
    // SAFETY: `tm` is a plain C struct we own and fully initialise before `mktime` reads it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month as i32 - 1;
    tm.tm_mday = day as i32;
    tm.tm_hour = hour;
    tm.tm_min = minute;
    tm.tm_sec = second;
    tm.tm_isdst = -1;
    // SAFETY: see above; mktime only normalises the struct in place.
    unsafe { libc::mktime(&mut tm) as f64 }
}

/// `_parse_history_date`: a `--since` / `--until` YYYY-MM-DD filter as an epoch second.
fn parse_history_date(
    raw: Option<&str>,
    parser: &Parser,
    flag: &str,
    end_of_day: bool,
) -> Result<Option<f64>, ParseExit> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let Some((year, month, day)) = parse_ymd(raw) else {
        return Err(parser.error(&format!("{flag} expects YYYY-MM-DD, got '{raw}'")));
    };
    let (hour, minute, second) = if end_of_day { (23, 59, 59) } else { (0, 0, 0) };
    Ok(Some(local_epoch(year, month, day, hour, minute, second)))
}

struct History(Rc<Deps>);

impl Command for History {
    fn name(&self) -> &'static str {
        "history"
    }
    fn summary(&self) -> &'static str {
        "List past (EXITED / ARCHIVED) sessions — filter by --tag / --cwd / --since / --until."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = self
            .0
            .parser(self.name(), self.summary())
            .arg(Arg::option("--tag").help("only sessions carrying this tag"))
            .arg(Arg::option("--cwd").help("only sessions whose cwd contains this substring"))
            .arg(
                Arg::option("--since")
                    .metavar("YYYY-MM-DD")
                    .help("ended on or after this date"),
            )
            .arg(
                Arg::option("--until")
                    .metavar("YYYY-MM-DD")
                    .help("ended on or before this date"),
            );
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let dates = parse_history_date(args.get_one("since"), &parser, "--since", false).and_then(
            |since| {
                parse_history_date(args.get_one("until"), &parser, "--until", true)
                    .map(|until| (since, until))
            },
        );
        let (since, until) = match dates {
            Ok(dates) => dates,
            Err(exit) => return Ok(exit.emit()),
        };
        let (tag, cwd) = (args.get_one("tag"), args.get_one("cwd"));
        self.0.service.reconcile()?;
        let shown = self.0.service.store().query(|session| {
            matches!(session.state, State::Exited | State::Archived)
                && tag.is_none_or(|tag| session.tags.iter().any(|t| t == tag))
                && cwd.is_none_or(|cwd| session.cwd.contains(cwd))
                && {
                    let ended = session
                        .ended_at
                        .as_ref()
                        .and_then(serde_json::Number::as_f64)
                        .filter(|ended| *ended != 0.0)
                        .unwrap_or_else(|| session.activity_at());
                    since.is_none_or(|since| ended >= since)
                        && until.is_none_or(|until| ended <= until)
                }
        });
        println!("{}", render_history(&shown, now()));
        Ok(0)
    }
}

struct Chat(Rc<Deps>);

impl Command for Chat {
    fn name(&self) -> &'static str {
        "chat"
    }
    fn summary(&self) -> &'static str {
        "Inspect a session's chats: `chat ls <session>` lists its ChatRefs + bundle paths."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = self
            .0
            .parser(self.name(), self.summary())
            .arg(
                Arg::positional("subcommand")
                    .choices(&["ls"])
                    .help("ls — list the session's chats"),
            )
            .arg(Arg::positional("session"));
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let name = args.get_one("session").unwrap_or_default();
        let Some(session) = self.0.service.get(name)? else {
            eprintln!("tx chat ls: session '{name}' not found");
            return Ok(1);
        };
        println!("{}", render_chats(&session, now()));
        Ok(0)
    }
}

// ----- start ---------------------------------------------------------------------------------

struct Start(Rc<Deps>);

impl Command for Start {
    fn name(&self) -> &'static str {
        "start"
    }
    fn summary(&self) -> &'static str {
        "Ensure the Views home base + tx-assistant exist, then attach Views."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = self.0.parser(self.name(), self.summary()).arg(
            Arg::options(&["-r", "--restart"])
                .flag()
                .help("kill an existing tx-assistant first so warmup recreates it"),
        );
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let (deps, tmux) = (&self.0, &self.0.tmux);
        let repo = deps.repo_root();

        let assistant_target = deps
            .service
            .get("tx-assistant")?
            .map_or_else(|| "tx-assistant".to_owned(), |s| s.tmux_name().to_owned());
        if args.get_flag("restart") && tmux.has_session(&assistant_target) {
            tmux.kill_session(&assistant_target);
        }
        if tmux.has_session(&assistant_target) {
            println!("tx-assistant already running.");
        } else {
            let assistant = repo.join("bin").join("tx-assistant");
            Process::new(&assistant)
                .arg("--warm")
                .status()
                .map_err(run_error(&assistant))?;
            println!("tx-assistant session created.");
        }

        if !tmux.has_session("Views") {
            let spec = SpawnSpec::for_view(
                "Views",
                repo.to_string_lossy(),
                default_shell(&deps.env),
                &deps.engines.borrow(),
            );
            deps.service.spawn_view(spec)?;
            println!("Views session created.");
        }

        if deps.inside_tmux() {
            tmux.switch_client("Views")?;
        } else {
            let _ = std::io::stdout().flush();
            Process::new(tmux.binary())
                .args(["attach", "-t", "Views"])
                .status()
                .map_err(run_error(tmux.binary()))?;
        }
        Ok(0)
    }
}

// ----- attach (the fzf picker) ---------------------------------------------------------------

/// A process exit code as Python's `returncode` (a signal is `-signum`).
fn returncode(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|signal| -signal))
        .unwrap_or(1)
}

/// `tempfile.gettempdir()`: `$TMPDIR` / `$TEMP` / `$TMP`, then the platform dirs, then cwd.
fn temp_dir(env: &Env) -> PathBuf {
    let candidates = ["TMPDIR", "TEMP", "TMP"]
        .iter()
        .filter_map(|name| env.var(name).filter(|value| !value.is_empty()))
        .map(PathBuf::from)
        .chain(["/tmp", "/var/tmp", "/usr/tmp"].map(PathBuf::from));
    for dir in candidates {
        if tempfile::Builder::new().tempfile_in(&dir).is_ok() {
            return dir;
        }
    }
    env.cwd.clone()
}

struct Attach(Rc<Deps>);

impl Command for Attach {
    fn name(&self) -> &'static str {
        "attach"
    }
    fn summary(&self) -> &'static str {
        "Open the interactive session picker (fzf)."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = self
            .0
            .parser(self.name(), self.summary())
            .arg(
                Arg::options(&["-f", "--filter"])
                    .dest("query")
                    .default("")
                    .metavar("QUERY")
                    .help("pre-fill the search with QUERY"),
            )
            .arg(Arg::options(&["-j", "--jump"]).flag().help(
                "Enter focuses the existing pane hosting the session instead of nest-attaching \
                 here (popup-friendly)",
            ))
            .arg(
                Arg::option("--host")
                    .optional()
                    .constant("personal")
                    .metavar("ALIAS")
                    .help(
                        "attach a session on remote ssh ALIAS (default 'personal') by running \
                         that host's own tx picker over ssh",
                    ),
            )
            .arg(
                Arg::option("--all")
                    .dest("mix")
                    .flag()
                    .help("(unsupported) merged local+remote picker — use --host ALIAS"),
            );
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };

        if let Some(host) = args.get_one("host").filter(|host| !host.is_empty()) {
            return self.attach_remote(host);
        }
        if args.get_flag("mix") {
            eprintln!(
                "tx attach --all (a merged local+remote picker) is not supported; use `tx attach \
                 --host ALIAS` to attach a remote host's sessions over ssh."
            );
            return Ok(2);
        }

        // Size the NAME column once and export it so each reload-sync `tx _list` matches.
        let service = &self.0.service;
        service.reconcile()?;
        let longest = service
            .store()
            .all()
            .iter()
            .filter(|session| session.is_alive())
            .map(|session| session.name.chars().count() as i64)
            .max()
            .unwrap_or(0);
        let namew = picker_namew(detect_term_cols(&self.0.env) as i64, longest);

        // The two-press Ctrl-D kill's arm file; removed (drop) whenever we leave.
        let arm_file = tempfile::Builder::new()
            .prefix("tx-kill-arm.")
            .rand_bytes(8)
            .tempfile_in(temp_dir(&self.0.env))?
            .into_temp_path();
        let query = args.get_one("query").unwrap_or_default();
        let picker = Picker {
            deps: &self.0,
            opts: self.fzf_opts(namew, query),
            namew,
            arm_file: &arm_file,
        };
        picker.run(args.get_flag("jump"))
    }
}

impl Attach {
    /// `tx attach --host ALIAS`: run the remote host's own picker over `ssh -t`, stamping
    /// `@remote-session` on the launching pane while it runs.
    fn attach_remote(&self, host: &str) -> Result<i32, BoxError> {
        let remote_command = r#"$SHELL -lc "if command -v tx >/dev/null 2>&1; then tx attach; else tmux attach; fi""#;
        let tmux = &self.0.tmux;
        let pane = self.0.env.var("TMUX_PANE").filter(|pane| !pane.is_empty());
        if let Some(pane) = pane {
            tmux.set_option(pane, "@remote-session", host, true)?;
        }
        let _ = std::io::stdout().flush();
        let status = Process::new("ssh")
            .args(["-t", host, remote_command])
            .status();
        if let Some(pane) = pane {
            tmux.unset_option(pane, "@remote-session", true);
        }
        Ok(returncode(status.map_err(run_error("ssh"))?))
    }

    /// The fzf argv (a 1:1 port of the bash `opts` array); `{1}` / `{2}` / `$TX_ARM_FILE` /
    /// `$FZF_PORT` stay literal for fzf and its child shell.
    fn fzf_opts(&self, namew: i64, query: &str) -> Vec<String> {
        use palette::{
            ACCENT_ANSI, ACCENT_HEX, BOLD, DIM_FG_HEX, FG_ANSI, FG_HEX, RESET, SELECTION_BG,
            WARN_ANSI,
        };
        let namew = namew.max(0) as usize;
        let bin_tx = self.0.bin_tx();
        let bin_tx = bin_tx.display();
        let reload = format!("reload-sync({bin_tx} _list)");
        // `display-popup` runs in the server's environment: bake the home into the command.
        let edit_tag = format!(
            "env TX_IDE_HOME={} {bin_tx} _edit-tag {{1}}",
            self.0.home.root().display()
        );
        let header_cols = format!(
            "{:<namew$}   {:<LOCATION_W$} {:<7} {:<6} {:<ROLE_W$} TAGS",
            "NAME", "LOCATION", "STARTED", "IDLE", "ROLE"
        );
        let focus_cmd = format!(
            "printf '{ACCENT_ANSI}{BOLD}%s{RESET} {FG_ANSI}{BOLD}%s{RESET}\\n%s' {{1}} {{2}} \
             '{header_cols}'"
        );
        let arm_cmd = format!(
            "printf '{WARN_ANSI}{BOLD} ⚠  Kill \"%s\"? [y/N]{RESET}\\n%s' {{1}} '{header_cols}'"
        );
        let color = format!(
            "fg:{DIM_FG_HEX},pointer:{ACCENT_HEX},fg+:{DIM_FG_HEX}:regular,\
             bg+:{SELECTION_BG}:regular,hl:{ACCENT_HEX},hl+:{ACCENT_HEX},\
             header:{DIM_FG_HEX},footer:{DIM_FG_HEX},prompt:{DIM_FG_HEX},query:{FG_HEX}"
        );
        [
            "fzf".to_owned(),
            "--exact".to_owned(),
            "--ansi".to_owned(),
            "--prompt=  ❯ ".to_owned(),
            "--height=100%".to_owned(),
            "--reverse".to_owned(),
            "--delimiter=\t".to_owned(),
            "--with-nth=5..".to_owned(),
            "--listen".to_owned(),
            "--track".to_owned(),
            format!("--color={color}"),
            format!("--header={header_cols}"),
            format!("--query={query}"),
            format!(
                "--bind=start:execute-silent(( while sleep 1; do curl -fsS -XPOST \
                 \"localhost:$FZF_PORT\" -d \"{reload}\" >/dev/null 2>&1 || exit 0; done ) \
                 &)+unbind(y,n)"
            ),
            format!("--bind=ctrl-r:{reload}"),
            format!(
                "--bind=focus:transform-header({focus_cmd})+execute-silent(: \
                 >\"$TX_ARM_FILE\")+unbind(y,n)"
            ),
            format!(
                "--bind=ctrl-t:execute(tmux display-popup -E -h 5 -w 60% \"{edit_tag}\")+{reload}"
            ),
            format!(
                "--bind=ctrl-d:execute-silent(printf '%s' {{1}} \
                 >\"$TX_ARM_FILE\")+transform-header({arm_cmd})+rebind(y,n)"
            ),
            format!(
                "--bind=y:execute-silent({bin_tx} kill {{1}} >/dev/null 2>&1; : \
                 >\"$TX_ARM_FILE\")+{reload}+unbind(y,n)"
            ),
            format!(
                "--bind=n:transform-header({focus_cmd})+execute-silent(: \
                 >\"$TX_ARM_FILE\")+unbind(y,n)"
            ),
        ]
        .into()
    }
}

/// One `tx attach` run: fzf over the feed, then the post-selection action.
struct Picker<'a> {
    deps: &'a Deps,
    opts: Vec<String>,
    namew: i64,
    arm_file: &'a Path,
}

impl Picker<'_> {
    /// Re-render, run fzf, act — looping only on a recoverable miss (the bash `while :`).
    fn run(&self, jump: bool) -> Result<i32, BoxError> {
        loop {
            let Some(selection) = self.select()? else {
                return Ok(0);
            };
            let fields: Vec<&str> = selection.trim_end_matches('\n').split('\t').collect();
            let name = fields[0];
            // Field 4 is the row's tmux target (D7); fall back to name resolution.
            let target = match fields.get(3).filter(|target| !target.is_empty()) {
                Some(target) => (*target).to_owned(),
                None => self.tmux_target(name)?,
            };
            if jump {
                if self.jump_to_session(name, &target)? {
                    return Ok(0);
                }
                eprintln!("tx: could not jump to or switch to session {name}");
                std::thread::sleep(RETRY_PAUSE);
                continue;
            }
            if self.nest_attach(name, &target)? {
                return Ok(0);
            }
            std::thread::sleep(RETRY_PAUSE);
        }
    }

    /// Run fzf once over a fresh feed; `None` on abort / an empty selection.
    fn select(&self) -> Result<Option<String>, BoxError> {
        let live = self.deps.service.live_sessions()?;
        let feed = picker_display_rows(&live, self.namew, now());
        let _ = std::io::stdout().flush();
        let mut child = Process::new(&self.opts[0])
            .args(&self.opts[1..])
            .env("NAMEW", self.namew.to_string())
            .env("TX_ARM_FILE", self.arm_file)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(run_error("fzf"))?;
        let mut stdin = child.stdin.take();
        let writer = std::thread::spawn(move || {
            // A picker that exits without reading the feed is fine (communicate() ignores EPIPE).
            if let Some(stdin) = stdin.as_mut() {
                let _ = stdin.write_all(feed.as_bytes());
            }
        });
        let mut stdout = Vec::new();
        if let Some(mut out) = child.stdout.take() {
            out.read_to_end(&mut stdout)?;
        }
        let status = child.wait()?;
        let _ = writer.join();
        let stdout = String::from_utf8_lossy(&stdout).into_owned();
        Ok((status.success() && !stdout.trim().is_empty()).then_some(stdout))
    }

    fn tmux_target(&self, name: &str) -> Result<String, BoxError> {
        Ok(self
            .deps
            .service
            .get(name)?
            .map_or_else(|| name.to_owned(), |record| record.tmux_name().to_owned()))
    }

    /// Nest-attach into the launching Views pane, else switch-client / foreground attach.
    /// False = the session vanished (re-loop).
    fn nest_attach(&self, name: &str, target: &str) -> Result<bool, BoxError> {
        if !self.deps.tmux.has_session(target) {
            eprintln!("tx: session '{name}' does not exist");
            return Ok(false);
        }
        if self.respawn_into_view_pane(target)? {
            return Ok(true);
        }
        self.switch_or_attach(target)?;
        Ok(true)
    }

    /// `--jump`: focus the pane already hosting the session; then nest-attach, then switch.
    fn jump_to_session(&self, name: &str, target: &str) -> Result<bool, BoxError> {
        let tmux = &self.deps.tmux;
        if !tmux.has_session(target) {
            eprintln!("tx: session '{name}' does not exist");
            return Ok(false);
        }
        let current_session = tmux.current_session_name().unwrap_or_default();
        if current_session == target {
            return Ok(true);
        }
        if let Some(pane) = tmux.pane_for_session(target, &current_session)
            && pane.split(':').next() == Some(current_session.as_str())
        {
            return Ok(tmux.select_window(&pane) && tmux.select_pane(&pane));
        }
        if self.respawn_into_view_pane(target)? {
            return Ok(true);
        }
        Ok(tmux.switch_client(target).is_ok())
    }

    /// From a shell pane of a Views home, nest-attach `target` into that pane (`respawn-pane -k`
    /// with a `set -m` bash wrapper that keeps the pane alive after detach).
    fn respawn_into_view_pane(&self, target: &str) -> Result<bool, BoxError> {
        let tmux = &self.deps.tmux;
        if !self.deps.inside_tmux() {
            return Ok(false);
        }
        let origin_pane = tmux.current_pane_id();
        let current_session = tmux.current_session_name().unwrap_or_default();
        let Some(origin_pane) = origin_pane else {
            return Ok(false);
        };
        if current_session.is_empty() || !tmux.is_view(&current_session) {
            return Ok(false);
        }
        let origin_cmd = tmux.display_message("#{pane_current_command}", Some(&origin_pane));
        if !origin_cmd.is_some_and(|cmd| SHELL_COMMANDS.contains(&cmd.as_str())) {
            return Ok(false);
        }
        let wrapper = format!(
            "set -m; TMUX= tmux attach -t {}; exec ${{SHELL:-zsh}}",
            shlex_quote(target)
        );
        tmux.respawn_pane(&origin_pane, &format!("bash -c {}", shlex_quote(&wrapper)))?;
        Ok(true)
    }

    /// Switch the calling client (inside tmux; a failure is tolerated) or foreground-attach.
    fn switch_or_attach(&self, target: &str) -> Result<(), BoxError> {
        let tmux = &self.deps.tmux;
        if self.deps.inside_tmux() {
            let _ = tmux.switch_client(target);
        } else {
            let _ = std::io::stdout().flush();
            tmux.attach_session(target)?;
        }
        Ok(())
    }
}

// ----- _edit-session / _edit-tag -------------------------------------------------------------

struct EditSession(Rc<Deps>);

impl Command for EditSession {
    fn name(&self) -> &'static str {
        "_edit-session"
    }
    fn summary(&self) -> &'static str {
        "Internal: edit the focused pane's tx session name and tags."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = self
            .0
            .parser(self.name(), self.summary())
            .arg(Arg::positional("pane"));
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let pane = args.get_one("pane").unwrap_or_default();
        let tmux = &self.0.tmux;
        // The firing pane (captured by the binding), not the popup's own pane.
        let target = tmux
            .inner_for_pane(pane)
            .or_else(|| tmux.display_message("#{session_name}", Some(pane)));
        let Some(target) = target.filter(|target| !tmux.is_view(target)) else {
            return Err(ListingError::NoSessionInPane.into());
        };
        let service = &self.0.service;
        let Some(session) = service.get(&target)? else {
            return Err(ListingError::NotTxManaged.into());
        };
        let Some((name, edited_tags)) = edit_session(&session.name, &session.tags.join(","))?
        else {
            return Ok(0);
        };
        // Both fields were collected before writing; address by the stable id.
        service.rename(&session.id, &name)?;
        let tags = split_tags(&edited_tags);
        if tags != session.tags {
            service.tag(&session.id, tags)?;
        }
        Ok(0)
    }
}

/// `_prompt_with_default`: fzf's query editor prefilled with `default`; `None` when cancelled.
fn prompt_with_default(prompt: &str, default: &str) -> Result<Option<String>, ListingError> {
    let output = Process::new("fzf")
        .args([
            "--disabled",
            "--query",
            default,
            "--prompt",
            prompt,
            "--header",
            "Enter: accept field. Esc / Ctrl-C: cancel.",
            "--reverse",
            "--no-info",
            "--no-separator",
            "--bind",
            "enter:accept-or-print-query",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(run_error("fzf"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(Some(text.strip_suffix('\n').unwrap_or(&text).to_owned()))
}

struct EditTag(Rc<Deps>);

impl Command for EditTag {
    fn name(&self) -> &'static str {
        "_edit-tag"
    }
    fn summary(&self) -> &'static str {
        "Internal: prefilled tag editor for the picker's Ctrl-T popup."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = self
            .0
            .parser(self.name(), self.summary())
            .arg(Arg::positional("session"));
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let wanted = args.get_one("session").unwrap_or_default();
        let Some(session) = self.0.service.get(wanted)? else {
            eprintln!("tx: session '{wanted}' not found (not tx-managed)");
            return Ok(1);
        };
        let Some(edited) =
            prompt_with_default(&format!("Tags for {wanted}: "), &session.tags.join(","))?
        else {
            return Ok(1);
        };
        self.0.service.tag(&session.name, split_tags(&edited))?;
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ymd_follows_strptime() {
        // Expected from datetime.strptime(raw, "%Y-%m-%d") under python3.14.
        assert_eq!(parse_ymd("2026-09-15"), Some((2026, 9, 15)));
        assert_eq!(parse_ymd("2026-9-5"), Some((2026, 9, 5)));
        assert_eq!(parse_ymd("2024-02-29"), Some((2024, 2, 29)));
        assert_eq!(parse_ymd("2026-09- 5"), Some((2026, 9, 5)));
        for bad in [
            "15-09-2026",
            "2026-13-01",
            "2026-02-29",
            "2026-00-10",
            "2026-09-31",
            "0000-01-01",
            "2026-09-15x",
            "2026-09",
            "",
            "26-09-15",
            "2026-009-15",
        ] {
            assert_eq!(parse_ymd(bad), None, "{bad}");
        }
    }

    #[test]
    fn local_epoch_end_of_day_is_one_second_before_midnight() {
        let start = local_epoch(2026, 9, 15, 0, 0, 0);
        let end = local_epoch(2026, 9, 15, 23, 59, 59);
        let next = local_epoch(2026, 9, 16, 0, 0, 0);
        assert_eq!(next - end, 1.0);
        assert!(end - start >= 82_799.0);
    }
}
