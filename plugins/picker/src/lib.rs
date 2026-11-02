//! tx-core plugin `picker`: the fzf session picker (prefix+t). It lists the running agents and
//! the views but the home base in one flat list, newest activity first, without the roles the
//! TAB panel hides. `_list` is its feed (fzf re-runs it to reload), `pick` is the interactive
//! loop with its vim normal and search modes, and `_picker-popup` shows a pick in a popup
//! (`p` then Enter, or alt-p). `_picker-key`, `_roles`, `_toggle-hide` and `_roles-panel` serve
//! the fzf binds. Port of main's `tx attach` without `--host`/`--all` and without the digit
//! picks. Publishes `keys/picker.tmux`.

mod picker_filter;
mod picker_keys;

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use tx_core::events::now;
use tx_core::{Agent, ListFilter, Plugin, PluginError, Registry, TxCore, Verb, VerbError, VerbIo};

use tx_client::render::{
    ACCENT_ANSI, BOLD, FG_ANSI, LOCATION_W, PickerEntry, RESET, ROLE_W, Row, SELECTION_BG,
    WARN_ANSI, listed, picker_namew, picker_rows, role_of,
};
use tx_client::tmux::{ACCENT_HEX, BG_DEEP_HEX, DIM_FG_HEX, FG_HEX, Ui, shell_quote};
use tx_client::views::{self, View};

use picker_filter::{chips_line, hidden_path, panel_legend, panel_rows, read_hidden};
use picker_keys::{
    DIGITS_FILE_ENV, KeyState, NORMAL_PROMPT, POPUP_LINE, POPUP_MARK, SEARCH_PROMPT, ignored_keys,
    normal_footer, search_footer, toggle_normal_keys,
};

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
        r.verb(Arc::new(PickerPopup));
        r.verb(Arc::new(PickerKey));
        r.verb(Arc::new(Roles));
        r.verb(Arc::new(ToggleHide));
        r.verb(Arc::new(RolesPanel));
        r.resource("keys/picker.tmux", include_bytes!("../tmux/keys.tmux"))
    }
}

const DEFAULT_NAMEW: i64 = 18;
const DEFAULT_COLS: i64 = 80;
const RETRY_PAUSE: Duration = Duration::from_millis(1200);
/// Lets the picker's own popup close first: tmux drops a popup opened while another is up.
const POPUP_DELAY: &str = "0.1";
/// Foreground commands of a pane that may be replaced by a nested attach.
const HOSTING_COMMANDS: [&str; 6] = ["zsh", "bash", "sh", "fish", "dash", "tmux"];

const PICK_USAGE: &str = "\
usage: tx pick [-f|--filter QUERY] [-j|--jump]
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

/// Which binds of other plugins' verbs the picker offers: `t` and ctrl-t need `_edit-tag`, `d`
/// and `y` need `kill`; without them those keys are ignored.
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

