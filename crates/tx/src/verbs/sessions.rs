//! The session verbs of `cli.py`: the spawn family, the record mutations, messaging, `migrate`,
//! and the hidden seams (`_tmux-name`, `_pane-info`, `focus-envelope`, `selfcheck`, `_relabel`,
//! `_codex-update`). Plus Q32's `revive` (hidden: the reference's help lists 25 verbs).

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use cordis::{BoxError, Component, Ctx};
use serde_json::Number;

use crate::app::{COMMANDS, Command, ENGINES, ENV, EVENTS, Env, HOME, SERVICE, Visibility};
use crate::argparse::{Arg, Matches, Parser};
use crate::artifact::ARTIFACT_SCHEMA_VERSION;
use crate::artifact_store::ArtifactStore;
use crate::engines::EngineRegistry;
use crate::engines::adapter::{Effort, LaunchOptions, env_set};
use crate::engines::codex_update;
use crate::events::{self, EventLog};
use crate::grouping::GroupResolver;
use crate::history::{History, IngestMode};
use crate::migrations::{migrate_artifacts, migrate_sessions};
use crate::roles::{RoleError, load_role_priming};
use crate::service::{Revival, ServiceError, SessionService};
use crate::session::{
    ChatRef, Engine, LlmSession, Origin, Role, SCHEMA_VERSION, Session, SessionKind, State,
};
use crate::shlex::shlex_join;
use crate::skills::{SKILLS_ENV, resolve_role_skills};
use crate::spawn::{SpawnSpec, infer_role};
use crate::storage::Home;

use super::common::{default_cwd, default_shell, env_pair, group_value, parse_env, split_tags};

/// Everything the session verbs reach.
struct Deps {
    service: Rc<SessionService>,
    env: Rc<Env>,
    home: Rc<Home>,
    engines: Rc<RefCell<EngineRegistry>>,
    events: Rc<EventLog>,
}

type Run = fn(&Deps, Parser, &[String]) -> Result<i32, BoxError>;

/// One `tx <verb>`: its parser starts as `ArgumentParser(prog="tx <verb>", description=summary)`.
struct Verb {
    name: &'static str,
    summary: &'static str,
    deps: Rc<Deps>,
    run: Run,
}

impl Command for Verb {
    fn name(&self) -> &'static str {
        self.name
    }
    fn summary(&self) -> &'static str {
        self.summary
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = Parser::new(&format!("tx {}", self.name)).description(self.summary);
        (self.run)(&self.deps, parser, argv)
    }
}

