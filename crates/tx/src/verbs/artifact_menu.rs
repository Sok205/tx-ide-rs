//! `_artifact-menu` / `_artifact-pick` / `_artifact-go`: the prefix+a seam (PoC).
//!
//! The key fires INSIDE the nested session — the outer bind forwards it one level down while the
//! pane hosts a `tmux` client — so `#{session_name}` is the tx id and `#{client_name}` the nested
//! client whose screen is the hosting pane. In an llm session `_artifact-menu` opens a popup over
//! the pane running `_artifact-pick`, a sectioned list — the artifacts the session touched, its
//! parent, its live children (sessions it spawned: workers, shells, nvim companions) — drawn like
//! the tx picker's rows. Enter (or a digit) re-hosts the pane on the
//! chosen view (the llm session keeps running detached); f then a digit or Enter moves the
//! terminal to the pane already showing it; p then a digit or Enter nest-attaches it in a popup
//! over the terminal, leaving the panes as they are; d asks about the cursor's row and y deletes
//! it — a session is killed, an artifact is archived (hidden from the picker and `tx artifact
//! ls`, history kept) and its live view closed — and the list redraws without it.
//! In an nvim view the key goes back to the controlling session — the artifact's most recent live
//! non-`user` toucher (else its creator), or the companion's `parent` — focusing the pane still
//! showing it, else re-hosting this pane on it.

use std::rc::Rc;

use cordis::BoxError;

use crate::app::{Command, Env};
use crate::artifact::{Artifact, USER_ACTOR};
use crate::artifact_picker::{self, HINT, Item, Pick, row_label, visible_width};
use crate::artifact_service::ArtifactService;
use crate::palette::{DIM_FG_ANSI, DIM_FG_HEX, FG_ANSI, FG_HEX, RESET_FG, tag_ansi, tag_hex};
use crate::render::{LOCATION_W, actor_label, location_text};
use crate::service::SessionService;
use crate::session::{Session, SessionKind};
use crate::shlex::shlex_join;
use crate::spawn::SpawnSpec;
use crate::storage::Home;
use crate::verbs::artifacts::view_name;

use super::common::{bin_tx, nest_attach_command};

const NAME_FLOOR: usize = 12;
const NAME_CEILING: usize = 40;
/// The `(k) ` choice key drawn at the row's left, as choose-tree does: `(1)` up to nine rows,
/// `(M-a)` past that.
const DIGIT_LABEL_WIDTH: usize = 4;
const META_LABEL_WIDTH: usize = 6;
/// Popup border plus one column of padding each side.
const POPUP_FRAME: usize = 4;
/// Header, rows, a blank line, the notice line, the hint, and the border rows.
const POPUP_ROWS_OVERHEAD: usize = 6;
/// Assumed client width when tmux cannot report one.
const DEFAULT_WIDTH: usize = 120;
/// The NAME…columns separator.
const NAME_GAP: usize = 3;

pub(super) struct ArtifactNav {
    pub(super) service: Rc<SessionService>,
    pub(super) artifacts: Rc<ArtifactService>,
    pub(super) home: Rc<Home>,
    pub(super) env: Rc<Env>,
}

pub(super) struct ArtifactMenu(pub(super) Rc<ArtifactNav>);

impl Command for ArtifactMenu {
    fn name(&self) -> &'static str {
        "_artifact-menu"
    }
    fn summary(&self) -> &'static str {
        "Internal: prefix+a — in an llm session, open the artifacts picker over the pane; in an \
         nvim view, back to the controlling session. Usage: _artifact-menu SESSION CLIENT"
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let [session, client] = argv else {
            eprintln!("usage: tx _artifact-menu SESSION CLIENT");
            return Ok(2);
        };
        self.0.menu(session, client)
    }
}

pub(super) struct ArtifactPick(pub(super) Rc<ArtifactNav>);

impl Command for ArtifactPick {
    fn name(&self) -> &'static str {
        "_artifact-pick"
    }
    fn summary(&self) -> &'static str {
        "Internal: the picker inside prefix+a's popup — Enter attaches the row's view in the pane; \
         f / p then a digit or Enter focus the pane already showing it / open it in a popup; d \
         then y kills the cursor's session or archives its artifact. \
         Usage: _artifact-pick SESSION CLIENT"
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let [session, client] = argv else {
            eprintln!("usage: tx _artifact-pick SESSION CLIENT");
            return Ok(2);
        };
        self.0.pick(session, client)
    }
}

