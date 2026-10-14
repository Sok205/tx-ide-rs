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
use crate::render::reltime;
use crate::service::SessionService;
use crate::session::{OtherRole, Session, SessionKind};
use crate::shlex::shlex_join;
use crate::spawn::SpawnSpec;
use crate::storage::Home;

use super::common::{bin_tx, nest_attach_command};

const TITLE_WIDTH: usize = 32;

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
        let companions: Vec<Session> = self
            .service
            .live_sessions()?
            .into_iter()
            .filter(|candidate| candidate.parent.as_deref() == Some(record.id.as_str()))
            .filter(|candidate| {
                matches!(&candidate.kind, SessionKind::Other(other)
                    if other.role == OtherRole::Nvim && other.artifact_id.is_none())
            })
            .collect();

        let mut items: Vec<(String, String, String)> = Vec::new();
        for artifact in &artifacts {
            let title = artifact.title.as_deref().unwrap_or(&artifact.filename);
            items.push((
                format!(
                    "{:<width$} {:>3}r {:>5}",
                    truncate(title, TITLE_WIDTH),
                    artifact.history().len(),
                    reltime(Some(artifact.updated_at()), now),
                    width = TITLE_WIDTH
                ),
                String::new(),
                self.go_command(session, client, "artifact", &artifact.id),
            ));
        }
        for companion in &companions {
            items.push((
                format!(
                    "{:<width$} nvim {:>5}",
                    truncate(&companion.name, TITLE_WIDTH),
                    reltime(companion.created_at.as_ref(), now),
                    width = TITLE_WIDTH
                ),
                String::new(),
                self.go_command(session, client, "session", &companion.id),
            ));
        }
        if items.is_empty() {
            items.push((
                "-no artifacts or companions".into(),
                String::new(),
                String::new(),
            ));
        }
        for (index, item) in items.iter_mut().enumerate().take(9) {
            item.1 = (index + 1).to_string();
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

fn truncate(text: &str, width: usize) -> String {
    let mut characters: Vec<char> = text.chars().collect();
    if characters.len() <= width {
        return text.to_owned();
    }
    characters.truncate(width - 1);
    characters.push('…');
    characters.into_iter().collect()
}
