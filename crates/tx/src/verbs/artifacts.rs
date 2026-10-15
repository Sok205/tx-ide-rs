//! `tx artifact <subcommand>` and `tx sync` (cli.py `ArtifactCommand` / `SyncCommand`).

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use cordis::{BoxError, Component, Ctx};

use crate::app::{COMMANDS, Command, ENV, EVENTS, Env, HOME, SERVICE, Visibility};
use crate::argparse::{Arg, Matches, ParseExit, Parser};
use crate::artifact::{Artifact, Kind, USER_ACTOR};
use crate::artifact_kinds::{Opened, view_suffix};
use crate::artifact_service::ArtifactService;
use crate::events;
use crate::grouping::GroupResolver;
use crate::render::{render_artifact_show, render_artifacts};
use crate::service::SessionService;
use crate::session::Session;
use crate::storage::{
    Config, ConfigError, Home, LocalStorage, RemoteSpec, Storage, StorageError, expand_user,
};
use crate::sync::{self, SyncError, SyncResult};
use crate::verbs::artifact_menu::{ArtifactGo, ArtifactMenu, ArtifactNav, ArtifactPick};
use crate::verbs::common::split_tags;

/// The display name of an artifact's nvim view: the artifact's title (`-diff` / `-tour` suffixed
/// for those kinds), with a short id appended only when a live session already holds that name.
pub(crate) fn view_name(service: &SessionService, artifact: &Artifact) -> Result<String, BoxError> {
    let title = format!("{}{}", artifact.title, view_suffix(&artifact.kind));
    if service.is_live_name(&title)? {
        let short_id: String = artifact.id.chars().take(8).collect();
        return Ok(format!("{title} ({short_id})"));
    }
    Ok(title)
}

pub struct ArtifactVerbs;

impl Component for ArtifactVerbs {
    fn name(&self) -> &str {
        "verbs.artifacts"
    }
    fn inject(&self) -> &[&'static str] {
        &["commands", "service", "home", "events", "env"]
    }
    fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        let (service, home, log, env) = (
            ctx.get(SERVICE)?,
            ctx.get(HOME)?,
            ctx.get(EVENTS)?,
            ctx.get(ENV)?,
        );
        let artifacts =
            ArtifactService::new(&home, Rc::clone(&log), env.actor(), warn_once_stderr());
        let table = ctx.get(COMMANDS)?;
        table.register(
            ctx,
            Visibility::Public,
            ArtifactCmd {
                service: Rc::clone(&service),
                artifacts,
                env: Rc::clone(&env),
            },
        );
        let nav = Rc::new(ArtifactNav {
            service,
            artifacts: Rc::new(ArtifactService::new(
                &home,
                log,
                env.actor(),
                warn_once_stderr(),
            )),
            home: Rc::clone(&home),
            env: Rc::clone(&env),
        });
        table.register(ctx, Visibility::Hidden, ArtifactMenu(Rc::clone(&nav)));
        table.register(ctx, Visibility::Hidden, ArtifactPick(Rc::clone(&nav)));
        table.register(ctx, Visibility::Hidden, ArtifactGo(nav));
        table.register(ctx, Visibility::Public, SyncCmd { home, env });
        Ok(())
    }
}

/// Each distinct skip line once per invocation (Q19, as the session store does).
fn warn_once_stderr() -> Rc<dyn Fn(&str)> {
    let seen = RefCell::new(HashSet::new());
    Rc::new(move |line: &str| {
        if seen.borrow_mut().insert(line.to_owned()) {
            eprintln!("{line}");
        }
    })
}

fn parse(parser: &Parser, argv: &[String]) -> Result<Matches, i32> {
    parser.parse(argv).map_err(|exit| exit.emit())
}

fn fail(exit: ParseExit) -> Result<i32, BoxError> {
    Ok(exit.emit())
}

// ----- tx artifact ----------------------------------------------------------------------------

