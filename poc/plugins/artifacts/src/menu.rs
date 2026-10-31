//! `_artifact-menu` / `_artifact-pick` / `_artifact-go`: the prefix+a seam, ported from main's
//! `verbs/artifact_menu.rs`.
//!
//! The key fires INSIDE the nested session (the bind forwards it one level down while the pane
//! hosts a `tmux` client), so `#{session_name}` is the session's name and `#{client_name}` the
//! nested client whose screen is the hosting pane. In an agent session `_artifact-menu` opens a
//! popup over the pane running `_artifact-pick`: the artifacts the session touched, its parent,
//! its live children. Enter (or a digit) re-hosts the pane on the chosen view (the agent keeps
//! running detached); f then a digit or Enter moves the terminal to the pane already showing it;
//! p then a digit or Enter nest-attaches it in a popup over the terminal.
//! In an artifact's view (a tx-client view bound to it) the key goes back to the controlling
//! session: the artifact's most recent live non-`user` toucher, else its creator, else the view's
//! parent agent; in another view, to its parent agent.

use tx_client::render::{
    DIM_FG_ANSI, FG_ANSI, LOCATION_W, RESET_FG, location_text, tag_ansi, tag_hex,
};
use tx_client::tmux::{Ui, shell_quote};
use tx_client::views::View;
use tx_core::{Agent, AgentId, ListFilter, LlmEngineState, TxCore, VerbError, VerbIo};

use crate::artifact::{Artifact, Kind, USER_ACTOR};
use crate::picker::{self, HINT, Item, Pick, row_label, visible_width};
use crate::render::{actor_label, updated_at};
use crate::service::ArtifactService;
use crate::tmux::{self as tm, DIM_FG_HEX, FG_HEX};
use crate::views::{self, Opener, Shown};

const NAME_FLOOR: usize = 12;
const NAME_CEILING: usize = 40;
/// The `(k) ` choice key drawn at the row's left: `(1)` up to nine rows, `(M-a)` past that.
const DIGIT_LABEL_WIDTH: usize = 4;
const META_LABEL_WIDTH: usize = 6;
/// Popup border plus one column of padding each side.
const POPUP_FRAME: usize = 4;
/// Header, rows, a blank line, the notice line, the hint, and the border rows.
const POPUP_ROWS_OVERHEAD: usize = 6;
/// Assumed client width when tmux cannot report one.
const DEFAULT_WIDTH: usize = 120;
const NAME_GAP: usize = 3;

pub const MENU_VERB: &str = "_artifact-menu";
pub const PICK_VERB: &str = "_artifact-pick";
pub const GO_VERB: &str = "_artifact-go";

/// What a pick does with the chosen view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Re-host the pane the key fired in on the view.
    Attach,
    /// Move the terminal to the pane already hosting the view.
    Focus,
    /// Nest-attach the view in a popup over the terminal; the panes stay as they are.
    Popup,
}

impl Mode {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "attach" => Some(Self::Attach),
            "focus" => Some(Self::Focus),
            "popup" => Some(Self::Popup),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Attach => "attach",
            Self::Focus => "focus",
            Self::Popup => "popup",
        }
    }
}

/// What a row stands for, as `_artifact-go` names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub kind: &'static str,
    pub id: String,
}

/// One picker line before layout: (n) NAME   LOCATION chips.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub location: String,
    pub chips: Vec<String>,
}

/// The picker's sections for one session, in display order, with a target per row.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Sections {
    pub sections: Vec<(&'static str, Vec<Row>)>,
    pub targets: Vec<Target>,
}

