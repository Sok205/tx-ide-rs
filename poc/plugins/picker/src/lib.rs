//! tx-core plugin `picker`: the fzf session picker (prefix+t). `_list` is its feed (fzf re-runs
//! it to reload) and `pick` is the interactive loop. Port of main's `tx attach` without
//! `--host`/`--all`. Publishes `keys/picker.tmux`.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use tx_core::events::now;
use tx_core::{
    Lifecycle, ListFilter, Plugin, PluginError, Registry, Session, TxCore, Verb, VerbError, VerbIo,
};

use tx_client::render::{
    ACCENT_ANSI, BOLD, FG_ANSI, LOCATION_W, RESET, ROLE_W, Row, WARN_ANSI, picker_namew,
    picker_rows,
};
use tx_client::tmux::Ui;

pub const PLUGIN_ID: &str = "picker";

#[derive(Clone, Copy, Debug, Default)]
pub struct PickerPlugin;

impl Plugin for PickerPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.verb(Arc::new(List));
        r.verb(Arc::new(Pick));
        r.resource("keys/picker.tmux", include_bytes!("../tmux/keys.tmux"))
    }
}

const DEFAULT_NAMEW: i64 = 18;
const DEFAULT_COLS: i64 = 80;
const RETRY_PAUSE: Duration = Duration::from_millis(1200);
/// Foreground commands of a pane that may be replaced by a nested attach.
const HOSTING_COMMANDS: [&str; 6] = ["zsh", "bash", "sh", "fish", "dash", "tmux"];

const PICK_USAGE: &str = "\
usage: txc pick [-f|--filter QUERY] [-j|--jump]
(--host and --all are not supported)
";

/// `NAMEW` as the picker exports it: ASCII digits only, else 18.
fn namew_from(raw: Option<&str>) -> i64 {
    raw.filter(|raw| !raw.is_empty() && raw.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(DEFAULT_NAMEW)
}

/// Terminal width: the controlling tty, else `COLUMNS`, else 80.
pub fn detect_term_cols() -> i64 {
    tty_columns()
        .or_else(|| {
            std::env::var("COLUMNS")
                .ok()
                .and_then(|raw| raw.trim().parse().ok())
                .filter(|columns| *columns > 0)
        })
        .unwrap_or(DEFAULT_COLS)
}

fn tty_columns() -> Option<i64> {
    let tty = File::open("/dev/tty").ok()?;
    // SAFETY: TIOCGWINSZ fills a winsize we own; the fd is valid for the call.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let status = unsafe { libc::ioctl(tty.as_raw_fd(), libc::TIOCGWINSZ, &mut size) };
    (status == 0 && size.ws_col > 0).then_some(i64::from(size.ws_col))
}

/// Which binds of other plugins' verbs the picker offers: ctrl-t needs `_edit-tag`, ctrl-d and
/// `y` need `kill`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binds {
    pub edit_tag: bool,
    pub kill: bool,
}

impl Binds {
    fn of(core: &TxCore) -> Self {
        Self {
            edit_tag: core.verb("_edit-tag").is_some(),
            kill: core.verb("kill").is_some(),
        }
    }
}