/// The fzf argv, a port of main's `Attach::fzf_opts`: normal mode (`❯`) unless a query is
/// given, search mode (`/`) then. `{1}`, `{2}`, `$TX_ARM_FILE`, `$TX_DIGITS_FILE` and
/// `$FZF_PORT` stay literal for fzf and the shells it spawns; the two files reach them through
/// the child's environment. `arguments` are the core's `cli_arguments()`, which every
/// re-invocation of `bin` carries. `hidden` seeds the chips line.
pub fn fzf_args(
    bin: &Path,
    socket: &Path,
    arguments: &[String],
    namew: i64,
    query: &str,
    hidden: &[&str],
    binds: Binds,
) -> Vec<String> {
    let namew = namew.max(0) as usize;
    let bin = bin.display();
    let socket = socket.display();
    let arguments = arguments.join(" ");
    let tx = format!("{bin} {arguments}");
    let reload = format!("reload-sync({tx} _list)");
    let edit_tag = format!("{tx} _edit-tag {{1}}");
    let chips = format!("{tx} _roles --chips");
    let picker_key = |key: &str| format!("transform({tx} _picker-key {key})");
    let header_cols = format!(
        "{:<namew$}   {:<LOCATION_W$} {:<7} {:<6} {:<ROLE_W$} TAGS",
        "NAME", "LOCATION", "STARTED", "IDLE", "ROLE"
    );
    let focus_cmd = format!(
        "printf '{ACCENT_ANSI}{BOLD}%s{RESET} {FG_ANSI}{BOLD}%s{RESET}\\n' {{1}} {{2}}; {chips}; \
         printf '%s' '{header_cols}'"
    );
    let arm_cmd = format!(
        "printf '{WARN_ANSI}{BOLD} ⚠  Kill \"%s\"? [y/N]{RESET}\\n' {{1}}; {chips}; \
         printf '%s' '{header_cols}'"
    );
    let unbind_normal = toggle_normal_keys("unbind");
    let (prompt, start_mode, footer) = if query.is_empty() {
        (NORMAL_PROMPT, String::new(), normal_footer(false))
    } else {
        (SEARCH_PROMPT, format!("+{unbind_normal}"), search_footer())
    };
    let enter_search = format!(
        "change-prompt({SEARCH_PROMPT})+{unbind_normal}+execute-silent(: >\"$TX_ARM_FILE\"; : \
         >\"${DIGITS_FILE_ENV}\")+transform-header({focus_cmd})+change-footer:{}",
        search_footer()
    );
    let mut args: Vec<String> = [
        "fzf".to_owned(),
        "--exact".to_owned(),
        "--ansi".to_owned(),
        format!("--prompt={prompt}"),
        "--height=100%".to_owned(),
        "--reverse".to_owned(),
        "--delimiter=\t".to_owned(),
        "--with-nth=5..".to_owned(),
        "--listen".to_owned(),
        "--track".to_owned(),
        "--with-shell=sh -c".to_owned(),
        "--color=fg:#a9b1d6,pointer:#7aa2f7,fg+:#a9b1d6:regular,bg+:236:regular,hl:#7aa2f7,hl+:#7aa2f7,header:#a9b1d6,footer:#a9b1d6,prompt:#a9b1d6,query:#c0caf5".to_owned(),
        format!("--header={}\n{header_cols}", chips_line(hidden)),
        format!("--footer={footer}"),
        format!("--query={query}"),
        format!(
            "--bind=start:execute-silent(( while sleep 1; do [ -s \"${DIGITS_FILE_ENV}\" ] || \
             curl -fsS -XPOST \"localhost:$FZF_PORT\" -d \"{reload}\" >/dev/null 2>&1 || exit 0; \
             done ) &){start_mode}"
        ),
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
            "--bind=click-header:execute-silent({tx} _toggle-hide --click \"$FZF_CLICK_HEADER_COLUMN\" \"$FZF_CLICK_HEADER_WORD\")+{reload}+transform-header({focus_cmd})"
        ),
    ]
    .into();
    if binds.edit_tag {
        let edit_tag_bind = format!(
            "execute(tmux -S {socket} display-popup -E -h 4 -w 60% \"{edit_tag}\")+{reload}"
        );
        args.push(format!("--bind=ctrl-t:{edit_tag_bind}"));
        args.push(format!("--bind=t:{edit_tag_bind}"));
    } else {
        args.push("--bind=t:ignore".to_owned());
    }
    if binds.kill {
        args.push(format!(
            "--bind=d:execute-silent(printf '%s' {{1}} >\"$TX_ARM_FILE\")+transform-header({arm_cmd})"
        ));
        // `y` stays bound in normal mode (an unbound key would type), so it checks the arm.
        args.push(format!(
            "--bind=y:execute-silent([ -s \"$TX_ARM_FILE\" ] && {tx} kill {{1}} >/dev/null 2>&1; : >\"$TX_ARM_FILE\")+{reload}+transform-header({focus_cmd})"
        ));
    } else {
        args.push("--bind=d:ignore,y:ignore".to_owned());
    }
    args.extend([
        format!("--bind=n:transform-header({focus_cmd})+execute-silent(: >\"$TX_ARM_FILE\")"),
        "--bind=j:down,k:up,g:first,G:last,q:abort".to_owned(),
        "--bind=ctrl-d:half-page-down,ctrl-u:half-page-up".to_owned(),
        format!("--bind=/:{enter_search}"),
        format!("--bind=i:{enter_search}"),
        format!("--bind=esc:{}", picker_key("esc")),
        format!("--bind=enter:{}", picker_key("enter")),
    ]);
    args.extend(
        ignored_keys()
            .iter()
            .map(|key| format!("--bind={key}:ignore")),
    );
    args
}