const HELP_ROWS: [(&str, &str); 11] = [
    (
        "create <file> --title T [--kind document|tour]",
        "a document (default) or tour from a file",
    ),
    (
        "create --kind diff --title T [--cwd W] [--base SHA]",
        "a diff of worktree W (default: cwd) against SHA (default: merge-base)",
    ),
    (
        "modify <id> [<file>]",
        "stamp modified_at; diff: rewrite the patch; file: replace a document/tour's file",
    ),
    ("open <id> [--tag T]", "open in a bound nvim view"),
    (
        "close <id>",
        "run a pending close step now (a diff's temp worktree)",
    ),
    ("path <id>", "print the artifact directory"),
    (
        "archive <id>",
        "hide a finished artifact from prefix+a and ls (content kept)",
    ),
    ("unarchive <id>", "bring an archived artifact back"),
    (
        "ls [--session S] [--kind K] [--archived]",
        "list artifacts (--session: what a session owns; --archived: include hidden)",
    ),
    ("show <id>", "metadata + the kind's subfield"),
    (
        "doctor [--repair]",
        "check store invariants; --repair runs close steps of ended views",
    ),
];

const KIND_CHOICES: [&str; 3] = ["document", "diff", "tour"];

struct ArtifactCmd {
    service: Rc<SessionService>,
    artifacts: ArtifactService,
    env: Rc<Env>,
}

impl Command for ArtifactCmd {
    fn name(&self) -> &'static str {
        "artifact"
    }
    fn summary(&self) -> &'static str {
        "Create / modify / open durable artifacts — documents, diffs, tours (tx artifact <subcommand>)."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let Some(sub) = argv.first() else {
            print_help();
            return Ok(0);
        };
        let rest = &argv[1..];
        match sub.as_str() {
            "-h" | "--help" | "help" => {
                print_help();
                Ok(0)
            }
            "create" => self.create(rest),
            "modify" => self.modify(rest),
            "open" => self.open(rest),
            "close" => self.close(rest),
            "path" => self.path(rest),
            "archive" => self.set_archived(rest, true),
            "unarchive" => self.set_archived(rest, false),
            "ls" => self.ls(rest),
            "show" => self.show(rest),
            "doctor" => self.doctor(rest),
            other => {
                eprintln!("tx artifact: unknown subcommand '{other}'");
                print_help();
                Ok(2)
            }
        }
    }
}

fn print_help() {
    let mut out = String::from("usage: tx artifact <subcommand> [args]\n\nsubcommands:\n");
    for (usage, summary) in HELP_ROWS {
        out.push_str(&format!("  {usage:<52} {summary}\n"));
    }
    print!("{out}");
}

fn sub_parser(sub: &str) -> Parser {
    Parser::new(&format!("tx artifact {sub}"))
}

impl ArtifactCmd {
    /// `$TX_SESSION_ID`, else the current tmux session's `@tx_id` (only inside tmux), else `user`.
    fn actor(&self) -> String {
        let env_id = self.env.actor();
        if !env_id.is_empty() {
            return env_id.to_owned();
        }
        let tmux = self.service.tmux();
        tmux.current_session_name()
            .and_then(|current| tmux.get_tx_id(&current))
            .unwrap_or_else(|| USER_ACTOR.to_owned())
    }

    fn sessions_and_artifacts(&self) -> (Vec<Session>, Vec<Artifact>) {
        (self.service.store().all(), self.artifacts.all())
    }

    fn resolve(&self, args: &Matches) -> Result<String, BoxError> {
        Ok(self
            .artifacts
            .resolve_id(args.get_one("id").unwrap_or_default())?)
    }