const VERBS: &[(&str, &str, Visibility, Run)] = &[
    (
        "spawn",
        "Spawn a detached tmux session (--tag mandatory).",
        Visibility::Public,
        spawn,
    ),
    (
        "spawn-nvim",
        "Spawn a detached nvim companion (--diff opens a diffview; --open opens a file).",
        Visibility::Public,
        spawn_nvim,
    ),
    (
        "spawn-view",
        "Spawn a detached view session (a live @tx_view tmux home, not a store record).",
        Visibility::Public,
        spawn_view,
    ),
    (
        "tag",
        "Read or set a session's tags (comma-separated).",
        Visibility::Public,
        tag,
    ),
    (
        "group",
        "Read or set a session's effort-group override (--clear returns to derived).",
        Visibility::Public,
        group,
    ),
    (
        "rename",
        "Rename a session's display name (record; a process leaves its tmux id untouched).",
        Visibility::Public,
        rename,
    ),
    (
        "whoami",
        "Print the current session's display name (resolves #S — the id for a process).",
        Visibility::Public,
        whoami,
    ),
    (
        "send-message",
        "Peer-message another agent session (delivered in a <from-agent> envelope).",
        Visibility::Public,
        send_message,
    ),
    (
        "send-user-message",
        "Message another session as the operator (<from-user>, not a peer agent).",
        Visibility::Public,
        send_user_message,
    ),
    (
        "kill",
        "End a tmux session and mark its record EXITED.",
        Visibility::Public,
        kill,
    ),
    (
        "archive",
        "Retire a session (mark ARCHIVED, keep the record) + force a full history ingest.",
        Visibility::Public,
        archive,
    ),
    (
        "rm",
        "Delete a session record (by id or name).",
        Visibility::Public,
        rm,
    ),
    (
        "migrate",
        "Upgrade $TX_IDE_HOME records to the current schemas (sessions v3→v6, artifacts v1→v2).",
        Visibility::Public,
        migrate,
    ),
    (
        "revive",
        "Revive an EXITED record whose tmux session is still live (back to its initial state).",
        Visibility::Hidden,
        revive,
    ),
    (
        "focus-envelope",
        "Internal: build the tx-assistant context envelope for a pane (M-focus, S6).",
        Visibility::Hidden,
        focus_envelope,
    ),
    (
        "_pane-info",
        "Internal: a record's display name + comma-joined tags (pane-border reader; two lines).",
        Visibility::Hidden,
        pane_info,
    ),
    (
        "_tmux-name",
        "Internal: the LIVE tmux target (id for a process) for a name/id; empty if not live.",
        Visibility::Hidden,
        tmux_name,
    ),
    (
        "selfcheck",
        "Internal: round-trip a record through SessionStore + exercise the C3 terminal guard.",
        Visibility::Hidden,
        selfcheck,
    ),
    (
        "_relabel",
        "Internal: mirror display name + tags into @tx_name on every live session (choose-tree).",
        Visibility::Hidden,
        relabel,
    ),
    (
        codex_update::UPDATE_VERB,
        "Internal: download and run the Codex installer (the detached update child).",
        Visibility::Hidden,
        codex_update_child,
    ),
];

pub struct SessionVerbs;

impl Component for SessionVerbs {
    fn name(&self) -> &str {
        "verbs.sessions"
    }
    fn inject(&self) -> &[&'static str] {
        &["commands", "service", "env", "home", "engines", "events"]
    }
    fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        let deps = Rc::new(Deps {
            service: ctx.get(SERVICE)?,
            env: ctx.get(ENV)?,
            home: ctx.get(HOME)?,
            engines: ctx.get(ENGINES)?,
            events: ctx.get(EVENTS)?,
        });
        let table = ctx.get(COMMANDS)?;
        for &(name, summary, visibility, run) in VERBS {
            table.register(
                ctx,
                visibility,
                Verb {
                    name,
                    summary,
                    deps: Rc::clone(&deps),
                    run,
                },
            );
        }
        Ok(())
    }
}

/// `parser.parse_args(argv)`: on `-h` / a usage error, emit it and return its exit code.
macro_rules! parse {
    ($parser:expr, $argv:expr) => {
        match $parser.parse($argv) {
            Ok(matches) => matches,
            Err(exit) => return Ok(exit.emit()),
        }
    };
}

/// A positional argparse always fills (`""` only if absent, which parsing rules out).
fn required<'a>(matches: &'a Matches, dest: &str) -> &'a str {
    matches.get_one(dest).unwrap_or_default()
}

/// `args.<opt> or <fallback>` for a string option (an empty value is falsy).
fn or_else(matches: &Matches, dest: &str, fallback: impl FnOnce() -> String) -> String {
    match matches.get_one(dest).filter(|value| !value.is_empty()) {
        Some(value) => value.to_owned(),
        None => fallback(),
    }
}

fn group_arg() -> Arg {
    Arg::option("--group")
        .value_parser(group_value)
        .help("explicit effort-group override (default: derived at read time)")
}

fn env_arg() -> Arg {
    Arg::option("--env").append().value_parser(env_pair)
}

fn env_values(matches: &Matches) -> Vec<(String, String)> {
    parse_env(matches.get_many("env").unwrap_or_default())
}

// ----- spawn family ------------------------------------------------------------------------