/// Pure: the ARTIFACTS the session touched (newest first; a kind other than document is the
/// row's first chip), its live PARENT, its live CHILDREN
/// (agents and unbound views; an artifact's view is a row under its artifact). `views` are the
/// tx-client views; `live` the running core records. `locate` gives a tmux session's LOCATION
/// cell, `name_of` a touch author's name.
pub fn sections(
    record: &Agent,
    touched: &[Artifact],
    live: &[Agent],
    views: &[View],
    locate: impl Fn(&str) -> String,
    name_of: impl Fn(&str) -> String,
) -> Sections {
    let mut touched: Vec<&Artifact> = touched.iter().collect();
    touched.sort_by(|a, b| updated_at(b).total_cmp(&updated_at(a)));
    let mut out = Sections::default();
    let mut rows = Vec::new();
    for artifact in touched {
        let bound_view = views
            .iter()
            .find(|view| view.bound.as_deref() == Some(artifact.id.as_str()))
            .map(|view| view.name.as_str());
        let mut chips = Vec::new();
        if !matches!(artifact.kind, Kind::Document { .. }) {
            chips.push(artifact.kind.name().to_owned());
        }
        for touch in artifact.history() {
            let author = name_of(&touch.session_id);
            if !chips.contains(&author) {
                chips.push(author);
            }
        }
        rows.push(Row {
            name: artifact.display_name().to_owned(),
            location: bound_view.map_or_else(|| "—".to_owned(), &locate),
            chips,
        });
        out.targets.push(Target {
            kind: "artifact",
            id: artifact.id.clone(),
        });
    }
    if !rows.is_empty() {
        out.sections.push(("ARTIFACTS", std::mem::take(&mut rows)));
    }
    let session_row = |session: &Agent| {
        (
            Row {
                name: session.name.clone(),
                location: locate(&session.name),
                chips: session.tags.clone(),
            },
            Target {
                kind: "session",
                id: session.id.to_string(),
            },
        )
    };
    let parent: Vec<(Row, Target)> = record
        .parent_id
        .as_ref()
        .and_then(|parent| live.iter().find(|candidate| &candidate.id == parent))
        .map(session_row)
        .into_iter()
        .collect();
    let child_agents = live
        .iter()
        .filter(|candidate| candidate.parent_id.as_ref() == Some(&record.id))
        .map(session_row);
    let child_views = views
        .iter()
        .filter(|view| view.parent_id.as_ref() == Some(&record.id) && view.bound.is_none())
        .map(|view| {
            (
                Row {
                    name: view.name.clone(),
                    location: locate(&view.name),
                    chips: view.tags.clone(),
                },
                Target {
                    kind: "view",
                    id: view.name.clone(),
                },
            )
        });
    let children: Vec<(Row, Target)> = child_agents.chain(child_views).collect();
    for (title, related) in [("PARENT", parent), ("CHILDREN", children)] {
        if related.is_empty() {
            continue;
        }
        let (section, targets): (Vec<Row>, Vec<Target>) = related.into_iter().unzip();
        out.targets.extend(targets);
        out.sections.push((title, section));
    }
    out
}

/// The picker's content laid out for a client's width; `names` and `targets` are per row.
struct Listing {
    title: String,
    header: String,
    items: Vec<Item>,
    names: Vec<String>,
    targets: Vec<Target>,
}

pub struct Nav<'a> {
    pub core: &'a TxCore,
    pub ui: Ui,
    pub artifacts: ArtifactService,
    pub editor: &'a str,
}

fn failed(error: impl std::fmt::Display) -> VerbError {
    VerbError::Failed(error.to_string())
}

