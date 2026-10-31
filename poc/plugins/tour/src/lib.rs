//! tx-core plugin `tour`: a code tour is an artifact of kind `tour` whose working copy
//! `current.lua` is a Lua file returning the ordered stops (`{file, line, label, message,
//! severity}`, `file` relative to the tour's worktree). `txc tour create` registers one through
//! the artifacts service; `txc tour open` (also what `txc artifact open` and prefix+a run for a
//! tour, by the open-by-kind convention) starts a tx-client view of kind `tour`: nvim in the
//! worktree running tx-client's `require("tx-tour")(<current.lua>)`, bound to the artifact.
//!
//! A tour is revised like a document: edit `$(txc artifact path <id>)/current.lua` in place and
//! snapshot it with `txc artifact modify <id>`. There is no `tour modify`.
//!
//! Trust: the stops file is Lua run inside nvim, at create (validation) and at every open. It is
//! as trusted as the operator or agent who wrote it, the same model as production.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use serde_json::{Map, Value};
use tx_artifacts::artifact::{Artifact, Kind, USER_ACTOR};
use tx_artifacts::service::ArtifactService;
use tx_client::tmux::Ui;
use tx_client::views::{self, ViewKind, ViewRequest};
use tx_core::{Plugin, PluginError, Registry, TxCore, Verb, VerbError, VerbIo};

pub const PLUGIN_ID: &str = "tour";
/// The verb, and the artifact kind it opens.
pub const KIND: &str = "tour";
/// The artifact's filename: its working copy is `current.lua`, its revisions `revs/<n>.lua`.
const FILENAME: &str = "stops.lua";
/// The working copy, recorded as the kind's `stops` fact.
pub const STOPS: &str = "current.lua";
const VIEW_SUFFIX: &str = "-tour";

const USAGE: &str = "usage: txc tour create <stops.lua> --title T [--cwd W] | open <id> [--tag T]";

/// Run by `nvim -l -` with the stops path as `arg[1]`: exits 1 with the reason on stderr unless
/// the file loads and returns a list.
const VALIDATOR: &str = r#"local ok, stops = pcall(dofile, arg[1])
if not ok then io.stderr:write(tostring(stops), "\n") os.exit(1) end
if type(stops) ~= "table" or not vim.islist(stops) then
  io.stderr:write("does not return a list of stops\n") os.exit(1)
end"#;

#[derive(Clone, Copy, Debug, Default)]
pub struct TourPlugin;

impl Plugin for TourPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.verb(Arc::new(TourVerb));
        Ok(())
    }
}

struct TourVerb;

impl Verb for TourVerb {
    fn name(&self) -> &'static str {
        KIND
    }

    fn usage(&self) -> &'static str {
        "tour create <stops.lua> --title T [--cwd W] | open <id> [--tag T]"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let command = match parse_args(argv) {
            Ok(command) => command,
            Err(message) => {
                writeln!(io.stderr, "txc tour: {message}\n{USAGE}").map_err(write_failed)?;
                return Ok(2);
            }
        };
        let artifacts = tx_artifacts::service(core);
        let line = match command {
            Subcommand::Create {
                stops,
                title,
                worktree,
            } => create(&artifacts, &stops, title, worktree.as_deref())?,
            Subcommand::Open { token, tags } => open(core, &artifacts, &token, tags)?,
        };
        writeln!(io.stdout, "{line}").map_err(write_failed)?;
        Ok(0)
    }
}

#[derive(Debug, PartialEq)]
enum Subcommand {
    Create {
        stops: PathBuf,
        title: String,
        worktree: Option<PathBuf>,
    },
    Open {
        token: String,
        tags: Option<Vec<String>>,
    },
}