fn spawn(deps: &Deps, parser: Parser, argv: &[String]) -> Result<i32, BoxError> {
    let parser = parser
        .arg(Arg::positional("name"))
        .arg(Arg::option("--tag").required())
        .arg(group_arg())
        .arg(Arg::option("--cwd"))
        .arg(Arg::option("--cmd").help(
            "a full, hand-written launch command (shell / nvim / other, or an explicit agent \
             command); cannot combine with --prompt/--model/--effort",
        ))
        .arg(
            Arg::option("--engine")
                .choices(&[
                    Engine::Claude.as_str(),
                    Engine::Codex.as_str(),
                    Engine::Antigravity.as_str(),
                ])
                .help(
                    "build the launch command for this agent engine via its adapter (default: \
                     claude). With --cmd, declares the engine to stamp on the record (the command \
                     stays yours; the engine is never inferred from it)",
                ),
        )
        .arg(
            Arg::option("--prompt").help(
                "initial/priming prompt for an --engine agent spawn (auto-submits in the TUI)",
            ),
        )
        .arg(Arg::option("--model").help("model override for an --engine agent spawn"))
        .arg(
            Arg::option("--effort")
                .int_choices(1..6)
                .metavar("{1,2,3,4,5}")
                .help("reasoning-effort tier for an --engine agent spawn (default: 3)"),
        )
        .arg(Arg::option("--role").append().metavar("NAME[,NAME…]").help(
            "role file(s) to inject additively into the agent's system prompt (COMMON is \
                 always included first). NAME resolves to user-agents/NAME.md (replaces) or \
                 agents/NAME.md, plus user-agents/NAME.local.md (extends)",
        ))
        .arg(
            Arg::option("--read-only")
                .flag()
                .help("run an engine-built agent in a tx worktree with repository edits blocked"),
        )
        .arg(Arg::option("--chrome").flag().help(
            "grant the engine's browser-automation tooling (off by default — on claude it costs \
             ~1,300 tokens of standing system prompt per session)",
        ))
        .arg(env_arg());
    let matches = parse!(parser, argv);
    let tag = required(&matches, "tag");
    let tags = split_tags(tag);
    if tags.is_empty() {
        return Ok(parser.error("--tag requires at least one value").emit());
    }
    let has_cmd = matches.get_one("cmd").is_some();
    let read_only = matches.get_flag("read_only");
    // Q16 FIX: an explicit (even empty) --prompt is a launch-building flag.
    let builds_launch = matches.get_one("prompt").is_some()
        || matches
            .get_one("model")
            .is_some_and(|model| !model.is_empty())
        || matches.get_int("effort").is_some()
        || matches
            .get_many("role")
            .is_some_and(|roles| !roles.is_empty());
    if has_cmd && builds_launch {
        return Ok(parser
            .error(
                "--prompt/--model/--effort/--role build a launch command and cannot be combined \
                 with --cmd (the full hand-written command)",
            )
            .emit());
    }
    if read_only && has_cmd {
        return Ok(parser
            .error("--read-only requires an engine-built launch; it cannot enforce --cmd")
            .emit());
    }
    if matches.get_flag("chrome") && has_cmd {
        return Ok(parser
            .error("--chrome requires an engine-built launch; put the engine's own flag in --cmd")
            .emit());
    }
    let mut environment = env_values(&matches);
    let (command, engine) = match resolve_command(deps, &matches, &mut environment) {
        Ok(resolved) => resolved,
        Err(ResolveError::Role(error)) => return Ok(parser.error(&error.to_string()).emit()),
        Err(ResolveError::Other(error)) => return Err(error),
    };
    let role = infer_role(&command, &deps.engines.borrow());
    if read_only && role != Role::Llm {
        return Ok(parser.error("--read-only requires an agent launch").emit());
    }
    let cwd = or_else(&matches, "cwd", || {
        default_cwd(deps.service.tmux(), &deps.env)
    });
    let mut spec = SpawnSpec::for_process(
        required(&matches, "name"),
        tags,
        cwd,
        command,
        &deps.engines.borrow(),
    );
    spec.env = environment;
    spec.engine = engine;
    spec.read_only = read_only;
    spec.group = matches.get_one("group").map(str::to_owned);
    let session = if role == Role::Llm {
        deps.service.spawn_worker(spec, false)?
    } else {
        deps.service.spawn(spec)?
    };
    println!(
        "Spawned '{}' (cwd={}, tag={tag})",
        session.name, session.cwd
    );
    Ok(0)
}