impl Nav<'_> {
    /// The record of the session the key fired in; `None` is reported on the status line.
    fn record(&self, session: &str, client: &str) -> Option<Agent> {
        let record = self
            .core
            .resolve(session)
            .and_then(|id| self.core.get_agent(&id))
            .ok();
        if record.is_none() {
            self.ui.display_status(client, "prefix+a: not a tx session");
        }
        record
    }

    /// Running records, after a refresh so a vanished session does not show (best effort).
    fn live(&self) -> Vec<Agent> {
        let _ = self.core.refresh_all();
        let ids = self
            .core
            .list_agents(&ListFilter::new().running())
            .unwrap_or_default();
        ids.iter()
            .filter_map(|id| self.core.get_agent(id).ok())
            .collect()
    }

    fn views(&self) -> Result<Vec<View>, VerbError> {
        tx_client::views::list(&self.ui).map_err(failed)
    }

    fn name_of(&self, session_id: &str) -> String {
        let names = session_id
            .parse::<AgentId>()
            .ok()
            .and_then(|id| self.core.get_agent(&id).ok())
            .map(|session| (session_id.to_owned(), session.name))
            .into_iter()
            .collect();
        actor_label(session_id, &names)
    }

    pub fn menu(&self, session: &str, client: &str) -> Result<i32, VerbError> {
        if let Some(view) = tx_client::views::get(&self.ui, session).map_err(failed)? {
            let parent = view.parent_id.as_ref().map(ToString::to_string);
            let back = match &view.bound {
                Some(artifact_id) => self.controlling_of(artifact_id)?.or(parent),
                None => parent,
            };
            return self.back_to(session, client, back);
        }
        let Some(record) = self.record(session, client) else {
            return Ok(1);
        };
        let listing = self.listing(&record, client)?;
        let content = listing
            .items
            .iter()
            .map(|item| visible_width(item.text()))
            .chain([visible_width(&listing.header), HINT.chars().count()])
            .max()
            .unwrap_or(0);
        let width = (content + POPUP_FRAME).min(self.client_width(client));
        let height = listing.items.len().max(1) + POPUP_ROWS_OVERHEAD;
        self.ui
            .display_popup(
                client,
                &width.to_string(),
                &height.to_string(),
                &listing.title,
                &self.txc_command(&[PICK_VERB, session, client])?,
            )
            .map_err(failed)?;
        Ok(0)
    }

    /// From a view: to the session it goes back to, or a notice when there is none.
    fn back_to(&self, session: &str, client: &str, back: Option<String>) -> Result<i32, VerbError> {
        match back {
            Some(back) => self.go_back(session, client, &back),
            None => {
                self.ui
                    .display_status(client, "prefix+a: nothing to go back to");
                Ok(1)
            }
        }
    }

    fn controlling_of(&self, artifact_id: &str) -> Result<Option<String>, VerbError> {
        self.artifacts
            .require(artifact_id)
            .map(|artifact| self.controlling_session(&artifact))
            .map_err(failed)
    }

    /// Inside the popup: run the list (a focus on a view no pane shows is declined with a
    /// notice), then hand the choice to a background `_artifact-go` so the popup is gone before
    /// the pane it sits on is re-hosted.
    pub fn pick(&self, session: &str, client: &str) -> Result<i32, VerbError> {
        let Some(record) = self.record(session, client) else {
            return Ok(1);
        };
        let listing = self.listing(&record, client)?;
        let decline = |pick: Pick| match pick {
            Pick::Attach(_) | Pick::Popup(_) => None,
            Pick::Focus(index) => match self.hosting_session(&listing.targets[index]) {
                Ok(hosting) => {
                    let hosted =
                        hosting.is_some_and(|name| self.ui.pane_for_session(&name, "").is_some());
                    (!hosted).then(|| format!("{} is not attached anywhere", listing.names[index]))
                }
                Err(error) => Some(error.to_string()),
            },
        };
        let chosen = picker::pick(&listing.header, &listing.items, decline)
            .map_err(|error| failed(format!("picker: {error}")))?;
        let Some(chosen) = chosen else {
            return Ok(0);
        };
        let (mode, index) = match chosen {
            Pick::Attach(index) => (Mode::Attach, index),
            Pick::Focus(index) => (Mode::Focus, index),
            Pick::Popup(index) => (Mode::Popup, index),
        };
        let target = &listing.targets[index];
        let command = self.txc_command(&[
            GO_VERB,
            session,
            client,
            mode.name(),
            target.kind,
            &target.id,
        ])?;
        tm::run_shell_background(&self.ui, &format!("{command} || true")).map_err(failed)?;
        Ok(0)
    }

    fn client_width(&self, client: &str) -> usize {
        tm::client_width(&self.ui, client).unwrap_or(DEFAULT_WIDTH)
    }

    fn listing(&self, record: &Agent, client: &str) -> Result<Listing, VerbError> {
        let touched = self.artifacts.artifacts_for_session(record.id.as_str());
        let live = self.live();
        let views = self.views()?;
        let Sections { sections, targets } = sections(
            record,
            &touched,
            &live,
            &views,
            |name| location_text(&self.ui.attached_to(name)),
            |id| self.name_of(id),
        );
        let all_rows: Vec<&Row> = sections.iter().flat_map(|(_, rows)| rows).collect();
        let layout = Layout::fit(
            &all_rows,
            self.client_width(client).saturating_sub(POPUP_FRAME),
        );
        let names = all_rows.iter().map(|row| row.name.clone()).collect();
        let mut items = Vec::new();
        let mut index = 0;
        for (title, rows) in &sections {
            items.push(Item::Section((*title).to_owned()));
            for row in rows {
                items.push(Item::Row(row.render(index, &layout)));
                index += 1;
            }
        }
        Ok(Listing {
            title: format!(" {} ", record.name),
            header: layout.header(),
            items,
            names,
            targets,
        })
    }

    /// The tmux session a row's view runs in: the agent's, the view itself, or the live view
    /// bound to the artifact (`None` when the artifact has no live view).
    fn hosting_session(&self, target: &Target) -> Result<Option<String>, VerbError> {
        Ok(match target.kind {
            "session" => target
                .id
                .parse::<AgentId>()
                .ok()
                .and_then(|id| self.core.get_agent(&id).ok())
                .map(|session| session.name),
            "view" => Some(target.id.clone()),
            _ => self.live_view(&target.id)?.map(|shown| shown.name),
        })
    }

    fn live_view(&self, artifact_id: &str) -> Result<Option<Shown>, VerbError> {
        views::live_view(&self.ui, artifact_id)
    }

    /// `<txc> <cli_arguments> argv…`, shell-quoted: `run-shell` and the popup run in the
    /// server's environment, which knows neither the directories nor the socket.
    fn txc_command(&self, argv: &[&str]) -> Result<String, VerbError> {
        let exe =
            std::env::current_exe().map_err(|error| failed(format!("current exe: {error}")))?;
        let mut words = vec![shell_quote(&exe.to_string_lossy())];
        words.extend(
            self.core
                .config()
                .cli_arguments()
                .iter()
                .map(|word| shell_quote(word)),
        );
        words.extend(argv.iter().map(|word| shell_quote(word)));
        Ok(words.join(" "))
    }

    pub fn go_artifact(
        &self,
        session: &str,
        client: &str,
        mode: Mode,
        artifact_id: &str,
    ) -> Result<i32, VerbError> {
        let Some(record) = self.record(session, client) else {
            return Ok(1);
        };
        let shown = match self.live_view(artifact_id)? {
            Some(shown) => shown,
            None => {
                let artifact = self.artifacts.require(artifact_id).map_err(failed)?;
                if let Kind::Other { kind, .. } = &artifact.kind {
                    let Some(shown) =
                        self.open_by_convention(client, kind, artifact_id, &record.tags)?
                    else {
                        return Ok(1);
                    };
                    return self.show(session, client, mode, &shown);
                }
                let opener = Opener {
                    id: Some(record.id.clone()),
                    tags: record.tags.clone(),
                };
                views::open(
                    &self.ui,
                    &self.artifacts,
                    &artifact,
                    &opener,
                    None,
                    None,
                    self.editor,
                )?
                .into()
            }
        };
        self.show(session, client, mode, &shown)
    }

    /// Open-by-convention, as `artifact open` does: runs `txc <kind> open <id> [--tag …]`
    /// in-process, then finds the view it bound to the artifact. A missing opener or a failed
    /// open goes to the client's status line (`None`).
    fn open_by_convention(
        &self,
        client: &str,
        kind: &str,
        artifact_id: &str,
        tags: &[String],
    ) -> Result<Option<Shown>, VerbError> {
        let Some(verb) = self.core.verb(kind) else {
            self.ui.display_status(
                client,
                &format!("prefix+a: no opener for kind `{kind}` (expected verb `txc {kind} open`)"),
            );
            return Ok(None);
        };
        let mut argv = vec!["open".to_owned(), artifact_id.to_owned()];
        if !tags.is_empty() {
            argv.extend(["--tag".to_owned(), tags.join(",")]);
        }
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let mut io = VerbIo {
            stdin: &mut std::io::empty(),
            stdout: &mut stdout,
            stderr: &mut stderr,
        };
        let code = match verb.run(self.core, &mut io, &argv) {
            Ok(code) => code,
            Err(error) => {
                self.ui
                    .display_status(client, &format!("prefix+a: {kind} open: {error}"));
                return Ok(None);
            }
        };
        if code != 0 {
            let reason = String::from_utf8_lossy(&stderr);
            self.ui
                .display_status(client, &format!("prefix+a: {kind} open: {}", reason.trim()));
            return Ok(None);
        }
        self.live_view(artifact_id)
    }

    pub fn go_session(
        &self,
        session: &str,
        client: &str,
        mode: Mode,
        target_id: &str,
    ) -> Result<i32, VerbError> {
        let Some(target) = self.running(client, target_id) else {
            return Ok(1);
        };
        self.show(session, client, mode, &target.into())
    }

    pub fn go_view(
        &self,
        session: &str,
        client: &str,
        mode: Mode,
        name: &str,
    ) -> Result<i32, VerbError> {
        let Some(view) = tx_client::views::get(&self.ui, name).map_err(failed)? else {
            self.ui
                .display_status(client, &format!("prefix+a: {name} has ended"));
            return Ok(1);
        };
        self.show(session, client, mode, &view.into())
    }

    /// The target record when it runs and its tmux session exists; otherwise reported.
    fn running(&self, client: &str, target_id: &str) -> Option<Agent> {
        let target = target_id
            .parse::<AgentId>()
            .ok()
            .and_then(|id| self.core.get_agent(&id).ok());
        let Some(target) = target else {
            self.ui
                .display_status(client, &format!("prefix+a: session {target_id} not found"));
            return None;
        };
        if target.state == LlmEngineState::Ended || !self.ui.has_session(&target.name) {
            self.ui
                .display_status(client, &format!("prefix+a: {} has ended", target.name));
            return None;
        }
        Some(target)
    }

    /// The way back from a view: to the pane still showing the controlling session when this
    /// terminal can reach one, else re-host this pane on it.
    fn go_back(&self, session: &str, client: &str, target_id: &str) -> Result<i32, VerbError> {
        let Some(target) = self.running(client, target_id) else {
            return Ok(1);
        };
        if tm::focus_hosting_pane(&self.ui, client, &target.name).is_ok() {
            return Ok(0);
        }
        self.rehost(session, client, &target.into())
    }

    fn show(
        &self,
        session: &str,
        client: &str,
        mode: Mode,
        target: &Shown,
    ) -> Result<i32, VerbError> {
        match mode {
            Mode::Focus => match tm::focus_hosting_pane(&self.ui, client, &target.name) {
                Ok(()) => Ok(0),
                Err(reason) => {
                    self.ui
                        .display_status(client, &format!("prefix+a: {}: {reason}", target.name));
                    Ok(1)
                }
            },
            Mode::Popup => {
                let outer = tm::outer_client(&self.ui, client)
                    .map_or_else(|| client.to_owned(), |(outer, _)| outer);
                let prefix =
                    tm::global_option(&self.ui, "prefix").unwrap_or_else(|| "C-b".to_owned());
                let owner = self
                    .core
                    .resolve(session)
                    .and_then(|id| self.core.get_agent(&id));
                self.ui
                    .popup_attach(
                        &outer,
                        &popup_title(target, owner.ok().as_ref()),
                        &popup_footer(&prefix),
                        &target.name,
                    )
                    .map_err(failed)?;
                Ok(0)
            }
            Mode::Attach => self.rehost(session, client, target),
        }
    }

    /// Re-host the pane the firing client paints into on `target` (`respawn-pane -k` onto a
    /// nested attach; the session it showed keeps running detached).
    fn rehost(&self, session: &str, client: &str, target: &Shown) -> Result<i32, VerbError> {
        if session == target.name {
            self.ui
                .display_status(client, &format!("prefix+a: already in {}", target.name));
            return Ok(0);
        }
        let Some(pane) = tm::pane_hosting_client(&self.ui, client) else {
            self.ui
                .display_status(client, "prefix+a: this session is not hosted in a pane");
            return Ok(1);
        };
        self.ui
            .respawn_pane(&pane, &self.ui.nest_attach_command(&target.name))
            .map_err(failed)?;
        Ok(0)
    }

    /// The artifact's most recent live non-`user` toucher, else its creator (`None` when the
    /// creator was the bare terminal).
    fn controlling_session(&self, artifact: &Artifact) -> Option<String> {
        let history = artifact.history();
        for touch in history.iter().rev() {
            if touch.session_id == USER_ACTOR {
                continue;
            }
            let running = touch
                .session_id
                .parse::<AgentId>()
                .ok()
                .and_then(|id| self.core.get_agent(&id).ok())
                .is_some_and(|session| session.state != LlmEngineState::Ended);
            if running {
                return Some(touch.session_id.clone());
            }
        }
        let creator = &history[0].session_id;
        (creator != USER_ACTOR).then(|| creator.clone())
    }
}