fn parse_args(argv: &[String]) -> Result<Subcommand, String> {
    let (sub, rest) = argv.split_first().ok_or("missing subcommand")?;
    let mut positionals = Vec::new();
    let mut title = None;
    let mut cwd = None;
    let mut tag = None;
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        let slot = match word.as_str() {
            "--title" => &mut title,
            "--cwd" => &mut cwd,
            "--tag" => &mut tag,
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ => {
                positionals.push(word.clone());
                continue;
            }
        };
        *slot = Some(words.next().ok_or(format!("{word} needs a value"))?.clone());
    }
    let [positional] = positionals.as_slice() else {
        return Err(format!("{sub} takes exactly one argument"));
    };
    match sub.as_str() {
        "create" => {
            if tag.is_some() {
                return Err("--tag belongs to open".into());
            }
            let title = title
                .filter(|title| !title.is_empty())
                .ok_or("--title is required")?;
            Ok(Subcommand::Create {
                stops: PathBuf::from(positional),
                title,
                worktree: cwd.map(PathBuf::from),
            })
        }
        "open" => {
            if title.is_some() || cwd.is_some() {
                return Err("--title and --cwd belong to create".into());
            }
            let tags = match tag {
                None => None,
                Some(raw) => {
                    let tags: Vec<String> = raw
                        .split(',')
                        .filter(|tag| !tag.is_empty())
                        .map(str::to_owned)
                        .collect();
                    if tags.is_empty() {
                        return Err("--tag requires at least one value".into());
                    }
                    Some(tags)
                }
            };
            Ok(Subcommand::Open {
                token: positional.clone(),
                tags,
            })
        }
        other => Err(format!("unknown subcommand {other:?}")),
    }
}

fn failed(error: impl std::fmt::Display) -> VerbError {
    VerbError::Failed(error.to_string())
}

fn write_failed(error: std::io::Error) -> VerbError {
    VerbError::Failed(format!("write: {error}"))
}

