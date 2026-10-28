//! The verbs: `artifact <sub>`, `group`, and the hidden prefix+a trio. Ported from main's
//! `verbs/artifacts.rs` and the `group` verb of `verbs/sessions.rs`; arguments are parsed by
//! hand (main's argparse shim is not ported).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value};
use tx_core::{ListFilter, Session, SessionId, TxCore, Verb, VerbError, VerbIo};
use tx_ui::tmux::Ui;

use crate::artifact::{Artifact, KIND_NAMES, Kind, USER_ACTOR};
use crate::grouping::{GroupResolver, ext_of, own_group};
use crate::menu::{GO_VERB, MENU_VERB, Mode, Nav, PICK_VERB};
use crate::migrate::migrate;
use crate::render::{render_artifact_show, render_artifacts};
use crate::service::{ArtifactError, ArtifactService};
use crate::views::{self, Opener};
use crate::{PLUGIN_ID, service};

const HELP_ROWS: [(&str, &str); 12] = [
    (
        "create <file> --title T [--kind document|tour] [--cwd W]",
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
    ("open <id> [--tag T]", "open in a bound editor view"),
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
    ("migrate", "upgrade v1 / v2 / v3 records to v4"),
];

fn help() -> String {
    let mut out = String::from("usage: txc artifact <subcommand> [args]\n\nsubcommands:\n");
    for (usage, summary) in HELP_ROWS {
        out.push_str(&format!("  {usage:<56} {summary}\n"));
    }
    out
}

/// Positionals and `--flag value` / `--switch` options, in one pass. `switches` take no value.
struct Args {
    positionals: Vec<String>,
    options: HashMap<String, String>,
    switches: Vec<String>,
}

fn parse(argv: &[String], valued: &[&str], switches: &[&str]) -> Result<Args, String> {
    let mut args = Args {
        positionals: Vec::new(),
        options: HashMap::new(),
        switches: Vec::new(),
    };
    let mut rest = argv.iter();
    while let Some(word) = rest.next() {
        if let Some(name) = word.strip_prefix("--") {
            if valued.contains(&name) {
                let value = rest.next().ok_or(format!("--{name} needs a value"))?;
                args.options.insert(name.to_owned(), value.clone());
            } else if switches.contains(&name) {
                args.switches.push(name.to_owned());
            } else {
                return Err(format!("unknown option --{name}"));
            }
        } else {
            args.positionals.push(word.clone());
        }
    }
    Ok(args)
}

impl Args {
    fn option(&self, name: &str) -> Option<&str> {
        self.options.get(name).map(String::as_str)
    }
    fn switch(&self, name: &str) -> bool {
        self.switches.iter().any(|switch| switch == name)
    }
}

/// Every record the core holds; none when the store cannot be read.
fn all_sessions(core: &TxCore) -> Vec<Session> {
    core.sessions(&ListFilter::new()).unwrap_or_default()
}

/// id → name for every owner that is a session.
fn names_for<'a>(core: &TxCore, ids: impl IntoIterator<Item = &'a str>) -> HashMap<String, String> {
    let mut names = HashMap::new();
    for id in ids {
        if names.contains_key(id) {
            continue;
        }
        if let Some(session) = id
            .parse::<SessionId>()
            .ok()
            .and_then(|sid| core.get_session(&sid).ok())
        {
            names.insert(id.to_owned(), session.name);
        }
    }
    names
}

/// `txc artifact <subcommand>`.
pub struct ArtifactVerb {
    pub editor: Arc<str>,
}

/// What a subcommand produced: an exit code, or an artifact precondition to report.
enum Outcome {
    Code(i32),
    Usage(String),
    Artifact(ArtifactError),
}

impl From<ArtifactError> for Outcome {
    fn from(error: ArtifactError) -> Self {
        Self::Artifact(error)
    }
}

struct Ctx<'a, 'io> {
    core: &'a TxCore,
    io: &'a mut VerbIo<'io>,
    artifacts: ArtifactService,
    editor: &'a str,
}