/// What the picker lists: reconciled with tmux and with the views whose agent is gone closed,
/// the running agents and every view but the home base, less the hidden roles.
struct Listing {
    agents: Vec<Agent>,
    views: Vec<View>,
    hidden: Vec<&'static str>,
}

impl Listing {
    fn gather(core: &TxCore, ui: &Ui) -> Result<Self, VerbError> {
        core.refresh_all()?;
        views::close_orphans(ui, core).map_err(ui_failed)?;
        let ids = core.list_agents(&ListFilter::new().running())?;
        let agents = ids
            .iter()
            .map(|id| core.get_agent(id).map_err(VerbError::from))
            .collect::<Result<_, _>>()?;
        let views = views::list(ui).map_err(ui_failed)?;
        let hidden = read_hidden(&hidden_path(core)).map_err(hidden_failed)?;
        Ok(Listing {
            agents,
            views,
            hidden,
        })
    }

    fn entries(&self) -> impl Iterator<Item = PickerEntry<'_>> {
        self.agents
            .iter()
            .map(PickerEntry::Agent)
            .chain(self.views.iter().map(PickerEntry::View))
            .filter(|entry| listed(*entry) && !self.hidden.contains(&role_of(*entry)))
    }

    /// The widest NAME cell.
    fn longest_name(&self) -> i64 {
        self.entries()
            .map(|entry| entry.name().chars().count() as i64)
            .max()
            .unwrap_or(0)
    }

    /// The picker feed, newline-terminated.
    fn feed(&self, ui: &Ui, namew: i64) -> String {
        let targets = ui.live_targets();
        let attachments = ui.attachment_map();
        let rows: Vec<Row<'_>> = self
            .entries()
            .map(|entry| Row {
                entry,
                target: targets.get(entry.name()).map_or("", String::as_str),
                attached: attachments
                    .iter()
                    .find(|(name, _)| name == entry.name())
                    .map_or(&[][..], |(_, locations)| locations.as_slice()),
            })
            .collect();
        format!("{}\n", picker_rows(&rows, namew, now()))
    }
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
        let ui = Ui::from_core(core);
        let text = Listing::gather(core, &ui)?.feed(&ui, namew);
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
        let namew = picker_namew(
            detect_term_cols(),
            Listing::gather(core, &ui)?.longest_name(),
        );
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
        // Popup mode's pending-pick file (`_picker-key`); removed on drop.
        let digits_file = tempfile::Builder::new()
            .prefix("tx-picker-digits.")
            .rand_bytes(8)
            .tempfile()
            .map_err(|e| VerbError::Failed(format!("pending-pick file: {e}")))?
            .into_temp_path();
        let files = PickerFiles {
            arm: &arm_file,
            digits: &digits_file,
        };
        let hidden = read_hidden(&hidden_path(core)).map_err(hidden_failed)?;
        let args = fzf_args(
            &bin,
            core.socket_path(),
            &core.config().cli_arguments(),
            namew,
            &options.query,
            &hidden,
            Binds::of(core),
        );
        loop {
            let text = Listing::gather(core, &ui)?.feed(&ui, namew);
            let Some(output) = select(&args, namew, files, &text)? else {
                return Ok(0);
            };
            let (popup, selection) = split_popup(&output);
            // A popup pick on an empty list prints only the popup line: nothing picked, as with Enter.
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
                eprintln!("tx: session '{name}' does not exist");
            } else if popup {
                if open_popup(core, &ui, name)? {
                    return Ok(0);
                }
                eprintln!("tx: a popup needs a tmux client");
            } else if options.jump {
                if jump(&ui, name, target)? {
                    return Ok(0);
                }
                eprintln!("tx: could not jump to or switch to session {name}");
            } else {
                drop(arm_file);
                drop(digits_file);
                open(&ui, target, name)?;
                return Ok(0);
            }
            std::thread::sleep(RETRY_PAUSE);
        }
    }
}

