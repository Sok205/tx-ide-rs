//! `_artifact-menu` / `_artifact-go`: the prefix+a seam (PoC).
//!
//! The key fires INSIDE the nested session — the outer bind forwards it one level down while the
//! pane hosts a `tmux` client — so `#{session_name}` is the tx id and `#{client_name}` the nested
//! client whose screen is the hosting pane. In an llm session the menu (a native `display-menu`,
//! painted in the pane) lists the artifacts it touched and its live nvim companions; a pick
//! re-hosts the pane on that view while the llm session keeps running detached. In an nvim view
//! the same key re-hosts the pane on the controlling session: the artifact's most recent live
//! non-`user` toucher (else its creator), or the companion's `parent`.

use std::rc::Rc;

use cordis::BoxError;

use crate::app::{Command, Env};
use crate::artifact::{Artifact, USER_ACTOR};
use crate::artifact_service::ArtifactService;
use crate::events;
use crate::palette::{DIM_FG_HEX, FG_HEX, WARN_HEX, tag_hex};
use crate::render::{LOCATION_W, ROLE_W, actor_label, location_text, reltime};
use crate::service::SessionService;
use crate::session::{OtherRole, Session, SessionKind};
use crate::shlex::shlex_join;
use crate::spawn::SpawnSpec;
use crate::storage::Home;

use super::common::{bin_tx, nest_attach_command};

const NAME_FLOOR: usize = 12;
const NAME_CEILING: usize = 40;
/// Menu border + padding columns tmux adds around an item (`menu->width + 4`).
const MENU_FRAME: usize = 4;
/// The ` (k)` key suffix tmux appends to a keyed item.
const KEY_WIDTH: usize = 4;
/// Assumed client width when tmux cannot report one.
const DEFAULT_WIDTH: usize = 120;
/// NAME…chips separators plus the fixed IDLE / ROLE / note cells.
const FIXED_CELLS: usize = 3 + 6 + 1 + ROLE_W + 1 + 3;

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
        "Internal: prefix+a — in an llm session, a menu of its artifacts and nvim companions; in \
         an nvim view, back to the controlling session. Usage: _artifact-menu SESSION CLIENT"
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let [session, client] = argv else {
            eprintln!("usage: tx _artifact-menu SESSION CLIENT");
            return Ok(2);
        };
        self.0.menu(session, client)
    }
}

pub(super) struct ArtifactGo(pub(super) Rc<ArtifactNav>);

