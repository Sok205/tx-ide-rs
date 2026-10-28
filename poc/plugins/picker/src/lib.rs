//! tx-core plugin `picker`: the fzf session picker (prefix+t). `_list` is its feed (fzf re-runs
//! it to reload, filtered by the query and the hidden roles), `pick` the interactive loop with
//! a vim-style normal mode (numbered rows, `p` popup mode, TAB roles panel), and the internal
//! `_picker-key`, `_roles`, `_toggle-hide`, `_roles-panel`, `_popup-attach` behind its binds.
//! Port of main's `tx attach` without `--host`/`--all`. Publishes `keys/picker.tmux`.

pub mod filter;
pub mod keys;

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use tx_core::events::now;
use tx_core::{
    Lifecycle, ListFilter, Plugin, PluginError, Registry, Session, TxCore, Verb, VerbError, VerbIo,
};

use keys::{DIGITS_FILE_ENV, KeyState, NORMAL_PROMPT, POPUP_LINE, POPUP_MARK, SEARCH_PROMPT};
use tx_ui::popup::popup_session;
use tx_ui::render::{
    ACCENT_ANSI, ACCENT_HEX, BG_DEEP_HEX, BOLD, DIM_FG_HEX, FG_ANSI, FG_HEX, LOCATION_W,
    PICKER_NUMBER_W, RESET, ROLE_W, Row, SELECTION_BG, WARN_ANSI, picker_matches, picker_namew,
    picker_rows,
};
use tx_ui::tmux::Ui;

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
        r.verb(Arc::new(PickerKey));
        r.verb(Arc::new(Roles));
        r.verb(Arc::new(ToggleHide));
        r.verb(Arc::new(RolesPanel));
        r.verb(Arc::new(PopupAttach));
        r.resource("keys/picker.tmux", include_bytes!("../tmux/keys.tmux"))
    }
}

/// The roles the picker hides, remembered across runs in the plugin's state dir.
fn hide_file(core: &TxCore) -> PathBuf {
    core.config()
        .state_dir
        .join("plugins")
        .join(PLUGIN_ID)
        .join("hidden-roles")
}

fn failed(error: impl std::fmt::Display) -> VerbError {
    VerbError::Failed(error.to_string())
}

fn write_out(io: &mut VerbIo<'_>, text: &str) -> Result<(), VerbError> {
    io.stdout
        .write_all(text.as_bytes())
        .map_err(|e| failed(format!("write: {e}")))
}

/// This binary as `txc --state-dir <state>`, the way the fzf binds and run-shell call it.
fn tx_command(core: &TxCore) -> Result<String, VerbError> {
    let bin = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| failed(format!("current_exe: {e}")))?;
    Ok(format!(
        "{} --state-dir {}",
        bin.display(),
        core.config().state_dir.display()
    ))
}

const DEFAULT_NAMEW: i64 = 18;
const DEFAULT_COLS: i64 = 80;
const RETRY_PAUSE: Duration = Duration::from_millis(1200);
/// Foreground commands of a pane that may be replaced by a nested attach.
const HOSTING_COMMANDS: [&str; 6] = ["zsh", "bash", "sh", "fish", "dash", "tmux"];

const PICK_USAGE: &str = "\
usage: txc pick [-f|--filter QUERY] [-j|--jump] [--hide ROLE[,ROLE...]]
--hide replaces the remembered hidden roles (llm, nvim, view, shell); TAB toggles them
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

