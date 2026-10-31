//! Chat ops: `fork` and `resume`. Port of main's `chat.rs` / `verbs/chat.rs`.
//!
//! - **fork**: a NEW Process session running the engine's resume argv rewritten to fork; a
//!   pending `fork` ref the capture hook fills from the fork's first new chat id.
//! - **resume**: a NEW agent session resumed onto a past record's active chat.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value};
use tx_core::events::now;
use tx_core::{
    CoreError, CreateOptions, LaunchOptions, Lifecycle, ListFilter, Session, SessionId, TxCore,
    Verb, VerbError, VerbIo,
};

use crate::engines::prepare_chat_for_cwd;
use crate::engines::{Locator, agent_plan, fork_args};
use crate::ext::{
    AgentLaunch, ChatExt, ChatRef, Origin, Role, active_chat, agent_launch, chat_ext,
};
use crate::history_verbs::say;
use crate::{CHAT_ID, ChatError};

/// The fork's chat ref, pending until the capture hook reports the fork's own chat id.
fn pending_fork_ref(origin_session: &SessionId, origin_chat: &str) -> ChatRef {
    ChatRef {
        id: None,
        role: Role::Fork,
        how: "fork".to_owned(),
        since: now(),
        ended_at: None,
        origin: Some(Origin {
            session: origin_session.to_string(),
            chat: origin_chat.to_owned(),
            rest: Map::new(),
        }),
        transcript: None,
        rest: Map::new(),
    }
}

// ----- the operations ----------------------------------------------------------------------

pub struct Ops<'a> {
    pub core: &'a TxCore,
    /// `<data>/plugins/chat/`.
    pub chat_dir: PathBuf,
}

/// The pieces of a resolved source the ops share.
struct Source {
    session: Session,
    launch: AgentLaunch,
    chat_id: String,
}

impl<'a> Ops<'a> {
    pub fn new(core: &'a TxCore) -> Self {
        Self {
            core,
            chat_dir: core.config().plugin_data_dir(CHAT_ID),
        }
    }

    fn profile_file(&self, name: &str) -> PathBuf {
        self.chat_dir.join("profiles").join(format!("{name}.md"))
    }