/// The fzf argv. `{1}`, `{2}`, `$TX_ARM_FILE` and `$FZF_PORT` stay literal for fzf and the
/// shells it spawns; the arm file reaches them through the child's environment. `arguments`
/// are the core's `cli_arguments()`, which every re-invocation of `bin` carries.
pub fn fzf_args(
    bin: &Path,
    socket: &Path,
    arguments: &[String],
    namew: i64,
    query: &str,
    _arm_file: &Path,
    binds: Binds,
) -> Vec<String> {
    let namew = namew.max(0) as usize;
    let bin = bin.display();
    let socket = socket.display();
    let arguments = arguments.join(" ");
    let reload = format!("reload-sync({bin} {arguments} _list)");
    let edit_tag = format!("{bin} {arguments} _edit-tag {{1}}");
    let header_cols = format!(
        "{:<namew$}   {:<LOCATION_W$} {:<7} {:<6} {:<ROLE_W$} TAGS",
        "NAME", "LOCATION", "STARTED", "IDLE", "ROLE"
    );
    let focus_cmd = format!(
        "printf '{ACCENT_ANSI}{BOLD}%s{RESET} {FG_ANSI}{BOLD}%s{RESET}\\n%s' {{1}} {{2}} '{header_cols}'"
    );
    let arm_cmd = format!(
        "printf '{WARN_ANSI}{BOLD} ⚠  Kill \"%s\"? [y/N]{RESET}\\n%s' {{1}} '{header_cols}'"
    );
    let mut args: Vec<String> = [
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
        "--color=fg:#a9b1d6,pointer:#7aa2f7,fg+:#a9b1d6:regular,bg+:236:regular,hl:#7aa2f7,hl+:#7aa2f7,header:#a9b1d6,footer:#a9b1d6,prompt:#a9b1d6,query:#c0caf5".to_owned(),
        format!("--header={header_cols}"),
        format!("--query={query}"),
        format!(
            "--bind=start:execute-silent(( while sleep 1; do curl -fsS -XPOST \"localhost:$FZF_PORT\" -d \"{reload}\" >/dev/null 2>&1 || exit 0; done ) &)+unbind(y,n)"
        ),
        format!("--bind=ctrl-r:{reload}"),
        format!(
            "--bind=focus:transform-header({focus_cmd})+execute-silent(: >\"$TX_ARM_FILE\")+unbind(y,n)"
        ),
    ]
    .into();
    if binds.edit_tag {
        args.push(format!(
            "--bind=ctrl-t:execute(tmux -S {socket} display-popup -E -h 4 -w 60% \"{edit_tag}\")+{reload}"
        ));
    }
    if binds.kill {
        args.push(format!(
            "--bind=ctrl-d:execute-silent(printf '%s' {{1}} >\"$TX_ARM_FILE\")+transform-header({arm_cmd})+rebind(y,n)"
        ));
        args.push(format!(
            "--bind=y:execute-silent({bin} {arguments} kill {{1}} >/dev/null 2>&1; : >\"$TX_ARM_FILE\")+{reload}+unbind(y,n)"
        ));
    }
    args.push(format!(
        "--bind=n:transform-header({focus_cmd})+execute-silent(: >\"$TX_ARM_FILE\")+unbind(y,n)"
    ));
    args
}

/// Reconcile with tmux, then the running sessions.
fn running(core: &TxCore) -> Result<Vec<Session>, VerbError> {
    core.refresh_all()?;
    let ids = core.list_sessions(&ListFilter::new().lifecycle(Lifecycle::Running))?;
    ids.iter()
        .map(|id| core.get_session(id).map_err(VerbError::from))
        .collect()
}

/// The picker feed for `sessions`, newline-terminated.
fn feed(ui: &Ui, sessions: &[Session], namew: i64) -> String {
    let targets = ui.live_targets();
    let attachments = ui.attachment_map();
    let rows: Vec<Row<'_>> = sessions
        .iter()
        .map(|session| Row {
            session,
            target: targets.get(&session.name).map_or("", String::as_str),
            attached: attachments
                .iter()
                .find(|(name, _)| *name == session.name)
                .map_or(&[][..], |(_, locations)| locations.as_slice()),
        })
        .collect();
    format!("{}\n", picker_rows(&rows, namew, now()))
}

pub struct List;

impl Verb for List {
    fn name(&self) -> &'static str {
        "_list"
    }

    fn usage(&self) -> &'static str {
        "_list   picker feed (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, _argv: &[String]) -> Result<i32, VerbError> {
        let namew = namew_from(std::env::var("NAMEW").ok().as_deref());
        let sessions = running(core)?;
        let text = feed(&Ui::from_core(core), &sessions, namew);
        io.stdout
            .write_all(text.as_bytes())
            .map_err(|e| VerbError::Failed(format!("write: {e}")))?;
        Ok(0)
    }
}

struct PickOptions {
    query: String,
    jump: bool,
}