impl Verb for ArtifactVerb {
    fn name(&self) -> &'static str {
        "artifact"
    }

    fn usage(&self) -> &'static str {
        "artifact create|modify|open|close|path|archive|unarchive|ls|show|doctor|migrate   documents, diffs and tours (txc artifact help)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let Some((sub, rest)) = argv.split_first() else {
            write!(io.stdout, "{}", help()).map_err(VerbError::write)?;
            return Ok(0);
        };
        let mut ctx = Ctx {
            core,
            io,
            artifacts: service(core),
            editor: &self.editor,
        };
        let outcome = match sub.as_str() {
            "-h" | "--help" | "help" => {
                write!(ctx.io.stdout, "{}", help()).map_err(VerbError::write)?;
                return Ok(0);
            }
            "create" => ctx.create(rest)?,
            "modify" => ctx.modify(rest)?,
            "open" => ctx.open(rest)?,
            "close" => ctx.close(rest)?,
            "path" => ctx.path(rest)?,
            "archive" => ctx.set_archived(rest, true)?,
            "unarchive" => ctx.set_archived(rest, false)?,
            "ls" => ctx.ls(rest)?,
            "show" => ctx.show(rest)?,
            "doctor" => ctx.doctor(rest)?,
            "migrate" => ctx.migrate(rest)?,
            other => Outcome::Usage(format!("unknown subcommand '{other}'\n{}", help())),
        };
        match outcome {
            Outcome::Code(code) => Ok(code),
            Outcome::Usage(message) => {
                Err(VerbError::usage(format!("txc artifact {sub}: {message}")))
            }
            Outcome::Artifact(error) => {
                let _ = writeln!(ctx.io.stderr, "txc artifact: {error}");
                Ok(1)
            }
        }
    }
}

type Step = Result<Outcome, VerbError>;

/// The positionals a subcommand takes: `required` names, then up to `optional` more.
fn positionals(args: &Args, required: usize, optional: usize) -> Result<&[String], Outcome> {
    let count = args.positionals.len();
    if count < required || count > required + optional {
        return Err(Outcome::Usage(format!(
            "expected {required}{} argument(s), got {count}",
            if optional > 0 {
                format!("..{}", required + optional)
            } else {
                String::new()
            }
        )));
    }
    Ok(&args.positionals)
}

macro_rules! try_outcome {
    ($expr:expr) => {
        match $expr {
            Ok(value) => value,
            Err(outcome) => return Ok(Outcome::from(outcome)),
        }
    };
}

impl From<String> for Outcome {
    fn from(message: String) -> Self {
        Self::Usage(message)
    }
}

impl Ctx<'_, '_> {
    fn print(&mut self, text: &str) -> Result<(), VerbError> {
        writeln!(self.io.stdout, "{text}").map_err(VerbError::write)
    }

    /// The artifact a subcommand's single positional names.
    fn one_artifact(
        &self,
        argv: &[String],
        valued: &[&str],
        switches: &[&str],
    ) -> Result<(Args, String), Outcome> {
        let args = parse(argv, valued, switches)?;
        let id = self.artifacts.resolve_id(&positionals(&args, 1, 0)?[0])?;
        Ok((args, id))
    }