    fn create(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = sub_parser("create")
            .arg(
                Arg::positional("file")
                    .optional()
                    .help("the document or tour stops file (not for --kind diff)"),
            )
            .arg(Arg::option("--title").help("the artifact's title (required)"))
            .arg(
                Arg::option("--kind")
                    .choices(&KIND_CHOICES)
                    .default("document")
                    .help("what the artifact is (default: document)"),
            )
            .arg(
                Arg::option("--cwd")
                    .help("diff/tour: the worktree the artifact points into (default: cwd)"),
            )
            .arg(Arg::option("--base").help("diff: the pinned base commit (default: merge-base)"));
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let title = args.get_one("title").unwrap_or_default();
        if title.is_empty() {
            return fail(parser.error("--title is required"));
        }
        let kind_name = args.get_one("kind").unwrap_or("document");
        let file = args.get_one("file").filter(|file| !file.is_empty());
        let worktree = match args.get_one("cwd").filter(|cwd| !cwd.is_empty()) {
            Some(cwd) => PathBuf::from(cwd),
            None => std::env::current_dir()?,
        };
        let (kind, source) = match kind_name {
            "diff" => {
                if file.is_some() {
                    return fail(parser.error("a diff takes no file — pass --cwd and --base"));
                }
                let kind = Kind::Diff {
                    worktree,
                    base: args.get_one("base").unwrap_or_default().to_owned(),
                };
                (kind, None)
            }
            _ => {
                let Some(file) = file else {
                    return fail(parser.error(&format!("a {kind_name} is created from a file")));
                };
                let path = Path::new(file);
                if !path.is_file() {
                    return fail(parser.error(&format!("no such file: {file}")));
                }
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let kind = if kind_name == "tour" {
                    Kind::Tour {
                        worktree,
                        stops: name,
                    }
                } else {
                    Kind::Document { file: name }
                };
                (kind, Some(path))
            }
        };
        let artifact = self
            .artifacts
            .create(&self.actor(), kind, title.to_owned(), source)?;
        println!(
            "Created {} artifact {} ({})",
            artifact.kind.name(),
            artifact.id,
            artifact.title
        );
        Ok(0)
    }

    fn modify(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = sub_parser("modify").arg(Arg::positional("id")).arg(
            Arg::positional("file")
                .optional()
                .help("replace the document's or tour's file with this one"),
        );
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let artifact_id = self.resolve(&args)?;
        let file = args.get_one("file").filter(|file| !file.is_empty());
        if let Some(file) = file
            && !Path::new(file).is_file()
        {
            return fail(parser.error(&format!("no such file: {file}")));
        }
        let artifact = self
            .artifacts
            .modify(&artifact_id, &self.actor(), file.map(Path::new))?;
        println!("Modified {} artifact {}", artifact.kind.name(), artifact.id);
        Ok(0)
    }

    fn set_archived(&self, argv: &[String], archived: bool) -> Result<i32, BoxError> {
        let sub = if archived { "archive" } else { "unarchive" };
        let parser = sub_parser(sub).arg(Arg::positional("id"));
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let artifact_id = self.resolve(&args)?;
        let artifact = self
            .artifacts
            .set_archived(&artifact_id, &self.actor(), archived)?;
        if archived {
            println!(
                "Archived artifact {artifact_id} ({}) — hidden from prefix+a and ls",
                artifact.title
            );
        } else {
            println!("Unarchived artifact {artifact_id} ({})", artifact.title);
        }
        Ok(0)
    }

    fn close(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = sub_parser("close").arg(Arg::positional("id"));
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let artifact_id = self.resolve(&args)?;
        let ran = self.artifacts.close_all(&artifact_id)?;
        println!("Closed artifact {artifact_id}: {ran} close step(s) run");
        Ok(0)
    }

    fn path(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = sub_parser("path").arg(Arg::positional("id"));
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let artifact_id = self.resolve(&args)?;
        println!("{}", self.artifacts.dir(&artifact_id)?.display());
        Ok(0)
    }

    fn ls(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = sub_parser("ls")
            .arg(Arg::option("--session").help("only artifacts this session owns"))
            .arg(
                Arg::option("--kind")
                    .choices(&KIND_CHOICES)
                    .help("only artifacts of this kind"),
            )
            .arg(
                Arg::option("--archived")
                    .flag()
                    .help("include archived artifacts (hidden by default)"),
            );
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let include_archived = args.get_flag("archived");
        let session = args.get_one("session");
        let kind = args.get_one("kind");
        let artifacts: Vec<Artifact> = self
            .artifacts
            .all()
            .into_iter()
            .filter(|artifact| {
                (include_archived || !artifact.is_archived())
                    && session.is_none_or(|session| artifact.owner == session)
                    && kind.is_none_or(|kind| artifact.kind.name() == kind)
            })
            .collect();
        let names = self
            .service
            .store()
            .names_for(artifacts.iter().map(|artifact| artifact.owner.as_str()));
        println!("{}", render_artifacts(&artifacts, &names, events::now()));
        Ok(0)
    }