/// The fzf argv (a port of main's `fzf_opts`). `{1}`, `{2}`, `{q}`, `$TX_ARM_FILE`,
/// `$TX_DIGITS_FILE` and `$FZF_PORT` stay literal for fzf and the shells it spawns; the files
/// reach them through the child's environment.
pub fn fzf_args(
    tx: &str,
    socket: &Path,
    namew: i64,
    query: &str,
    hidden: &[String],
    binds: Binds,
) -> Vec<String> {
    let namew = namew.max(0) as usize;
    let socket = socket.display();
    let reload = format!("reload-sync({tx} _list {{q}})");
    // `{""q}`: fzf fills in the query when the POSTed reload runs, not when the loop starts.
    let posted_reload = format!("reload-sync({tx} _list {{\"\"q}})");
    let edit_tag = format!("{tx} _edit-tag {{1}}");
    let chips = format!("{tx} _roles --chips");
    let header_cols = format!(
        "{:>PICKER_NUMBER_W$} {:<namew$}   {:<LOCATION_W$} {:<7} {:<6} {:<ROLE_W$} TAGS",
        "#", "NAME", "LOCATION", "STARTED", "IDLE", "ROLE"
    );
    let picker_key = |key: &str| format!("transform({tx} _picker-key {key})");
    let focus_cmd = format!(
        "printf '{ACCENT_ANSI}{BOLD}%s{RESET} {FG_ANSI}{BOLD}%s{RESET}\\n' {{1}} {{2}}; {chips}; \
         printf '%s' '{header_cols}'"
    );
    let arm_cmd = format!(
        "printf '{WARN_ANSI}{BOLD} ⚠  Kill \"%s\"? [y/N]{RESET}\\n' {{1}}; {chips}; \
         printf '%s' '{header_cols}'"
    );
    let color = format!(
        "fg:{DIM_FG_HEX},pointer:{ACCENT_HEX},fg+:{DIM_FG_HEX}:regular,\
         bg+:{SELECTION_BG}:regular,hl:{ACCENT_HEX},hl+:{ACCENT_HEX},\
         header:{DIM_FG_HEX},footer:{DIM_FG_HEX},prompt:{DIM_FG_HEX},query:{FG_HEX}"
    );
    let unbind_normal = keys::toggle_normal_keys("unbind");
    let (prompt, start_mode, footer) = if query.is_empty() {
        (NORMAL_PROMPT, String::new(), keys::normal_footer(false, ""))
    } else {
        (
            SEARCH_PROMPT,
            format!("+{unbind_normal}"),
            keys::search_footer(),
        )
    };
    let enter_search = format!(
        "change-prompt({SEARCH_PROMPT})+{unbind_normal}+execute-silent(: >\"$TX_ARM_FILE\"; : \
         >\"${DIGITS_FILE_ENV}\")+transform-header({focus_cmd})+change-footer:{}",
        keys::search_footer()
    );
    let edit_tag_bind =
        format!("execute(tmux -S {socket} display-popup -E -h 4 -w 60% \"{edit_tag}\")+{reload}");
    let mut args: Vec<String> = [
        "fzf".to_owned(),
        "--disabled".to_owned(),
        "--ansi".to_owned(),
        format!("--prompt={prompt}"),
        "--height=100%".to_owned(),
        "--reverse".to_owned(),
        "--delimiter=\t".to_owned(),
        "--with-nth=5..".to_owned(),
        "--listen".to_owned(),
        "--track".to_owned(),
        "--with-shell=sh -c".to_owned(),
        format!("--color={color}"),
        format!("--header={}\n{header_cols}", filter::chips_line(hidden)),
        format!("--footer={footer}"),
        format!("--query={query}"),
        // The auto-reload pauses while a pick is pending (the digits file is non-empty).
        format!(
            "--bind=start:execute-silent(( while sleep 1; do [ -s \"${DIGITS_FILE_ENV}\" ] || \
             curl -fsS -XPOST \"localhost:$FZF_PORT\" -d \"{posted_reload}\" >/dev/null 2>&1 \
             || exit 0; done ) &){start_mode}"
        ),
        format!("--bind=change:reload({tx} _list {{q}})+first"),
        format!("--bind=ctrl-r:{reload}"),
        format!("--bind=r:{reload}"),
        format!("--bind=p:{}", picker_key("p")),
        // While searching, where `p` types: the row under the cursor, straight to a popup.
        format!("--bind=alt-p:print({POPUP_LINE})+accept"),
        format!("--bind=focus:transform-header({focus_cmd})+execute-silent(: >\"$TX_ARM_FILE\")"),
        // The roles panel runs in the picker's own terminal (fzf hands it over for the
        // duration): a tmux popup cannot nest inside the prefix+t picker popup.
        format!("--bind=tab:execute({tx} _roles-panel)+{reload}+transform-header({focus_cmd})"),
        format!(
            "--bind=click-header:execute-silent({tx} _toggle-hide \
             \"$FZF_CLICK_HEADER_WORD\")+{reload}+transform-header({focus_cmd})"
        ),
        format!("--bind=n:transform-header({focus_cmd})+execute-silent(: >\"$TX_ARM_FILE\")"),
        "--bind=j:down,k:up,g:first,G:last,q:abort".to_owned(),
        "--bind=ctrl-d:half-page-down,ctrl-u:half-page-up".to_owned(),
        format!("--bind=/:{enter_search}"),
        format!("--bind=i:{enter_search}"),
        format!("--bind=esc:{}", picker_key("esc")),
        format!("--bind=enter:{}", picker_key("enter")),
    ]
    .into();
    if binds.edit_tag {
        args.push(format!("--bind=ctrl-t:{edit_tag_bind}"));
        args.push(format!("--bind=t:{edit_tag_bind}"));
    } else {
        // `t` is a normal-mode action key, so it is never in the ignored set: unbound it would type.
        args.push("--bind=t:ignore".to_owned());
    }
    if binds.kill {
        args.push(format!(
            "--bind=d:execute-silent(printf '%s' {{1}} \
             >\"$TX_ARM_FILE\")+transform-header({arm_cmd})"
        ));
        // `y` stays bound in normal mode (an unbound key would type), so it checks the arm.
        args.push(format!(
            "--bind=y:execute-silent([ -s \"$TX_ARM_FILE\" ] && {tx} kill {{1}} >/dev/null \
             2>&1; : >\"$TX_ARM_FILE\")+{reload}"
        ));
    } else {
        args.push("--bind=d:ignore,y:ignore".to_owned());
    }
    args.extend((0..=9).map(|digit| format!("--bind={digit}:{}", picker_key(&digit.to_string()))));
    args.extend(
        keys::ignored_keys()
            .iter()
            .map(|key| format!("--bind={key}:ignore")),
    );
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

/// The picker feed for `sessions` matching `query` outside the hidden roles, newline-terminated.
fn feed(
    core: &TxCore,
    ui: &Ui,
    sessions: &[Session],
    query: &str,
    namew: i64,
) -> Result<String, VerbError> {
    let hidden = filter::read_hidden(&hide_file(core)).map_err(failed)?;
    let targets = ui.live_targets();
    let attachments = ui.attachment_map();
    let rows: Vec<Row<'_>> = sessions
        .iter()
        .filter(|session| {
            !hidden
                .iter()
                .any(|role| role == tx_ui::render::role_of(session))
        })
        .map(|session| Row {
            session,
            target: targets.get(&session.name).map_or("", String::as_str),
            attached: attachments
                .iter()
                .find(|(name, _)| *name == session.name)
                .map_or(&[][..], |(_, locations)| locations.as_slice()),
        })
        .filter(|row| picker_matches(row, query))
        .collect();
    Ok(format!("{}\n", picker_rows(&rows, namew, now())))
}

pub struct List;

impl Verb for List {
    fn name(&self) -> &'static str {
        "_list"
    }

    fn usage(&self) -> &'static str {
        "_list [QUERY]   picker feed (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let namew = namew_from(std::env::var("NAMEW").ok().as_deref());
        let query = argv.first().map(String::as_str).unwrap_or_default();
        let sessions = running(core)?;
        let text = feed(core, &Ui::from_core(core), &sessions, query, namew)?;
        write_out(io, &text)?;
        Ok(0)
    }
}