/// A popup pick's output is the popup line, then the selection.
fn split_popup(output: &str) -> (bool, &str) {
    match output
        .strip_prefix(POPUP_LINE)
        .and_then(|rest| rest.strip_prefix('\n'))
    {
        Some(selection) => (true, selection),
        None if output.trim_end() == POPUP_LINE => (true, ""),
        None => (false, output),
    }
}

/// alt-p: a background `_picker-popup` nest-attaches `name` in a popup on this client once the
/// picker's own popup has closed. False when there is no client to show it on.
fn open_popup(core: &TxCore, ui: &Ui, name: &str) -> Result<bool, VerbError> {
    let Some(client) = ui
        .inside()
        .then(|| ui.display_message("#{client_name}", None))
        .flatten()
    else {
        return Ok(false);
    };
    let bin =
        std::env::current_exe().map_err(|e| VerbError::Failed(format!("current_exe: {e}")))?;
    let mut words = vec![shell_quote(&bin.to_string_lossy())];
    words.extend(
        core.config()
            .cli_arguments()
            .iter()
            .map(|word| shell_quote(word)),
    );
    words.extend([POPUP_VERB, &client, name].map(shell_quote));
    ui.run_shell_background(&format!("sleep {POPUP_DELAY}; {}", words.join(" ")))
        .map_err(ui_failed)?;
    Ok(true)
}

const POPUP_VERB: &str = "_picker-popup";

/// `_picker-popup <client> <name>`: the agent or view `name` nest-attached in a popup over
/// `client`'s terminal; detaching closes it and the session keeps running.
pub struct PickerPopup;

impl Verb for PickerPopup {
    fn name(&self) -> &'static str {
        POPUP_VERB
    }

    fn usage(&self) -> &'static str {
        "_picker-popup <client> <name>   a picked session in a popup (alt-p, internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [client, name] = argv else {
            let _ = writeln!(io.stderr, "usage: tx {POPUP_VERB} <client> <name>");
            return Ok(2);
        };
        let ui = Ui::from_core(core);
        if !ui.has_session(name) {
            ui.display_status(client, &format!("tx: session '{name}' does not exist"));
            return Ok(1);
        }
        let (_, prefix) = ui.quiet(&["show-options", "-gv", "prefix"]);
        if let Err(error) = ui.popup_attach(
            client,
            &format!(" {name} "),
            &popup_footer(prefix.trim()),
            name,
        ) {
            ui.display_status(client, &format!("tx: {error}"));
            return Ok(1);
        }
        Ok(0)
    }
}

fn popup_footer(prefix: &str) -> String {
    format!(
        " #[fg={FG_HEX}]{prefix} d#[fg={DIM_FG_HEX}] closes the popup, the session keeps running"
    )
}

/// The files the fzf binds share with the picker: the kill's arm, popup mode's pending pick.
#[derive(Clone, Copy)]
struct PickerFiles<'a> {
    arm: &'a Path,
    digits: &'a Path,
}