    fn show(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = sub_parser("show").arg(Arg::positional("id"));
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let artifact = self.artifacts.require(&self.resolve(&args)?)?;
        let names = self
            .service
            .store()
            .names_for(std::iter::once(artifact.owner.as_str()));
        let (sessions, artifacts) = self.sessions_and_artifacts();
        let resolved = GroupResolver::new(&sessions, &artifacts).artifact_group(&artifact);
        let worktree_present = artifact.kind.worktree().map(Path::is_dir);
        println!(
            "{}",
            render_artifact_show(
                &artifact,
                &names,
                events::now(),
                &resolved,
                worktree_present
            )
        );
        Ok(0)
    }

    fn open(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = sub_parser("open").arg(Arg::positional("id")).arg(
            Arg::option("--tag").help("tags for the nvim view (overrides the invoker's tags)"),
        );
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let artifact_id = self.resolve(&args)?;
        let artifact = self.artifacts.require(&artifact_id)?;
        let tags = self.open_tags(args.get_one("tag"))?;
        if tags.is_empty() {
            return fail(parser.error("--tag requires at least one value"));
        }
        let name = view_name(&self.service, &artifact)?;
        let Opened { spec, cleanup } =
            self.artifacts
                .open(&artifact_id, &self.actor(), name, tags)?;
        let cwd = spec.cwd.clone();
        let session = self.service.spawn_nvim(spec)?;
        self.service.bind_artifact(&session.id, &artifact_id)?;
        if let Some(cleanup) = cleanup {
            self.artifacts
                .record_cleanup(&artifact_id, &session.id, cleanup)?;
        }
        println!(
            "Opened {} artifact {artifact_id} in nvim view '{}' (cwd {cwd})",
            artifact.kind.name(),
            session.name
        );
        Ok(0)
    }

    /// E1: `--tag` overrides; else the invoking session's tags; else `["artifact"]`.
    fn open_tags(&self, tag_override: Option<&str>) -> Result<Vec<String>, BoxError> {
        if let Some(raw) = tag_override {
            return Ok(split_tags(raw));
        }
        let invoker = match self.service.tmux().current_session_name() {
            Some(current) => self.service.get(&current)?,
            None => None,
        };
        Ok(invoker
            .map(|session| session.tags)
            .filter(|tags| !tags.is_empty())
            .unwrap_or_else(|| vec!["artifact".to_owned()]))
    }

    fn doctor(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = sub_parser("doctor").arg(
            Arg::option("--repair")
                .flag()
                .help("remove stale cleanup notes whose view has ended"),
        );
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let view_is_alive = |view: &str| {
            self.service
                .store()
                .load(view)
                .ok()
                .flatten()
                .is_some_and(|session| session.is_alive())
        };
        if args.get_flag("repair") {
            for line in self.artifacts.repair(&view_is_alive)? {
                println!("  {line}");
            }
        }
        let problems = self.artifacts.doctor(&view_is_alive)?;
        if problems.is_empty() {
            println!("artifacts: clean");
            return Ok(0);
        }
        for problem in &problems {
            println!("  {problem}");
        }
        println!("artifacts: {} problem(s)", problems.len());
        Ok(1)
    }
}

// ----- tx sync --------------------------------------------------------------------------------

struct SyncCmd {
    home: Rc<Home>,
    env: Rc<Env>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SyncAction {
    Push,
    Pull,
    Status,
}

impl SyncAction {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "push" => Some(Self::Push),
            "pull" => Some(Self::Pull),
            "status" => Some(Self::Status),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Push => "push",
            Self::Pull => "pull",
            Self::Status => "status",
        }
    }
}