pub(super) struct ArtifactGo(pub(super) Rc<ArtifactNav>);

impl Command for ArtifactGo {
    fn name(&self) -> &'static str {
        "_artifact-go"
    }
    fn summary(&self) -> &'static str {
        "Internal: show an artifact's nvim view or a session — `attach` re-hosts the pane showing \
         SESSION on it, `focus` moves the terminal to the pane already hosting it, `popup` \
         nest-attaches it in a popup. Usage: _artifact-go SESSION CLIENT \
         {attach|focus|popup} {artifact|session} ID"
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let words: Vec<&str> = argv.iter().map(String::as_str).collect();
        let usage = || {
            eprintln!(
                "usage: tx _artifact-go SESSION CLIENT {{attach|focus|popup}} {{artifact|session}} ID"
            );
            Ok(2)
        };
        let [session, client, mode, kind, id] = words[..] else {
            return usage();
        };
        let Some(mode) = Mode::parse(mode) else {
            return usage();
        };
        match kind {
            "artifact" => self.0.go_artifact(session, client, mode, id),
            "session" => self.0.go_session(session, client, mode, id),
            _ => usage(),
        }
    }
}

/// What a pick does with the chosen view.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Re-host the pane the key fired in on the view.
    Attach,
    /// Move the terminal to the pane already hosting the view (reported when there is none).
    Focus,
    /// Nest-attach the view in a popup over the terminal; the panes stay as they are.
    Popup,
}

impl Mode {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "attach" => Some(Self::Attach),
            "focus" => Some(Self::Focus),
            "popup" => Some(Self::Popup),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Attach => "attach",
            Self::Focus => "focus",
            Self::Popup => "popup",
        }
    }
}

/// What a row stands for, as `_artifact-go` names it.
struct Target {
    kind: &'static str,
    id: String,
}

/// The picker's content for one llm session: sections and rows laid out for `client`'s width;
/// `names` and `targets` are per row, in row order.
struct Listing {
    title: String,
    header: String,
    items: Vec<Item>,
    names: Vec<String>,
    targets: Vec<Target>,
}

impl ArtifactNav {
    /// The record of the nested session the key fired in; `None` is reported on the status line.
    fn record(&self, session: &str, client: &str) -> Result<Option<Session>, BoxError> {
        let tmux = self.service.tmux();
        let record = tmux
            .get_tx_id(session)
            .map(|id| self.service.get(&id))
            .transpose()?
            .flatten();
        if record.is_none() {
            tmux.display_status(client, "prefix+a: not a tx session");
        }
        Ok(record)
    }

    fn menu(&self, session: &str, client: &str) -> Result<i32, BoxError> {
        let Some(record) = self.record(session, client)? else {
            return Ok(1);
        };
        match &record.kind {
            SessionKind::Llm(_) => {
                let listing = self.listing(&record, client)?;
                let content = listing
                    .items
                    .iter()
                    .map(|item| visible_width(item.text()))
                    .chain([visible_width(&listing.header), HINT.chars().count()])
                    .max()
                    .unwrap_or(0);
                let client_width = self.client_width(client);
                self.service.tmux().display_popup(
                    client,
                    &(content + POPUP_FRAME).min(client_width).to_string(),
                    &(listing.items.len().max(1) + POPUP_ROWS_OVERHEAD).to_string(),
                    &listing.title,
                    &self.tx_command(&["_artifact-pick", session, client]),
                )?;
                Ok(0)
            }
            SessionKind::Other(other) => {
                let back = match &other.artifact_id {
                    Some(artifact_id) => {
                        self.controlling_session(&self.artifacts.require(artifact_id)?)?
                    }
                    None => record.parent.clone(),
                };
                let Some(back) = back else {
                    self.service
                        .tmux()
                        .display_status(client, "prefix+a: nothing to go back to");
                    return Ok(1);
                };
                self.go_back(session, client, &back)
            }
        }
    }