/// `None` is a usage error.
fn parse_pick(argv: &[String]) -> Option<PickOptions> {
    let mut options = PickOptions {
        query: String::new(),
        jump: false,
    };
    let mut args = argv.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-f" | "--filter" => options.query = args.next()?.clone(),
            "-j" | "--jump" => options.jump = true,
            _ => return None,
        }
    }
    Some(options)
}

pub struct Pick;

impl Verb for Pick {
    fn name(&self) -> &'static str {
        "pick"
    }

    fn usage(&self) -> &'static str {
        "pick [-f QUERY] [-j]   fzf session picker (prefix+t)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let Some(options) = parse_pick(argv) else {
            let _ = write!(io.stderr, "{PICK_USAGE}");
            return Ok(2);
        };
        let ui = Ui::from_core(core);
        let longest = running(core)?
            .iter()
            .map(|session| session.name.chars().count() as i64)
            .max()
            .unwrap_or(0);
        let namew = picker_namew(detect_term_cols(), longest);
        let bin = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map_err(|e| VerbError::Failed(format!("current_exe: {e}")))?;
        // The two-press kill's arm file; removed on drop.
        let arm_file = tempfile::Builder::new()
            .prefix("tx-kill-arm.")
            .rand_bytes(8)
            .tempfile()
            .map_err(|e| VerbError::Failed(format!("arm file: {e}")))?
            .into_temp_path();
        let args = fzf_args(
            &bin,
            core.socket_path(),
            &core.config().cli_arguments(),
            namew,
            &options.query,
            &arm_file,
            Binds::of(core),
        );
        loop {
            let text = feed(&ui, &running(core)?, namew);
            let Some(selection) = select(&args, namew, &arm_file, &text)? else {
                return Ok(0);
            };
            let fields: Vec<&str> = selection.trim_end_matches('\n').split('\t').collect();
            let name = fields[0];
            let target = fields
                .get(3)
                .copied()
                .filter(|target| !target.is_empty())
                .unwrap_or(name);
            if !ui.has_session(name) {
                eprintln!("txc: session '{name}' does not exist");
            } else if options.jump {
                if jump(&ui, name, target)? {
                    return Ok(0);
                }
                eprintln!("txc: could not jump to or switch to session {name}");
            } else {
                drop(arm_file);
                open(&ui, target, name)?;
                return Ok(0);
            }
            std::thread::sleep(RETRY_PAUSE);
        }
    }
}

/// Run fzf once over `feed`; `None` on abort or an empty selection.
fn select(
    args: &[String],
    namew: i64,
    arm_file: &Path,
    feed: &str,
) -> Result<Option<String>, VerbError> {
    let mut child = Command::new(&args[0])
        .args(&args[1..])
        .env("NAMEW", namew.to_string())
        .env("TX_ARM_FILE", arm_file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| VerbError::Failed(format!("could not run fzf: {e}")))?;
    let mut stdin = child.stdin.take();
    let mut stdout = Vec::new();
    let status = std::thread::scope(|scope| {
        // fzf may exit without reading the whole feed; a broken pipe is fine.
        scope.spawn(|| {
            if let Some(stdin) = stdin.as_mut() {
                let _ = stdin.write_all(feed.as_bytes());
            }
            drop(stdin.take());
        });
        if let Some(mut out) = child.stdout.take() {
            let _ = out.read_to_end(&mut stdout);
        }
        child.wait()
    })
    .map_err(|e| VerbError::Failed(format!("fzf: {e}")))?;
    let stdout = String::from_utf8_lossy(&stdout).into_owned();
    Ok((status.success() && !stdout.trim().is_empty()).then_some(stdout))
}

fn ui_failed(error: impl std::fmt::Display) -> VerbError {
    VerbError::Failed(error.to_string())
}

/// How Enter on a picker row opens the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenMode {
    /// Respawn the launching view's shell pane into a nested attach.
    Nest,
    /// Switch the client that launched the picker.
    Switch,
    /// Attach this terminal.
    Attach,
}