impl Command for SyncCmd {
    fn name(&self) -> &'static str {
        "sync"
    }
    fn summary(&self) -> &'static str {
        "Manual archive sync of the reproducible corpus (push/pull/status) — never hot-path."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = Parser::new("tx sync")
            .description(self.summary())
            .arg(Arg::positional("action").choices(&["push", "pull", "status"]))
            .arg(
                Arg::option("--remote")
                    .metavar("PATH")
                    .help("a local filesystem remote (archive dir / S3 dogfood proxy)"),
            )
            .arg(
                Arg::option("--s3")
                    .metavar("BUCKET[/PREFIX]")
                    .help("select the S3 backend (deferred — reports 'not implemented')"),
            );
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let action = SyncAction::parse(args.get_one("action").unwrap_or_default())
            .ok_or("argparse choices admit only push / pull / status")?;
        let local = LocalStorage::new(self.home.root());
        let remote = match self.resolve_remote(&args) {
            Ok(remote) => remote,
            // Q26 FIX: a bad `sync` section is a one-line message, not a traceback.
            Err(ConfigError::Sync { source, .. }) => return Err(source.into()),
            Err(error) => return Err(error.into()),
        };
        if action == SyncAction::Status {
            return status(&local, remote.as_deref());
        }
        let Some(remote) = remote else {
            eprintln!(
                "tx sync: no remote configured — pass --remote PATH / --s3 BUCKET, or set the \
                 'sync' section in config.json"
            );
            return Ok(1);
        };
        let outcome = match action {
            SyncAction::Push => sync::sync_push(&local, remote.as_ref()),
            _ => sync::sync_pull(&local, remote.as_ref()),
        };
        match outcome {
            Ok(result) => {
                print_result(action, &remote.label(), &result);
                Ok(0)
            }
            Err(error) => {
                eprintln!("tx sync {}: {error}", action.name());
                Ok(1)
            }
        }
    }
}

impl SyncCmd {
    /// `--s3` beats `--remote` beats the `sync` section of `config.json` (empty values are
    /// Python-falsy and fall through).
    fn resolve_remote(&self, args: &Matches) -> Result<Option<Box<dyn Storage>>, ConfigError> {
        let home_dir = self.env.var("HOME").map(Path::new);
        if let Some(spec) = args.get_one("s3").filter(|spec| !spec.is_empty()) {
            let (bucket, prefix) = spec.split_once('/').unwrap_or((spec, ""));
            let remote = RemoteSpec::S3 {
                bucket: bucket.to_owned(),
                prefix: prefix.to_owned(),
            };
            return Ok(Some(remote.into_storage()));
        }
        if let Some(path) = args.get_one("remote").filter(|path| !path.is_empty()) {
            let remote = RemoteSpec::Local {
                path: expand_user(path, home_dir),
            };
            return Ok(Some(remote.into_storage()));
        }
        let config = Config::load(&self.home.config_path())?;
        Ok(config.sync_remote(home_dir)?.map(RemoteSpec::into_storage))
    }
}

fn status(local: &LocalStorage, remote: Option<&dyn Storage>) -> Result<i32, BoxError> {
    let corpus = sync::corpus_status(local)?;
    let presence = |present: bool| if present { "present" } else { "missing" };
    println!("Local corpus ({}):", local.label());
    println!("  records:        {}", corpus.records);
    println!("  history files:  {}", corpus.history_files);
    println!("  log.jsonl:      {}", presence(corpus.has_log));
    println!("  config.json:    {}", presence(corpus.has_config));
    println!("  total keys:     {}", corpus.total());
    let Some(remote) = remote else {
        println!("Remote: none configured (local-only; pass --remote/--s3 or set config.json).");
        return Ok(0);
    };
    let label = remote.label();
    let counts = sync::sync_diff(local, remote)
        .and_then(|to_push| Ok((to_push, sync::sync_diff(remote, local)?)));
    match counts {
        Ok((to_push, to_pull)) => {
            println!("Remote ({label}): {to_push} key(s) to push, {to_pull} key(s) to pull.");
        }
        Err(error @ SyncError::Storage(StorageError::Deferred)) => {
            println!("Remote ({label}): {error}");
        }
        Err(error) => return Err(error.into()),
    }
    Ok(0)
}

fn print_result(action: SyncAction, label: &str, result: &SyncResult) {
    let direction = if action == SyncAction::Push {
        "→"
    } else {
        "←"
    };
    let kept = if result.kept.is_empty() {
        String::new()
    } else {
        format!(", {} kept (destination newer)", result.kept.len())
    };
    println!(
        "sync {} (local {direction} {label}): {} added, {} updated, {} unchanged{kept}",
        action.name(),
        result.added.len(),
        result.updated.len(),
        result.unchanged.len(),
    );
}