/// Run fzf once over `feed`; `None` on abort or an empty selection.
fn select(
    args: &[String],
    namew: i64,
    files: PickerFiles<'_>,
    feed: &str,
) -> Result<Option<String>, VerbError> {
    let mut child = Command::new(&args[0])
        .args(&args[1..])
        .env("NAMEW", namew.to_string())
        .env("TX_ARM_FILE", files.arm)
        .env(DIGITS_FILE_ENV, files.digits)
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

fn hidden_failed(error: std::io::Error) -> VerbError {
    VerbError::Failed(format!("picker hidden roles: {error}"))
}

/// This binary and the core's arguments, as the binds re-invoke it.
fn tx_command(core: &TxCore) -> Result<String, VerbError> {
    let bin = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| VerbError::Failed(format!("current_exe: {e}")))?;
    Ok(format!(
        "{} {}",
        bin.display(),
        core.config().cli_arguments().join(" ")
    ))
}

/// `_picker-key <key>`: the normal-mode handler for esc, enter and p. Reads and rewrites the
/// pending-pick file, prints the fzf actions for `transform(...)`.
pub struct PickerKey;

impl Verb for PickerKey {
    fn name(&self) -> &'static str {
        "_picker-key"
    }

    fn usage(&self) -> &'static str {
        "_picker-key <esc|enter|p>   the picker's normal-mode key handler (internal)"
    }

    fn run(&self, _core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let Some(path) = std::env::var_os(DIGITS_FILE_ENV).filter(|path| !path.is_empty()) else {
            let _ = writeln!(
                io.stderr,
                "tx _picker-key: {DIGITS_FILE_ENV} is not set (run from the picker)"
            );
            return Ok(2);
        };
        let key = argv.first().map(String::as_str).unwrap_or_default();
        let pending = std::fs::read_to_string(&path)
            .map_err(|e| VerbError::Failed(format!("pending-pick file: {e}")))?;
        let state = KeyState {
            searching: std::env::var("FZF_PROMPT").is_ok_and(|prompt| prompt == SEARCH_PROMPT),
            popup: pending.trim() == POPUP_MARK,
        };
        let Some(reply) = picker_keys::press(key, &state) else {
            let _ = writeln!(io.stderr, "tx _picker-key: unknown key '{key}'");
            return Ok(2);
        };
        std::fs::write(&path, reply.pending())
            .map_err(|e| VerbError::Failed(format!("pending-pick file: {e}")))?;
        writeln!(io.stdout, "{}", reply.actions)
            .map_err(|e| VerbError::Failed(format!("write: {e}")))?;
        Ok(0)
    }
}

/// `_roles [--chips]`: the header's chips line, else the TAB panel's feed.
pub struct Roles;

impl Verb for Roles {
    fn name(&self) -> &'static str {
        "_roles"
    }

    fn usage(&self) -> &'static str {
        "_roles [--chips]   the picker's role chips: header line, else TAB panel feed (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let hidden = read_hidden(&hidden_path(core)).map_err(hidden_failed)?;
        let text = if argv.iter().any(|arg| arg == "--chips") {
            chips_line(&hidden)
        } else {
            panel_rows(&hidden)
        };
        writeln!(io.stdout, "{text}").map_err(|e| VerbError::Failed(format!("write: {e}")))?;
        Ok(0)
    }
}

/// `_toggle-hide <role>|--all|--click <column> <line>`: flip a role in the hidden set. `--click`
/// takes the word of the clicked header line under the column; a word that is not a role (a
/// click on the header's other words) is ignored.
pub struct ToggleHide;

