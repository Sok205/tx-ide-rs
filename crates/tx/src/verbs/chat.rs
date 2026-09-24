//! Chat-op verbs (cli.py): `fork`, `handover`, `rollover`, `resume`, and the hidden
//! `_chat-op-finish` / `_chat-op-watch` a distiller / the detached watchdog run. Thin façades over
//! [`crate::chat::ChatOps`]: argv parsing + rendering only.
//!
//! Q6 FIX: `resume`'s live-name clash check consults the store (a live record of that name), not
//! only tmux — a worker is tmux-named by its id, so the tmux check alone never saw it.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use cordis::{BoxError, Component, Ctx};

use crate::app::{COMMANDS, Command, ENGINES, ENV, EVENTS, Env, HOME, SERVICE, Visibility};
use crate::argparse::{Arg, Parser};
use crate::chat::{ChatOps, WatchTimings, active_chat};
use crate::engines::EngineRegistry;
use crate::engines::claude::bundle_dir;
use crate::events::{self, EventLog};
use crate::history::History;
use crate::service::SessionService;
use crate::session::{ChatRef, Origin};
use crate::shlex::shlex_join;
use crate::spawn::SpawnSpec;
use crate::storage::Home;
use crate::verbs::common::group_value;

pub struct ChatVerbs;

impl Component for ChatVerbs {
    fn name(&self) -> &str {
        "verbs.chat"
    }
    fn inject(&self) -> &[&'static str] {
        &["commands", "service", "engines", "home", "events", "env"]
    }
    fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        let deps = Deps {
            service: ctx.get(SERVICE)?,
            engines: ctx.get(ENGINES)?,
            home: ctx.get(HOME)?,
            events: ctx.get(EVENTS)?,
            env: ctx.get(ENV)?,
        };
        let table = ctx.get(COMMANDS)?;
        table.register(ctx, Visibility::Public, Resume(deps.clone()));
        table.register(ctx, Visibility::Public, Fork(deps.clone()));
        table.register(ctx, Visibility::Public, Handover(deps.clone()));
        table.register(ctx, Visibility::Public, Rollover(deps.clone()));
        table.register(ctx, Visibility::Hidden, ChatOpFinish(deps.clone()));
        table.register(ctx, Visibility::Hidden, ChatOpWatch(deps));
        Ok(())
    }
}

#[derive(Clone)]
struct Deps {
    service: Rc<SessionService>,
    engines: Rc<RefCell<EngineRegistry>>,
    home: Rc<Home>,
    events: Rc<EventLog>,
    env: Rc<Env>,
}

impl Deps {
    fn ops(&self) -> ChatOps<'_> {
        ChatOps {
            service: &self.service,
            engines: &self.engines,
            home: &self.home,
            events: &self.events,
            env: &self.env,
        }
    }
}

fn parser(command: &dyn Command) -> Parser {
    Parser::new(&format!("tx {}", command.name())).description(command.summary())
}

/// `chat_id[:8]`.
fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

// ----- fork --------------------------------------------------------------------------------

struct Fork(Deps);

impl Command for Fork {
    fn name(&self) -> &'static str {
        "fork"
    }
    fn summary(&self) -> &'static str {
        "Fork a session's chat into a NEW session that starts with the full history (§4)."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = parser(self)
            .arg(Arg::positional("source").help("the session to fork (id or name)"))
            .arg(
                Arg::positional("new_name")
                    .optional()
                    .help("name for the fork (default <source>-fork)"),
            )
            .arg(
                Arg::option("--read-only")
                    .flag()
                    .help("create the new fork in a tx worktree with repository edits blocked"),
            )
            .arg(
                Arg::option("--group")
                    .value_parser(group_value)
                    .help("explicit effort-group override (default: derived via the source)"),
            );
        let args = match parser.parse(argv) {
            Ok(args) => args,
            Err(exit) => return Ok(exit.emit()),
        };
        let source = args.get_one("source").unwrap_or_default();
        let new = self.0.ops().fork(
            source,
            args.get_one("new_name"),
            args.get_flag("read_only"),
            args.get_one("group").map(str::to_owned),
        )?;
        let label = active_chat(&new)
            .and_then(|chat| chat.id.as_deref())
            .map_or_else(|| "pending".to_owned(), short);
        println!(
            "Forked '{source}' → '{}' (chat {label}, cwd={})",
            new.name, new.cwd
        );
        Ok(0)
    }
}

// ----- handover ----------------------------------------------------------------------------

struct Handover(Deps);