    fn create(&mut self, argv: &[String]) -> Step {
        let args = try_outcome!(parse(argv, &["title", "kind", "cwd", "base"], &[]));
        let given = try_outcome!(positionals(&args, 0, 1));
        let title = args.option("title").unwrap_or_default();
        if title.is_empty() {
            return Ok(Outcome::Usage("--title is required".into()));
        }
        let kind_name = args.option("kind").unwrap_or("document");
        if !KIND_NAMES.contains(&kind_name) {
            return Ok(Outcome::Usage(format!(
                "--kind must be one of {}",
                KIND_NAMES.join(", ")
            )));
        }
        let worktree = match args.option("cwd").filter(|cwd| !cwd.is_empty()) {
            Some(cwd) => PathBuf::from(cwd),
            None => std::env::current_dir()
                .map_err(|error| VerbError::Failed(format!("current dir: {error}")))?,
        };
        let (kind, source) = match (kind_name, given.first()) {
            ("diff", Some(_)) => {
                return Ok(Outcome::Usage(
                    "a diff takes no file; pass --cwd and --base".into(),
                ));
            }
            ("diff", None) => {
                let base = args.option("base").unwrap_or_default().to_owned();
                (Kind::Diff { worktree, base }, None)
            }
            (_, None) => {
                return Ok(Outcome::Usage(format!(
                    "a {kind_name} is created from a file"
                )));
            }
            (_, Some(file)) => {
                let path = Path::new(file);
                if !path.is_file() {
                    return Ok(Outcome::Usage(format!("no such file: {file}")));
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
        let actor = self.artifacts.actor().to_owned();
        let artifact = try_outcome!(
            self.artifacts
                .create(&actor, kind, title.to_owned(), source)
        );
        self.print(&format!(
            "Created {} artifact {} ({})",
            artifact.kind.name(),
            artifact.id,
            artifact.title
        ))?;
        Ok(Outcome::Code(0))
    }

    fn modify(&mut self, argv: &[String]) -> Step {
        let args = try_outcome!(parse(argv, &[], &[]));
        let given = try_outcome!(positionals(&args, 1, 1));
        let artifact_id = try_outcome!(self.artifacts.resolve_id(&given[0]));
        let file = given.get(1).map(Path::new);
        if let Some(file) = file.filter(|file| !file.is_file()) {
            return Ok(Outcome::Usage(format!("no such file: {}", file.display())));
        }
        let actor = self.artifacts.actor().to_owned();
        let artifact = try_outcome!(self.artifacts.modify(&artifact_id, &actor, file));
        self.print(&format!(
            "Modified {} artifact {}",
            artifact.kind.name(),
            artifact.id
        ))?;
        Ok(Outcome::Code(0))
    }

    fn set_archived(&mut self, argv: &[String], archived: bool) -> Step {
        let (_, artifact_id) = try_outcome!(self.one_artifact(argv, &[], &[]));
        let actor = self.artifacts.actor().to_owned();
        let artifact = try_outcome!(self.artifacts.set_archived(&artifact_id, &actor, archived));
        let line = if archived {
            format!(
                "Archived artifact {artifact_id} ({}); hidden from prefix+a and ls",
                artifact.title
            )
        } else {
            format!("Unarchived artifact {artifact_id} ({})", artifact.title)
        };
        self.print(&line)?;
        Ok(Outcome::Code(0))
    }

    fn close(&mut self, argv: &[String]) -> Step {
        let (_, artifact_id) = try_outcome!(self.one_artifact(argv, &[], &[]));
        let ran = try_outcome!(self.artifacts.close_all(&artifact_id));
        self.print(&format!(
            "Closed artifact {artifact_id}: {ran} close step(s) run"
        ))?;
        Ok(Outcome::Code(0))
    }

    fn path(&mut self, argv: &[String]) -> Step {
        let (_, artifact_id) = try_outcome!(self.one_artifact(argv, &[], &[]));
        let artifact = try_outcome!(self.artifacts.require(&artifact_id));
        let dir = self.artifacts.files().content_dir(&artifact);
        self.print(&dir.display().to_string())?;
        Ok(Outcome::Code(0))
    }

    fn resolved_group(&self, artifact: &Artifact) -> String {
        let sessions = all_sessions(self.core);
        let artifacts = self.artifacts.all();
        GroupResolver::new(&sessions, &artifacts).artifact_group(artifact)
    }

    fn ls(&mut self, argv: &[String]) -> Step {
        let args = try_outcome!(parse(argv, &["session", "kind"], &["archived"]));
        try_outcome!(positionals(&args, 0, 0));
        let scan = self.artifacts.scan();
        for skipped in &scan.skipped {
            let _ = writeln!(self.io.stderr, "{}", skipped.warning());
        }
        let owner = args.option("session").map(|token| {
            self.core
                .resolve(token)
                .map_or_else(|_| token.to_owned(), |id| id.to_string())
        });
        let kind = args.option("kind");
        let archived = args.switch("archived");
        let mut artifacts = scan.artifacts;
        artifacts.retain(|artifact| {
            (archived || !artifact.is_archived())
                && owner.as_ref().is_none_or(|owner| &artifact.owner == owner)
                && kind.is_none_or(|kind| artifact.kind.name() == kind)
        });
        let names = names_for(self.core, artifacts.iter().map(|a| a.owner.as_str()));
        let text = render_artifacts(&artifacts, &names, tx_core::events::now());
        self.print(&text)?;
        Ok(Outcome::Code(0))
    }

    fn show(&mut self, argv: &[String]) -> Step {
        let (_, artifact_id) = try_outcome!(self.one_artifact(argv, &[], &[]));
        let artifact = try_outcome!(self.artifacts.require(&artifact_id));
        let names = names_for(self.core, [artifact.owner.as_str()]);
        let resolved = self.resolved_group(&artifact);
        let text = render_artifact_show(&artifact, &names, tx_core::events::now(), &resolved);
        self.print(&text)?;
        Ok(Outcome::Code(0))
    }

    fn invoker(&self) -> Option<Session> {
        let actor = self.artifacts.actor();
        if actor == USER_ACTOR {
            return None;
        }
        self.core.get_session(&actor.parse().ok()?).ok()
    }

    fn open(&mut self, argv: &[String]) -> Step {
        let (args, artifact_id) = try_outcome!(self.one_artifact(argv, &["tag"], &[]));
        let artifact = try_outcome!(self.artifacts.require(&artifact_id));
        let tags = match args.option("tag") {
            Some(raw) => {
                let tags: Vec<String> = raw
                    .split(',')
                    .filter(|tag| !tag.is_empty())
                    .map(str::to_owned)
                    .collect();
                if tags.is_empty() {
                    return Ok(Outcome::Usage("--tag requires at least one value".into()));
                }
                Some(tags)
            }
            None => None,
        };
        let invoker = self.invoker();
        let opener = Opener {
            id: invoker.as_ref().map(|session| session.id.clone()),
            tags: invoker.map(|session| session.tags).unwrap_or_default(),
        };
        let id = views::open(
            self.core,
            &self.artifacts,
            &artifact,
            &opener,
            tags,
            self.editor,
        )?;
        let view = self.core.get_session(&id)?;
        self.print(&format!(
            "Opened {} artifact {artifact_id} in view '{}' (cwd {})",
            artifact.kind.name(),
            view.name,
            view.cwd.display()
        ))?;
        Ok(Outcome::Code(0))
    }

    fn doctor(&mut self, argv: &[String]) -> Step {
        let args = try_outcome!(parse(argv, &[], &["repair"]));
        try_outcome!(positionals(&args, 0, 0));
        let core = self.core;
        let view_is_alive = |view: &str| {
            view.parse::<SessionId>()
                .ok()
                .and_then(|id| core.get_session(&id).ok())
                .is_some_and(|session| !session.lifecycle.is_terminal())
        };
        if args.switch("repair") {
            for line in try_outcome!(self.artifacts.repair(&view_is_alive)) {
                self.print(&format!("  {line}"))?;
            }
        }
        let problems = try_outcome!(self.artifacts.doctor(&view_is_alive));
        if problems.is_empty() {
            self.print("artifacts: clean")?;
            return Ok(Outcome::Code(0));
        }
        for problem in &problems {
            self.print(&format!("  {problem}"))?;
        }
        self.print(&format!("artifacts: {} problem(s)", problems.len()))?;
        Ok(Outcome::Code(1))
    }

    fn migrate(&mut self, argv: &[String]) -> Step {
        let args = try_outcome!(parse(argv, &[], &[]));
        try_outcome!(positionals(&args, 0, 0));
        let report = migrate(self.artifacts.store());
        let actor = self.artifacts.actor().to_owned();
        for name in &report.migrated {
            let id = name.trim_end_matches(".json");
            try_outcome!(self.artifacts.append("artifact-migrate", id, &actor));
            self.print(&format!("  migrated {name}"))?;
        }
        for (name, reason) in &report.skipped {
            self.print(&format!("  skipped {name}: {reason}"))?;
        }
        self.print(&format!(
            "artifacts: {} migrated, {} skipped",
            report.migrated.len(),
            report.skipped.len()
        ))?;
        Ok(Outcome::Code(0))
    }
}

/// `txc group <session> [<group> | --clear]`: a session's effort-group override,
/// `ext["artifacts"].group`.
pub struct GroupVerb;

const GROUP_USAGE: &str = "usage: txc group <session> [<group> | --clear]";

impl Verb for GroupVerb {
    fn name(&self) -> &'static str {
        "group"
    }

    fn usage(&self) -> &'static str {
        "group <session> [<group> | --clear]   read or set a session's effort-group override"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let usage = |message: &str| {
            Err(VerbError::usage(format!(
                "txc group: {message}\n{GROUP_USAGE}"
            )))
        };
        let args = match parse(argv, &[], &["clear"]) {
            Ok(args) => args,
            Err(message) => return usage(&message),
        };
        let clear = args.switch("clear");
        let (token, group) = match args.positionals.as_slice() {
            [token] => (token, None),
            [token, group] => (token, Some(group.as_str())),
            _ => return usage("expected a session and at most one group"),
        };
        if clear && group.is_some() {
            return usage("give a group or --clear, not both");
        }
        if group == Some("") {
            return usage("a group cannot be empty; use --clear to drop the override");
        }
        let session = core.resolve_session(token)?;
        if let Err(problem) = ext_of(&session) {
            let _ = writeln!(io.stderr, "txc group: {problem}");
        }
        if clear || group.is_some() {
            let owned = group.map(str::to_owned);
            core.update_ext(&session.id, PLUGIN_ID, |value| set_group(value, owned))?;
            let line = if clear {
                format!(
                    "Cleared group override on '{}' (back to derived)",
                    session.name
                )
            } else {
                format!(
                    "Grouped '{}' (group={})",
                    session.name,
                    group.unwrap_or_default()
                )
            };
            writeln!(io.stdout, "{line}").map_err(VerbError::write)?;
            return Ok(0);
        }
        let sessions = all_sessions(core);
        let artifacts = service(core).all();
        let resolved = GroupResolver::new(&sessions, &artifacts).session_group(&session);
        let own = own_group(&session).unwrap_or_else(|| "—".to_owned());
        writeln!(io.stdout, "own:      {own}\nresolved: {resolved}").map_err(VerbError::write)?;
        Ok(0)
    }
}

/// Sets or clears `group` in `ext["artifacts"]`, keeping `bound`. A malformed value (not an
/// object) is replaced; an object left empty is removed.
fn set_group(value: &mut Value, group: Option<String>) {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    let object = value.as_object_mut().expect("an object");
    match group {
        Some(group) => {
            object.insert("group".to_owned(), Value::String(group));
        }
        None => {
            object.remove("group");
        }
    }
    if object.is_empty() {
        *value = Value::Null;
    }
}

/// `_artifact-menu <session> <client>`: prefix+a.
pub struct MenuVerb {
    pub editor: Arc<str>,
}

/// `_artifact-pick <session> <client>`: the list inside the popup.
pub struct PickVerb {
    pub editor: Arc<str>,
}

/// `_artifact-go <session> <client> attach|focus|popup artifact|session <id>`.
pub struct GoVerb {
    pub editor: Arc<str>,
}

fn nav<'a>(core: &'a TxCore, editor: &'a str) -> Nav<'a> {
    Nav {
        core,
        ui: Ui::from_core(core),
        artifacts: service(core),
        editor,
    }
}

impl Verb for MenuVerb {
    fn name(&self) -> &'static str {
        MENU_VERB
    }

