//! tx-core plugin `layouts`: saved pane layouts, `tx layout`. A layout is one YAML file,
//! `<config>/layouts/<name>.yaml` (its format is in [`pane`]), read only when the verb runs.
//! `ls` lists the names, `path` prints a layout's file (it need not exist yet), `check`
//! validates a file as `open` reads it, with no tmux and no agents, `open` builds the layout's agents, commands and shells as one view ([`open`]), and `save` writes a
//! view's window as a layout ([`save`]).

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tx_core::{Plugin, PluginError, Registry, TxCore, Verb, VerbError, VerbIo};

pub mod open;
pub mod pane;
pub mod save;

pub const PLUGIN_ID: &str = "layouts";
/// Under the config directory: one `<name>.yaml` per layout.
pub const LAYOUTS_DIR: &str = "layouts";
const EXTENSION: &str = "yaml";

const USAGE: &str = "usage: tx layout ls | open NAME [--cwd DIR] [--agent A] [--tag T]… [--detach] | save NAME [--view V] [--force] | path NAME | check NAME";

#[derive(Clone, Copy, Debug, Default)]
pub struct LayoutsPlugin;

impl Plugin for LayoutsPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.verb(Arc::new(LayoutVerb));
        Ok(())
    }
}

/// `<config>/layouts/`. Nothing is created: the operator's editor writes the first file.
pub fn layouts_dir(core: &TxCore) -> PathBuf {
    core.config().directories.config.join(LAYOUTS_DIR)
}

fn failed(error: impl std::fmt::Display) -> VerbError {
    VerbError::Failed(error.to_string())
}

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Ls,
    Check(String),
    Open(OpenArgs),
    Path(String),
    Save(SaveArgs),
}

#[derive(Debug, Default, PartialEq, Eq)]
struct SaveArgs {
    name: String,
    view: Option<String>,
    force: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct OpenArgs {
    name: String,
    cwd: Option<String>,
    agent: Option<String>,
    tags: Vec<String>,
    detach: bool,
}

fn parse_args(argv: &[String]) -> Option<Action> {
    let (action, rest) = argv.split_first()?;
    match (action.as_str(), rest) {
        ("ls", []) => Some(Action::Ls),
        ("path", [name]) => Some(Action::Path(name.clone())),
        ("check", [name]) => Some(Action::Check(name.clone())),
        ("open", _) => {
            let mut args = OpenArgs::default();
            let mut name = None;
            let mut rest = rest.iter();
            while let Some(word) = rest.next() {
                match word.as_str() {
                    "--cwd" => args.cwd = Some(rest.next()?.clone()),
                    "--agent" => args.agent = Some(rest.next()?.clone()),
                    "--tag" => args.tags.push(rest.next()?.clone()),
                    "--detach" => args.detach = true,
                    flag if flag.starts_with("--") => return None,
                    _ if name.is_none() => name = Some(word.clone()),
                    _ => return None,
                }
            }
            args.name = name?;
            Some(Action::Open(args))
        }
        ("save", _) => {
            let mut args = SaveArgs::default();
            let mut name = None;
            let mut rest = rest.iter();
            while let Some(word) = rest.next() {
                match word.as_str() {
                    "--view" => args.view = Some(rest.next()?.clone()),
                    "--force" => args.force = true,
                    flag if flag.starts_with("--") => return None,
                    _ if name.is_none() => name = Some(word.clone()),
                    _ => return None,
                }
            }
            args.name = name?;
            Some(Action::Save(args))
        }
        _ => None,
    }
}

/// `<directory>/<name>.yaml`, for a name the home's choice line and a file stem can both hold.
fn layout_path(directory: &Path, name: &str) -> Result<PathBuf, VerbError> {
    if !pane::is_valid_name(name) {
        return Err(failed(format!(
            "invalid layout name {name:?} (allowed: [A-Za-z0-9_.-]+)"
        )));
    }
    Ok(directory.join(format!("{name}.{EXTENSION}")))
}

/// Layout `name`'s panes, read and validated: what `open` builds and `check` counts, with the
/// same errors.
fn read_layout(directory: &Path, name: &str) -> Result<Vec<pane::Pane>, VerbError> {
    let path = layout_path(directory, name)?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(failed(format!(
                "no layout {name}: {} does not exist (`tx layout ls` lists the layouts)",
                path.display()
            )));
        }
        Err(error) => return Err(failed(format!("{}: {error}", path.display()))),
    };
    pane::parse(&path, &text).map_err(failed)
}

