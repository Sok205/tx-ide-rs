//! `tx artifact <subcommand>` and `tx sync` (cli.py `ArtifactCommand` / `SyncCommand`).

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;
use std::rc::Rc;

use cordis::{BoxError, Component, Ctx};

use crate::app::{COMMANDS, Command, ENV, EVENTS, Env, HOME, SERVICE, Visibility};
use crate::argparse::{Arg, Matches, ParseExit, Parser};
use crate::artifact::{Artifact, USER_ACTOR};
use crate::artifact_service::ArtifactService;
use crate::events;
use crate::grouping::GroupResolver;
use crate::render::{render_artifact_show, render_artifacts};
use crate::service::SessionService;
use crate::session::Session;
use crate::spawn::SpawnSpec;
use crate::storage::{
    Config, ConfigError, Home, LocalStorage, RemoteSpec, Storage, StorageError, expand_user,
};
use crate::sync::{self, SyncError, SyncResult};
use crate::verbs::common::{group_value, split_tags};

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
        let artifacts = ArtifactService::new(&home, log, env.actor(), warn_once_stderr());
        let table = ctx.get(COMMANDS)?;
        table.register(
            ctx,
            Visibility::Public,
            ArtifactCmd {
                service,
                artifacts,
                env: Rc::clone(&env),
            },
        );
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