    fn usage(&self) -> &'static str {
        "_artifact-menu <session> <client>   prefix+a: the artifacts picker, or back from a view (internal)"
    }

    fn run(&self, core: &TxCore, _io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [session, client] = argv else {
            return Err(self.usage_error());
        };
        nav(core, &self.editor).menu(session, client)
    }
}

impl Verb for PickVerb {
    fn name(&self) -> &'static str {
        PICK_VERB
    }

    fn usage(&self) -> &'static str {
        "_artifact-pick <session> <client>   the picker inside prefix+a's popup (internal)"
    }

    fn run(&self, core: &TxCore, _io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [session, client] = argv else {
            return Err(self.usage_error());
        };
        nav(core, &self.editor).pick(session, client)
    }
}

impl Verb for GoVerb {
    fn name(&self) -> &'static str {
        GO_VERB
    }

    fn usage(&self) -> &'static str {
        "_artifact-go <session> <client> attach|focus|popup artifact|session <id>   show a view (internal)"
    }

    fn run(&self, core: &TxCore, _io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let words: Vec<&str> = argv.iter().map(String::as_str).collect();
        let [session, client, mode, kind, id] = words[..] else {
            return Err(self.usage_error());
        };
        let Some(mode) = Mode::parse(mode) else {
            return Err(self.usage_error());
        };
        let nav = nav(core, &self.editor);
        match kind {
            "artifact" => nav.go_artifact(session, client, mode, id),
            "session" => nav.go_session(session, client, mode, id),
            _ => Err(self.usage_error()),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn set_group_keeps_bound_and_drops_an_empty_object() {
        let mut value = json!({"bound": "a"});
        set_group(&mut value, Some("g".into()));
        assert_eq!(value, json!({"bound": "a", "group": "g"}));
        set_group(&mut value, None);
        assert_eq!(value, json!({"bound": "a"}));
        let mut value = json!({"group": "g"});
        set_group(&mut value, None);
        assert_eq!(value, Value::Null);
        let mut malformed = json!("junk");
        set_group(&mut malformed, Some("g".into()));
        assert_eq!(malformed, json!({"group": "g"}));
    }

    #[test]
    fn args_split_positionals_options_and_switches() {
        let argv: Vec<String> = ["id", "--title", "c", "file", "--clear"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let args = parse(&argv, &["title"], &["clear"]).unwrap();
        assert_eq!(args.positionals, ["id", "file"]);
        assert_eq!(args.option("title"), Some("c"));
        assert!(args.switch("clear"));
        assert!(parse(&argv[..2], &["title"], &[]).is_err());
        assert!(parse(&["--nope".to_owned()], &[], &[]).is_err());
    }
}