    /// Inside the popup: run the list (a focus on a view no pane shows is declined with a notice),
    /// then hand the choice to a background `_artifact-go` so the popup is gone before the pane
    /// it sits on is re-hosted. A delete is done here and now and the list comes back without
    /// the row (a failure is reported under it); the popup closes when nothing is left to list.
    fn pick(&self, session: &str, client: &str) -> Result<i32, BoxError> {
        let Some(record) = self.record(session, client)? else {
            return Ok(1);
        };
        let mut failure: Option<String> = None;
        let (listing, mode, index) = loop {
            let listing = self.listing(&record, client)?;
            if listing.targets.is_empty() {
                if let Some(failure) = failure {
                    self.service
                        .tmux()
                        .display_status(client, &format!("prefix+a: {failure}"));
                }
                return Ok(0);
            }
            // A focus needs a pane to go to: declined in place when nothing shows the row's view.
            let decline = |pick: Pick| match pick {
                Pick::Attach(_) | Pick::Popup(_) | Pick::Delete(_) => None,
                Pick::Focus(index) => {
                    let target = &listing.targets[index];
                    let hosted = self.hosting_session(target).is_some_and(|tmux_name| {
                        self.service
                            .tmux()
                            .pane_for_session(&tmux_name, "")
                            .is_some()
                    });
                    (!hosted).then(|| format!("{} is not attached anywhere", listing.names[index]))
                }
            };
            let confirm_text = |index: usize| match listing.targets[index].kind {
                "session" => format!("⚠  Kill \"{}\"? [y/N]", listing.names[index]),
                _ => format!(
                    "⚠  Archive \"{}\" and close its view? [y/N]",
                    listing.names[index]
                ),
            };
            let Some(pick) = artifact_picker::pick(
                &listing.header,
                &listing.items,
                failure.take(),
                decline,
                confirm_text,
            )?
            else {
                return Ok(0);
            };
            match pick {
                Pick::Attach(index) => break (listing, Mode::Attach, index),
                Pick::Focus(index) => break (listing, Mode::Focus, index),
                Pick::Popup(index) => break (listing, Mode::Popup, index),
                Pick::Delete(index) => {
                    failure = self
                        .delete(&record, &listing.targets[index])
                        .err()
                        .map(|error| format!("\"{}\": {error}", listing.names[index]));
                }
            }
        };
        let target = &listing.targets[index];
        let command = self.tx_command(&[
            "_artifact-go",
            session,
            client,
            mode.name(),
            target.kind,
            &target.id,
        ]);
        self.service
            .tmux()
            .run_shell_background(&format!("{command} || true"))?;
        Ok(0)
    }

    /// A confirmed delete: a session is killed; an artifact has its live views killed and is
    /// archived, the llm session the key fired in being the actor.
    fn delete(&self, record: &Session, target: &Target) -> Result<(), BoxError> {
        match target.kind {
            "session" => {
                self.service.kill(&target.id)?;
                Ok(())
            }
            _ => self.archive_artifact(record, &target.id),
        }
    }

    fn archive_artifact(&self, record: &Session, artifact_id: &str) -> Result<(), BoxError> {
        let views = self
            .service
            .live_sessions()?
            .into_iter()
            .filter(|candidate| {
                matches!(&candidate.kind, SessionKind::Other(other)
                if other.artifact_id.as_deref() == Some(artifact_id))
            });
        for view in views {
            self.service.kill(&view.id)?;
        }
        self.artifacts.set_archived(artifact_id, &record.id, true)?;
        Ok(())
    }

    fn client_width(&self, client: &str) -> usize {
        self.service
            .tmux()
            .client_width(client)
            .unwrap_or(DEFAULT_WIDTH)
    }

