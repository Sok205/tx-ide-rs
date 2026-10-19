//! tx-core plugin `mockup`: agents write JSON screen descriptions, a ratatui viewer renders
//! them in a per-agent `mockup-<agent>` view, and the operator's terminal is switched there
//! and back. The `model` module is the pure description format; `markup`, `render` and
//! `viewer` are pure too. This file is the UI layer: the verbs and the switching.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use tx_core::{
    CoreError, Plugin, PluginError, Registry, Session, SessionId, TxCore, Verb, VerbError, VerbIo,
};
use tx_ui::tmux::{Ui, UiError, shell_quote};

pub mod markup;
pub mod model;
pub mod render;
pub mod viewer;

pub const PLUGIN_ID: &str = "mockup";
pub const VIEW_VERB: &str = "_mockup-view";
/// Views are `mockup-<agent name>`.
pub const VIEW_PREFIX: &str = "mockup-";
/// Session dirs are `<project>/.superpowers/mockup/<agent id>`.
pub const SESSION_DIRS: &str = ".superpowers/mockup";

const VIEW_OPTION: &str = "@tx-view";
const DIR_OPTION: &str = "@tx-mockup-dir";
const RETURN_OPTION: &str = "@tx-mockup-return";

const USAGE: &str = "usage: txc mockup start --project DIR [--for S] | show [--for S] | wait [--for S] | stop [--for S]";

#[derive(Clone, Copy, Debug, Default)]
pub struct MockupPlugin;

impl Plugin for MockupPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.verb(Arc::new(MockupVerb));
        r.verb(Arc::new(ViewVerb));
        r.resource(
            "agents/COMMON.local.md",
            include_bytes!("../../../../agents/COMMON.mockup.local.md"),
        )?;
        r.resource(
            "agents/skills/tx-mockup/SKILL.md",
            include_bytes!("../../../../agents/skills/tx-mockup/SKILL.md"),
        )
    }
}

pub fn view_name(agent: &str) -> String {
    format!("{VIEW_PREFIX}{agent}")
}

enum Sub {
    Start(PathBuf),
    Show,
    Wait,
    Stop,
}

struct Args {
    sub: Sub,
    agent: Option<String>,
}

fn parse_args(argv: &[String]) -> Option<Args> {
    let (sub, rest) = argv.split_first()?;
    let mut project = None;
    let mut agent = None;
    let mut rest = rest.iter();
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--project" => project = Some(PathBuf::from(rest.next()?)),
            "--for" => agent = Some(rest.next()?.clone()),
            _ => return None,
        }
    }
    let sub = match sub.as_str() {
        "start" => Sub::Start(project?),
        "show" => Sub::Show,
        "wait" => Sub::Wait,
        "stop" => Sub::Stop,
        _ => return None,
    };
    Some(Args { sub, agent })
}

/// `--for`, else the caller's own session: `$TX_SESSION_ID`, only when `$TMUX` names the
/// core's own server (the rule `txc send` uses for its sender).
fn resolve_agent(core: &TxCore, token: Option<&str>) -> Result<Option<SessionId>, CoreError> {
    match token {
        Some(token) => core.resolve(token).map(Some),
        None => Ok(agent_from_env(core)),
    }
}

fn agent_from_env(core: &TxCore) -> Option<SessionId> {
    let tmux = std::env::var("TMUX").ok()?;
    let socket = tmux.split(',').next()?;
    if Path::new(socket) != core.socket_path() {
        return None;
    }
    std::env::var("TX_SESSION_ID").ok()?.parse().ok()
}

fn failed(error: UiError) -> VerbError {
    VerbError::Failed(error.to_string())
}

fn write_failed(error: std::io::Error) -> VerbError {
    VerbError::Failed(format!("write: {error}"))
}

