//! `_artifact-menu` / `_artifact-pick` / `_artifact-go`: the prefix+a seam, ported from main's
//! `verbs/artifact_menu.rs`.
//!
//! The key fires INSIDE the nested session (the bind forwards it one level down while the pane
//! hosts a `tmux` client), so `#{session_name}` is the session's name and `#{client_name}` the
//! nested client whose screen is the hosting pane. In an agent session `_artifact-menu` opens a
//! popup over the pane running `_artifact-pick`: the artifacts the session owns, its parent, its
//! live children. Enter (or a digit) re-hosts the pane on the chosen view (the agent keeps
//! running detached); f then a digit or Enter moves the terminal to the pane already showing it;
//! p then a digit or Enter nest-attaches it in a popup over the terminal; d asks about the
//! cursor's row and y deletes it (a session is stopped, an artifact archived and its live view
//! stopped) and the list redraws without it.
//! In an artifact's view the key goes back to the artifact's owner while it runs; in another
//! companion, to its `parent`.

use tx_core::{Launch, Lifecycle, ListFilter, Session, SessionId, TxCore, VerbError};
use tx_ui::editor::Terminal;
use tx_ui::popup as tm;
use tx_ui::render::{
    DIM_FG_ANSI, FG_ANSI, LOCATION_W, RESET_FG, location_text, pad, tag_ansi, trunc,
};
use tx_ui::tmux::Ui;

use crate::artifact::{Artifact, USER_ACTOR};
use crate::grouping::bound_artifact;
use crate::kinds::describe;
use crate::picker::{self, HINT, Item, Pick, row_label, visible_width};
use crate::render::actor_label;
use crate::service::ArtifactService;
use crate::views::{self, Opener};

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