/// The popup's title: the view's name, its tags as chips, then `◂ owner`.
fn popup_title(view: &Shown, owner: Option<&Agent>) -> String {
    let mut title = format!(" {} ", view.name);
    for tag in &view.tags {
        title.push_str(&format!("#[fg={},nobold][{tag}]#[default] ", tag_hex(tag)));
    }
    if let Some(owner) = owner {
        title.push_str(&format!(
            "#[fg={DIM_FG_HEX},nobold]◂ {}#[default] ",
            owner.name
        ));
    }
    title
}

/// The popup's bottom line: how to leave it.
fn popup_footer(prefix: &str) -> String {
    format!(
        " #[fg={FG_HEX}]{prefix} d#[fg={DIM_FG_HEX}] close, the view keeps running   \
         #[fg={FG_HEX}]:q#[fg={DIM_FG_HEX}] in nvim ends the view"
    )
}

impl Row {
    fn chips_width(&self) -> usize {
        self.chips.iter().map(|chip| chip.chars().count() + 3).sum()
    }

    /// The picker's SGR styling; chips past the layout's chip budget are dropped.
    fn render(&self, index: usize, layout: &Layout) -> String {
        let name = pad(&truncate(&self.name, layout.namew), layout.namew);
        let mut line = format!(
            "{DIM_FG_ANSI}{:<label_width$}{FG_ANSI}{name}{RESET_FG}   ",
            row_label(index),
            label_width = layout.label_width
        );
        if layout.location {
            line.push_str(&format!(
                "{DIM_FG_ANSI}{}{RESET_FG} ",
                pad(&self.location, LOCATION_W)
            ));
        }
        let mut used = 0;
        for chip in &self.chips {
            used += chip.chars().count() + 3;
            if used > layout.chips {
                break;
            }
            line.push_str(&format!(" {}[{chip}]{RESET_FG}", tag_ansi(chip)));
        }
        line
    }
}