/// The session dir `start` recorded in `ext["mockup"].dir`.
fn session_dir(session: &Session) -> Option<PathBuf> {
    session
        .ext
        .get(PLUGIN_ID)?
        .get("dir")?
        .as_str()
        .map(PathBuf::from)
}

/// The project a session dir lives under (`<project>/.superpowers/mockup/<id>`).
fn project_of(dir: &Path) -> &Path {
    dir.ancestors()
        .nth(3)
        .expect("a session dir is <project>/.superpowers/mockup/<id>")
}

/// `<txc> --state-dir <state> [--socket <socket>] _mockup-view <dir> <view>`, shell-quoted.
fn viewer_command(core: &TxCore, dir: &Path, view: &str) -> Result<String, VerbError> {
    let exe = std::env::current_exe()
        .map_err(|error| VerbError::Failed(format!("current exe: {error}")))?;
    let configuration = core.config();
    let mut words = vec![
        shell_quote(&exe.to_string_lossy()),
        "--state-dir".to_owned(),
        shell_quote(&configuration.state_dir.to_string_lossy()),
    ];
    if let Some(socket) = &configuration.tmux_socket {
        words.push("--socket".to_owned());
        words.push(shell_quote(&socket.to_string_lossy()));
    }
    words.push(VIEW_VERB.to_owned());
    words.push(shell_quote(&dir.to_string_lossy()));
    words.push(shell_quote(view));
    Ok(words.join(" "))
}

/// Creates the view running the viewer unless it is already there, then marks it.
fn ensure_view(core: &TxCore, ui: &Ui, view: &str, dir: &Path) -> Result<(), VerbError> {
    let command = viewer_command(core, dir, view)?;
    if !ui.has_session(view) {
        let project = project_of(dir).to_string_lossy().into_owned();
        ui.run(&["new-session", "-d", "-s", view, "-c", &project, &command])
            .map_err(failed)?;
    } else if ui
        .display_message("#{pane_dead}", Some(&format!("={view}:")))
        .as_deref()
        == Some("1")
    {
        ui.respawn_pane(&format!("={view}:"), &command)
            .map_err(failed)?;
    }
    ui.run(&[
        "set-window-option",
        "-t",
        &format!("={view}:"),
        "remain-on-exit",
        "on",
    ])
    .map_err(failed)?;
    let dir = dir.to_string_lossy();
    for (option, value) in [
        (VIEW_OPTION, "1"),
        (DIR_OPTION, dir.as_ref()),
        ("status", "off"),
    ] {
        ui.set_session_option(view, option, value).map_err(failed)?;
    }
    Ok(())
}

/// The sessions whose panes nest-attach `agent`, followed transitively, nearest first.
fn hosts_of(ui: &Ui, agent: &str) -> Vec<String> {
    let map = ui.attachment_map();
    let mut hosts: Vec<String> = Vec::new();
    let mut queue = VecDeque::from([agent.to_owned()]);
    while let Some(inner) = queue.pop_front() {
        let found = map
            .iter()
            .filter(|(nested, _)| *nested == inner)
            .flat_map(|(_, locations)| locations)
            .map(|location| location.host.clone());
        for host in found {
            if host != agent && !hosts.contains(&host) {
                hosts.push(host.clone());
                queue.push_back(host);
            }
        }
    }
    hosts
}

/// Picks the operator's terminal from `list-clients -F
/// '#{client_name}\t#{client_session}\t#{client_activity}'`. Candidates sit on `agent` or on
/// one of its `hosts` (nearest first). The outermost wins, since a nested attach is a client
/// too and its activity shadows the outer one's; among equals, the most recently active.
/// Clients sitting in a `mockup-*` view are never picked. Returns `(client, session)`.
pub fn pick_client(clients: &str, agent: &str, hosts: &[String]) -> Option<(String, String)> {
    clients
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let client = fields.next()?;
            let session = fields.next()?;
            let activity: u64 = fields.next()?.trim().parse().ok()?;
            if session.starts_with(VIEW_PREFIX) {
                return None;
            }
            let depth = if session == agent {
                0
            } else {
                hosts.iter().position(|host| host == session)? + 1
            };
            Some((depth, activity, client, session))
        })
        .max_by_key(|(depth, activity, _, _)| (*depth, *activity))
        .map(|(_, _, client, session)| (client.to_owned(), session.to_owned()))
}