/// `check`'s line, `NAME ok: N panes`, or the error `open` would fail with.
fn check(directory: &Path, name: &str) -> Result<String, VerbError> {
    let panes = read_layout(directory, name)?;
    Ok(format!("{name} ok: {} panes", panes.len()))
}

/// The names of the `.yaml` files in `directory` whose stem is a valid name, sorted; none when
/// the directory does not exist yet.
fn list(directory: &Path) -> Result<Vec<String>, VerbError> {
    let in_directory = |error: std::io::Error| failed(format!("{}: {error}", directory.display()));
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(in_directory(error)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let path = entry.map_err(in_directory)?.path();
        if path
            .extension()
            .is_none_or(|extension| extension != EXTENSION)
        {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
            && pane::is_valid_name(stem)
        {
            names.push(stem.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// `tx layout …`.
pub struct LayoutVerb;

impl Verb for LayoutVerb {
    fn name(&self) -> &'static str {
        "layout"
    }

    fn usage(&self) -> &'static str {
        "layout ls | open NAME [--cwd DIR] [--agent A] [--tag T]… [--detach] | save NAME [--view V] [--force] | path NAME | check NAME   open a saved pane layout as a view, save a view as one, or check a layout file"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let Some(action) = parse_args(argv) else {
            let _ = writeln!(io.stderr, "{USAGE}");
            return Ok(2);
        };
        let directory = layouts_dir(core);
        match action {
            Action::Ls => {
                for name in list(&directory)? {
                    writeln!(io.stdout, "{name}").map_err(failed)?;
                }
                Ok(0)
            }
            Action::Path(name) => {
                let path = layout_path(&directory, &name)?;
                writeln!(io.stdout, "{}", path.display()).map_err(failed)?;
                Ok(0)
            }
            Action::Save(args) => {
                let path = layout_path(&directory, &args.name)?;
                save::save(
                    core,
                    io,
                    &save::SaveRequest {
                        view: args.view.as_deref(),
                        path: &path,
                        force: args.force,
                    },
                )
            }
            Action::Check(name) => {
                writeln!(io.stdout, "{}", check(&directory, &name)?).map_err(failed)?;
                Ok(0)
            }
            Action::Open(args) => {
                let panes = read_layout(&directory, &args.name)?;
                let cwd = match &args.cwd {
                    Some(cwd) => PathBuf::from(cwd),
                    None => {
                        std::env::current_dir().map_err(|error| failed(format!("cwd: {error}")))?
                    }
                };
                let cwd = std::fs::canonicalize(&cwd)
                    .map_err(|error| failed(format!("{}: {error}", cwd.display())))?;
                open::open(
                    core,
                    io,
                    &open::OpenRequest {
                        layout: &args.name,
                        panes: &panes,
                        cwd: &cwd,
                        agent: args.agent.as_deref(),
                        tags: &args.tags,
                        detach: args.detach,
                    },
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn args_parse_per_subcommand() {
        assert_eq!(parse_args(&argv(&["ls"])), Some(Action::Ls));
        assert_eq!(
            parse_args(&argv(&["check", "pair"])),
            Some(Action::Check("pair".to_owned()))
        );
        assert_eq!(
            parse_args(&argv(&["path", "pair"])),
            Some(Action::Path("pair".to_owned()))
        );
        assert_eq!(
            parse_args(&argv(&[
                "open", "pair", "--cwd", "/r", "--agent", "dev", "--tag", "a", "--tag", "b",
                "--detach"
            ])),
            Some(Action::Open(OpenArgs {
                name: "pair".to_owned(),
                cwd: Some("/r".to_owned()),
                agent: Some("dev".to_owned()),
                tags: vec!["a".to_owned(), "b".to_owned()],
                detach: true,
            }))
        );
        assert_eq!(
            parse_args(&argv(&["save", "mine", "--view", "sh1", "--force"])),
            Some(Action::Save(SaveArgs {
                name: "mine".to_owned(),
                view: Some("sh1".to_owned()),
                force: true,
            }))
        );
        assert_eq!(
            parse_args(&argv(&["save", "mine"])),
            Some(Action::Save(SaveArgs {
                name: "mine".to_owned(),
                ..SaveArgs::default()
            }))
        );
        assert_eq!(
            parse_args(&argv(&["open", "--tag", "t", "pair"])),
            Some(Action::Open(OpenArgs {
                name: "pair".to_owned(),
                tags: vec!["t".to_owned()],
                ..OpenArgs::default()
            }))
        );
        for bad in [
            &[][..],
            &["fly"],
            &["save"],
            &["save", "a", "b"],
            &["save", "a", "--view"],
            &["save", "a", "--cwd", "x"],
            &["ls", "x"],
            &["path"],
            &["path", "a", "b"],
            &["check"],
            &["check", "a", "b"],
            &["check", "a", "--force"],
            &["open"],
            &["open", "a", "b"],
            &["open", "a", "--cwd"],
            &["open", "a", "--force"],
        ] {
            assert_eq!(parse_args(&argv(bad)), None, "{bad:?}");
        }
    }

    #[test]
    fn a_layout_path_needs_a_valid_name() {
        let directory = Path::new("/c/layouts");
        assert_eq!(
            layout_path(directory, "pair").unwrap(),
            Path::new("/c/layouts/pair.yaml")
        );
        for bad in ["../x", "a b", ""] {
            assert!(layout_path(directory, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn ls_lists_valid_yaml_stems_sorted() {
        let directory = std::env::temp_dir().join(format!("txlayouts-ls-{}", std::process::id()));
        assert!(
            list(&directory).unwrap().is_empty(),
            "a missing directory lists nothing"
        );
        std::fs::create_dir_all(&directory).unwrap();
        for file in [
            "solo.yaml",
            "pair.yaml",
            "bad name.yaml",
            "notes.txt",
            "x.yml",
        ] {
            std::fs::write(directory.join(file), "panes: [{}]\n").unwrap();
        }
        assert_eq!(list(&directory).unwrap(), ["pair", "solo"]);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn check_counts_the_panes_or_fails_as_open_does() {
        let directory =
            std::env::temp_dir().join(format!("txlayouts-check-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let directory = directory.as_path();
        let write = |name: &str, text: &str| {
            std::fs::write(directory.join(format!("{name}.yaml")), text).unwrap();
        };
        write(
            "pair",
            "panes:\n  - agent: developer\n  - agent: qa\n    split: right\n    of: 0\n    size: 50%\n  - command: nvim .\n    split: below\n    of: 1\n    size: 40%\n  - split: below\n    of: 0\n    size: 30%\n",
        );
        assert_eq!(check(directory, "pair").unwrap(), "pair ok: 4 panes");
        let bad = [
            ("empty", "panes: []\n"),
            ("two", "panes:\n  - agent: a\n    command: b\n"),
            ("blank", "panes:\n  - agent: \"\"\n"),
            ("stray", "panes:\n  - command: x\n    model: m\n"),
            (
                "first",
                "panes:\n  - split: below\n    of: 0\n    size: 10\n",
            ),
            ("unplaced", "panes:\n  - {}\n  - {}\n"),
            (
                "forward",
                "panes:\n  - {}\n  - split: below\n    of: 1\n    size: 10\n",
            ),
            (
                "size",
                "panes:\n  - {}\n  - split: below\n    of: 0\n    size: 120%\n",
            ),
            ("unknown", "panes:\n  - colour: red\n"),
            (
                "value",
                "panes:\n  - {}\n  - split: up\n    of: 0\n    size: 10\n",
            ),
            ("yaml", "panes: [\n"),
        ];
        for (name, text) in bad {
            write(name, text);
        }
        for name in bad.iter().map(|(name, _)| *name).chain(["missing", "a b"]) {
            let checked = check(directory, name).unwrap_err().to_string();
            let read = read_layout(directory, name).unwrap_err().to_string();
            assert_eq!(checked, read, "{name}: the error open fails with");
            assert!(checked.contains(name) || name == "a b", "{name}: {checked}");
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
}