/// Which picker columns fit: the number and NAME always, LOCATION when there is room, chips in
/// whatever is left.
struct Layout {
    label_width: usize,
    namew: usize,
    location: bool,
    chips: usize,
}

impl Layout {
    fn fit(rows: &[&Row], width: usize) -> Self {
        let longest = rows
            .iter()
            .map(|row| row.name.chars().count())
            .max()
            .unwrap_or(0);
        let chips_wanted = rows.iter().map(|row| row.chips_width()).max().unwrap_or(0);
        let label_width = if rows.len() > 9 {
            META_LABEL_WIDTH
        } else {
            DIGIT_LABEL_WIDTH
        };
        let fixed = label_width + NAME_GAP;
        let mut namew = longest.clamp(NAME_FLOOR, NAME_CEILING);
        let spare = |namew: usize| width.saturating_sub(namew + fixed);
        if spare(namew) < chips_wanted {
            namew = NAME_FLOOR.max(width.saturating_sub(fixed + chips_wanted).min(namew));
        }
        let mut left = spare(namew);
        let mut chips = chips_wanted.min(left);
        left -= chips;
        let location = left > LOCATION_W;
        if location {
            left -= LOCATION_W + 1;
        }
        chips += left;
        Self {
            label_width,
            namew,
            location,
            chips,
        }
    }