enum ResolveError {
    /// A `RoleError`: the CLI turns it into an argparse error (exit 2).
    Role(RoleError),
    Other(BoxError),
}

impl From<RoleError> for ResolveError {
    fn from(error: RoleError) -> Self {
        Self::Role(error)
    }
}

/// `_resolve_command`: `--cmd` verbatim (engine = `--engine`, else inferred from its binary); an
/// agent spawn built by the engine adapter; else a login shell.
fn resolve_command(
    deps: &Deps,
    matches: &Matches,
    environment: &mut Vec<(String, String)>,
) -> Result<(String, Option<Engine>), ResolveError> {
    let requested = matches
        .get_one("engine")
        .map(str::parse::<Engine>)
        .transpose()
        .map_err(|error| ResolveError::Other(error.into()))?;
    let engines = deps.engines.borrow();
    if let Some(cmd) = matches.get_one("cmd") {
        return Ok((
            cmd.to_owned(),
            requested.or_else(|| engines.engine_for_command(cmd)),
        ));
    }
    let roles = matches.get_many("role");
    let role_names: Vec<String> = roles
        .unwrap_or_default()
        .iter()
        .flat_map(|value| split_tags(value))
        .collect();
    if roles.is_some() && role_names.is_empty() {
        return Err(RoleError::EmptyRoleList.into());
    }
    let prompt = matches.get_one("prompt");
    let model = matches.get_one("model");
    let effort = matches.get_int("effort");
    if requested.is_none()
        && prompt.is_none()
        && model.is_none()
        && effort.is_none()
        && role_names.is_empty()
    {
        return Ok((default_shell(&deps.env), None));
    }
    let engine = requested.unwrap_or(Engine::Claude);
    // The grant travels on the launch env so prepare_workspace can link the skills.
    let skill_names = resolve_role_skills(&deps.home, &role_names)?;
    if !skill_names.is_empty() {
        env_set(environment, SKILLS_ENV, skill_names.join(","));
    }
    let priming = if role_names.is_empty() {
        None
    } else {
        Some(load_role_priming(&deps.home, &role_names)?)
    };
    let adapter = engines
        .get(engine)
        .ok_or_else(|| ResolveError::Other(ServiceError::EngineNotRegistered(engine).into()))?;
    let options = LaunchOptions {
        model,
        effort: effort
            .and_then(|level| u8::try_from(level).ok())
            .and_then(Effort::from_level),
        initial_prompt: prompt,
        read_only: matches.get_flag("read_only"),
        browser: matches.get_flag("chrome"),
        role_priming: priming.as_deref(),
    };
    let argv = adapter
        .build_launch_command(&options, environment)
        .map_err(|error| ResolveError::Other(error.into()))?;
    Ok((shlex_join(&argv), Some(engine)))
}

fn spawn_nvim(deps: &Deps, parser: Parser, argv: &[String]) -> Result<i32, BoxError> {
    let parser = parser
        .arg(Arg::positional("name"))
        .arg(Arg::option("--tag").required().help(
            "scope tag(s), comma-separated — a companion takes the same tag as the session it \
             belongs to, so the pair surfaces together in the operator's filters",
        ))
        .arg(group_arg())
        .arg(Arg::option("--cwd").help("working directory (default: the firing pane's)"))
        .arg(Arg::option("--diff").optional().constant("main").help(
            "open a diffview of the worktree against BASE (default: main) — prefer the \
             merge-base over a branch name, which shows commits you lack as deletions once the \
             branch moves ahead",
        ))
        .arg(Arg::option("--open").help("open FILE on startup"))
        .arg(env_arg());
    let matches = parse!(parser, argv);
    let tag = required(&matches, "tag");
    let tags = split_tags(tag);
    if tags.is_empty() {
        return Ok(parser.error("--tag requires at least one value").emit());
    }
    let diff = matches.get_one("diff");
    let open = matches.get_one("open");
    let cwd = or_else(&matches, "cwd", || {
        default_cwd(deps.service.tmux(), &deps.env)
    });
    let mut spec = SpawnSpec::for_nvim(required(&matches, "name"), tags, cwd, diff, open);
    spec.env = env_values(&matches);
    spec.group = matches.get_one("group").map(str::to_owned);
    let session = deps.service.spawn_nvim(spec)?;
    let details: Vec<String> = [("diff", diff), ("open", open)]
        .into_iter()
        .filter_map(|(label, value)| value.map(|value| format!("{label}={value}")))
        .collect();
    let suffix = if details.is_empty() {
        String::new()
    } else {
        format!(", {}", details.join(", "))
    };
    println!(
        "Spawned nvim '{}' (cwd={}, tag={tag}{suffix})",
        session.name, session.cwd
    );
    Ok(0)
}