struct PickOptions {
    query: String,
    jump: bool,
    hide: Option<String>,
}

/// `None` is a usage error.
fn parse_pick(argv: &[String]) -> Option<PickOptions> {
    let mut options = PickOptions {
        query: String::new(),
        jump: false,
        hide: None,
    };
    let mut args = argv.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-f" | "--filter" => options.query = args.next()?.clone(),
            "-j" | "--jump" => options.jump = true,
            "--hide" => options.hide = Some(args.next()?.clone()),
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
        "pick [-f QUERY] [-j] [--hide ROLES]   fzf session picker (prefix+t)"
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
        let namew = picker_namew(detect_term_cols() - PICKER_NUMBER_W as i64 - 1, longest);
        let tx = tx_command(core)?;
        let temp = |prefix: &str| {
            tempfile::Builder::new()
                .prefix(prefix)
                .rand_bytes(8)
                .tempfile()
                .map_err(|e| failed(format!("{prefix} file: {e}")))
                .map(tempfile::NamedTempFile::into_temp_path)
        };
        // The two-press kill's arm file and the pending pick; both removed on drop.
        let arm_file = temp("tx-kill-arm.")?;
        let digits_file = temp("tx-picker-digits.")?;
        let hide_file = hide_file(core);
        let hidden = match &options.hide {
            Some(hide) => {
                let hidden = filter::parse_hidden(hide);
                filter::write_hidden(&hide_file, &hidden).map_err(failed)?;
                hidden
            }
            None => filter::read_hidden(&hide_file).map_err(failed)?,
        };
        let args = fzf_args(
            &tx,
            core.socket_path(),
            namew,
            &options.query,
            &hidden,
            Binds::of(core),
        );
        loop {
            let text = feed(core, &ui, &running(core)?, &options.query, namew)?;
            let Some(output) = select(&args, namew, &arm_file, &digits_file, &text)? else {
                return Ok(0);
            };
            let (popup, selection) = match output.strip_prefix(&format!("{POPUP_LINE}\n")) {
                Some(selection) => (true, selection),
                None => (false, output.as_str()),
            };
            // A popup pick on an empty list prints only the popup line: nothing picked.
            if selection.trim().is_empty() {
                return Ok(0);
            }
            let fields: Vec<&str> = selection.trim_end_matches('\n').split('\t').collect();
            let name = fields[0];
            let target = fields
                .get(3)
                .copied()
                .filter(|target| !target.is_empty())
                .unwrap_or(name);
            if !ui.has_session(name) {
                eprintln!("txc: session '{name}' does not exist");
            } else if popup {
                if open_popup(&ui, &tx, name)? {
                    return Ok(0);
                }
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

/// Popup mode / alt-p: nest-attach the session in a popup over this terminal. A background job
/// opens it once the picker has exited — tmux drops a popup opened while the picker's own is
/// still up, so the job waits for that one to close. False = no tmux client here (re-loop).
fn open_popup(ui: &Ui, tx: &str, name: &str) -> Result<bool, VerbError> {
    let client = ui
        .inside()
        .then(|| ui.display_message("#{client_name}", None))
        .flatten();
    let Some(client) = client else {
        eprintln!("txc: a popup needs tmux");
        return Ok(false);
    };
    let command = format!(
        "sleep 0.1; {tx} _popup-attach {} {}",
        tx_ui::tmux::shell_quote(&client),
        tx_ui::tmux::shell_quote(name)
    );
    ui.run_shell_background(&command).map_err(failed)?;
    Ok(true)
}

/// Run fzf once over `feed`; `None` on abort or an empty selection.
fn select(
    args: &[String],
    namew: i64,
    arm_file: &Path,
    digits_file: &Path,
    feed: &str,
) -> Result<Option<String>, VerbError> {
    let mut child = Command::new(&args[0])
        .args(&args[1..])
        .env("NAMEW", namew.to_string())
        .env("TX_ARM_FILE", arm_file)
        .env(DIGITS_FILE_ENV, digits_file)
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
            return Err(failed(ui.attach_exec(target)));
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
        .map_err(failed)
}

pub struct PickerKey;

impl Verb for PickerKey {
    fn name(&self) -> &'static str {
        "_picker-key"
    }

    fn usage(&self) -> &'static str {
        "_picker-key <key>   the picker's normal-mode key handler: prints fzf actions (internal)"
    }

    fn run(&self, _core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let Some(path) = std::env::var_os(DIGITS_FILE_ENV).filter(|path| !path.is_empty()) else {
            return Err(failed(format!(
                "{DIGITS_FILE_ENV} is not set (run from the txc pick picker)"
            )));
        };
        let key = argv.first().map(String::as_str).unwrap_or_default();
        let pending = std::fs::read_to_string(&path).map_err(failed)?;
        let pending = pending.trim();
        let (popup, digits) = match pending.strip_prefix(POPUP_MARK) {
            Some(digits) => (true, digits),
            None => (false, pending),
        };
        let state = KeyState {
            searching: std::env::var("FZF_PROMPT").ok().as_deref() == Some(SEARCH_PROMPT),
            total: std::env::var("FZF_TOTAL_COUNT")
                .ok()
                .and_then(|count| count.parse().ok())
                .unwrap_or_default(),
            popup,
            digits,
        };
        let Some(reply) = keys::press(key, &state) else {
            let _ = writeln!(io.stderr, "txc _picker-key: unknown key '{key}'");
            return Ok(2);
        };
        std::fs::write(&path, reply.pending()).map_err(failed)?;
        write_out(io, &format!("{}\n", reply.actions))?;
        Ok(0)
    }
}

pub struct Roles;

impl Verb for Roles {
    fn name(&self) -> &'static str {
        "_roles"
    }

    fn usage(&self) -> &'static str {
        "_roles [--chips]   the picker's role chips (--chips: header line; default: TAB panel feed) (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let hidden = filter::read_hidden(&hide_file(core)).map_err(failed)?;
        let text = if argv.iter().any(|arg| arg == "--chips") {
            filter::chips_line(&hidden)
        } else {
            filter::panel_rows(&hidden)
        };
        write_out(io, &format!("{text}\n"))?;
        Ok(0)
    }
}