fn start(
    core: &TxCore,
    ui: &Ui,
    io: &mut VerbIo<'_>,
    id: &SessionId,
    view: &str,
    project: &Path,
) -> Result<i32, VerbError> {
    let project = std::path::absolute(project)
        .map_err(|error| VerbError::Failed(format!("{}: {error}", project.display())))?;
    let dir = project.join(SESSION_DIRS).join(id.as_str());
    let content = dir.join("content");
    let state = dir.join("state");
    for directory in [&content, &state] {
        std::fs::create_dir_all(directory)
            .map_err(|error| VerbError::Failed(format!("{}: {error}", directory.display())))?;
    }
    let dir_text = dir.to_string_lossy().into_owned();
    core.update_ext(id, PLUGIN_ID, |value| *value = json!({ "dir": dir_text }))?;
    ensure_view(core, ui, view, &dir)?;
    let line = json!({
        "view": view,
        "content_dir": content.to_string_lossy(),
        "state_dir": state.to_string_lossy(),
    });
    writeln!(io.stdout, "{line}").map_err(write_failed)?;
    Ok(0)
}

fn show(
    core: &TxCore,
    ui: &Ui,
    io: &mut VerbIo<'_>,
    session: &Session,
    view: &str,
) -> Result<i32, VerbError> {
    let Some(dir) = session_dir(session) else {
        let _ = writeln!(io.stderr, "txc mockup: run 'txc mockup start' first");
        return Ok(1);
    };
    ensure_view(core, ui, view, &dir)?;
    if !clients_on(ui, view).is_empty() {
        return Ok(0);
    }
    let hosts = hosts_of(ui, &session.name);
    let (_, clients) = ui.quiet(&[
        "list-clients",
        "-F",
        "#{client_name}\t#{client_session}\t#{client_activity}",
    ]);
    let Some((client, return_to)) = pick_client(&clients, &session.name, &hosts) else {
        writeln!(io.stdout, "mockup ready in view {view}").map_err(write_failed)?;
        return Ok(0);
    };
    ui.set_session_option(view, RETURN_OPTION, &return_to)
        .map_err(failed)?;
    ui.run(&["switch-client", "-c", &client, "-t", &format!("={view}")])
        .map_err(failed)?;
    Ok(0)
}

fn wait(io: &mut VerbIo<'_>, session: &Session) -> Result<i32, VerbError> {
    let Some(dir) = session_dir(session) else {
        let _ = writeln!(io.stderr, "txc mockup: run 'txc mockup start' first");
        return Ok(1);
    };
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before 1970")
        .as_millis();
    let path = dir.join("content").join(format!("waiting-{millis}.json"));
    std::fs::write(&path, "{}")
        .map_err(|error| VerbError::Failed(format!("{}: {error}", path.display())))?;
    Ok(0)
}

/// `txc mockup start|show|wait|stop`.
pub struct MockupVerb;

impl Verb for MockupVerb {
    fn name(&self) -> &'static str {
        "mockup"
    }

    fn usage(&self) -> &'static str {
        "mockup start --project DIR [--for S] | show [--for S] | wait [--for S] | stop [--for S]   ratatui mockups for brainstorming"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let Some(args) = parse_args(argv) else {
            let _ = writeln!(io.stderr, "{USAGE}");
            return Ok(2);
        };
        let Some(id) = resolve_agent(core, args.agent.as_deref())? else {
            let _ = writeln!(io.stderr, "txc mockup: no agent session (use --for)");
            return Ok(2);
        };
        let session = core.get_session(&id)?;
        let view = view_name(&session.name);
        let ui = Ui::from_core(core);
        match args.sub {
            Sub::Start(project) => start(core, &ui, io, &id, &view, &project),
            Sub::Show => show(core, &ui, io, &session, &view),
            Sub::Wait => wait(io, &session),
            Sub::Stop => {
                switch_back(&ui, &view);
                ui.kill_session(&view);
                Ok(0)
            }
        }
    }
}