impl Verb for ToggleHide {
    fn name(&self) -> &'static str {
        "_toggle-hide"
    }

    fn usage(&self) -> &'static str {
        "_toggle-hide <role>|--all|--click <column> <line>   flip a role the picker hides; --all: all on, or all off when none is hidden (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let path = hidden_path(core);
        let word = match argv {
            [all] if all == "--all" => {
                picker_filter::toggle_all(&path).map_err(hidden_failed)?;
                return Ok(0);
            }
            [click, column, line] if click == "--click" => {
                picker_filter::word_at(line, column.parse().unwrap_or_default())
            }
            [word] => word.as_str(),
            _ => {
                let _ = writeln!(io.stderr, "usage: tx {}", self.usage());
                return Ok(2);
            }
        };
        if let Some(role) = picker_filter::role_named(word) {
            picker_filter::toggle(&path, role).map_err(hidden_failed)?;
        }
        Ok(0)
    }
}

/// `_roles-panel`: the TAB roles panel, an fzf over the four roles in the picker's own
/// terminal; space toggles the cursor's role, `a` toggles all, Enter/TAB/q/Esc close.
pub struct RolesPanel;

impl Verb for RolesPanel {
    fn name(&self) -> &'static str {
        "_roles-panel"
    }

    fn usage(&self) -> &'static str {
        "_roles-panel   the picker's TAB roles panel (internal)"
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
        let feed = panel_rows(&read_hidden(&hidden_path(core)).map_err(hidden_failed)?);
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
                "--with-shell=sh -c",
                "--prompt=  roles ",
                &format!("--header={}", panel_legend()),
                &format!("--color={color}"),
                &format!("--bind=space:{toggle}"),
                &format!("--bind=a:{toggle_all}"),
                "--bind=j:down,k:up",
                "--bind=enter,tab,q,esc:abort",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .map_err(|e| VerbError::Failed(format!("could not run fzf: {e}")))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(feed.as_bytes());
        }
        child
            .wait()
            .map_err(|e| VerbError::Failed(format!("fzf: {e}")))?;
        Ok(0)
    }
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

/// `current_hosts`: the launching session is a view whose shell panes host nested attaches
/// (the home base, the tutor); from any other session the client is switched. `target_hosts`:
/// the picked session is such a view itself, which is switched to, never nested into a pane.
fn open_mode(
    inside: bool,
    current_hosts: bool,
    target_hosts: bool,
    pane_command: Option<&str>,
) -> OpenMode {
    if !inside {
        return OpenMode::Attach;
    }
    if current_hosts
        && !target_hosts
        && pane_command.is_some_and(|command| HOSTING_COMMANDS.contains(&command))
    {
        OpenMode::Nest
    } else {
        OpenMode::Switch
    }
}

/// The open mode for the caller's surroundings and the picked `name`, with the pane to nest
/// into.
fn open_mode_here(ui: &Ui, name: &str) -> (OpenMode, Option<String>) {
    let inside = ui.inside();
    let pane = inside.then(|| ui.current_pane_id()).flatten();
    let current_hosts = pane.is_some()
        && ui
            .current_session_name()
            .is_some_and(|session| ui.hosts_nested_attaches(&session));
    let command = pane
        .as_deref()
        .and_then(|pane| ui.display_message("#{pane_current_command}", Some(pane)));
    let mode = open_mode(
        inside,
        current_hosts,
        ui.hosts_nested_attaches(name),
        command.as_deref(),
    );
    (mode, pane)
}

