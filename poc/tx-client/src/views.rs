//! Views: the non-LLM tmux sessions tx-client owns (the home base, editors, shells, the
//! timeline, tutor and mockup viewers). A view has no core record and never shows in `txc ls`;
//! its fields live in tmux session options only, so nothing goes stale when the server dies.
//! Core marks its own sessions with `@tx-id`, and nothing here kills or renames one of those.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use tx_core::{CoreError, SessionId, TxCore};

use crate::tmux::{Ui, UiError};

/// `1` on every view; `tmux.conf` and the key plugins test for it.
pub const VIEW_OPTION: &str = "@tx-view";
pub const KIND_OPTION: &str = "@tx-view-kind";
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
    Shell,
    Timeline,
    Tutor,
    Mockup,
}

impl ViewKind {
    pub const ALL: [ViewKind; 6] = [
        ViewKind::Home,
        ViewKind::Editor,
        ViewKind::Shell,
        ViewKind::Timeline,
        ViewKind::Tutor,
        ViewKind::Mockup,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ViewKind::Home => "home",
            ViewKind::Editor => "editor",
            ViewKind::Shell => "shell",
            ViewKind::Timeline => "timeline",
            ViewKind::Tutor => "tutor",
            ViewKind::Mockup => "mockup",
        }
    }

    /// Whether the picker's Enter may replace one of this view's shell panes with a nested
    /// attach: the home base and the tutor (its viewer pane) are built for that; a shell,
    /// editor, mockup or timeline view is the operator's own and gets the client switched.
    pub fn hosts_nested_attaches(self) -> bool {
        matches!(self, ViewKind::Home | ViewKind::Tutor)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown view kind {0:?} (expected home, editor, shell, timeline, tutor or mockup)")]
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
    pub parent_id: Option<SessionId>,
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
    pub parent_id: Option<&'a SessionId>,
    pub bound: Option<&'a str>,
}

#[derive(Debug, thiserror::Error)]
pub enum ViewError {
    #[error("'{0}' is an agent's session; stop it through core (txc kill)")]
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

/// `new-session -d` running `program arguments…` in the working directory, then the view's
/// options. The program and its arguments go to tmux as separate words, so no shell is in the
/// way.
pub fn open(ui: &Ui, request: &ViewRequest<'_>) -> Result<View, ViewError> {
    if ui.has_session(request.name) {
        return Err(ViewError::NameTaken(request.name.to_owned()));
    }
    // `-c` is format-expanded by tmux, so a literal `#` must be doubled.
    let working_directory = request
        .working_directory
        .to_string_lossy()
        .replace('#', "##");
    let mut create = vec![
        "new-session",
        "-d",
        "-s",
        request.name,
        "-c",
        &working_directory,
        request.program,
    ];
    create.extend(request.arguments.iter().map(String::as_str));
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

/// Every session marked `@tx-view 1` with a kind, in tmux's listing order.
pub fn list(ui: &Ui) -> Result<Vec<View>, ViewError> {
    let listing = ui.run(&["list-sessions", "-F", LIST_FORMAT])?;
    Ok(parse_views(&listing))
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
/// (checked against tmux through `refresh_session`). Returns the closed names.
pub fn close_orphans(ui: &Ui, core: &TxCore) -> Result<Vec<String>, ViewError> {
    let mut closed = Vec::new();
    for view in list(ui)? {
        let Some(parent) = &view.parent_id else {
            continue;
        };
        let orphaned = match core.refresh_session(parent) {
            Ok(refreshed) => refreshed.session().lifecycle.is_terminal(),
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

/// One `LIST_FORMAT` line per session; a session without `@tx-view 1` or without a known kind
/// is not a view and is skipped.
pub fn parse_views(listing: &str) -> Vec<View> {
    listing
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let [name, marker, kind, parent, bound, tags, created, path] = fields.as_slice()
            else {
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
            ViewKind::Mockup,
            ViewKind::Timeline,
        ] {
            assert!(!kind.hosts_nested_attaches(), "{kind}");
        }
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
        let ui = Ui::new(dir.path().join("tmux.sock"), conf);
        let parent: SessionId = "11111111-2222-3333-4444-555555555555".parse().unwrap();
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
            ui.run(&["display-message", "-p", "-t", "=sh1:", "#{pane_start_command}"])
                .unwrap()
                .trim(),
            "sleep 30"
        );

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
        assert!(matches!(close(&ui, "agent"), Err(ViewError::OwnedByCore(name)) if name == "agent"));
        assert!(matches!(rename(&ui, "agent", "x"), Err(ViewError::OwnedByCore(_))));
        assert!(matches!(close(&ui, "plain"), Err(ViewError::NotAView(_))));
        assert!(matches!(close(&ui, "nope"), Err(ViewError::NotFound(_))));
        assert!(matches!(rename(&ui, "sh1", "agent"), Err(ViewError::NameTaken(_))));
        assert_eq!(
            list(&ui).unwrap().iter().map(|view| view.name.as_str()).collect::<Vec<_>>(),
            ["sh1"]
        );

        rename(&ui, "sh1", "sh2").unwrap();
        assert!(get(&ui, "sh1").unwrap().is_none());
        assert_eq!(get(&ui, "sh2").unwrap().unwrap().kind, ViewKind::Shell);
        close(&ui, "sh2").unwrap();
        assert!(!ui.has_session("sh2"));
        assert!(ui.has_session("agent"));
        ui.quiet(&["kill-server"]);
    }
}