impl Command for Handover {
    fn name(&self) -> &'static str {
        "handover"
    }
    fn summary(&self) -> &'static str {
        "Distill a session's chat into a focused brief for a NEW worker session (§5)."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = parser(self)
            .arg(Arg::positional("source").help("the session to hand over from (id or name)"))
            .arg(Arg::positional("task").help("the task to distill a brief for"))
            .arg(
                Arg::positional("new_name")
                    .optional()
                    .help("name for the worker (default <source>-handover)"),
            )
            .arg(
                Arg::option("--self-catch-up")
                    .flag()
                    .help("skip the distiller — the worker reads the source bundle itself (CHD1)"),
            )
            .arg(
                Arg::option("--read-only")
                    .flag()
                    .help("create the new worker in a tx worktree with repository edits blocked"),
            );
        let args = match parser.parse(argv) {
            Ok(args) => args,
            Err(exit) => return Ok(exit.emit()),
        };
        let source = args.get_one("source").unwrap_or_default();
        let self_catch_up = args.get_flag("self_catch_up");
        let worker = self.0.ops().handover(
            source,
            args.get_one("task").unwrap_or_default(),
            args.get_one("new_name"),
            self_catch_up,
            args.get_flag("read_only"),
        )?;
        let how = if self_catch_up {
            "self-catch-up"
        } else {
            "distilling brief"
        };
        println!("Handover '{source}' → worker '{worker}' ({how}; launches when ready)");
        Ok(0)
    }
}

// ----- rollover ----------------------------------------------------------------------------

struct Rollover(Deps);

impl Command for Rollover {
    fn name(&self) -> &'static str {
        "rollover"
    }
    fn summary(&self) -> &'static str {
        "Rotate a session onto a fresh chat in the SAME pane (context exhausted) (§6)."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = parser(self)
            .arg(
                Arg::positional("session")
                    .optional()
                    .help("the session to roll over (default: the one you are in)"),
            )
            .arg(
                Arg::option("--self-catch-up")
                    .flag()
                    .help("skip the distiller — the successor reads the bundle itself (CHD1)"),
            );
        let args = match parser.parse(argv) {
            Ok(args) => args,
            Err(exit) => return Ok(exit.emit()),
        };
        let self_catch_up = args.get_flag("self_catch_up");
        self.0
            .ops()
            .rollover(args.get_one("session"), self_catch_up)?;
        let how = if self_catch_up {
            "self-catch-up"
        } else {
            "summarizing first"
        };
        println!(
            "Rollover scheduled ({how}); the same session rotates onto a fresh chat when ready"
        );
        Ok(0)
    }
}

// ----- hidden finish / watch ---------------------------------------------------------------

struct ChatOpFinish(Deps);

impl Command for ChatOpFinish {
    fn name(&self) -> &'static str {
        "_chat-op-finish"
    }
    fn summary(&self) -> &'static str {
        "Internal: complete a handover/rollover from its op-spec (idempotent, CHD5)."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = parser(self).arg(Arg::positional("op_id"));
        let args = match parser.parse(argv) {
            Ok(args) => args,
            Err(exit) => return Ok(exit.emit()),
        };
        self.0
            .ops()
            .chat_op_finish(args.get_one("op_id").unwrap_or_default())?;
        Ok(0)
    }
}

struct ChatOpWatch(Deps);

impl Command for ChatOpWatch {
    fn name(&self) -> &'static str {
        "_chat-op-watch"
    }
    fn summary(&self) -> &'static str {
        "Internal: detached watchdog — finish a chat-op if its distiller flakes, then tear down."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = parser(self).arg(Arg::positional("op_id"));
        let args = match parser.parse(argv) {
            Ok(args) => args,
            Err(exit) => return Ok(exit.emit()),
        };
        let timings = WatchTimings::from_env(&self.0.env);
        self.0
            .ops()
            .chat_op_watch(args.get_one("op_id").unwrap_or_default(), timings)?;
        Ok(0)
    }
}

// ----- resume ------------------------------------------------------------------------------

struct Resume(Deps);