pub struct ToggleHide;

impl Verb for ToggleHide {
    fn name(&self) -> &'static str {
        "_toggle-hide"
    }

    fn usage(&self) -> &'static str {
        "_toggle-hide <role>|--all   flip a role in the picker's hide file (internal)"
    }

    fn run(&self, core: &TxCore, _io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let path = hide_file(core);
        let word = argv.first().map(String::as_str).unwrap_or_default();
        if word == "--all" {
            filter::toggle_all(&path).map_err(failed)?;
        } else if filter::ROLES.contains(&word) {
            filter::toggle(&path, word).map_err(failed)?;
        }
        Ok(0)
    }
}

pub struct RolesPanel;

impl Verb for RolesPanel {
    fn name(&self) -> &'static str {
        "_roles-panel"
    }

    fn usage(&self) -> &'static str {
        "_roles-panel   the picker's TAB roles panel: fzf over the roles, space toggles one (internal)"
    }

    fn run(&self, core: &TxCore, _io: &mut VerbIo<'_>, _argv: &[String]) -> Result<i32, VerbError> {
        let tx = tx_command(core)?;
        let reload = format!("reload-sync({tx} _roles)");
        let toggle = format!("execute-silent({tx} _toggle-hide {{1}})+{reload}");
        let toggle_all = format!("execute-silent({tx} _toggle-hide --all)+{reload}");
        let color = format!(
            "bg:{BG_DEEP_HEX},fg:{DIM_FG_HEX},pointer:{ACCENT_HEX},fg+:{FG_HEX}:regular,\
             bg+:{SELECTION_BG}:regular,header:{DIM_FG_HEX},prompt:{ACCENT_HEX}"
        );
        let hidden = filter::read_hidden(&hide_file(core)).map_err(failed)?;
        let mut child = Command::new("fzf")
            .args([
                "--ansi",
                "--disabled",
                "--reverse",
                "--no-info",
                "--no-separator",
                "--track",
                "--id-nth=1",
                "--delimiter=\t",
                "--with-nth=2..",
                "--height=100%",
                "--prompt=  roles ",
                &format!("--header={}", filter::panel_legend()),
                &format!("--color={color}"),
                &format!("--bind=space:{toggle}"),
                &format!("--bind=a:{toggle_all}"),
                "--bind=j:down,k:up",
                "--bind=enter,tab,q,esc:abort",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .map_err(|e| failed(format!("could not run fzf: {e}")))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(filter::panel_rows(&hidden).as_bytes());
        }
        child.wait().map_err(failed)?;
        Ok(0)
    }
}

