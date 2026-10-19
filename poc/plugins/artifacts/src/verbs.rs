//! The verbs: `artifact <sub>`, `group`, and the hidden prefix+a trio. Ported from main's
//! `verbs/artifacts.rs` and the `group` verb of `verbs/sessions.rs`; arguments are parsed by
//! hand (main's argparse shim is not ported).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value};
use tx_core::{ListFilter, Session, SessionId, TxCore, Verb, VerbError, VerbIo};
use tx_ui::tmux::Ui;

use crate::artifact::{Artifact, USER_ACTOR};
use crate::grouping::{GroupResolver, ext_of, own_group};
use crate::menu::{GO_VERB, MENU_VERB, Mode, Nav, PICK_VERB};
use crate::render::{render_artifact_show, render_artifacts};
use crate::service::{ArtifactError, ArtifactService};
use crate::views::{self, Opener};
use crate::{PLUGIN_ID, service};

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
        "unified diff between two revisions (default: last two)",
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

fn help() -> String {
    let mut out = String::from("usage: txc artifact <subcommand> [args]\n\nsubcommands:\n");
    for (usage, summary) in HELP_ROWS {
        out.push_str(&format!("  {usage:<40} {summary}\n"));
    }
    out
}

fn write_failed(error: std::io::Error) -> VerbError {
    VerbError::Failed(format!("write: {error}"))
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

/// Every record the core holds; unreadable ones are skipped.
fn all_sessions(core: &TxCore) -> Vec<Session> {
    core.list_sessions(&ListFilter::new())
        .unwrap_or_default()
        .iter()
        .filter_map(|id| core.get_session(id).ok())
        .collect()
}

/// id → name for every touch author that is a session.
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

fn read_file(path: &Path) -> Result<Vec<u8>, VerbError> {
    std::fs::read(path).map_err(|error| VerbError::Failed(format!("{}: {error}", path.display())))
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
        "artifact create|modify|group|ls|show|diff|open|doctor   durable versioned artifacts (txc artifact help)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let Some((sub, rest)) = argv.split_first() else {
            write!(io.stdout, "{}", help()).map_err(write_failed)?;
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
                write!(ctx.io.stdout, "{}", help()).map_err(write_failed)?;
                return Ok(0);
            }
            "create" => ctx.create(rest)?,
            "modify" => ctx.modify(rest)?,
            "group" => ctx.group(rest)?,
            "ls" => ctx.ls(rest)?,
            "show" => ctx.show(rest)?,
            "diff" => ctx.diff(rest)?,
            "open" => ctx.open(rest)?,
            "doctor" => ctx.doctor(rest)?,
            other => Outcome::Usage(format!("unknown subcommand '{other}'\n{}", help())),
        };
        match outcome {
            Outcome::Code(code) => Ok(code),
            Outcome::Usage(message) => {
                let _ = writeln!(ctx.io.stderr, "txc artifact {sub}: {message}");
                Ok(2)
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
        writeln!(self.io.stdout, "{text}").map_err(write_failed)
    }

    fn create(&mut self, argv: &[String]) -> Step {
        let args = try_outcome!(parse(argv, &["title", "group"], &[]));
        let [file] = try_outcome!(positionals(&args, 1, 0)) else {
            unreachable!("one positional");
        };
        let path = Path::new(file);
        if !path.is_file() {
            return Ok(Outcome::Usage(format!("no such file: {file}")));
        }
        if args.option("group") == Some("") {
            return Ok(Outcome::Usage("a group cannot be empty".into()));
        }
        let content = read_file(path)?;
        let filename = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let actor = self.artifacts.actor().to_owned();
        let artifact = try_outcome!(self.artifacts.create(
            &actor,
            &content,
            args.option("title").map(str::to_owned),
            filename,
            args.option("group").map(str::to_owned),
        ));
        self.print(&format!(
            "Created artifact {} ({})",
            artifact.id, artifact.filename
        ))?;
        Ok(Outcome::Code(0))
    }

    fn modify(&mut self, argv: &[String]) -> Step {
        let args = try_outcome!(parse(argv, &["changes"], &[]));
        let given = try_outcome!(positionals(&args, 1, 1));
        let artifact_id = try_outcome!(self.artifacts.resolve_id(&given[0]));
        let before = try_outcome!(self.artifacts.require(&artifact_id)).latest_rev();
        let changes = args.option("changes").map(str::to_owned);
        let actor = self.artifacts.actor().to_owned();
        let file = given.get(1);
        let artifact = match file {
            Some(file) => {
                let path = Path::new(file);
                if !path.is_file() {
                    return Ok(Outcome::Usage(format!("no such file: {file}")));
                }
                let content = read_file(path)?;
                try_outcome!(
                    self.artifacts
                        .modify(&artifact_id, &actor, &content, changes)
                )
            }
            None => try_outcome!(
                self.artifacts
                    .snapshot_current(&artifact_id, &actor, changes)
            ),
        };
        if artifact.latest_rev() != before {
            self.print(&format!(
                "Modified artifact {} → rev {}",
                artifact.id,
                artifact.latest_rev()
            ))?;
            return Ok(Outcome::Code(0));
        }
        if file.is_none() {
            self.print(&format!(
                "No change: the working copy is identical to rev {before}; nothing to snapshot."
            ))?;
            return Ok(Outcome::Code(0));
        }
        let mut notice = format!(
            "No change: the supplied file is identical to rev {before}; nothing snapshotted."
        );
        if try_outcome!(self.artifacts.current_is_dirty(&artifact)) {
            notice.push_str(&format!(
                " NOTE: the working copy still has unsnapshotted edits; run \
                 `txc artifact modify {}` (no file) to snapshot them.",
                artifact.id
            ));
        }
        self.print(&notice)?;
        Ok(Outcome::Code(0))
    }

    fn group(&mut self, argv: &[String]) -> Step {
        let args = try_outcome!(parse(argv, &[], &["clear"]));
        let given = try_outcome!(positionals(&args, 1, 1));
        let clear = args.switch("clear");
        let group = given.get(1).map(String::as_str);
        if clear && group.is_some() {
            return Ok(Outcome::Usage("give a group or --clear, not both".into()));
        }
        if group == Some("") {
            return Ok(Outcome::Usage(
                "a group cannot be empty; use --clear to drop the override".into(),
            ));
        }
        let artifact_id = try_outcome!(self.artifacts.resolve_id(&given[0]));
        let actor = self.artifacts.actor().to_owned();
        if clear {
            try_outcome!(self.artifacts.set_group(&artifact_id, &actor, None));
            self.print(&format!(
                "Cleared group override on artifact {artifact_id} (back to derived)"
            ))?;
            return Ok(Outcome::Code(0));
        }
        if let Some(group) = group {
            try_outcome!(
                self.artifacts
                    .set_group(&artifact_id, &actor, Some(group.to_owned()))
            );
            self.print(&format!("Grouped artifact {artifact_id} (group={group})"))?;
            return Ok(Outcome::Code(0));
        }
        let artifact = try_outcome!(self.artifacts.require(&artifact_id));
        let resolved = self.resolved_group(&artifact);
        self.print(&format!(
            "own:      {}",
            artifact.group.as_deref().unwrap_or("—")
        ))?;
        self.print(&format!("resolved: {resolved}"))?;
        Ok(Outcome::Code(0))
    }

    fn resolved_group(&mut self, artifact: &Artifact) -> String {
        let sessions = all_sessions(self.core);
        let artifacts = self.artifacts.all();
        GroupResolver::new(&sessions, &artifacts).artifact_group(artifact)
    }

    fn ls(&mut self, argv: &[String]) -> Step {
        let args = try_outcome!(parse(argv, &["session"], &[]));
        try_outcome!(positionals(&args, 0, 0));
        let scan = self.artifacts.scan();
        for skipped in &scan.skipped {
            let _ = writeln!(self.io.stderr, "{}", skipped.warning());
        }
        let mut artifacts = scan.artifacts;
        if let Some(token) = args.option("session") {
            let id = self
                .core
                .resolve(token)
                .map_or_else(|_| token.to_owned(), |id| id.to_string());
            artifacts.retain(|artifact| artifact.history().iter().any(|t| t.session_id == id));
        }
        let names = names_for(
            self.core,
            artifacts
                .iter()
                .flat_map(Artifact::history)
                .map(|touch| touch.session_id.as_str()),
        );
        let text = render_artifacts(&artifacts, &names, tx_core::events::now());
        self.print(&text)?;
        Ok(Outcome::Code(0))
    }

    fn show(&mut self, argv: &[String]) -> Step {
        let args = try_outcome!(parse(argv, &[], &[]));
        let given = try_outcome!(positionals(&args, 1, 0));
        let artifact_id = try_outcome!(self.artifacts.resolve_id(&given[0]));
        let artifact = try_outcome!(self.artifacts.require(&artifact_id));
        let dirty = try_outcome!(self.artifacts.current_is_dirty(&artifact));
        let names = names_for(
            self.core,
            artifact
                .history()
                .iter()
                .map(|touch| touch.session_id.as_str()),
        );
        let resolved = self.resolved_group(&artifact);
        let text =
            render_artifact_show(&artifact, dirty, &names, tx_core::events::now(), &resolved);
        self.print(&text)?;
        Ok(Outcome::Code(0))
    }

    fn diff(&mut self, argv: &[String]) -> Step {
        let args = try_outcome!(parse(argv, &[], &[]));
        let given = try_outcome!(positionals(&args, 1, 2));
        let revs = match &given[1..] {
            [] => None,
            [a, b] => match (a.parse::<i64>(), b.parse::<i64>()) {
                (Ok(a), Ok(b)) => Some((a, b)),
                _ => return Ok(Outcome::Usage("revisions are integers".into())),
            },
            _ => {
                return Ok(Outcome::Usage(
                    "give both revs or neither (default: the last two)".into(),
                ));
            }
        };
        let artifact_id = try_outcome!(self.artifacts.resolve_id(&given[0]));
        let out = try_outcome!(self.artifacts.diff(&artifact_id, revs));
        if out.is_empty() {
            self.print("(no differences)")?;
        } else {
            write!(self.io.stdout, "{out}").map_err(write_failed)?;
        }
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
        let args = try_outcome!(parse(argv, &["tag", "cwd"], &[]));
        let given = try_outcome!(positionals(&args, 1, 0));
        let artifact_id = try_outcome!(self.artifacts.resolve_id(&given[0]));
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
        let cwd = args
            .option("cwd")
            .filter(|cwd| !cwd.is_empty())
            .map_or_else(
                || self.artifacts.files().content_dir(&artifact),
                PathBuf::from,
            );
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
            &cwd,
            self.editor,
        )?;
        let view = self.core.get_session(&id)?;
        let path = self.artifacts.files().current_path(&artifact);
        self.print(&format!(
            "Opened artifact {artifact_id} in nvim view '{}' ({})",
            view.name,
            path.display()
        ))?;
        Ok(Outcome::Code(0))
    }

    fn doctor(&mut self, argv: &[String]) -> Step {
        let args = try_outcome!(parse(argv, &[], &["repair"]));
        try_outcome!(positionals(&args, 0, 0));
        if args.switch("repair") {
            for line in try_outcome!(self.artifacts.repair_orphans()) {
                self.print(&format!("  {line}"))?;
            }
        }
        let problems = try_outcome!(self.artifacts.doctor());
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
        let usage = |io: &mut VerbIo<'_>, message: &str| {
            let _ = writeln!(io.stderr, "txc group: {message}\n{GROUP_USAGE}");
            Ok(2)
        };
        let args = match parse(argv, &[], &["clear"]) {
            Ok(args) => args,
            Err(message) => return usage(io, &message),
        };
        let clear = args.switch("clear");
        let (token, group) = match args.positionals.as_slice() {
            [token] => (token, None),
            [token, group] => (token, Some(group.as_str())),
            _ => return usage(io, "expected a session and at most one group"),
        };
        if clear && group.is_some() {
            return usage(io, "give a group or --clear, not both");
        }
        if group == Some("") {
            return usage(
                io,
                "a group cannot be empty; use --clear to drop the override",
            );
        }
        let id = core.resolve(token)?;
        let session = core.get_session(&id)?;
        if let Err(problem) = ext_of(&session) {
            let _ = writeln!(io.stderr, "txc group: {problem}");
        }
        if clear || group.is_some() {
            let owned = group.map(str::to_owned);
            core.update_ext(&id, PLUGIN_ID, |value| set_group(value, owned))?;
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
            writeln!(io.stdout, "{line}").map_err(write_failed)?;
            return Ok(0);
        }
        let sessions = all_sessions(core);
        let artifacts = service(core).all();
        let resolved = GroupResolver::new(&sessions, &artifacts).session_group(&session);
        let own = own_group(&session).unwrap_or_else(|| "—".to_owned());
        writeln!(io.stdout, "own:      {own}\nresolved: {resolved}").map_err(write_failed)?;
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

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [session, client] = argv else {
            let _ = writeln!(io.stderr, "usage: txc {MENU_VERB} <session> <client>");
            return Ok(2);
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

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [session, client] = argv else {
            let _ = writeln!(io.stderr, "usage: txc {PICK_VERB} <session> <client>");
            return Ok(2);
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

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let words: Vec<&str> = argv.iter().map(String::as_str).collect();
        let usage = |io: &mut VerbIo<'_>| {
            let _ = writeln!(
                io.stderr,
                "usage: txc {GO_VERB} <session> <client> attach|focus|popup artifact|session <id>"
            );
            Ok(2)
        };
        let [session, client, mode, kind, id] = words[..] else {
            return usage(io);
        };
        let Some(mode) = Mode::parse(mode) else {
            return usage(io);
        };
        let nav = nav(core, &self.editor);
        match kind {
            "artifact" => nav.go_artifact(session, client, mode, id),
            "session" => nav.go_session(session, client, mode, id),
            _ => usage(io),
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
        let argv: Vec<String> = ["id", "--changes", "c", "file", "--clear"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let args = parse(&argv, &["changes"], &["clear"]).unwrap();
        assert_eq!(args.positionals, ["id", "file"]);
        assert_eq!(args.option("changes"), Some("c"));
        assert!(args.switch("clear"));
        assert!(parse(&argv[..2], &["changes"], &[]).is_err());
        assert!(parse(&["--nope".to_owned()], &[], &[]).is_err());
    }
}
