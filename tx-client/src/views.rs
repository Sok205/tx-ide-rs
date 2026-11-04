//! Views: the non-LLM tmux sessions tx-client owns (the home base, editors, diffviews, shells,
//! the timeline, tutor and mockup viewers). A view has no core record and never shows in `tx ls`;
//! its fields live in tmux session options only, so nothing goes stale when the server dies.
//! Core marks its own sessions with `@tx-id`, and nothing here kills or renames one of those.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use tx_core::{AgentId, CoreError, TxCore};

use crate::tmux::{Ui, UiError};

/// `1` on every view; `tmux.conf` and the key plugins test for it.
pub const VIEW_OPTION: &str = "@tx-view";
pub const KIND_OPTION: &str = "@tx-view-kind";
/// `1` on the home view's floor pane: the pane the home plugin keeps its TUI in.
pub const HOME_FLOOR_OPTION: &str = "@tx-home-floor";
const PARENT_OPTION: &str = "@tx-view-parent";
const BOUND_OPTION: &str = "@tx-view-bound";
const TAGS_OPTION: &str = "@tx-view-tags";
const CREATED_OPTION: &str = "@tx-view-created";
/// What core sets on an agent's session.
const CORE_ID_OPTION: &str = "@tx-id";

const LIST_FORMAT: &str = "#{session_name}\t#{@tx-view}\t#{@tx-view-kind}\t#{@tx-view-parent}\t#{@tx-view-bound}\t#{@tx-view-tags}\t#{@tx-view-created}\t#{session_path}";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewKind {
    Home,
    Editor,
    /// nvim showing a diff artifact in diffview.
    Diff,
    /// nvim showing a tour artifact's stops as diagnostics and a quickfix list.
    Tour,
    Shell,
    Timeline,
    Tutor,
    Mockup,
}

impl ViewKind {
    pub const ALL: [ViewKind; 8] = [
        ViewKind::Home,
        ViewKind::Editor,
        ViewKind::Diff,
        ViewKind::Tour,
        ViewKind::Shell,
        ViewKind::Timeline,
        ViewKind::Tutor,
        ViewKind::Mockup,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ViewKind::Home => "home",
            ViewKind::Editor => "editor",
            ViewKind::Diff => "diff",
            ViewKind::Tour => "tour",
            ViewKind::Shell => "shell",
            ViewKind::Timeline => "timeline",
            ViewKind::Tutor => "tutor",
            ViewKind::Mockup => "mockup",
        }
    }

    /// Whether the picker's Enter may replace one of this view's shell panes with a nested
    /// attach: the home base and the tutor (its viewer pane) are built for that; a shell,
    /// editor, diff, tour, mockup or timeline view is the operator's own and gets the client
    /// switched.
    pub fn hosts_nested_attaches(self) -> bool {
        matches!(self, ViewKind::Home | ViewKind::Tutor)
    }
}

#[derive(Debug, thiserror::Error)]
#[error(
    "unknown view kind {0:?} (expected home, editor, diff, tour, shell, timeline, tutor or mockup)"
)]
pub struct InvalidViewKind(pub String);

impl FromStr for ViewKind {
    type Err = InvalidViewKind;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        ViewKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == text)
            .ok_or_else(|| InvalidViewKind(text.to_owned()))
    }
}

impl std::fmt::Display for ViewKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A non-LLM tmux session owned by tx-client.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    /// The tmux session name; unique on the server, shared with agents' tmux names.
    pub name: String,
    pub kind: ViewKind,
    pub working_directory: PathBuf,
    pub tags: Vec<String>,
    /// The agent it belongs to (tx id), if any; the view closes once that agent is gone.
    pub parent_id: Option<AgentId>,
    /// Owner-defined, e.g. an artifact id.
    pub bound: Option<String>,
    pub created_at: f64,
}

pub struct ViewRequest<'a> {
    pub name: &'a str,
    pub kind: ViewKind,
    pub program: &'a str,
    pub arguments: &'a [String],
    pub working_directory: &'a Path,
    pub tags: &'a [String],
    pub parent_id: Option<&'a AgentId>,
    pub bound: Option<&'a str>,
}