/// Default path: nest into the launching view's shell pane, else switch the client, else
/// attach this terminal.
fn open(ui: &Ui, target: &str, name: &str) -> Result<(), VerbError> {
    match open_mode_here(ui, name) {
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
    if let (OpenMode::Nest, Some(pane)) = open_mode_here(ui, name) {
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
            Path::new("/x/tx"),
            Path::new("/k/tmux.sock"),
            &["--data-dir".to_owned(), "/s".to_owned()],
            12,
            "",
            &["shell"],
            binds,
        )
    }

    fn bind_of<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
        args.iter().find_map(|arg| {
            arg.strip_prefix("--bind=")
                .and_then(|bind| bind.strip_prefix(key))
                .and_then(|bind| bind.strip_prefix(':'))
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
        let tx = "/x/tx --data-dir /s";
        let reload = format!("reload-sync({tx} _list)");
        let chips = format!("{tx} _roles --chips");
        let focus = format!(
            "printf '\x1b[38;2;122;162;247m\x1b[1m%s\x1b[0m \x1b[38;2;192;202;245m\x1b[1m%s\x1b[0m\\n' {{1}} {{2}}; {chips}; printf '%s' '{HEADER}'"
        );
        let arm = format!(
            "printf '\x1b[38;2;224;175;104m\x1b[1m ⚠  Kill \"%s\"? [y/N]\x1b[0m\\n' {{1}}; {chips}; printf '%s' '{HEADER}'"
        );
        let unbind = toggle_normal_keys("unbind");
        let search = format!(
            "change-prompt(  / )+{unbind}+execute-silent(: >\"$TX_ARM_FILE\"; : >\"$TX_DIGITS_FILE\")+transform-header({focus})+change-footer:{}",
            search_footer()
        );
        let mut expected: Vec<String> = vec![
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
            "--with-shell=sh -c".into(),
            "--color=fg:#a9b1d6,pointer:#7aa2f7,fg+:#a9b1d6:regular,bg+:236:regular,hl:#7aa2f7,hl+:#7aa2f7,header:#a9b1d6,footer:#a9b1d6,prompt:#a9b1d6,query:#c0caf5".into(),
            format!("--header={}\n{HEADER}", chips_line(&["shell"])),
            format!("--footer={}", normal_footer(false)),
            "--query=".into(),
            format!("--bind=start:execute-silent(( while sleep 1; do [ -s \"$TX_DIGITS_FILE\" ] || curl -fsS -XPOST \"localhost:$FZF_PORT\" -d \"{reload}\" >/dev/null 2>&1 || exit 0; done ) &)"),
            format!("--bind=ctrl-r:{reload}"),
            format!("--bind=r:{reload}"),
            format!("--bind=p:transform({tx} _picker-key p)"),
            "--bind=alt-p:print(popup)+accept".into(),
            format!("--bind=focus:transform-header({focus})+execute-silent(: >\"$TX_ARM_FILE\")"),
            format!("--bind=tab:execute({tx} _roles-panel)+{reload}+transform-header({focus})"),
            format!("--bind=click-header:execute-silent({tx} _toggle-hide --click \"$FZF_CLICK_HEADER_COLUMN\" \"$FZF_CLICK_HEADER_WORD\")+{reload}+transform-header({focus})"),
            format!("--bind=ctrl-t:execute(tmux -S /k/tmux.sock display-popup -E -h 4 -w 60% \"{tx} _edit-tag {{1}}\")+{reload}"),
            format!("--bind=t:execute(tmux -S /k/tmux.sock display-popup -E -h 4 -w 60% \"{tx} _edit-tag {{1}}\")+{reload}"),
            format!("--bind=d:execute-silent(printf '%s' {{1}} >\"$TX_ARM_FILE\")+transform-header({arm})"),
            format!("--bind=y:execute-silent([ -s \"$TX_ARM_FILE\" ] && {tx} kill {{1}} >/dev/null 2>&1; : >\"$TX_ARM_FILE\")+{reload}+transform-header({focus})"),
            format!("--bind=n:transform-header({focus})+execute-silent(: >\"$TX_ARM_FILE\")"),
            "--bind=j:down,k:up,g:first,G:last,q:abort".into(),
            "--bind=ctrl-d:half-page-down,ctrl-u:half-page-up".into(),
            format!("--bind=/:{search}"),
            format!("--bind=i:{search}"),
            format!("--bind=esc:transform({tx} _picker-key esc)"),
            format!("--bind=enter:transform({tx} _picker-key enter)"),
        ];
        expected.extend(
            ignored_keys()
                .iter()
                .map(|key| format!("--bind={key}:ignore")),
        );
        assert_eq!(args(ALL), expected);
    }

    #[test]
    fn a_query_starts_in_search_mode() {
        let actual = fzf_args(
            Path::new("/x/tx"),
            Path::new("/k/tmux.sock"),
            &[],
            12,
            "foo",
            &[],
            ALL,
        );
        assert!(actual.contains(&"--prompt=  / ".to_owned()));
        assert!(actual.contains(&format!("--footer={}", search_footer())));
        let start = bind_of(&actual, "start").unwrap();
        assert!(start.ends_with(&format!(") &)+{}", toggle_normal_keys("unbind"))));
    }

    #[test]
    fn digits_are_ignored_in_normal_mode() {
        let actual = args(ALL);
        for digit in '0'..='9' {
            assert_eq!(
                bind_of(&actual, &digit.to_string()),
                Some("ignore"),
                "{digit}"
            );
        }
    }

    #[test]
    fn fzf_args_without_edit_tag_ignores_t() {
        let actual = args(Binds {
            edit_tag: false,
            kill: true,
        });
        assert_eq!(bind_of(&actual, "ctrl-t"), None, "{actual:?}");
        assert_eq!(bind_of(&actual, "t"), Some("ignore"));
        assert!(!actual.iter().any(|arg| arg.contains("_edit-tag")));
        assert_eq!(actual.len(), args(ALL).len() - 1);
        for key in ["d", "y", "n", "ctrl-r"] {
            assert!(bind_of(&actual, key).is_some(), "{key}");
        }
    }

    #[test]
    fn fzf_args_without_kill_ignores_d_and_y() {
        let actual = args(Binds {
            edit_tag: true,
            kill: false,
        });
        assert!(actual.contains(&"--bind=d:ignore,y:ignore".to_owned()));
        assert!(!actual.iter().any(|arg| arg.contains("/s kill ")));
        assert_eq!(actual.len(), args(ALL).len() - 1);
        for key in ["ctrl-t", "t", "n", "ctrl-r"] {
            assert!(bind_of(&actual, key).is_some(), "{key}");
        }
    }

    #[test]
    fn open_mode_by_surroundings() {
        for command in [Some("zsh"), Some("tx"), None] {
            for target_hosts in [false, true] {
                assert_eq!(
                    open_mode(false, true, target_hosts, command),
                    OpenMode::Attach
                );
                assert_eq!(
                    open_mode(false, false, target_hosts, command),
                    OpenMode::Attach
                );
            }
        }
        for shell in HOSTING_COMMANDS {
            assert_eq!(
                open_mode(true, true, false, Some(shell)),
                OpenMode::Nest,
                "{shell}"
            );
            // A home base or tutor picked from a home base is switched to, never nested.
            assert_eq!(
                open_mode(true, true, true, Some(shell)),
                OpenMode::Switch,
                "{shell}"
            );
        }
        assert_eq!(open_mode(true, false, false, Some("zsh")), OpenMode::Switch);
        assert_eq!(open_mode(true, true, false, Some("nvim")), OpenMode::Switch);
        assert_eq!(open_mode(true, true, false, Some("tx")), OpenMode::Switch);
        assert_eq!(open_mode(true, true, false, None), OpenMode::Switch);
        assert_eq!(open_mode(true, false, false, None), OpenMode::Switch);
    }

    #[test]
    fn alt_p_output_is_split_from_the_selection() {
        let row = "sh\t\tL\t$4\tsh\n";
        assert_eq!(split_popup(&format!("popup\n{row}")), (true, row));
        assert_eq!(split_popup(row), (false, row));
        assert_eq!(split_popup("popup\n"), (true, ""));
        assert_eq!(split_popup("popup"), (true, ""));
        // A session named `popup` picked with Enter is a plain pick.
        let named = "popup\t\tL\t$1\tpopup\n";
        assert_eq!(split_popup(named), (false, named));
    }

    #[test]
    fn the_popup_footer_names_the_prefix() {
        assert!(popup_footer("C-a").contains("C-a d"));
    }
}