fn spawn_view(deps: &Deps, parser: Parser, argv: &[String]) -> Result<i32, BoxError> {
    let parser = parser
        .arg(Arg::positional("name"))
        .arg(Arg::option("--cwd"))
        .arg(Arg::option("--cmd"))
        .arg(env_arg());
    let matches = parse!(parser, argv);
    let cwd = or_else(&matches, "cwd", || {
        default_cwd(deps.service.tmux(), &deps.env)
    });
    let cmd = or_else(&matches, "cmd", || default_shell(&deps.env));
    let mut spec =
        SpawnSpec::for_view(required(&matches, "name"), cwd, cmd, &deps.engines.borrow());
    spec.env = env_values(&matches);
    let cwd = spec.cwd.clone();
    let name = deps.service.spawn_view(spec)?;
    println!("Spawned view '{name}' (cwd={cwd})");
    Ok(0)
}

// ----- mutations ---------------------------------------------------------------------------

fn tag(deps: &Deps, parser: Parser, argv: &[String]) -> Result<i32, BoxError> {
    let parser = parser
        .arg(Arg::positional("name"))
        .arg(Arg::positional("tags").optional());
    let matches = parse!(parser, argv);
    let name = required(&matches, "name");
    let Some(tags) = matches.get_one("tags") else {
        let Some(session) = deps.service.get(name)? else {
            eprintln!("tx tag: session '{name}' not found");
            return Ok(1);
        };
        println!("{}", session.tags.join(","));
        return Ok(0);
    };
    let session = deps.service.tag(name, split_tags(tags))?;
    println!("Tagged '{}' (tag={tags})", session.name);
    Ok(0)
}

fn group(deps: &Deps, parser: Parser, argv: &[String]) -> Result<i32, BoxError> {
    let parser = parser
        .arg(Arg::positional("name"))
        .arg(
            Arg::positional("group")
                .optional()
                .help("the explicit group to set"),
        )
        .arg(
            Arg::option("--clear")
                .flag()
                .help("drop the override — back to derived"),
        );
    let matches = parse!(parser, argv);
    let name = required(&matches, "name");
    let clear = matches.get_flag("clear");
    let group = matches.get_one("group");
    if clear && group.is_some() {
        return Ok(parser.error("give a group or --clear, not both").emit());
    }
    if group == Some("") {
        return Ok(parser
            .error("a group cannot be empty — use --clear to drop the override")
            .emit());
    }
    if clear {
        let session = deps.service.set_group(name, None)?;
        println!(
            "Cleared group override on '{}' (back to derived)",
            session.name
        );
        return Ok(0);
    }
    if let Some(group) = group {
        let session = deps.service.set_group(name, Some(group.to_owned()))?;
        println!("Grouped '{}' (group={group})", session.name);
        return Ok(0);
    }
    let Some(session) = deps.service.get(name)? else {
        eprintln!("tx group: session '{name}' not found");
        return Ok(1);
    };
    let sessions = deps.service.store().all();
    let scan = ArtifactStore::new(&deps.home).all();
    for skipped in &scan.skipped {
        eprintln!("{}", skipped.warning());
    }
    let resolver = GroupResolver::new(&sessions, &scan.artifacts);
    println!("own:      {}", session.group.as_deref().unwrap_or("—"));
    println!("resolved: {}", resolver.session_group(&session));
    Ok(0)
}