pub struct PopupAttach;

impl Verb for PopupAttach {
    fn name(&self) -> &'static str {
        "_popup-attach"
    }

    fn usage(&self) -> &'static str {
        "_popup-attach <client> <session>   nest-attach the session in a popup over the client (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [client, token] = argv else {
            let _ = writeln!(io.stderr, "usage: txc _popup-attach <client> <session>");
            return Ok(2);
        };
        let ui = Ui::from_core(core);
        let session = core.resolve(token).and_then(|id| core.get_session(&id));
        let Ok(session) = session else {
            ui.display_status(client, &format!("txc: session {token} not found"));
            return Ok(1);
        };
        popup_session(
            &ui,
            &core.socket_path().to_string_lossy(),
            client,
            &session,
            None,
        )
        .map_err(failed)?;
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    const ALL: Binds = Binds {
        edit_tag: true,
        kill: true,
    };

    fn args(binds: Binds, query: &str) -> Vec<String> {
        fzf_args(
            "/x/txc --state-dir /s",
            Path::new("/k/tmux.sock"),
            12,
            query,
            &[],
            binds,
        )
    }

    fn bind<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
        args.iter()
            .filter_map(|arg| arg.strip_prefix("--bind="))
            .find_map(|bind| bind.strip_prefix(&format!("{key}:")))
    }

    #[test]
    fn namew_env_parsing() {
        assert_eq!(namew_from(Some("7")), 7);
        assert_eq!(namew_from(Some("")), 18);
        assert_eq!(namew_from(Some("7a")), 18);
        assert_eq!(namew_from(None), 18);
    }

    #[test]
    fn fzf_args_filter_through_the_feed_and_route_keys_to_txc() {
        let actual = args(ALL, "");
        assert!(actual.contains(&"--disabled".to_owned()));
        assert!(actual.contains(&"--prompt=  ❯ ".to_owned()));
        assert_eq!(
            bind(&actual, "change"),
            Some("reload(/x/txc --state-dir /s _list {q})+first")
        );
        assert_eq!(
            bind(&actual, "enter"),
            Some("transform(/x/txc --state-dir /s _picker-key enter)")
        );
        assert_eq!(
            bind(&actual, "7"),
            Some("transform(/x/txc --state-dir /s _picker-key 7)")
        );
        assert_eq!(bind(&actual, "alt-p"), Some("print(popup)+accept"));
        assert!(
            bind(&actual, "tab")
                .unwrap()
                .starts_with("execute(/x/txc --state-dir /s _roles-panel)+")
        );
        assert!(
            bind(&actual, "t")
                .unwrap()
                .contains("tmux -S /k/tmux.sock display-popup")
        );
        assert!(
            bind(&actual, "y")
                .unwrap()
                .contains("/x/txc --state-dir /s kill {1}")
        );
        assert!(bind(&actual, "x").is_some_and(|action| action == "ignore"));
        let header = actual
            .iter()
            .find(|arg| arg.starts_with("--header="))
            .unwrap();
        assert!(
            header
                .ends_with(" # NAME           LOCATION                  STARTED IDLE   ROLE  TAGS"),
            "{header:?}"
        );
    }

    #[test]
    fn a_query_starts_in_search_mode() {
        let actual = args(ALL, "web");
        assert!(actual.contains(&"--prompt=  / ".to_owned()));
        assert!(actual.contains(&"--query=web".to_owned()));
        assert!(bind(&actual, "start").unwrap().contains("+unbind("));
    }

    #[test]
    fn fzf_args_without_edit_tag_has_no_tag_binds() {
        let actual = args(
            Binds {
                edit_tag: false,
                kill: true,
            },
            "",
        );
        assert!(bind(&actual, "ctrl-t").is_none(), "{actual:?}");
        assert_eq!(bind(&actual, "t"), Some("ignore"), "{actual:?}");
        assert!(!actual.iter().any(|arg| arg.contains("_edit-tag")));
    }

    #[test]
    fn fzf_args_without_kill_ignores_d_and_y() {
        let actual = args(
            Binds {
                edit_tag: true,
                kill: false,
            },
            "",
        );
        assert!(!actual.iter().any(|arg| arg.contains(" kill {1}")));
        assert!(actual.contains(&"--bind=d:ignore,y:ignore".to_owned()));
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