/// The clients currently showing `view`.
fn clients_on(ui: &Ui, view: &str) -> Vec<String> {
    let (_, out) = ui.quiet(&[
        "list-clients",
        "-t",
        &format!("={view}"),
        "-F",
        "#{client_name}",
    ]);
    out.lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Switches every client on `view` back to where `show` came from, best effort.
fn switch_back(ui: &Ui, view: &str) {
    let Some(return_to) = ui.session_option(view, RETURN_OPTION) else {
        return;
    };
    if !ui.has_session(&return_to) {
        return;
    }
    for client in clients_on(ui, view) {
        ui.quiet(&[
            "switch-client",
            "-c",
            &client,
            "-t",
            &format!("={return_to}"),
        ]);
    }
}

/// `txc _mockup-view <session_dir> <view>`: the viewer process the view runs.
pub struct ViewVerb;

impl Verb for ViewVerb {
    fn name(&self) -> &'static str {
        VIEW_VERB
    }

    fn usage(&self) -> &'static str {
        "_mockup-view <session_dir> <view>   the viewer (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [dir, view] = argv else {
            let _ = writeln!(io.stderr, "usage: txc {VIEW_VERB} <session_dir> <view>");
            return Ok(2);
        };
        let ui = Ui::from_core(core);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            viewer::run(Path::new(dir), || switch_back(&ui, view))
        }));
        switch_back(&ui, view);
        match outcome {
            Ok(result) => result.map_err(|error| VerbError::Failed(format!("viewer: {error}")))?,
            Err(_) => return Err(VerbError::Failed("viewer panicked".to_owned())),
        }
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::pick_client;

    const AGENT: &str = "agent";

    fn hosts(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn picked(clients: &str, hosts: &[String]) -> Option<(String, String)> {
        pick_client(clients, AGENT, hosts)
    }

    #[test]
    fn direct_attach() {
        let clients = "/dev/ttys001\tagent\t100\n";
        assert_eq!(
            picked(clients, &[]),
            Some(("/dev/ttys001".to_owned(), "agent".to_owned()))
        );
    }

    #[test]
    fn via_home_view() {
        // The nested attach inside home's pane is itself a client of `agent`, and it is the
        // more recently active one; the outer client on `home` still wins.
        let clients = "/dev/ttys002\tagent\t101\n/dev/ttys001\thome\t100\n";
        assert_eq!(
            picked(clients, &hosts(&["home"])),
            Some(("/dev/ttys001".to_owned(), "home".to_owned()))
        );
    }

    #[test]
    fn most_active_wins() {
        let clients =
            "/dev/ttys001\tagent\t100\n/dev/ttys002\tagent\t200\n/dev/ttys003\tagent\t150\n";
        assert_eq!(
            picked(clients, &[]),
            Some(("/dev/ttys002".to_owned(), "agent".to_owned()))
        );
    }

    #[test]
    fn mockup_clients_excluded() {
        let clients = "/dev/ttys001\tagent\t100\n/dev/ttys002\tmockup-agent\t999\n";
        assert_eq!(
            picked(clients, &hosts(&["mockup-agent"])),
            Some(("/dev/ttys001".to_owned(), "agent".to_owned()))
        );
    }

    #[test]
    fn none() {
        assert_eq!(picked("", &[]), None);
        assert_eq!(
            picked("/dev/ttys001\tother\t100\n", &hosts(&["home"])),
            None
        );
    }
}