fn rename(deps: &Deps, parser: Parser, argv: &[String]) -> Result<i32, BoxError> {
    let parser = parser
        .arg(Arg::positional("name"))
        .arg(Arg::positional("new_name"));
    let matches = parse!(parser, argv);
    let session = deps
        .service
        .rename(required(&matches, "name"), required(&matches, "new_name"))?;
    println!("Renamed to '{}'", session.name);
    Ok(0)
}

/// `#S` is a process's id; print the store-owned display name (raw `#S` when untracked).
fn whoami(deps: &Deps, parser: Parser, argv: &[String]) -> Result<i32, BoxError> {
    parse!(parser, argv);
    let Some(current) = deps.service.tmux().current_session_name() else {
        eprintln!("tx whoami: not inside a tmux session");
        return Ok(1);
    };
    match deps.service.get(&current)? {
        Some(record) => println!("{}", record.name),
        None => println!("{current}"),
    }
    Ok(0)
}

fn kill(deps: &Deps, parser: Parser, argv: &[String]) -> Result<i32, BoxError> {
    let parser = parser.arg(Arg::positional("name"));
    let matches = parse!(parser, argv);
    let name = required(&matches, "name");
    // `None` when it ended a view (a live @tx_view session, not a record) — Q3.
    let session = deps.service.kill(name)?;
    println!(
        "Killed '{}'",
        session
            .as_ref()
            .map_or(name, |session| session.name.as_str())
    );
    Ok(0)
}

/// Retire + a blocking, complete history mirror (the bundle is final after this).
fn archive(deps: &Deps, parser: Parser, argv: &[String]) -> Result<i32, BoxError> {
    let parser = parser.arg(Arg::positional("name"));
    let matches = parse!(parser, argv);
    let session = deps.service.archive(required(&matches, "name"))?;
    let engines = deps.engines.borrow();
    let bundles = History::new(&deps.home, &engines).ingest_session(
        deps.service.store(),
        &session.id,
        IngestMode::Wait,
    )?;
    let suffix = if bundles.is_empty() {
        String::new()
    } else {
        format!(" (ingested {} chat bundle(s))", bundles.len())
    };
    println!("Archived '{}'{suffix}", session.name);
    Ok(0)
}

fn rm(deps: &Deps, parser: Parser, argv: &[String]) -> Result<i32, BoxError> {
    let parser = parser.arg(Arg::positional("target"));
    let matches = parse!(parser, argv);
    let target = required(&matches, "target");
    if deps.service.remove(target)? {
        println!("Removed record for '{target}'");
        return Ok(0);
    }
    eprintln!("tx rm: no record for '{target}'");
    Ok(1)
}

/// Q32 FIX: the explicit way back for an exited record whose `@tx_id` session is still live.
fn revive(deps: &Deps, parser: Parser, argv: &[String]) -> Result<i32, BoxError> {
    let parser = parser.arg(Arg::positional("name"));
    let matches = parse!(parser, argv);
    match deps.service.revive(required(&matches, "name"))? {
        Revival::Revived(session) => println!("Revived '{}'", session.name),
        Revival::AlreadyLive(session) => println!("'{}' is already live", session.name),
    }
    Ok(0)
}

// ----- messaging ---------------------------------------------------------------------------

fn send_message(deps: &Deps, parser: Parser, argv: &[String]) -> Result<i32, BoxError> {
    let parser = parser
        .arg(Arg::positional("target").help("the recipient's display name, as `tx ls` prints it"))
        .arg(Arg::positional("body").help(
            "single-line message — escape literal newlines as \\n; the recipient receives it \
             wrapped in a <from-agent session='<your-name>'> envelope",
        ));
    let matches = parse!(parser, argv);
    deps.service
        .send_message(required(&matches, "target"), required(&matches, "body"))?;
    Ok(0)
}