    fn listing(&self, record: &Session, client: &str) -> Result<Listing, BoxError> {
        let mut artifacts = self.artifacts.artifacts_for_session(&record.id);
        artifacts.sort_by(|a, b| updated_at(b).total_cmp(&updated_at(a)));
        let live = self.service.live_sessions()?;
        let names = self.service.store().names_for(
            artifacts
                .iter()
                .flat_map(|artifact| artifact.history())
                .map(|touch| touch.session_id.as_str()),
        );

        // (section title, rows) in display order; rows are numbered across sections.
        let mut sections: Vec<(&str, Vec<Row>)> = Vec::new();
        let mut targets: Vec<Target> = Vec::new();
        let mut rows: Vec<Row> = Vec::new();
        for artifact in &artifacts {
            let view = live.iter().find(|candidate| {
                matches!(&candidate.kind, SessionKind::Other(other)
                    if other.artifact_id.as_deref() == Some(artifact.id.as_str()))
            });
            let mut chips = Vec::new();
            for touch in artifact.history() {
                let author = actor_label(&touch.session_id, &names);
                if !chips.contains(&author) {
                    chips.push(author);
                }
            }
            rows.push(Row {
                name: artifact
                    .title
                    .clone()
                    .unwrap_or_else(|| artifact.filename.clone()),
                location: view
                    .map_or_else(|| "—".to_owned(), |view| location_text(&view.attached_to)),
                chips,
            });
            targets.push(Target {
                kind: "artifact",
                id: artifact.id.clone(),
            });
        }
        if !rows.is_empty() {
            sections.push(("ARTIFACTS", std::mem::take(&mut rows)));
        }
        let parent = record
            .parent
            .as_deref()
            .and_then(|parent_id| live.iter().find(|candidate| candidate.id == parent_id));
        // Artifact views are already rows above, under their artifact.
        let children = live.iter().filter(|candidate| {
            candidate.parent.as_deref() == Some(record.id.as_str())
                && !matches!(&candidate.kind, SessionKind::Other(other) if other.artifact_id.is_some())
        });
        for (title, related) in [
            ("PARENT", parent.into_iter().collect::<Vec<_>>()),
            ("CHILDREN", children.collect()),
        ] {
            if related.is_empty() {
                continue;
            }
            for related_session in related {
                rows.push(Row {
                    name: related_session.name.clone(),
                    location: location_text(&related_session.attached_to),
                    chips: related_session.tags.clone(),
                });
                targets.push(Target {
                    kind: "session",
                    id: related_session.id.clone(),
                });
            }
            sections.push((title, std::mem::take(&mut rows)));
        }

        // Laid out for the popup's usable width so no row wraps.
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

    /// The tmux session a row's view runs in: the companion itself, or the live nvim view bound to
    /// the artifact (`None` when the artifact has no live view).
    fn hosting_session(&self, target: &Target) -> Option<String> {
        match target.kind {
            "session" => Some(target.id.clone()),
            _ => self
                .service
                .live_sessions()
                .ok()?
                .into_iter()
                .find_map(|candidate| {
                    matches!(&candidate.kind, SessionKind::Other(other)
                    if other.artifact_id.as_deref() == Some(target.id.as_str()))
                    .then(|| candidate.tmux_name().to_owned())
                }),
        }
    }

    /// This binary with the home baked in: `run-shell` and the popup run in the server's
    /// environment, which has neither (as prefix+f does).
    fn tx_command(&self, argv: &[&str]) -> String {
        let home = format!("TX_IDE_HOME={}", self.home.root().display());
        let tx = bin_tx(&self.env).display().to_string();
        let mut words = vec!["env", &home, &tx];
        words.extend_from_slice(argv);
        shlex_join(&words)
    }

    fn go_artifact(
        &self,
        session: &str,
        client: &str,
        mode: Mode,
        artifact_id: &str,
    ) -> Result<i32, BoxError> {
        let Some(record) = self.record(session, client)? else {
            return Ok(1);
        };
        let live_view = self.service.live_sessions()?.into_iter().find(|candidate| {
            matches!(&candidate.kind, SessionKind::Other(other)
                if other.artifact_id.as_deref() == Some(artifact_id))
        });
        let view = match live_view {
            Some(view) => view,
            None => self.open_view(&record, artifact_id)?,
        };
        self.show(session, client, mode, &view)
    }

    /// `tx artifact open` without the CLI: a bound nvim view whose `parent` is the opener.
    fn open_view(&self, opener: &Session, artifact_id: &str) -> Result<Session, BoxError> {
        let artifact = self.artifacts.require(artifact_id)?;
        let content_path = self.artifacts.content_path(artifact_id)?;
        let cwd = self.artifacts.files().content_dir(&artifact);
        let tags = if opener.tags.is_empty() {
            vec!["artifact".to_owned()]
        } else {
            opener.tags.clone()
        };
        let name = view_name(&self.service, &artifact)?;
        let mut spec = SpawnSpec::for_nvim(
            name,
            tags,
            cwd.to_string_lossy(),
            None,
            Some(&content_path.to_string_lossy()),
        );
        spec.parent = Some(opener.id.clone());
        let view = self.service.spawn_nvim(spec)?;
        self.service.bind_artifact(&view.id, artifact_id)?;
        self.artifacts.opened(artifact_id, &opener.id)?;
        Ok(view)
    }

    fn go_session(
        &self,
        session: &str,
        client: &str,
        mode: Mode,
        target_id: &str,
    ) -> Result<i32, BoxError> {
        let tmux = self.service.tmux();
        let Some(target) = self.service.get(target_id)? else {
            tmux.display_status(client, &format!("prefix+a: session {target_id} not found"));
            return Ok(1);
        };
        if !target.is_alive() || !tmux.has_session(target.tmux_name()) {
            tmux.display_status(client, &format!("prefix+a: {} has ended", target.name));
            return Ok(1);
        }
        self.show(session, client, mode, &target)
    }

    /// The way back from a view: to the pane still showing the controlling session when there is
    /// one this terminal can reach (the view was reached by focusing), else re-host it here (the
    /// view was attached in its place, which left it detached).
    fn go_back(&self, session: &str, client: &str, target_id: &str) -> Result<i32, BoxError> {
        let tmux = self.service.tmux();
        let Some(target) = self.service.get(target_id)? else {
            tmux.display_status(client, &format!("prefix+a: session {target_id} not found"));
            return Ok(1);
        };
        if !target.is_alive() || !tmux.has_session(target.tmux_name()) {
            tmux.display_status(client, &format!("prefix+a: {} has ended", target.name));
            return Ok(1);
        }
        if tmux.focus_hosting_pane(client, target.tmux_name()).is_ok() {
            return Ok(0);
        }
        self.rehost(session, client, &target)
    }

    /// Bring `target` in front of the user: in `Focus` mode move the terminal to a pane already
    /// hosting it (reported when none does); in `Popup` mode nest-attach it in a popup over the
    /// outer terminal (the nested client's own screen when there is no outer one); in `Attach`
    /// mode re-host here.
    fn show(
        &self,
        session: &str,
        client: &str,
        mode: Mode,
        target: &Session,
    ) -> Result<i32, BoxError> {
        let tmux = self.service.tmux();
        match mode {
            Mode::Focus => match tmux.focus_hosting_pane(client, target.tmux_name()) {
                Ok(()) => Ok(0),
                Err(reason) => {
                    tmux.display_status(client, &format!("prefix+a: {}: {reason}", target.name));
                    Ok(1)
                }
            },
            Mode::Popup => {
                let outer = tmux
                    .outer_client(client)
                    .map_or_else(|| client.to_owned(), |(outer, _)| outer);
                let prefix = tmux
                    .global_option("prefix")
                    .unwrap_or_else(|| "C-b".to_owned());
                let owner = self.service.get(session)?;
                tmux.popup_attach(
                    &outer,
                    &popup_title(target, owner.as_ref()),
                    &popup_footer(&prefix),
                    target.tmux_name(),
                )?;
                Ok(0)
            }
            Mode::Attach => self.rehost(session, client, target),
        }
    }

    /// Re-host the pane the firing client paints into on `target` (`respawn-pane -k` onto a
    /// nested attach; the session it showed keeps running detached). The pane is found by the
    /// client's tty, so a session hosted in two panes swaps only the one the key was pressed in.
    fn rehost(&self, session: &str, client: &str, target: &Session) -> Result<i32, BoxError> {
        let tmux = self.service.tmux();
        if session == target.tmux_name() {
            tmux.display_status(client, &format!("prefix+a: already in {}", target.name));
            return Ok(0);
        }
        let Some(pane) = tmux.pane_hosting_client(client) else {
            tmux.display_status(client, "prefix+a: this session is not hosted in a pane");
            return Ok(1);
        };
        tmux.respawn_pane(&pane, &nest_attach_command(target.tmux_name()))?;
        Ok(0)
    }

    /// The artifact's most recent live non-`user` toucher, else its creator (`None` when the
    /// creator was the bare terminal).
    fn controlling_session(&self, artifact: &Artifact) -> Result<Option<String>, BoxError> {
        let history = artifact.history();
        for touch in history.iter().rev() {
            if touch.session_id == USER_ACTOR {
                continue;
            }
            if self
                .service
                .get(&touch.session_id)?
                .is_some_and(|session| session.is_alive())
            {
                return Ok(Some(touch.session_id.clone()));
            }
        }
        let creator = &history[0].session_id;
        Ok((creator != USER_ACTOR).then(|| creator.clone()))
    }
}

fn updated_at(artifact: &Artifact) -> f64 {
    artifact.updated_at().as_f64().unwrap_or(0.0)
}

/// The popup's title, as the picker shows a row: the view's name, then its tags as chips, then
/// `◂ owner` — the display name of the session the key fired in (none when it is not tx-managed).
fn popup_title(view: &Session, owner: Option<&Session>) -> String {
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

/// One picker line, laid out like a picker row: (n) NAME   LOCATION chips.
struct Row {
    name: String,
    location: String,
    chips: Vec<String>,
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

    /// The picker's column header (drawn dim by the picker).
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
        // One extra column: each chip carries a leading space.
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