#[derive(Debug, thiserror::Error)]
pub enum ViewError {
    #[error("'{0}' is an agent's session; stop it through core (tx kill)")]
    OwnedByCore(String),
    #[error("'{0}' is not a view")]
    NotAView(String),
    #[error("no session named '{0}'")]
    NotFound(String),
    #[error("a session named '{0}' already exists")]
    NameTaken(String),
    #[error(transparent)]
    Tmux(#[from] UiError),
    #[error(transparent)]
    Core(#[from] CoreError),
}

/// The shell a view pane runs when nothing else is asked for.
pub fn default_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| "sh".to_owned())
}

fn exact_session(name: &str) -> String {
    format!("={name}")
}

/// A view-only target: the session must exist, carry no `@tx-id`, and be marked `@tx-view`.
fn guard(ui: &Ui, name: &str) -> Result<(), ViewError> {
    if !ui.has_session(name) {
        return Err(ViewError::NotFound(name.to_owned()));
    }
    if ui.session_option(name, CORE_ID_OPTION).is_some() {
        return Err(ViewError::OwnedByCore(name.to_owned()));
    }
    if !ui.is_view(name) {
        return Err(ViewError::NotAView(name.to_owned()));
    }
    Ok(())
}

/// What a view's program runs with: `arguments`, and for nvim a startup command that puts
/// `nvim_runtime` first on the runtimepath and sources its `plugin/tx.lua`. It is `-c`, run
/// after the user's init, not `--cmd`: lazy.nvim resets the runtimepath during init and would
/// drop an entry added before it.
fn program_arguments(program: &str, arguments: &[String], nvim_runtime: &Path) -> Vec<String> {
    let mut words = arguments.to_vec();
    if Path::new(program)
        .file_name()
        .is_some_and(|name| name == "nvim")
    {
        // `:set` ends a value at an unescaped space.
        let runtime = nvim_runtime
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace(' ', "\\ ");
        words.push("-c".to_owned());
        words.push(format!("set rtp^={runtime} | runtime plugin/tx.lua"));
    }
    words
}

/// A directory for tmux's `-c`, which is format-expanded: a literal `#` must be doubled.
fn tmux_directory(path: &Path) -> String {
    path.to_string_lossy().replace('#', "##")
}

/// A session name from a title: every byte outside `[A-Za-z0-9_-]` becomes `-`, at most 64
/// bytes, `artifact` when nothing is left.
pub fn sanitize(title: &str) -> String {
    let name: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .take(64)
        .collect();
    if name.trim_matches('-').is_empty() {
        "artifact".to_owned()
    } else {
        name
    }
}