const HELP_ROWS: [(&str, &str); 8] = [
    (
        "create <file> [--title T] [--group G]",
        "register a new artifact from a file",
    ),
    (
        "modify <id> [<file>] [--changes ...]",
        "snapshot a new revision (no file = the working copy)",
    ),
    (
        "group <id> [<group> | --clear]",
        "read or set the effort-group override",
    ),
    (
        "ls [--session S]",
        "list artifacts (--session: what a session touched)",
    ),
    ("show <id>", "metadata + the full touch/version log"),
    (
        "diff <id> [<revA> <revB>]",
        "difflib diff between two revisions (default: last two)",
    ),
    (
        "open <id> [--tag T] [--cwd D]",
        "open the working copy in an nvim view bound to the artifact",
    ),
    (
        "doctor [--repair]",
        "check store invariants; --repair removes orphan rev files",
    ),
];

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
        "Create / modify / inspect durable versioned artifacts (tx artifact <subcommand>)."
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
            "group" => self.group(rest),
            "ls" => self.ls(rest),
            "show" => self.show(rest),
            "diff" => self.diff(rest),
            "open" => self.open(rest),
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
        out.push_str(&format!("  {usage:<40} {summary}\n"));
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
            .arg(Arg::positional("file"))
            .arg(Arg::option("--title"))
            .arg(
                Arg::option("--group")
                    .value_parser(group_value)
                    .help("explicit effort-group override (default: derived from the creator)"),
            );
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let file = args.get_one("file").unwrap_or_default();
        let path = Path::new(file);
        if !path.is_file() {
            return fail(parser.error(&format!("no such file: {file}")));
        }
        let content = read_file(path)?;
        let filename = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let artifact = self.artifacts.create(
            &self.actor(),
            &content,
            args.get_one("title").map(str::to_owned),
            filename,
            args.get_one("group").map(str::to_owned),
        )?;
        println!("Created artifact {} ({})", artifact.id, artifact.filename);
        Ok(0)
    }

    fn group(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = sub_parser("group")
            .arg(Arg::positional("id"))
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
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let clear = args.get_flag("clear");
        let group = args.get_one("group");
        if clear && group.is_some() {
            return fail(parser.error("give a group or --clear, not both"));
        }
        if group == Some("") {
            return fail(
                parser.error("a group cannot be empty — use --clear to drop the override"),
            );
        }
        let artifact_id = self.resolve(&args)?;
        if clear {
            self.artifacts
                .set_group(&artifact_id, &self.actor(), None)?;
            println!("Cleared group override on artifact {artifact_id} (back to derived)");
            return Ok(0);
        }
        if let Some(group) = group {
            self.artifacts
                .set_group(&artifact_id, &self.actor(), Some(group.to_owned()))?;
            println!("Grouped artifact {artifact_id} (group={group})");
            return Ok(0);
        }
        let artifact = self.artifacts.require(&artifact_id)?;
        let (sessions, artifacts) = self.sessions_and_artifacts();
        let resolver = GroupResolver::new(&sessions, &artifacts);
        println!("own:      {}", artifact.group.as_deref().unwrap_or("—"));
        println!("resolved: {}", resolver.artifact_group(&artifact));
        Ok(0)
    }

    fn modify(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = sub_parser("modify")
            .arg(Arg::positional("id"))
            .arg(Arg::positional("file").optional())
            .arg(Arg::option("--changes"));
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let artifact_id = self.resolve(&args)?;
        let before = self.artifacts.require(&artifact_id)?.latest_rev();
        let changes = args.get_one("changes").map(str::to_owned);
        let file = args.get_one("file");
        let artifact = match file {
            Some(file) => {
                let path = Path::new(file);
                if !path.is_file() {
                    return fail(parser.error(&format!("no such file: {file}")));
                }
                let content = read_file(path)?;
                self.artifacts
                    .modify(&artifact_id, &self.actor(), &content, changes)?
            }
            None => self
                .artifacts
                .snapshot_current(&artifact_id, &self.actor(), changes)?,
        };
        if artifact.latest_rev() != before {
            println!(
                "Modified artifact {} → rev {}",
                artifact.id,
                artifact.latest_rev()
            );
            return Ok(0);
        }
        println!("{}", self.no_op_notice(&artifact, before, file.is_some())?);
        Ok(0)
    }

    /// E3: a supplied-file no-op states the real comparison and flags a dirty working copy.
    fn no_op_notice(
        &self,
        artifact: &Artifact,
        last_rev: u64,
        from_file: bool,
    ) -> Result<String, BoxError> {
        if !from_file {
            return Ok(format!(
                "No change — the working copy is identical to rev {last_rev}; nothing to snapshot."
            ));
        }
        let mut notice = format!(
            "No change — the supplied file is identical to rev {last_rev}; nothing snapshotted."
        );
        if self.artifacts.current_is_dirty(artifact)? {
            notice.push_str(&format!(
                " NOTE: the working copy still has unsnapshotted edits — run \
                 `tx artifact modify {}` (no file) to snapshot them.",
                artifact.id
            ));
        }
        Ok(notice)
    }

    fn ls(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = sub_parser("ls")
            .arg(Arg::option("--session").help("only artifacts this session created or touched"));
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let artifacts = match args.get_one("session") {
            Some(session) => self.artifacts.artifacts_for_session(session),
            None => self.artifacts.all(),
        };
        let names = self.service.store().names_for(
            artifacts
                .iter()
                .flat_map(|artifact| artifact.history())
                .map(|touch| touch.session_id.as_str()),
        );
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
        let dirty = self.artifacts.current_is_dirty(&artifact)?;
        let names = self.service.store().names_for(
            artifact
                .history()
                .iter()
                .map(|touch| touch.session_id.as_str()),
        );
        let (sessions, artifacts) = self.sessions_and_artifacts();
        let resolved = GroupResolver::new(&sessions, &artifacts).artifact_group(&artifact);
        println!(
            "{}",
            render_artifact_show(&artifact, dirty, &names, events::now(), Some(&resolved))
        );
        Ok(0)
    }

    fn diff(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = sub_parser("diff")
            .arg(Arg::positional("id"))
            .arg(Arg::positional("rev_a").optional().int())
            .arg(Arg::positional("rev_b").optional().int());
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let revs = match (args.get_int("rev_a"), args.get_int("rev_b")) {
            (Some(a), Some(b)) => Some((a, b)),
            (None, None) => None,
            _ => return fail(parser.error("give both revs or neither (default: the last two)")),
        };
        let artifact_id = self.resolve(&args)?;
        let out = self.artifacts.diff(&artifact_id, revs)?;
        if out.is_empty() {
            println!("(no differences)");
        } else {
            print!("{out}");
        }
        Ok(0)
    }

    fn open(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = sub_parser("open")
            .arg(Arg::positional("id"))
            .arg(Arg::option("--tag").help("tags for the nvim view (overrides the invoker's tags)"))
            .arg(
                Arg::option("--cwd")
                    .help("working directory for the view (default: the artifact's dir)"),
            );
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        let artifact_id = self.resolve(&args)?;
        let artifact = self.artifacts.require(&artifact_id)?;
        let content_path = self.artifacts.content_path(&artifact_id)?;
        let content_path = content_path.to_string_lossy();
        let tags = self.open_tags(args.get_one("tag"))?;
        if tags.is_empty() {
            return fail(parser.error("--tag requires at least one value"));
        }
        let cwd = match args.get_one("cwd").filter(|cwd| !cwd.is_empty()) {
            Some(cwd) => cwd.to_owned(),
            None => self
                .artifacts
                .files()
                .content_dir(&artifact)
                .to_string_lossy()
                .into_owned(),
        };
        let name = format!("art-{}", artifact_id.chars().take(8).collect::<String>());
        let spec = SpawnSpec::for_nvim(name, tags, cwd, None, Some(&content_path));
        let session = self.service.spawn_nvim(spec)?;
        self.service.bind_artifact(&session.id, &artifact_id)?;
        self.artifacts.opened(&artifact_id, &self.actor())?;
        println!(
            "Opened artifact {artifact_id} in nvim view '{}' ({content_path})",
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
        let parser = sub_parser("doctor").arg(Arg::option("--repair").flag().help(
            "remove orphan rev files so a retried modify can claim the slot (run when quiescent)",
        ));
        let args = match parse(&parser, argv) {
            Ok(args) => args,
            Err(code) => return Ok(code),
        };
        if args.get_flag("repair") {
            for line in self.artifacts.repair_orphans()? {
                println!("  {line}");
            }
        }
        let problems = self.artifacts.doctor()?;
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

fn read_file(path: &Path) -> Result<Vec<u8>, BoxError> {
    std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()).into())
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