fn open_mode(inside: bool, current_is_view: bool, pane_command: Option<&str>) -> OpenMode {
    if !inside {
        return OpenMode::Attach;
    }
    if current_is_view && pane_command.is_some_and(|command| HOSTING_COMMANDS.contains(&command)) {
        OpenMode::Nest
    } else {
        OpenMode::Switch
    }
}

/// The open mode for the caller's surroundings, with the pane to nest into.
fn open_mode_here(ui: &Ui) -> (OpenMode, Option<String>) {
    let inside = ui.inside();
    let pane = inside.then(|| ui.current_pane_id()).flatten();
    let current_is_view = pane.is_some()
        && ui
            .current_session_name()
            .is_some_and(|session| ui.is_view(&session));
    let command = pane
        .as_deref()
        .and_then(|pane| ui.display_message("#{pane_current_command}", Some(pane)));
    let mode = open_mode(inside, current_is_view, command.as_deref());
    (mode, pane)
}

/// Default path: nest into the launching view's shell pane, else switch the client, else
/// attach this terminal.
fn open(ui: &Ui, target: &str, name: &str) -> Result<(), VerbError> {
    match open_mode_here(ui) {
        (OpenMode::Nest, Some(pane)) => nest(ui, &pane, target)?,
        (OpenMode::Attach, _) => {
            let _ = std::io::stdout().flush();
            return Err(ui_failed(ui.attach_exec(target)));
        }
        _ => {
            let _ = ui.switch_client(name);
        }
    }
    Ok(())
}

/// `--jump`: focus the pane already hosting the session, else nest, else switch. False when
/// none of those worked.
fn jump(ui: &Ui, name: &str, target: &str) -> Result<bool, VerbError> {
    let current = ui.current_session_name().unwrap_or_default();
    if current == name {
        return Ok(true);
    }
    if let Some(pane) = ui.pane_for_session(name, &current)
        && pane.split(':').next() == Some(current.as_str())
    {
        return Ok(ui.select_window(&pane) && ui.select_pane(&pane));
    }
    if let (OpenMode::Nest, Some(pane)) = open_mode_here(ui) {
        nest(ui, &pane, target)?;
        return Ok(true);
    }
    Ok(ui.switch_client(name).is_ok())
}