fn send_user_message(deps: &Deps, parser: Parser, argv: &[String]) -> Result<i32, BoxError> {
    let parser = parser
        .arg(Arg::positional("target"))
        .arg(Arg::positional("body"));
    let matches = parse!(parser, argv);
    deps.service
        .send_user_message(required(&matches, "target"), required(&matches, "body"))?;
    Ok(0)
}

// ----- schema migration --------------------------------------------------------------------

fn migrate(deps: &Deps, parser: Parser, argv: &[String]) -> Result<i32, BoxError> {
    parse!(parser, argv);
    let sessions = migrate_sessions(&deps.home.sessions_dir(), deps.service.tmux())?;
    for name in &sessions.migrated {
        println!("  migrated {name} → v{SCHEMA_VERSION}");
    }
    for name in &sessions.views_removed {
        println!("  view     {name} → stamped @tx_view, record removed");
    }
    for (name, reason) in &sessions.skipped {
        println!("  skipped  {name} ({reason})");
    }
    println!(
        "migrated {} record(s) to v{SCHEMA_VERSION}; retired {} view record(s); left {} untouched.",
        sessions.migrated.len(),
        sessions.views_removed.len(),
        sessions.skipped.len()
    );
    let artifacts = migrate_artifacts(&deps.home.artifacts_dir());
    for name in &artifacts.migrated {
        println!("  migrated {name} → artifact v{ARTIFACT_SCHEMA_VERSION}");
    }
    for (name, reason) in &artifacts.skipped {
        println!("  skipped  {name} ({reason})");
    }
    println!(
        "migrated {} artifact record(s) to v{ARTIFACT_SCHEMA_VERSION}; left {} untouched.",
        artifacts.migrated.len(),
        artifacts.skipped.len()
    );
    Ok(0)
}

// ----- internal seams ----------------------------------------------------------------------

/// Printed without a trailing newline: `bin/tx-assistant` prepends it to the input line.
fn focus_envelope(deps: &Deps, parser: Parser, argv: &[String]) -> Result<i32, BoxError> {
    let parser = parser.arg(Arg::positional("pane"));
    let matches = parse!(parser, argv);
    let envelope = deps.service.focus_envelope(required(&matches, "pane"))?;
    let mut stdout = std::io::stdout();
    stdout.write_all(envelope.as_bytes())?;
    stdout.flush()?;
    Ok(0)
}

/// The pane border's single read: line 1 the display name, line 2 the tags; both empty for an
/// absent or unreadable record (a bad record never breaks the border).
fn pane_info(deps: &Deps, parser: Parser, argv: &[String]) -> Result<i32, BoxError> {
    let parser = parser.arg(Arg::positional("session_id"));
    let matches = parse!(parser, argv);
    let session_id = required(&matches, "session_id");
    let record = if session_id.is_empty() {
        None
    } else {
        deps.service.store().load(session_id).ok().flatten()
    };
    match record {
        Some(record) => println!("{}\n{}", record.name, record.tags.join(",")),
        None => println!("\n"),
    }
    Ok(0)
}

/// A name/id → its live tmux target; empty (exit 0) when no live record matches.
fn tmux_name(deps: &Deps, parser: Parser, argv: &[String]) -> Result<i32, BoxError> {
    let parser = parser.arg(Arg::positional("token"));
    let matches = parse!(parser, argv);
    if let Some(record) = deps.service.get(required(&matches, "token"))?
        && deps.service.tmux().has_session(record.tmux_name())
    {
        println!("{}", record.tmux_name());
    }
    Ok(0)
}

/// `bin/tmux-session-relabel`: `@tx_name` = name (+ ` [tags]`) on every record whose session is
/// live. A read-only display mirror; a session gone between scan and set is skipped.
fn relabel(deps: &Deps, parser: Parser, argv: &[String]) -> Result<i32, BoxError> {
    parse!(parser, argv);
    let tmux = deps.service.tmux();
    let live = tmux.list_sessions("#{session_name}");
    for session in deps.service.store().all() {
        if !live.iter().any(|name| name == session.tmux_name()) {
            continue;
        }
        let mut label = session.name.clone();
        if !session.tags.is_empty() {
            label.push_str(&format!(" [{}]", session.tags.join(",")));
        }
        let target = format!("={}:", session.tmux_name());
        // Best effort: the session may have vanished since the scan.
        let _ = tmux.set_option(&target, "@tx_name", &label, false);
    }
    Ok(0)
}