/// `new-session -d` running `program arguments…` in the working directory, then the view's
/// options. The program and its arguments go to tmux as separate words, so no shell is in the
/// way; an nvim view also gets tx-client's nvim runtime (`program_arguments`).
pub fn open(ui: &Ui, request: &ViewRequest<'_>) -> Result<View, ViewError> {
    if ui.has_session(request.name) {
        return Err(ViewError::NameTaken(request.name.to_owned()));
    }
    let working_directory = tmux_directory(request.working_directory);
    let mut create = vec![
        "new-session",
        "-d",
        "-s",
        request.name,
        "-c",
        &working_directory,
        request.program,
    ];
    let arguments = program_arguments(request.program, request.arguments, ui.nvim_runtime());
    create.extend(arguments.iter().map(String::as_str));
    ui.run(&create)?;
    let created_at = tx_core::events::now();
    let tags = request.tags.join(",");
    let mut options = vec![
        (VIEW_OPTION, "1".to_owned()),
        (KIND_OPTION, request.kind.as_str().to_owned()),
        (CREATED_OPTION, created_at.to_string()),
    ];
    if let Some(parent) = request.parent_id {
        options.push((PARENT_OPTION, parent.to_string()));
    }
    if let Some(bound) = request.bound {
        options.push((BOUND_OPTION, bound.to_owned()));
    }
    if !tags.is_empty() {
        options.push((TAGS_OPTION, tags));
    }
    for (option, value) in &options {
        ui.set_session_option(request.name, option, value)?;
    }
    Ok(View {
        name: request.name.to_owned(),
        kind: request.kind,
        working_directory: request.working_directory.to_path_buf(),
        tags: request.tags.to_vec(),
        parent_id: request.parent_id.cloned(),
        bound: request.bound.map(str::to_owned),
        created_at,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitDirection {
    /// Side by side (`split-window -h`).
    Horizontal,
    /// Stacked (`split-window -v`).
    Vertical,
}

pub struct SplitRequest<'a> {
    /// The pane to split: a tmux target such as `=view:.{top-right}` or a `%id`.
    pub target: &'a str,
    pub direction: SplitDirection,
    /// `-l`: lines or columns, or a percentage such as `50%`; tmux halves the pane when unset.
    pub size: Option<&'a str>,
    pub program: &'a str,
    pub arguments: &'a [String],
    pub working_directory: &'a Path,
}

/// The `split-window` argv for `request`, printing the new pane's id.
fn split_arguments(request: &SplitRequest<'_>, nvim_runtime: &Path) -> Vec<String> {
    let direction = match request.direction {
        SplitDirection::Horizontal => "-h",
        SplitDirection::Vertical => "-v",
    };
    let mut words = vec!["split-window".to_owned(), direction.to_owned()];
    if let Some(size) = request.size {
        words.extend(["-l".to_owned(), size.to_owned()]);
    }
    words.extend([
        "-t".to_owned(),
        request.target.to_owned(),
        "-c".to_owned(),
        tmux_directory(request.working_directory),
        "-P".to_owned(),
        "-F".to_owned(),
        "#{pane_id}".to_owned(),
        request.program.to_owned(),
    ]);
    words.extend(program_arguments(
        request.program,
        request.arguments,
        nvim_runtime,
    ));
    words
}

/// `split-window` running `program arguments…` beside or under `target`, the way `open` runs
/// a view's first pane: separate words, no shell, and an nvim pane gets tx-client's runtime.
/// Returns the new pane's id. A single word with spaces still goes through tmux's shell.
pub fn split(ui: &Ui, request: &SplitRequest<'_>) -> Result<String, ViewError> {
    let words = split_arguments(request, ui.nvim_runtime());
    let arguments: Vec<&str> = words.iter().map(String::as_str).collect();
    Ok(ui.run(&arguments)?.trim().to_owned())
}

/// Every session marked `@tx-view 1` with a kind, in tmux's listing order; none when no server
/// runs yet (`open` starts it).
pub fn list(ui: &Ui) -> Result<Vec<View>, ViewError> {
    match ui.run(&["list-sessions", "-F", LIST_FORMAT]) {
        Ok(listing) => Ok(parse_views(&listing)),
        Err(error) if error.is_no_server() => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

pub fn get(ui: &Ui, name: &str) -> Result<Option<View>, ViewError> {
    Ok(list(ui)?.into_iter().find(|view| view.name == name))
}

/// `kill-session`; refuses an agent's session (`@tx-id`) and anything that is not a view.
pub fn close(ui: &Ui, name: &str) -> Result<(), ViewError> {
    guard(ui, name)?;
    ui.run(&["kill-session", "-t", &exact_session(name)])?;
    Ok(())
}

/// `rename-session`, under the same guard as `close`.
pub fn rename(ui: &Ui, name: &str, new_name: &str) -> Result<(), ViewError> {
    guard(ui, name)?;
    if ui.has_session(new_name) {
        return Err(ViewError::NameTaken(new_name.to_owned()));
    }
    ui.run(&["rename-session", "-t", &exact_session(name), new_name])?;
    Ok(())
}

/// Closes every view whose parent agent is gone: forgotten by core, or no longer running
/// (checked against tmux through `refresh_agent`). Returns the closed names.
pub fn close_orphans(ui: &Ui, core: &TxCore) -> Result<Vec<String>, ViewError> {
    let mut closed = Vec::new();
    for view in list(ui)? {
        let Some(parent) = &view.parent_id else {
            continue;
        };
        let orphaned = match core.refresh_agent(parent) {
            Ok(refreshed) => refreshed.agent().state == tx_core::LlmEngineState::Ended,
            Err(CoreError::NotFound(_)) => true,
            Err(error) => return Err(error.into()),
        };
        if orphaned {
            close(ui, &view.name)?;
            closed.push(view.name);
        }
    }
    Ok(closed)
}

/// A pane of the home view, as [`home_panes`] lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HomePane {
    pub id: String,
    /// It carries [`HOME_FLOOR_OPTION`].
    pub floor: bool,
    pub dead: bool,
    pub command: String,
}

/// Every pane of `session`, its first window's first pane first.
pub fn home_panes(ui: &Ui, session: &str) -> Result<Vec<HomePane>, UiError> {
    let format = format!(
        "#{{pane_id}}\t#{{{HOME_FLOOR_OPTION}}}\t#{{pane_dead}}\t#{{pane_current_command}}"
    );
    let listing = ui.run(&[
        "list-panes",
        "-s",
        "-t",
        &exact_session(session),
        "-F",
        &format,
    ])?;
    Ok(parse_home_panes(&listing))
}

fn parse_home_panes(listing: &str) -> Vec<HomePane> {
    listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(4, '\t');
            Some(HomePane {
                id: fields.next()?.to_owned(),
                floor: fields.next()? == "1",
                dead: fields.next()? == "1",
                command: fields.next()?.to_owned(),
            })
        })
        .collect()
}

/// One `LIST_FORMAT` line per session; a session without `@tx-view 1` or without a known kind
/// is not a view and is skipped.
pub fn parse_views(listing: &str) -> Vec<View> {
    listing
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let [name, marker, kind, parent, bound, tags, created, path] = fields.as_slice() else {
                return None;
            };
            if *marker != "1" {
                return None;
            }
            Some(View {
                name: (*name).to_owned(),
                kind: kind.parse().ok()?,
                working_directory: PathBuf::from(path),
                tags: tags
                    .split(',')
                    .filter(|tag| !tag.is_empty())
                    .map(str::to_owned)
                    .collect(),
                parent_id: (!parent.is_empty()).then(|| parent.parse().ok()).flatten(),
                bound: (!bound.is_empty()).then(|| (*bound).to_owned()),
                created_at: created.parse().unwrap_or_default(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_panes_are_parsed_in_order_with_their_mark() {
        assert_eq!(
            parse_home_panes("%0\t\t0\tzsh\n%4\t1\t1\ttx\nbogus\n"),
            [
                HomePane {
                    id: "%0".into(),
                    floor: false,
                    dead: false,
                    command: "zsh".into(),
                },
                HomePane {
                    id: "%4".into(),
                    floor: true,
                    dead: true,
                    command: "tx".into(),
                },
            ]
        );
    }

    #[test]
    fn kinds_round_trip_through_their_names() {
        for kind in ViewKind::ALL {
            assert_eq!(kind.as_str().parse::<ViewKind>().unwrap(), kind);
        }
        assert!("agent".parse::<ViewKind>().is_err());
        assert!(ViewKind::Home.hosts_nested_attaches());
        assert!(ViewKind::Tutor.hosts_nested_attaches());
        for kind in [
            ViewKind::Shell,
            ViewKind::Editor,
            ViewKind::Diff,
            ViewKind::Tour,
            ViewKind::Mockup,
            ViewKind::Timeline,
        ] {
            assert!(!kind.hosts_nested_attaches(), "{kind}");
        }
    }

    #[test]
    fn nvim_views_get_the_runtime_after_their_own_arguments() {
        let arguments = ["/a/plan.md".to_owned()];
        let runtime = Path::new("/d/tx ide/nvim");
        let expected = [
            "/a/plan.md",
            "-c",
            "set rtp^=/d/tx\\ ide/nvim | runtime plugin/tx.lua",
        ];
        assert_eq!(program_arguments("nvim", &arguments, runtime), expected);
        assert_eq!(
            program_arguments("/opt/homebrew/bin/nvim", &arguments, runtime),
            expected
        );
        for program in ["sh", "fake-nvim", "nvim-qt", "vim"] {
            assert_eq!(
                program_arguments(program, &arguments, runtime),
                arguments,
                "{program}"
            );
        }
    }

    #[test]
    fn titles_become_session_names() {
        assert_eq!(sanitize("plan.md"), "plan-md");
        assert_eq!(sanitize("My plan: v2"), "My-plan--v2");
        assert_eq!(sanitize("..."), "artifact");
        assert_eq!(sanitize(""), "artifact");
        assert_eq!(sanitize(&"x".repeat(100)).len(), 64);
        assert!(tx_core::validate_name(&sanitize("é/ü")).is_ok());
    }

    #[test]
    fn split_doubles_hashes_and_gives_nvim_the_runtime() {
        let arguments = ["plan.md".to_owned()];
        let request = SplitRequest {
            target: "%3",
            direction: SplitDirection::Vertical,
            size: Some("50%"),
            program: "nvim",
            arguments: &arguments,
            working_directory: Path::new("/w/proj #1"),
        };
        assert_eq!(
            split_arguments(&request, Path::new("/d/nvim")),
            [
                "split-window",
                "-v",
                "-l",
                "50%",
                "-t",
                "%3",
                "-c",
                "/w/proj ##1",
                "-P",
                "-F",
                "#{pane_id}",
                "nvim",
                "plan.md",
                "-c",
                "set rtp^=/d/nvim | runtime plugin/tx.lua",
            ]
        );
        let shell = SplitRequest {
            direction: SplitDirection::Horizontal,
            size: None,
            program: "zsh",
            arguments: &[],
            ..request
        };
        assert_eq!(
            split_arguments(&shell, Path::new("/d/nvim")),
            [
                "split-window",
                "-h",
                "-t",
                "%3",
                "-c",
                "/w/proj ##1",
                "-P",
                "-F",
                "#{pane_id}",
                "zsh",
            ]
        );
    }

    #[test]
    fn parse_views_reads_every_field_and_skips_non_views() {
        let listing = "home\t1\thome\t\t\t\t1700000000.5\t/Users/me\n\
                       agent\t\t\t\t\t\t\t/w\n\
                       legacy\t1\t\t\t\t\t\t/tmp\n\
                       sh1\t1\tshell\tabc-123\ta50727bb\tqa,review\t1700000001\t/tmp/repo\n\
                       short\t1\tshell\n";
        let views = parse_views(listing);
        assert_eq!(views.len(), 2);
        assert_eq!(
            views[0],
            View {
                name: "home".to_owned(),
                kind: ViewKind::Home,
                working_directory: PathBuf::from("/Users/me"),
                tags: vec![],
                parent_id: None,
                bound: None,
                created_at: 1700000000.5,
            }
        );
        assert_eq!(
            views[1],
            View {
                name: "sh1".to_owned(),
                kind: ViewKind::Shell,
                working_directory: PathBuf::from("/tmp/repo"),
                tags: vec!["qa".to_owned(), "review".to_owned()],
                parent_id: Some("abc-123".parse().unwrap()),
                bound: Some("a50727bb".to_owned()),
                created_at: 1700000001.0,
            }
        );
    }

    /// Against a throwaway server with no config, so the operator's tmux is never loaded.
    #[test]
    fn open_list_close_and_the_core_guard_on_a_scratch_server() {
        let dir = tempfile::Builder::new()
            .prefix("txviews")
            .tempdir_in("/tmp")
            .unwrap();
        let conf = dir.path().join("tmux.conf");
        std::fs::write(&conf, "").unwrap();
        let ui = Ui::new(dir.path().join("tmux.sock"), conf, dir.path().join("nvim"));
        let parent: AgentId = "11111111-2222-3333-4444-555555555555".parse().unwrap();
        // No server yet: no views, no session, and `open` below starts the server.
        match list(&ui) {
            Ok(views) => assert!(views.is_empty()),
            Err(ViewError::Tmux(UiError::Spawn(_))) => {
                eprintln!("tmux not available; skipping");
                return;
            }
            Err(error) => panic!("{error}"),
        }
        assert!(!ui.has_session("sh1"));
        let arguments = ["30".to_owned()];
        let tags = ["qa".to_owned(), "x".to_owned()];
        let request = ViewRequest {
            name: "sh1",
            kind: ViewKind::Shell,
            program: "sleep",
            arguments: &arguments,
            working_directory: Path::new("/tmp"),
            tags: &tags,
            parent_id: Some(&parent),
            bound: Some("bound-1"),
        };
        let opened = match open(&ui, &request) {
            Ok(view) => view,
            Err(ViewError::Tmux(UiError::Spawn(_))) => {
                eprintln!("tmux not available; skipping");
                return;
            }
            Err(error) => panic!("{error}"),
        };
        assert_eq!(opened.kind, ViewKind::Shell);
        assert!(matches!(open(&ui, &request), Err(ViewError::NameTaken(name)) if name == "sh1"));
        assert_eq!(
            ui.run(&[
                "display-message",
                "-p",
                "-t",
                "=sh1:",
                "#{pane_start_command}"
            ])
            .unwrap()
            .trim(),
            "sleep 30"
        );

        let hashed = dir.path().join("proj #S");
        std::fs::create_dir(&hashed).unwrap();
        let pane = split(
            &ui,
            &SplitRequest {
                target: "=sh1:",
                direction: SplitDirection::Horizontal,
                size: Some("30%"),
                program: "sleep",
                arguments: &arguments,
                working_directory: &hashed,
            },
        )
        .unwrap();
        assert!(pane.starts_with('%'), "{pane}");
        let started = ui
            .run(&[
                "display-message",
                "-p",
                "-t",
                &pane,
                "#{pane_start_command}\t#{pane_start_path}",
            ])
            .unwrap();
        assert_eq!(started.trim(), format!("sleep 30\t{}", hashed.display()));

        let listed = get(&ui, "sh1").unwrap().unwrap();
        assert_eq!(listed.parent_id.as_ref(), Some(&parent));
        assert_eq!(listed.bound.as_deref(), Some("bound-1"));
        assert_eq!(listed.tags, tags);
        assert_eq!(listed.working_directory, PathBuf::from("/tmp"));
        assert!((listed.created_at - opened.created_at).abs() < 0.001);
        assert!(ui.is_view("sh1"));
        assert!(!ui.is_home("sh1"));
        assert!(!ui.hosts_nested_attaches("sh1"));

        // An agent's session, as core marks it, and a plain session of the user's.
        ui.run(&["new-session", "-d", "-s", "agent", "sleep", "30"])
            .unwrap();
        ui.set_session_option("agent", CORE_ID_OPTION, parent.as_str())
            .unwrap();
        ui.run(&["new-session", "-d", "-s", "plain", "sleep", "30"])
            .unwrap();
        assert!(
            matches!(close(&ui, "agent"), Err(ViewError::OwnedByCore(name)) if name == "agent")
        );
        assert!(matches!(
            rename(&ui, "agent", "x"),
            Err(ViewError::OwnedByCore(_))
        ));
        assert!(matches!(close(&ui, "plain"), Err(ViewError::NotAView(_))));
        assert!(matches!(close(&ui, "nope"), Err(ViewError::NotFound(_))));
        assert!(matches!(
            rename(&ui, "sh1", "agent"),
            Err(ViewError::NameTaken(_))
        ));
        assert_eq!(
            list(&ui)
                .unwrap()
                .iter()
                .map(|view| view.name.as_str())
                .collect::<Vec<_>>(),
            ["sh1"]
        );

        rename(&ui, "sh1", "sh2").unwrap();
        assert!(get(&ui, "sh1").unwrap().is_none());
        assert_eq!(get(&ui, "sh2").unwrap().unwrap().kind, ViewKind::Shell);
        close(&ui, "sh2").unwrap();
        assert!(!ui.has_session("sh2"));
        assert!(ui.has_session("agent"));
        ui.quiet(&["kill-server"]);
        assert!(
            list(&ui).unwrap().is_empty(),
            "a stopped server lists no views"
        );
    }
}