/// Refuses a stops file nvim cannot load as a list, with nvim's own error.
pub fn validate(stops: &Path) -> Result<(), VerbError> {
    let mut child = Command::new("nvim")
        .args(["--clean", "-l", "-"])
        .arg(stops)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| failed(format!("cannot run nvim to check the stops: {error}")))?;
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(VALIDATOR.as_bytes())
        .map_err(write_failed)?;
    let output = child.wait_with_output().map_err(failed)?;
    if output.status.success() {
        return Ok(());
    }
    Err(failed(format!(
        "{} is not a tour: {}",
        stops.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

/// The kind as stored: `{"kind": "tour", "worktree": <absolute>, "stops": "current.lua"}`.
pub fn tour_kind(worktree: &Path) -> Kind {
    let mut facts = Map::new();
    facts.insert(
        "worktree".into(),
        Value::String(worktree.to_string_lossy().into_owned()),
    );
    facts.insert("stops".into(), Value::String(STOPS.into()));
    Kind::Other {
        kind: KIND.into(),
        facts,
    }
}

/// Validates the stops and registers them as a new tour artifact (rev 0 and `current.lua`).
pub fn create_tour(
    artifacts: &ArtifactService,
    stops: &Path,
    title: String,
    worktree: Option<&Path>,
) -> Result<Artifact, VerbError> {
    let worktree = match worktree {
        Some(worktree) => worktree.to_path_buf(),
        None => std::env::current_dir().map_err(failed)?,
    };
    let worktree = worktree
        .canonicalize()
        .ok()
        .filter(|path| path.is_dir())
        .ok_or_else(|| failed(format!("no such directory: {}", worktree.display())))?;
    validate(stops)?;
    let content =
        std::fs::read(stops).map_err(|error| failed(format!("{}: {error}", stops.display())))?;
    artifacts
        .create_with_kind(
            artifacts.actor(),
            tour_kind(&worktree),
            &content,
            Some(title),
            FILENAME.to_owned(),
            None,
        )
        .map_err(failed)
}

fn create(
    artifacts: &ArtifactService,
    stops: &Path,
    title: String,
    worktree: Option<&Path>,
) -> Result<String, VerbError> {
    let artifact = create_tour(artifacts, stops, title, worktree)?;
    let tour = tour_of(artifacts, &artifact)?;
    Ok(format!(
        "Created tour artifact {} (worktree {})",
        artifact.id,
        tour.worktree.display()
    ))
}

/// Where a tour's nvim runs and what it loads.
#[derive(Debug, PartialEq)]
pub struct Tour {
    pub worktree: PathBuf,
    pub stops: PathBuf,
}

/// The tour an artifact describes; its worktree must still exist.
pub fn tour_of(artifacts: &ArtifactService, artifact: &Artifact) -> Result<Tour, VerbError> {
    let Kind::Other { kind, facts } = &artifact.kind else {
        return Err(failed(format!(
            "artifact {} is a {}, not a tour",
            artifact.id,
            artifact.kind.name()
        )));
    };
    if kind != KIND {
        return Err(failed(format!(
            "artifact {} is a {kind}, not a tour",
            artifact.id
        )));
    }
    let fact = |name: &str| {
        facts
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| failed(format!("artifact {} has no {name}", artifact.id)))
    };
    let worktree = PathBuf::from(fact("worktree")?);
    if !worktree.is_dir() {
        return Err(failed(format!(
            "tour worktree `{}` is gone",
            worktree.display()
        )));
    }
    let stops = artifacts
        .dir(&artifact.id)
        .map_err(failed)?
        .join(fact("stops")?);
    Ok(Tour { worktree, stops })
}

/// `s` as a single-quoted Lua string literal.
fn lua_string(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\n', "\\n");
    format!("'{escaped}'")
}

/// nvim's arguments for a tour. tx-client appends its own `-c` that puts the runtime holding
/// `tx-tour` on the runtimepath, after these; so the tour loads on `VimEnter`, which fires once
/// every `-c` has run.
pub fn nvim_arguments(stops: &Path) -> Vec<String> {
    vec![
        "-c".to_owned(),
        format!(
            "autocmd VimEnter * ++once lua require('tx-tour')({})",
            lua_string(&stops.to_string_lossy())
        ),
    ]
}

fn open(
    core: &TxCore,
    artifacts: &ArtifactService,
    token: &str,
    tags: Option<Vec<String>>,
) -> Result<String, VerbError> {
    let artifact_id = artifacts.resolve_id(token).map_err(failed)?;
    let artifact = artifacts.require(&artifact_id).map_err(failed)?;
    let ui = Ui::from_core(core);
    if let Some(shown) = tx_artifacts::views::live_view(&ui, core, &artifact_id)? {
        let switched = ui.inside()
            && ui
                .quiet(&["switch-client", "-t", &format!("={}", shown.name)])
                .0;
        let note = if switched { "; switched to it" } else { "" };
        return Ok(format!(
            "Tour {artifact_id} is already open in view '{}'{note}",
            shown.name
        ));
    }
    let tour = tour_of(artifacts, &artifact)?;
    let invoker = (artifacts.actor() != USER_ACTOR)
        .then(|| artifacts.actor().parse().ok())
        .flatten()
        .and_then(|id| core.get_session(&id).ok());
    let tags = tags
        .or_else(|| {
            invoker
                .as_ref()
                .map(|session| session.tags.clone())
                .filter(|tags| !tags.is_empty())
        })
        .unwrap_or_else(|| vec!["artifact".to_owned()]);
    let name = view_name(&ui, &artifact);
    let view = views::open(
        &ui,
        &ViewRequest {
            name: &name,
            kind: ViewKind::Tour,
            program: "nvim",
            arguments: &nvim_arguments(&tour.stops),
            working_directory: &tour.worktree,
            tags: &tags,
            parent_id: invoker.as_ref().map(|session| &session.id),
            bound: Some(&artifact_id),
        },
    )
    .map_err(failed)?;
    artifacts
        .opened(&artifact_id, artifacts.actor())
        .map_err(failed)?;
    Ok(format!(
        "Opened tour {artifact_id} in view '{}' ({} in {})",
        view.name,
        tour.stops.display(),
        tour.worktree.display()
    ))
}

/// `<title>-tour`, with the id's first 8 chars appended when a session already holds that name.
fn view_name(ui: &Ui, artifact: &Artifact) -> String {
    let base = format!(
        "{}{VIEW_SUFFIX}",
        tx_artifacts::views::sanitize(artifact.display_name())
    );
    if ui.has_session(&base) {
        let short: String = artifact.id.chars().take(8).collect();
        return format!("{base}-{short}");
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn arguments_parse_into_create_and_open() {
        assert_eq!(
            parse_args(&words("create s.lua --title walk --cwd /r")).unwrap(),
            Subcommand::Create {
                stops: "s.lua".into(),
                title: "walk".into(),
                worktree: Some("/r".into()),
            }
        );
        assert_eq!(
            parse_args(&words("create s.lua --title walk")).unwrap(),
            Subcommand::Create {
                stops: "s.lua".into(),
                title: "walk".into(),
                worktree: None,
            }
        );
        assert_eq!(
            parse_args(&words("open ab12 --tag qa,x")).unwrap(),
            Subcommand::Open {
                token: "ab12".into(),
                tags: Some(vec!["qa".into(), "x".into()]),
            }
        );
        assert_eq!(
            parse_args(&words("open ab12")).unwrap(),
            Subcommand::Open {
                token: "ab12".into(),
                tags: None,
            }
        );
        for bad in [
            "",
            "create s.lua",
            "create --title t",
            "create a b --title t",
            "create s.lua --title t --tag x",
            "open",
            "open a --cwd /r",
            "open a --tag ,",
            "open a --title",
            "open a --nope x",
            "modify a",
        ] {
            assert!(parse_args(&words(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_nvim_command_quotes_the_path_and_waits_for_vim_enter() {
        assert_eq!(
            nvim_arguments(Path::new("/d/it's\\a/current.lua")),
            [
                "-c",
                "autocmd VimEnter * ++once lua require('tx-tour')('/d/it\\'s\\\\a/current.lua')"
            ]
        );
    }

    fn nvim_available() -> bool {
        Command::new("nvim")
            .arg("--version")
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    #[test]
    fn create_records_the_worktree_and_open_needs_it() {
        if !nvim_available() {
            eprintln!("nvim not available; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("repo");
        std::fs::create_dir(&worktree).unwrap();
        let stops = dir.path().join("stops.lua");
        let text =
            "return {{ file = 'a.rs', line = 1, label = 'l', message = 'm', severity = 'INFO' }}\n";
        std::fs::write(&stops, text).unwrap();
        let artifacts = ArtifactService::new(dir.path().join("store"), USER_ACTOR);

        let artifact = create_tour(&artifacts, &stops, "walk".into(), Some(&worktree)).unwrap();
        let canonical = worktree.canonicalize().unwrap();
        assert_eq!(artifact.kind, tour_kind(&canonical));
        assert_eq!(artifact.kind.name(), "tour");
        assert_eq!(artifact.title.as_deref(), Some("walk"));
        let current = artifacts.dir(&artifact.id).unwrap().join(STOPS);
        assert_eq!(artifacts.files().current_path(&artifact), current);
        assert_eq!(std::fs::read_to_string(&current).unwrap(), text);
        let reread = artifacts.require(&artifact.id).unwrap();
        assert_eq!(reread.kind, artifact.kind);

        assert_eq!(
            tour_of(&artifacts, &reread).unwrap(),
            Tour {
                worktree: canonical,
                stops: current,
            }
        );
        std::fs::remove_dir(&worktree).unwrap();
        let error = tour_of(&artifacts, &reread).unwrap_err().to_string();
        assert!(
            error.starts_with("tour worktree `") && error.ends_with("` is gone"),
            "{error}"
        );
    }

    #[test]
    fn create_refuses_what_nvim_cannot_load_as_a_list() {
        if !nvim_available() {
            eprintln!("nvim not available; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let artifacts = ArtifactService::new(dir.path().join("store"), USER_ACTOR);
        for (text, reason) in [
            ("return {", "unexpected symbol"),
            ("return 5", "does not return a list of stops"),
            ("return { a = 1 }", "does not return a list of stops"),
            ("error('boom')", "boom"),
        ] {
            let stops = dir.path().join("bad.lua");
            std::fs::write(&stops, text).unwrap();
            let error = create_tour(&artifacts, &stops, "t".into(), Some(dir.path()))
                .unwrap_err()
                .to_string();
            assert!(error.contains(reason), "{text:?}: {error}");
        }
        let error = create_tour(
            &artifacts,
            &dir.path().join("bad.lua"),
            "t".into(),
            Some(&dir.path().join("nope")),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("no such directory"), "{error}");
        assert!(artifacts.all().is_empty());
    }

    #[test]
    fn another_kind_is_not_a_tour() {
        let dir = tempfile::tempdir().unwrap();
        let artifacts = ArtifactService::new(dir.path().join("store"), USER_ACTOR);
        let document = artifacts
            .create(USER_ACTOR, b"x", None, Some("plan.md".into()), None)
            .unwrap();
        let error = tour_of(&artifacts, &document).unwrap_err().to_string();
        assert!(error.contains("is a document, not a tour"), "{error}");
    }
}