fn codex_update_child(_deps: &Deps, _parser: Parser, _argv: &[String]) -> Result<i32, BoxError> {
    Ok(codex_update::run_update())
}

/// The S0 smoke test: round-trip a demo record through the store, exercise the C3 guard, clean
/// up. Touches only the store and the log.
fn selfcheck(deps: &Deps, _parser: Parser, _argv: &[String]) -> Result<i32, BoxError> {
    deps.home.ensure()?;
    let store = deps.service.store();
    let mut failures: Vec<&str> = Vec::new();
    let mut check = |condition: bool, label: &'static str| {
        if !condition {
            failures.push(label);
        }
    };

    let session_id = uuid::Uuid::new_v4().to_string();
    let now = Number::from_f64(events::now());
    let cwd = deps.home.root().to_string_lossy().into_owned();
    let demo = Session {
        id: session_id.clone(),
        name: "s1a-selfcheck".into(),
        state: State::initial_for(Role::Llm),
        cwd: cwd.clone(),
        initial_cmd: "claude --dangerously-skip-permissions".into(),
        tags: vec!["s1a".into(), "selfcheck".into()],
        group: None,
        spawn_env: Vec::new(),
        parent: None,
        pid: None,
        attached_to: Vec::new(),
        created_at: now.clone(),
        ended_at: None,
        schema_version: Number::from(SCHEMA_VERSION),
        kind: SessionKind::Llm(LlmSession {
            engine: Engine::Claude,
            chats: vec![ChatRef {
                id: None,
                role: "original".into(),
                cwd,
                transcript_path: String::new(),
                origin: Origin {
                    how: "spawn".into(),
                    session_id: session_id.clone(),
                    chat_id: None,
                },
                bundle_path: None,
                started_at: now.clone(),
                ended_at: None,
                summary: String::new(),
                engine: Some(Engine::Claude),
            }],
            last_activity: now,
            turn_started_at: None,
        }),
    };

    check(
        demo.state == State::Idle,
        "llm spawn state is IDLE (initial_for)",
    );
    store.save(&demo)?;
    deps.events.append(
        "selfcheck",
        &format!("created {} ({session_id})", demo.name),
        deps.env.actor(),
    )?;

    let loaded = store.load(&session_id)?;
    check(loaded.is_some(), "load returns the saved record");
    if let Some(mut loaded) = loaded {
        check(loaded == demo, "save → load round-trips identically");
        check(
            loaded.transition_to(State::Working),
            "IDLE → WORKING applies",
        );
        check(
            loaded.transition_to(State::Exited),
            "WORKING → EXITED applies",
        );
        check(
            !loaded.transition_to(State::Working),
            "C3: EXITED is absorbing (refused)",
        );
        check(
            !loaded.transition_to(State::Exited),
            "no-op transition reports no change",
        );
    }

    let found = store.find_by_name("s1a-selfcheck");
    check(
        found.is_some_and(|found| found.id == session_id),
        "find_by_name locates the record",
    );
    check(
        store.all().iter().any(|session| session.id == session_id),
        "all() lists the record",
    );
    check(store.delete(&session_id)?, "delete removes the record");
    check(
        store.load(&session_id)?.is_none(),
        "record is gone after delete",
    );

    if !failures.is_empty() {
        println!("S1a self-check FAILED ✗");
        for label in failures {
            println!("  - {label}");
        }
        return Ok(1);
    }
    println!("S1a self-check PASSED ✓");
    println!(
        "  home={}  schema v{SCHEMA_VERSION}  records now={}",
        deps.home.root().display(),
        store.all().len()
    );
    Ok(0)
}