    /// `base`, `base-2`, `base-3` … : the first not carried by a Running record.
    pub fn next_free_name(&self, base: &str) -> Result<String, ChatError> {
        let mut running = Vec::new();
        for id in self
            .core
            .list_sessions(&ListFilter::new().lifecycle(Lifecycle::Running))?
        {
            match self.core.get_session(&id) {
                Ok(session) => running.push(session.name),
                Err(CoreError::NotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        let taken = |name: &str| running.iter().any(|n| n == name);
        if !taken(base) {
            return Ok(base.to_owned());
        }
        Ok((2..)
            .map(|n| format!("{base}-{n}"))
            .find(|name| !taken(name))
            .expect("an unbounded range always has a free name"))
    }

    fn lookup(&self, op: &str, token: &str) -> Result<Session, ChatError> {
        let not_found = || ChatError::NotFound {
            op: op.to_owned(),
            token: token.to_owned(),
        };
        let id = match self.core.resolve(token) {
            Ok(id) => id,
            Err(CoreError::NotFound(_)) => return Err(not_found()),
            Err(error) => return Err(error.into()),
        };
        match self.core.get_session(&id) {
            Ok(session) => Ok(session),
            Err(CoreError::NotFound(_)) => Err(not_found()),
            Err(error) => Err(error.into()),
        }
    }

    /// A record with an agent launch and an active chat, else `<op>: '<name>' has no <missing>`.
    fn source(&self, op: &str, token: &str, missing: &str) -> Result<Source, ChatError> {
        let session = self.lookup(op, token)?;
        let ext = chat_ext(&session)?;
        let no_chat = || ChatError::NoChat {
            op: op.to_owned(),
            name: session.name.clone(),
            missing: missing.to_owned(),
        };
        let launch = agent_launch(&session, &ext).ok_or_else(no_chat)?;
        let chat_id = active_chat(&ext)
            .and_then(|chat| chat.id.clone())
            .ok_or_else(no_chat)?;
        Ok(Source {
            session,
            launch,
            chat_id,
        })
    }

    fn update_chat(&self, id: &SessionId, f: impl FnOnce(&mut ChatExt)) -> Result<(), ChatError> {
        self.core.update_ext(id, CHAT_ID, |value| {
            let Ok(mut ext) = (match &*value {
                Value::Null => Ok(ChatExt::default()),
                other => serde_json::from_value::<ChatExt>(other.clone()),
            }) else {
                return;
            };
            f(&mut ext);
            if let Ok(updated) = serde_json::to_value(&ext) {
                *value = updated;
            }
        })?;
        Ok(())
    }

    fn agent_options(launch: &AgentLaunch) -> LaunchOptions {
        let mut options = LaunchOptions::agent(&launch.engine).profile(launch.profile.clone());
        if let Some(model) = &launch.model {
            options = options.model(model);
        }
        if let Some(effort) = launch.effort {
            options = options.effort(effort);
        }
        options
    }

    // ----- fork --------------------------------------------------------------------------

    /// Branch the source's active chat into a NEW session carrying its full history.
    pub fn fork(&self, source: &str, name: Option<&str>) -> Result<Session, ChatError> {
        let src = self.source("fork", source, "chat to fork")?;
        let base = match name.filter(|name| !name.is_empty()) {
            Some(name) => name.to_owned(),
            None => format!("{}-fork", src.session.name),
        };
        let name = self.next_free_name(&base)?;
        let cwd = &src.session.cwd;
        let mut plan = agent_plan(
            self.core,
            &src.launch,
            cwd,
            &self.profile_file(&name),
            Some(&src.chat_id),
            None,
        )?;
        fork_args(&src.launch.engine, &mut plan.args);
        tx_core::engine::apply_workspace(cwd, &plan.workspace)?;
        let mut opts = CreateOptions::new(
            &name,
            cwd,
            LaunchOptions::process(&plan.program, &plan.args),
        )
        .parent(src.session.id.clone())
        .tags(src.session.tags.clone());
        for (key, value) in &plan.env {
            opts = opts.env(key, value);
        }
        let id = self.core.create_session(opts)?;
        let launch = src.launch.clone();
        let fork = pending_fork_ref(&src.session.id, &src.chat_id);
        self.update_chat(&id, |ext| {
            ext.engine = Some(launch.engine.clone());
            ext.launch = Some(launch);
            ext.chats = vec![fork];
        })?;
        Ok(self.core.get_session(&id)?)
    }

    // ----- resume ------------------------------------------------------------------------

    /// Re-create a past record's agent resumed onto its active chat.
    pub fn resume(
        &self,
        target: &str,
        as_name: Option<&str>,
        cwd: Option<&Path>,
        loc: &Locator,
    ) -> Result<Session, ChatError> {
        let src = self.source("resume", target, "chat to resume")?;
        let record = &src.session;
        let name = as_name
            .filter(|name| !name.is_empty())
            .unwrap_or(&record.name)
            .to_owned();
        if self.running_name(&name)? {
            return Err(ChatError::LiveName(name));
        }
        let explicit = cwd.filter(|cwd| !cwd.as_os_str().is_empty());
        let cwd = explicit.unwrap_or(&record.cwd).to_owned();
        if !cwd.is_dir() {
            return Err(ChatError::CwdMissing {
                cwd: cwd.display().to_string(),
                remedy: match explicit {
                    None => "pass --cwd <dir>".to_owned(),
                    Some(_) => format!("'{}' is not a directory", cwd.display()),
                },
            });
        }
        let engine = &src.launch.engine;
        if cwd != record.cwd {
            prepare_chat_for_cwd(loc, engine, &src.chat_id, &record.cwd, &cwd)?;
        }
        let transcript = loc
            .locate_transcript(engine, &src.chat_id, Some(&cwd.to_string_lossy()))
            .map(|path| path.to_string_lossy().into_owned());
        let opts = CreateOptions::new(
            &name,
            &cwd,
            Self::agent_options(&src.launch).resume(&src.chat_id),
        )
        .parent(record.id.clone())
        .tags(record.tags.clone());
        let id = self.core.create_session(opts)?;
        let resumed = ChatRef {
            id: Some(src.chat_id.clone()),
            role: Role::Primary,
            how: "resume".into(),
            since: now(),
            ended_at: None,
            origin: Some(Origin {
                session: record.id.to_string(),
                chat: src.chat_id.clone(),
                rest: Map::new(),
            }),
            transcript,
            rest: Map::new(),
        };
        let engine = engine.clone();
        self.update_chat(&id, |ext| {
            ext.engine = Some(engine);
            ext.chats = vec![resumed];
        })?;
        Ok(self.core.get_session(&id)?)
    }

    fn running_name(&self, name: &str) -> Result<bool, ChatError> {
        for id in self
            .core
            .list_sessions(&ListFilter::new().lifecycle(Lifecycle::Running))?
        {
            match self.core.get_session(&id) {
                Ok(session) if session.name == name => return Ok(true),
                Ok(_) | Err(CoreError::NotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(false)
    }
}

// ----- verbs -------------------------------------------------------------------------------

/// A `ChatError` the user caused is one line on stderr and exit 1; a core error propagates.
fn report(io: &mut VerbIo<'_>, verb: &str, error: ChatError) -> Result<i32, VerbError> {
    let text = match &error {
        ChatError::NotFound { .. } | ChatError::NoChat { .. } => error.to_string(),
        ChatError::LiveName(_) | ChatError::CwdMissing { .. } => error.to_string(),
        ChatError::Core(_) => return Err(error.into()),
        _ => format!("{verb}: {error}"),
    };
    say(io.stderr, &format!("{text}\n"))?;
    Ok(1)
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

pub fn verbs() -> Vec<Arc<dyn Verb>> {
    vec![Arc::new(Fork), Arc::new(Resume)]
}

struct Fork;

const FORK_USAGE: &str = "usage: txc fork <source> [<new-name>]\n";

impl Verb for Fork {
    fn name(&self) -> &'static str {
        "fork"
    }

    fn usage(&self) -> &'static str {
        "fork <source> [<new-name>]   a NEW session on a fork of the source's chat"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let (source, name) = match argv {
            [source] if !source.starts_with('-') => (source.as_str(), None),
            [source, name] if !source.starts_with('-') && !name.starts_with('-') => {
                (source.as_str(), Some(name.as_str()))
            }
            _ => {
                say(io.stderr, FORK_USAGE)?;
                return Ok(2);
            }
        };
        match Ops::new(core).fork(source, name) {
            Ok(new) => {
                say(
                    io.stdout,
                    &format!(
                        "Forked '{source}' → '{}' (chat pending, cwd={})\n",
                        new.name,
                        new.cwd.display()
                    ),
                )?;
                Ok(0)
            }
            Err(error) => report(io, "fork", error),
        }
    }
}

/// `<target> [--as <name>] [--cwd <dir>]`, each option at most once.
fn parse_resume(argv: &[String]) -> Option<(&str, Option<&str>, Option<&str>)> {
    let mut positionals = Vec::new();
    let (mut as_name, mut cwd) = (None, None);
    let mut args = argv.iter();
    while let Some(arg) = args.next() {
        let slot = match arg.as_str() {
            "--as" => &mut as_name,
            "--cwd" => &mut cwd,
            flag if flag.starts_with('-') => return None,
            other => {
                positionals.push(other);
                continue;
            }
        };
        if slot.replace(args.next()?.as_str()).is_some() {
            return None;
        }
    }
    match positionals[..] {
        [target] => Some((target, as_name, cwd)),
        _ => None,
    }
}

struct Resume;

const RESUME_USAGE: &str = "usage: txc resume <session> [--as <name>] [--cwd <dir>]\n";

impl Verb for Resume {
    fn name(&self) -> &'static str {
        "resume"
    }

    fn usage(&self) -> &'static str {
        "resume <session> [--as <name>] [--cwd <dir>]   re-create a past session on its chat"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let Some((target, as_name, cwd)) = parse_resume(argv) else {
            say(io.stderr, RESUME_USAGE)?;
            return Ok(2);
        };
        let loc = Locator::from_env();
        match Ops::new(core).resume(target, as_name, cwd.map(Path::new), &loc) {
            Ok(new) => {
                let ext = chat_ext(&new).unwrap_or_default();
                let chat = ext.chats.first();
                let chat_id = chat.and_then(|c| c.id.clone()).unwrap_or_default();
                if chat.is_none_or(|c| c.transcript.is_none()) {
                    say(
                        io.stderr,
                        &format!(
                            "resume: warning — transcript for chat {} not found under {}; claude \
                             --resume may start a fresh conversation\n",
                            short(&chat_id),
                            new.cwd.display()
                        ),
                    )?;
                }
                let old = new
                    .parent
                    .as_ref()
                    .and_then(|parent| core.get_session(parent).ok())
                    .map_or_else(|| target.to_owned(), |s| s.name);
                say(
                    io.stdout,
                    &format!(
                        "Resumed '{old}' as '{}' (chat {}, cwd={})\n",
                        new.name,
                        short(&chat_id),
                        new.cwd.display()
                    ),
                )?;
                Ok(0)
            }
            Err(error) => report(io, "resume", error),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn next_free_name_skips_running_names() {
        let dir = tempfile::tempdir().unwrap();
        let core = TxCore::open(tx_core::CoreConfig::new(tx_core::Directories::under(
            dir.path(),
        )))
        .unwrap();
        let store = tx_core::SessionStore::new(dir.path().join("data/sessions"));
        let record = |id: &str, name: &str, lifecycle: &str| -> Session {
            serde_json::from_value(json!({
                "id": id, "name": name, "parent": null, "tags": [], "cwd": "/tmp",
                "created_at": 1.0, "ended_at": null, "last_modified_at": 1.0, "lifecycle": lifecycle,
                "launch": {"kind": "process", "program": "sh", "args": []}, "ext": {},
            }))
            .unwrap()
        };
        for session in [
            record("s1", "w", "running"),
            record("s2", "w-2", "running"),
            record("s3", "w-3", "stopped"),
            record("s4", "x", "exited"),
        ] {
            store.save(&session).unwrap();
        }
        let ops = Ops::new(&core);
        assert_eq!(ops.next_free_name("w").unwrap(), "w-3");
        assert_eq!(ops.next_free_name("x").unwrap(), "x");
        assert_eq!(ops.next_free_name("new").unwrap(), "new");
    }

    #[test]
    fn the_pending_fork_ref_names_its_origin() {
        let id: SessionId = "s".parse().unwrap();
        let r = pending_fork_ref(&id, "c");
        assert_eq!(r.how, "fork");
        assert!(r.id.is_none());
        assert_eq!(r.origin.unwrap().chat, "c");
    }
}