fn nest(ui: &Ui, pane: &str, target: &str) -> Result<(), VerbError> {
    ui.respawn_pane(pane, &ui.nest_attach_command(target))
        .map_err(ui_failed)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    const HEADER: &str = "NAME           LOCATION                  STARTED IDLE   ROLE  TAGS";

    const ALL: Binds = Binds {
        edit_tag: true,
        kill: true,
    };

    fn args(binds: Binds) -> Vec<String> {
        fzf_args(
            Path::new("/x/txc"),
            Path::new("/k/tmux.sock"),
            &["--data-dir".to_owned(), "/s".to_owned()],
            12,
            "",
            Path::new("/t/arm"),
            binds,
        )
    }

    fn has_bind(args: &[String], key: &str) -> bool {
        args.iter().any(|arg| {
            arg.strip_prefix("--bind=")
                .is_some_and(|bind| bind.starts_with(key))
        })
    }

    #[test]
    fn namew_env_parsing() {
        assert_eq!(namew_from(Some("7")), 7);
        assert_eq!(namew_from(Some("")), 18);
        assert_eq!(namew_from(Some("7a")), 18);
        assert_eq!(namew_from(None), 18);
    }

    #[test]
    fn fzf_args_are_exact() {
        let reload = "reload-sync(/x/txc --data-dir /s _list)";
        let focus = format!(
            "printf '\x1b[38;2;122;162;247m\x1b[1m%s\x1b[0m \x1b[38;2;192;202;245m\x1b[1m%s\x1b[0m\\n%s' {{1}} {{2}} '{HEADER}'"
        );
        let arm = format!(
            "printf '\x1b[38;2;224;175;104m\x1b[1m ⚠  Kill \"%s\"? [y/N]\x1b[0m\\n%s' {{1}} '{HEADER}'"
        );
        let expected: Vec<String> = vec![
            "fzf".into(),
            "--exact".into(),
            "--ansi".into(),
            "--prompt=  ❯ ".into(),
            "--height=100%".into(),
            "--reverse".into(),
            "--delimiter=\t".into(),
            "--with-nth=5..".into(),
            "--listen".into(),
            "--track".into(),
            "--color=fg:#a9b1d6,pointer:#7aa2f7,fg+:#a9b1d6:regular,bg+:236:regular,hl:#7aa2f7,hl+:#7aa2f7,header:#a9b1d6,footer:#a9b1d6,prompt:#a9b1d6,query:#c0caf5".into(),
            format!("--header={HEADER}"),
            "--query=".into(),
            format!("--bind=start:execute-silent(( while sleep 1; do curl -fsS -XPOST \"localhost:$FZF_PORT\" -d \"{reload}\" >/dev/null 2>&1 || exit 0; done ) &)+unbind(y,n)"),
            format!("--bind=ctrl-r:{reload}"),
            format!("--bind=focus:transform-header({focus})+execute-silent(: >\"$TX_ARM_FILE\")+unbind(y,n)"),
            format!("--bind=ctrl-t:execute(tmux -S /k/tmux.sock display-popup -E -h 4 -w 60% \"/x/txc --data-dir /s _edit-tag {{1}}\")+{reload}"),
            format!("--bind=ctrl-d:execute-silent(printf '%s' {{1}} >\"$TX_ARM_FILE\")+transform-header({arm})+rebind(y,n)"),
            format!("--bind=y:execute-silent(/x/txc --data-dir /s kill {{1}} >/dev/null 2>&1; : >\"$TX_ARM_FILE\")+{reload}+unbind(y,n)"),
            format!("--bind=n:transform-header({focus})+execute-silent(: >\"$TX_ARM_FILE\")+unbind(y,n)"),
        ];
        assert_eq!(args(ALL), expected);
    }

    #[test]
    fn fzf_args_without_edit_tag_has_no_ctrl_t() {
        let actual = args(Binds {
            edit_tag: false,
            kill: true,
        });
        assert!(!has_bind(&actual, "ctrl-t:"), "{actual:?}");
        assert!(!actual.iter().any(|arg| arg.contains("_edit-tag")));
        assert_eq!(actual.len(), args(ALL).len() - 1);
        for key in ["ctrl-d:", "y:", "n:", "ctrl-r:"] {
            assert!(has_bind(&actual, key), "{key}");
        }
    }

    #[test]
    fn fzf_args_without_kill_has_no_kill_binds() {
        let actual = args(Binds {
            edit_tag: true,
            kill: false,
        });
        assert!(!has_bind(&actual, "ctrl-d:"), "{actual:?}");
        assert!(!has_bind(&actual, "y:"), "{actual:?}");
        assert!(!actual.iter().any(|arg| arg.contains(" kill ")));
        assert_eq!(actual.len(), args(ALL).len() - 2);
        for key in ["ctrl-t:", "n:", "ctrl-r:"] {
            assert!(has_bind(&actual, key), "{key}");
        }
    }

    #[test]
    fn open_mode_by_surroundings() {
        for command in [Some("zsh"), Some("txc"), None] {
            assert_eq!(open_mode(false, true, command), OpenMode::Attach);
            assert_eq!(open_mode(false, false, command), OpenMode::Attach);
        }
        for shell in HOSTING_COMMANDS {
            assert_eq!(
                open_mode(true, true, Some(shell)),
                OpenMode::Nest,
                "{shell}"
            );
        }
        assert_eq!(open_mode(true, false, Some("zsh")), OpenMode::Switch);
        assert_eq!(open_mode(true, true, Some("nvim")), OpenMode::Switch);
        assert_eq!(open_mode(true, true, Some("txc")), OpenMode::Switch);
        assert_eq!(open_mode(true, true, None), OpenMode::Switch);
        assert_eq!(open_mode(true, false, None), OpenMode::Switch);
    }
}