/// Pure: the ARTIFACTS the session owns (newest first), its live PARENT, its live CHILDREN
/// (artifact views excluded: they are rows under their artifact). `locate` gives a session's
/// LOCATION cell, `name_of` an owner's name.
pub fn sections(
    record: &Session,
    owned: &[Artifact],
    live: &[Session],
    locate: impl Fn(&Session) -> String,
    name_of: impl Fn(&str) -> String,
) -> Sections {
    let mut owned: Vec<&Artifact> = owned.iter().collect();
    owned.sort_by(|a, b| b.modified_secs().total_cmp(&a.modified_secs()));
    let mut out = Sections::default();
    let mut rows = Vec::new();
    for artifact in owned {
        let view = live
            .iter()
            .find(|candidate| bound_artifact(candidate).as_deref() == Some(artifact.id.as_str()));
        let chips = vec![name_of(&artifact.owner), describe(&artifact.kind)];
        rows.push(Row {
            name: artifact.title.clone(),
            location: view.map_or_else(|| "—".to_owned(), &locate),
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
    let parent = record
        .parent
        .as_ref()
        .and_then(|parent| live.iter().find(|candidate| &candidate.id == parent));
    let children: Vec<&Session> = live
        .iter()
        .filter(|candidate| {
            candidate.parent.as_ref() == Some(&record.id) && bound_artifact(candidate).is_none()
        })
        .collect();
    for (title, related) in [
        ("PARENT", parent.into_iter().collect::<Vec<_>>()),
        ("CHILDREN", children),
    ] {
        if related.is_empty() {
            continue;
        }
        for session in related {
            rows.push(Row {
                name: session.name.clone(),
                location: locate(session),
                chips: session.tags.clone(),
            });
            out.targets.push(Target {
                kind: "session",
                id: session.id.to_string(),
            });
        }
        out.sections.push((title, std::mem::take(&mut rows)));
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

impl Nav<'_> {
    /// The record of the session the key fired in; `None` is reported on the status line.
    fn record(&self, session: &str, client: &str) -> Option<Session> {
        let record = self.core.resolve_session(session).ok();
        if record.is_none() {
            self.ui.display_status(client, "prefix+a: not a tx session");
        }
        record
    }

    /// Running records, after a refresh so a vanished session does not show (best effort).
    fn live(&self) -> Vec<Session> {
        let _ = self.core.refresh_all();
        self.core
            .sessions(&ListFilter::new().lifecycle(Lifecycle::Running))
            .unwrap_or_default()
    }

    fn name_of(&self, session_id: &str) -> String {
        let names = session_id
            .parse::<SessionId>()
            .ok()
            .and_then(|id| self.core.get_session(&id).ok())
            .map(|session| (session_id.to_owned(), session.name))
            .into_iter()
            .collect();
        actor_label(session_id, &names)
    }

    pub fn menu(&self, session: &str, client: &str) -> Result<i32, VerbError> {
        let Some(record) = self.record(session, client) else {
            return Ok(1);
        };
        let back = match bound_artifact(&record) {
            Some(artifact_id) => {
                let artifact = self
                    .artifacts
                    .require(&artifact_id)
                    .map_err(VerbError::other)?;
                Some(self.controlling_session(&artifact))
            }
            None => match (&record.launch, &record.parent) {
                (Launch::Process { .. }, Some(parent)) => Some(Some(parent.to_string())),
                _ => None,
            },
        };
        match back {
            None => {
                let listing = self.listing(&record, client);
                let content = listing
                    .items
                    .iter()
                    .map(|item| visible_width(item.text()))
                    .chain([visible_width(&listing.header), HINT.chars().count()])
                    .max()
                    .unwrap_or(0);
                let width = (content + POPUP_FRAME).min(self.client_width(client));
                let height = listing.items.len().max(1) + POPUP_ROWS_OVERHEAD;
                tm::display_popup(
                    &self.ui,
                    client,
                    &width.to_string(),
                    &height.to_string(),
                    &listing.title,
                    &tx_ui::txc::shell(self.core, &[PICK_VERB, session, client])?,
                )
                .map_err(VerbError::other)?;
                Ok(0)
            }
            Some(None) => {
                self.ui
                    .display_status(client, "prefix+a: nothing to go back to");
                Ok(1)
            }
            Some(Some(back)) => self.go_back(session, client, &back),
        }
    }

    /// Inside the popup: run the list (a focus on a view no pane shows is declined with a
    /// notice), then hand the choice to a background `_artifact-go` so the popup is gone before
    /// the pane it sits on is re-hosted. A delete runs here and now and the list comes back
    /// without the row (a failure shows under it); the popup closes when nothing is left.
    pub fn pick(&self, session: &str, client: &str) -> Result<i32, VerbError> {
        let Some(record) = self.record(session, client) else {
            return Ok(1);
        };
        // One terminal for every round: a delete redraws without leaving the alternate screen.
        let terminal =
            Terminal::open().map_err(|error| VerbError::failed(format!("terminal: {error}")))?;
        let mut failure: Option<String> = None;
        let (listing, mode, index) = loop {
            let listing = self.listing(&record, client);
            if listing.targets.is_empty() {
                if let Some(failure) = failure {
                    self.ui
                        .display_status(client, &format!("prefix+a: {failure}"));
                }
                return Ok(0);
            }
            let decline = |pick: Pick| match pick {
                Pick::Focus(index) => {
                    let hosted = self
                        .hosting_session(&listing.targets[index])
                        .is_some_and(|name| self.ui.pane_for_session(&name, "").is_some());
                    (!hosted).then(|| format!("{} is not attached anywhere", listing.names[index]))
                }
                _ => None,
            };
            let confirm_text = |index: usize| {
                let name = &listing.names[index];
                match listing.targets[index].kind {
                    "session" => format!("⚠  Kill \"{name}\"? [y/N]"),
                    _ => format!("⚠  Archive \"{name}\" and close its view? [y/N]"),
                }
            };
            let chosen = picker::pick(
                &terminal,
                &listing.header,
                &listing.items,
                failure.take(),
                decline,
                confirm_text,
            )
            .map_err(|error| VerbError::failed(format!("picker: {error}")))?;
            match chosen {
                None => return Ok(0),
                Some(Pick::Attach(index)) => break (listing, Mode::Attach, index),
                Some(Pick::Focus(index)) => break (listing, Mode::Focus, index),
                Some(Pick::Popup(index)) => break (listing, Mode::Popup, index),
                Some(Pick::Delete(index)) => {
                    failure = self
                        .delete(&record, &listing.targets[index])
                        .err()
                        .map(|error| format!("\"{}\": {error}", listing.names[index]));
                }
            }
        };
        let target = &listing.targets[index];
        let command = tx_ui::txc::shell(
            self.core,
            &[
                GO_VERB,
                session,
                client,
                mode.name(),
                target.kind,
                &target.id,
            ],
        )?;
        self.ui
            .run_shell_background(&format!("{command} || true"))
            .map_err(VerbError::other)?;
        Ok(0)
    }

    /// A confirmed delete: a session is stopped; an artifact has its live views stopped and is
    /// archived, the session the key fired in being the actor.
    fn delete(&self, record: &Session, target: &Target) -> Result<(), VerbError> {
        let stop = |id: &SessionId| {
            self.core
                .stop_session(id)
                .map(drop)
                .map_err(VerbError::from)
        };
        if target.kind == "session" {
            return stop(&target.id.parse().map_err(VerbError::other)?);
        }
        for view in self.live() {
            if bound_artifact(&view).as_deref() == Some(target.id.as_str()) {
                stop(&view.id)?;
            }
        }
        self.artifacts
            .set_archived(&target.id, record.id.as_str(), true)
            .map(drop)
            .map_err(VerbError::other)
    }

    fn client_width(&self, client: &str) -> usize {
        tm::client_width(&self.ui, client).unwrap_or(DEFAULT_WIDTH)
    }

    fn listing(&self, record: &Session, client: &str) -> Listing {
        let owned = self.artifacts.artifacts_for_session(record.id.as_str());
        let live = self.live();
        let Sections { sections, targets } = sections(
            record,
            &owned,
            &live,
            |session| location_text(&self.ui.attached_to(&session.name)),
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
        Listing {
            title: format!(" {} ", record.name),
            header: layout.header(),
            items,
            names,
            targets,
        }
    }

    /// The tmux session a row's view runs in: the session itself, or the live view bound to the
    /// artifact (`None` when the artifact has no live view).
    fn hosting_session(&self, target: &Target) -> Option<String> {
        match target.kind {
            "session" => target
                .id
                .parse::<SessionId>()
                .ok()
                .and_then(|id| self.core.get_session(&id).ok())
                .map(|session| session.name),
            _ => self.live_view(&target.id).map(|view| view.name),
        }
    }

    fn live_view(&self, artifact_id: &str) -> Option<Session> {
        self.live()
            .into_iter()
            .find(|candidate| bound_artifact(candidate).as_deref() == Some(artifact_id))
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
        let view = match self.live_view(artifact_id) {
            Some(view) => view,
            None => {
                let artifact = self
                    .artifacts
                    .require(artifact_id)
                    .map_err(VerbError::other)?;
                let opener = Opener {
                    id: Some(record.id.clone()),
                    tags: record.tags.clone(),
                };
                let id = views::open(
                    self.core,
                    &self.artifacts,
                    &artifact,
                    &opener,
                    None,
                    self.editor,
                )?;
                self.core.get_session(&id)?
            }
        };
        self.show(session, client, mode, &view)
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
        self.show(session, client, mode, &target)
    }

    /// The target record when it runs and its tmux session exists; otherwise reported.
    fn running(&self, client: &str, target_id: &str) -> Option<Session> {
        let target = target_id
            .parse::<SessionId>()
            .ok()
            .and_then(|id| self.core.get_session(&id).ok());
        let Some(target) = target else {
            self.ui
                .display_status(client, &format!("prefix+a: session {target_id} not found"));
            return None;
        };
        if target.lifecycle != Lifecycle::Running || !self.ui.has_session(&target.name) {
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
        self.rehost(session, client, &target)
    }

    fn show(
        &self,
        session: &str,
        client: &str,
        mode: Mode,
        target: &Session,
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
                let owner = self.core.resolve_session(session);
                tm::popup_session(
                    &self.ui,
                    &self.core.socket_path().to_string_lossy(),
                    client,
                    target,
                    owner.ok().as_ref(),
                )
                .map_err(VerbError::other)?;
                Ok(0)
            }
            Mode::Attach => self.rehost(session, client, target),
        }
    }

    /// Re-host the pane the firing client paints into on `target` (`respawn-pane -k` onto a
    /// nested attach; the session it showed keeps running detached).
    fn rehost(&self, session: &str, client: &str, target: &Session) -> Result<i32, VerbError> {
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
            .map_err(VerbError::other)?;
        Ok(0)
    }

    /// The artifact's owner while it runs (`None` when the owner was the bare terminal or has
    /// ended).
    fn controlling_session(&self, artifact: &Artifact) -> Option<String> {
        if artifact.owner == USER_ACTOR {
            return None;
        }
        let running = artifact
            .owner
            .parse::<SessionId>()
            .ok()
            .and_then(|id| self.core.get_session(&id).ok())
            .is_some_and(|session| session.lifecycle == Lifecycle::Running);
        running.then(|| artifact.owner.clone())
    }
}

impl Row {
    fn chips_width(&self) -> usize {
        self.chips.iter().map(|chip| chip.chars().count() + 3).sum()
    }

    /// The picker's SGR styling; chips past the layout's chip budget are dropped.
    fn render(&self, index: usize, layout: &Layout) -> String {
        let name = pad(&trunc(&self.name, layout.namew), layout.namew);
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

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn session(id: &str, name: &str, extra: Value) -> Session {
        let mut record = json!({
            "version": 1, "id": id, "name": name, "parent": null, "tags": [], "cwd": "/r",
            "created_at": 1.0, "ended_at": null, "lifecycle": "running",
            "launch": {"kind": "process", "program": "sleep", "args": []},
        });
        for (key, value) in extra.as_object().unwrap() {
            record[key] = value.clone();
        }
        Session::from_json(&record.to_string()).unwrap()
    }

    fn artifact(id: &str, title: &str, at: f64, owner: &str) -> Artifact {
        Artifact::from_value(&json!({
            "artifact_schema_version": 4, "id": id, "kind": "document", "title": title,
            "owner": owner, "created_at": at, "modified_at": at, "archived_at": null,
            "document": {"file": "f.md"},
        }))
        .unwrap()
    }

    #[test]
    fn the_listing_holds_what_the_session_owns_its_parent_and_its_children() {
        let record = session("a1", "agent", json!({"parent": "p1"}));
        let live = vec![
            record.clone(),
            session("p1", "boss", json!({"tags": ["x"]})),
            session("c1", "shell", json!({"parent": "a1"})),
            session(
                "v1",
                "plan-md",
                json!({"parent": "a1", "ext": {"artifacts": {"bound": "art-new"}}}),
            ),
        ];
        let owned = [
            artifact("art-old", "Old", 10.0, "a1"),
            artifact("art-new", "New", 20.0, "a1"),
        ];
        let out = sections(
            &record,
            &owned,
            &live,
            |session| format!("at-{}", session.name),
            |id| id.to_owned(),
        );
        let titles: Vec<&str> = out.sections.iter().map(|(title, _)| *title).collect();
        assert_eq!(titles, ["ARTIFACTS", "PARENT", "CHILDREN"]);
        let artifacts = &out.sections[0].1;
        assert_eq!(artifacts[0].name, "New");
        assert_eq!(artifacts[0].location, "at-plan-md");
        assert_eq!(artifacts[0].chips, ["a1", "document"]);
        assert_eq!(artifacts[1].location, "—");
        assert_eq!(out.sections[1].1[0].name, "boss");
        let children: Vec<&str> = out.sections[2].1.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(children, ["shell"], "the bound view is not a child row");
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
                ("session", "p1"),
                ("session", "c1")
            ]
        );
    }

    #[test]
    fn nothing_owned_and_no_relatives_is_empty() {
        let record = session("a1", "agent", json!({}));
        let out = sections(
            &record,
            &[],
            std::slice::from_ref(&record),
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