impl Command for ArtifactGo {
    fn name(&self) -> &'static str {
        "_artifact-go"
    }
    fn summary(&self) -> &'static str {
        "Internal: re-host the pane showing SESSION on an artifact's nvim view or on a session. \
         Usage: _artifact-go SESSION CLIENT {artifact|session} ID"
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let words: Vec<&str> = argv.iter().map(String::as_str).collect();
        match words[..] {
            [session, client, "artifact", artifact_id] => {
                self.0.go_artifact(session, client, artifact_id)
            }
            [session, client, "session", session_id] => {
                self.0.go_session(session, client, session_id)
            }
            _ => {
                eprintln!("usage: tx _artifact-go SESSION CLIENT {{artifact|session}} ID");
                Ok(2)
            }
        }
    }
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
            SessionKind::Llm(_) => self.show_menu(&record, session, client),
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
                self.go_session(session, client, &back)
            }
        }
    }

    fn show_menu(&self, record: &Session, session: &str, client: &str) -> Result<i32, BoxError> {
        let now = events::now();
        let mut artifacts = self.artifacts.artifacts_for_session(&record.id);
        artifacts.sort_by(|a, b| updated_at(b).total_cmp(&updated_at(a)));
        let live = self.service.live_sessions()?;
        let names = self.service.store().names_for(
            artifacts
                .iter()
                .flat_map(|artifact| artifact.history())
                .map(|touch| touch.session_id.as_str()),
        );

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
                started: reltime(Some(&artifact.created_at), now),
                idle: reltime(Some(artifact.updated_at()), now),
                role: "art",
                note: format!("{}r", artifact.history().len()),
                chips,
                command: self.go_command(session, client, "artifact", &artifact.id),
            });
        }
        for companion in live.iter().filter(|candidate| {
            candidate.parent.as_deref() == Some(record.id.as_str())
                && matches!(&candidate.kind, SessionKind::Other(other)
                    if other.role == OtherRole::Nvim && other.artifact_id.is_none())
        }) {
            rows.push(Row {
                name: companion.name.clone(),
                location: location_text(&companion.attached_to),
                started: reltime(companion.created_at.as_ref(), now),
                idle: "—".to_owned(),
                role: "nvim",
                note: String::new(),
                chips: companion.tags.clone(),
                command: self.go_command(session, client, "session", &companion.id),
            });
        }

        // tmux caps an item at the client's width minus its frame and the ` (k)` key, and cuts
        // an over-long one from the LEFT (`format_trim_right`), so every row is laid out to fit.
        let client_width = self
            .service
            .tmux()
            .client_width(client)
            .unwrap_or(DEFAULT_WIDTH);
        let layout = Layout::fit(&rows, client_width.saturating_sub(MENU_FRAME + KEY_WIDTH));
        let mut items: Vec<(String, String, String)> =
            vec![(layout.header(), String::new(), String::new())];
        if rows.is_empty() {
            items.push((
                "-  (no artifacts or companions)".into(),
                String::new(),
                String::new(),
            ));
        }
        for (index, row) in rows.iter().enumerate() {
            let key = if index < 9 {
                (index + 1).to_string()
            } else {
                String::new()
            };
            items.push((row.render(&layout), key, row.command.clone()));
        }
        self.service
            .tmux()
            .display_menu(client, &format!(" {} ", record.name), &items)?;
        Ok(0)
    }

    /// The menu item's tmux command: a background `run-shell` of this binary (the server's
    /// environment has no PATH or home to rely on; both are baked in, as prefix+f does).
    fn go_command(&self, session: &str, client: &str, kind: &str, id: &str) -> String {
        let home = format!("TX_IDE_HOME={}", self.home.root().display());
        let tx = bin_tx(&self.env).display().to_string();
        let command = shlex_join(&["env", &home, &tx, "_artifact-go", session, client, kind, id]);
        format!("run-shell -b \"{command} || true\"")
    }

    fn go_artifact(&self, session: &str, client: &str, artifact_id: &str) -> Result<i32, BoxError> {
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
        self.rehost(session, client, &view)
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
        let name = format!("art-{}", artifact_id.chars().take(8).collect::<String>());
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

    fn go_session(&self, session: &str, client: &str, target_id: &str) -> Result<i32, BoxError> {
        let tmux = self.service.tmux();
        let Some(target) = self.service.get(target_id)? else {
            tmux.display_status(client, &format!("prefix+a: session {target_id} not found"));
            return Ok(1);
        };
        if !target.is_alive() || !tmux.has_session(target.tmux_name()) {
            tmux.display_status(client, &format!("prefix+a: {} has ended", target.name));
            return Ok(1);
        }
        self.rehost(session, client, &target)
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

/// One menu line, laid out like a picker row: NAME   LOCATION STARTED IDLE ROLE chips.
struct Row {
    name: String,
    location: String,
    started: String,
    idle: String,
    role: &'static str,
    /// A dim annotation before the chips (an artifact's revision count).
    note: String,
    chips: Vec<String>,
    command: String,
}

impl Row {
    fn chips_width(&self) -> usize {
        self.chips.iter().map(|chip| chip.chars().count() + 3).sum()
    }

    /// tmux `#[…]` styles stand in for the picker's SGR escapes; literal `#` is doubled. Chips
    /// past the layout's chip budget are dropped (tmux would cut the row's left end instead).
    fn render(&self, layout: &Layout) -> String {
        let name = pad(&truncate(&self.name, layout.namew), layout.namew);
        let mut line = format!("#[fg={FG_HEX}]{}#[default]   ", escape(&name));
        if layout.location {
            line.push_str(&escape(&pad(&self.location, LOCATION_W)));
            line.push(' ');
        }
        if layout.started {
            line.push_str(&format!("{:<7} ", self.started));
        }
        line.push_str(&format!(
            "#[fg={WARN_HEX}]{:<6}#[default] #[fg={}]{:<ROLE_W$}#[default] #[fg={DIM_FG_HEX}]{:<3}#[default]",
            self.idle,
            tag_hex(self.role),
            self.role,
            self.note,
        ));
        let mut used = 0;
        for chip in &self.chips {
            used += chip.chars().count() + 3;
            if used > layout.chips {
                break;
            }
            line.push_str(&format!(
                " #[fg={}][{}]#[default]",
                tag_hex(chip),
                escape(chip)
            ));
        }
        line
    }
}

/// Which picker columns fit the client: NAME and the fixed cells always, LOCATION then STARTED
/// when there is room, chips in whatever is left.
struct Layout {
    namew: usize,
    location: bool,
    started: bool,
    chips: usize,
}

impl Layout {
    fn fit(rows: &[Row], width: usize) -> Self {
        let longest = rows
            .iter()
            .map(|row| row.name.chars().count())
            .max()
            .unwrap_or(0);
        let chips_wanted = rows.iter().map(Row::chips_width).max().unwrap_or(0);
        let mut namew = longest.clamp(NAME_FLOOR, NAME_CEILING);
        let spare = |namew: usize| width.saturating_sub(namew + FIXED_CELLS);
        if spare(namew) < chips_wanted {
            namew = NAME_FLOOR.max(width.saturating_sub(FIXED_CELLS + chips_wanted).min(namew));
        }
        let mut left = spare(namew);
        let mut chips = chips_wanted.min(left);
        left -= chips;
        let location = left > LOCATION_W;
        if location {
            left -= LOCATION_W + 1;
        }
        let started = left >= 8;
        if started {
            left -= 8;
        }
        chips += left;
        Self {
            namew,
            location,
            started,
            chips,
        }
    }

    /// The picker's column header as a disabled (unselectable) first line.
    fn header(&self) -> String {
        let mut line = format!(
            "-#[fg={DIM_FG_HEX}]{:<namew$}   ",
            "NAME",
            namew = self.namew
        );
        if self.location {
            line.push_str(&format!("{:<LOCATION_W$} ", "LOCATION"));
        }
        if self.started {
            line.push_str(&format!("{:<7} ", "STARTED"));
        }
        line.push_str(&format!(
            "{:<6} {:<ROLE_W$}     TAGS#[default]",
            "IDLE", "ROLE"
        ));
        line
    }
}

fn pad(text: &str, width: usize) -> String {
    let fill = width.saturating_sub(text.chars().count());
    format!("{text}{}", " ".repeat(fill))
}

fn escape(text: &str) -> String {
    text.replace('#', "##")
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