impl Command for Resume {
    fn name(&self) -> &'static str {
        "resume"
    }
    fn summary(&self) -> &'static str {
        "Re-spawn a past session + reattach its chat (claude --resume); collision-safe (§7)."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = parser(self)
            .arg(Arg::positional("target").help("the past session to resume (id or name)"))
            .arg(
                Arg::option("--as")
                    .dest("new_name")
                    .metavar("NAME")
                    .help("spawn under a new name (required on a live-name clash)"),
            )
            .arg(
                Arg::option("--cwd")
                    .metavar("DIR")
                    .help("override the cwd (required if the stored cwd is gone — C8)"),
            );
        let args = match parser.parse(argv) {
            Ok(args) => args,
            Err(exit) => return Ok(exit.emit()),
        };
        let deps = &self.0;
        let service = &deps.service;
        let target = args.get_one("target").unwrap_or_default();

        let Some(record) = service.get(target)? else {
            eprintln!("tx resume: no record for '{target}'");
            return Ok(1);
        };
        let Some(chat) = active_chat(&record).cloned() else {
            eprintln!(
                "tx resume: '{}' has no chat to resume — use `tx spawn` for a fresh session",
                record.name
            );
            return Ok(1);
        };
        let chat_id = chat.id.clone().unwrap_or_default();

        let name = args
            .get_one("new_name")
            .filter(|name| !name.is_empty())
            .unwrap_or(&record.name)
            .to_owned();
        // Q6 FIX: a live RECORD of that name clashes too (workers are tmux-named by id).
        if service.tmux().has_session(&name) || service.is_live_name(&name)? {
            eprintln!(
                "tx resume: a live session named '{name}' already exists — pass --as <new-name>"
            );
            return Ok(1);
        }

        // Transcripts are keyed by the munged cwd (C8): restore the stored cwd by default.
        let explicit_cwd = args.get_one("cwd").filter(|cwd| !cwd.is_empty());
        let cwd = explicit_cwd.unwrap_or(&record.cwd).to_owned();
        if !Path::new(&cwd).is_dir() {
            let remedy = match explicit_cwd {
                None => "pass --cwd <dir>".to_owned(),
                Some(_) => format!("'{cwd}' is not a directory"),
            };
            eprintln!("tx resume: cwd '{cwd}' does not exist — {remedy} (C8)");
            return Ok(1);
        }

        let Some(llm) = record.llm() else {
            return Err(format!("'{}' is not an llm session", record.name).into());
        };
        let engine = llm.engine;
        let adapter = deps
            .engines
            .borrow()
            .get(engine)
            .ok_or_else(|| format!("no engine adapter is registered for {engine}"))?;
        let transcript = {
            let engines = deps.engines.borrow();
            History::new(&deps.home, &engines).resolve_transcript(&chat_id, Some(&cwd), engine)
        };
        if transcript.is_none() {
            eprintln!(
                "tx resume: warning — transcript for chat {} not found under {cwd}; claude \
                 --resume may start a fresh conversation",
                short(&chat_id)
            );
        }

        let resume_cmd = shlex_join(&adapter.resume_command(
            &chat_id,
            Some(&record.initial_cmd),
            record.read_only(),
        )?);
        // parent = the SOURCE record (the work ancestor), not whoever ran `tx resume`.
        let mut spec = SpawnSpec::for_process(
            &name,
            record.tags.clone(),
            &cwd,
            resume_cmd,
            &deps.engines.borrow(),
        );
        spec.env = record.spawn_env.clone();
        spec.records_own_chat = true;
        spec.engine = Some(engine);
        spec.read_only = record.read_only();
        spec.parent = Some(record.id.clone());
        let mut new = service.spawn_worker_with(spec, name == record.name, |prepared| {
            adapter
                .prepare_chat_for_cwd(&chat_id, &chat.cwd, &prepared.cwd)
                .map_err(Into::into)
        })?;

        // role=original (it continues the source's conversation); origin.how="resume".
        let new_engine = new.llm().map_or(engine, |llm| llm.engine);
        let new_adapter = deps
            .engines
            .borrow()
            .get(new_engine)
            .ok_or_else(|| format!("no engine adapter is registered for {new_engine}"))?;
        let resumed = ChatRef {
            id: Some(chat_id.clone()),
            role: "original".to_owned(),
            cwd: new.cwd.clone(),
            transcript_path: new_adapter
                .resolve_transcript(&chat_id, &new.cwd)
                .to_string_lossy()
                .into_owned(),
            origin: Origin {
                how: "resume".to_owned(),
                session_id: new.id.clone(),
                chat_id: Some(chat_id.clone()),
            },
            bundle_path: Some(
                bundle_dir(&deps.home, &new.id, &chat_id)
                    .to_string_lossy()
                    .into_owned(),
            ),
            started_at: serde_json::Number::from_f64(events::now()),
            ended_at: None,
            summary: String::new(),
            engine: Some(new_engine),
        };
        if let Some(llm) = new.llm_mut() {
            llm.chats.push(resumed);
        }
        service.store().save(&new)?;
        println!(
            "Resumed '{}' as '{}' (chat {}, cwd={})",
            record.name,
            new.name,
            short(&chat_id),
            new.cwd
        );
        Ok(0)
    }
}