    fn header(&self) -> String {
        let mut line = format!(
            "{:<label_width$}{:<namew$}   ",
            "",
            "NAME",
            label_width = self.label_width,
            namew = self.namew
        );
        if self.location {
            line.push_str(&format!("{:<LOCATION_W$} ", "LOCATION"));
        }
        line.push_str(" TAGS");
        line
    }
}

fn pad(text: &str, width: usize) -> String {
    let fill = width.saturating_sub(text.chars().count());
    format!("{text}{}", " ".repeat(fill))
}

fn truncate(text: &str, width: usize) -> String {
    let mut characters: Vec<char> = text.chars().collect();
    if characters.len() <= width {
        return text.to_owned();
    }
    characters.truncate(width - 1);
    characters.push('…');
    characters.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn session(id: &str, name: &str, extra: Value) -> Agent {
        let mut record = json!({
            "version": 2, "id": id, "name": name, "engine": "claude", "working_directory": "/r",
            "roles": [], "state": "idle", "created_at": 1.0, "last_modified_at": 1.0,
            "archived_at": null, "tags": [], "parent_id": null,
        });
        for (key, value) in extra.as_object().unwrap() {
            record[key] = value.clone();
        }
        Agent::from_json(&record.to_string()).unwrap()
    }

    fn artifact(id: &str, title: &str, at: f64, authors: &[&str]) -> Artifact {
        let history: Vec<Value> = authors
            .iter()
            .enumerate()
            .map(|(rev, author)| json!({"session_id": author, "at": at, "rev": rev, "changes": null}))
            .collect();
        Artifact::from_value(&json!({
            "artifact_schema_version": 2, "id": id, "title": title, "filename": "f.md",
            "created_at": at, "group": null, "history": history,
        }))
        .unwrap()
    }

    fn view(name: &str, parent: Option<&str>, bound: Option<&str>) -> View {
        View {
            name: name.to_owned(),
            kind: tx_client::views::ViewKind::Editor,
            working_directory: "/r".into(),
            tags: vec!["t".to_owned()],
            parent_id: parent.map(|parent| parent.parse().unwrap()),
            bound: bound.map(str::to_owned),
            created_at: 1.0,
        }
    }

    #[test]
    fn the_listing_holds_what_the_session_touched_its_parent_and_its_children() {
        let record = session("a1", "agent", json!({"parent_id": "p1"}));
        let live = vec![
            record.clone(),
            session("p1", "boss", json!({"tags": ["x"]})),
            session("c1", "worker", json!({"parent_id": "a1"})),
        ];
        let views = [
            view("plan-md", Some("a1"), Some("art-new")),
            view("shell", Some("a1"), None),
            view("elsewhere", Some("p1"), None),
        ];
        let touched = [
            artifact("art-old", "Old", 10.0, &["a1"]),
            artifact("art-new", "New", 20.0, &["a1", "user", "a1"]),
            artifact("art-shut", "Shut", 5.0, &["a1"]),
        ];
        let out = sections(
            &record,
            &touched,
            &live,
            &views,
            |name| format!("at-{name}"),
            |id| id.to_owned(),
        );
        let titles: Vec<&str> = out.sections.iter().map(|(title, _)| *title).collect();
        assert_eq!(titles, ["ARTIFACTS", "PARENT", "CHILDREN"]);
        let artifacts = &out.sections[0].1;
        assert_eq!(artifacts[0].name, "New");
        assert_eq!(artifacts[0].location, "at-plan-md");
        assert_eq!(artifacts[0].chips, ["a1", "user"]);
        assert_eq!(artifacts[1].location, "—");
        assert_eq!(artifacts[2].location, "—");
        assert_eq!(out.sections[1].1[0].name, "boss");
        let children: Vec<&str> = out.sections[2].1.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            children,
            ["worker", "shell"],
            "bound views are not child rows"
        );
        assert_eq!(out.sections[2].1[1].chips, ["t"]);
        let kinds: Vec<(&str, &str)> = out
            .targets
            .iter()
            .map(|t| (t.kind, t.id.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("artifact", "art-new"),
                ("artifact", "art-old"),
                ("artifact", "art-shut"),
                ("session", "p1"),
                ("session", "c1"),
                ("view", "shell"),
            ]
        );
    }

    #[test]
    fn nothing_touched_and_no_relatives_is_empty() {
        let record = session("a1", "agent", json!({}));
        let out = sections(
            &record,
            &[],
            std::slice::from_ref(&record),
            &[],
            |_| String::new(),
            str::to_owned,
        );
        assert_eq!(out, Sections::default());
    }

    #[test]
    fn rows_fit_the_width() {
        let row = Row {
            name: "a-very-long-artifact-title-that-goes-on".into(),
            location: "home[1]".into(),
            chips: vec!["worker".into()],
        };
        let layout = Layout::fit(&[&row], 40);
        assert!(!layout.location);
        assert!(visible_width(&row.render(0, &layout)) <= 40);
        let wide = Layout::fit(&[&row], 120);
        assert!(wide.location);
        assert!(wide.header().contains("LOCATION"));
    }

    #[test]
    fn modes_round_trip() {
        for mode in [Mode::Attach, Mode::Focus, Mode::Popup] {
            assert_eq!(Mode::parse(mode.name()), Some(mode));
        }
        assert_eq!(Mode::parse("x"), None);
    }
}
