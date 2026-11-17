//! `tx setup`: which of the shipped plugins an install takes. It resolves a preset, a list or
//! the wizard's ticks against a directory of bundles, records the result in
//! `<data>/shipped/.selection` for `tx update` to lay out, seeds `config.yaml` when there is
//! none, and prints the result for the installer.
//!
//! The program opens no core: it only resolves the directories, as every program of tx does.
//! So `tx-setup …` run from a build and `tx setup …` are the same command line.

mod presets;
mod seed;
mod wizard;

use std::collections::BTreeSet;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use tx_core::config::SHIPPED_PLUGINS_DIR;
use tx_plugin::args::{Global, parse_global};
use tx_plugin::bundle::Selection;

use presets::{Bundles, FULL, PRESETS_DIR, Presets};

/// What every shipped program answers by exiting 0 at once: `tx update` runs it after
/// installing a copy.
const PROBE: &str = "--tx-probe";
const USAGE: &str =
    "usage: tx setup --bundles DIR [--preset NAME | --plugins a,b,c | --upgrade] [--list]";

/// Where the selection comes from.
#[derive(Debug, PartialEq, Eq)]
enum Mode {
    /// The wizard on a terminal, else `full`.
    Ask,
    Preset(String),
    Plugins(Vec<String>),
    Upgrade,
}

#[derive(Debug, PartialEq, Eq)]
struct Arguments {
    global: Global,
    bundles: PathBuf,
    mode: Mode,
    list: bool,
}

/// Why the run stopped, as printed after `tx setup: `.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    /// Exit 2.
    Usage(String),
    /// Exit 1.
    Failed(String),
}

fn failed(error: impl std::fmt::Display) -> Failure {
    Failure::Failed(error.to_string())
}

fn parse(arguments: &[String]) -> Result<Arguments, Failure> {
    let usage = |message: String| Failure::Usage(format!("{message}\n{USAGE}"));
    let (global, first) = parse_global(arguments).map_err(|error| usage(error.to_string()))?;
    let mut bundles = None;
    let mut mode = Mode::Ask;
    let mut list = false;
    let mut words = arguments[first..].iter();
    while let Some(flag) = words.next() {
        let mut value = || {
            words
                .next()
                .ok_or_else(|| usage(format!("{flag} needs a value")))
        };
        let chosen = match flag.as_str() {
            "--bundles" => {
                bundles = Some(PathBuf::from(value()?));
                continue;
            }
            "--list" => {
                list = true;
                continue;
            }
            "--preset" => Mode::Preset(value()?.clone()),
            "--plugins" => Mode::Plugins(
                value()?
                    .split(',')
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
                    .collect(),
            ),
            "--upgrade" => Mode::Upgrade,
            _ => return Err(usage(format!("unexpected argument {flag:?}"))),
        };
        if mode != Mode::Ask {
            return Err(usage(
                "--preset, --plugins and --upgrade exclude each other".to_owned(),
            ));
        }
        mode = chosen;
    }
    let bundles = bundles.ok_or_else(|| usage("--bundles DIR is required".to_owned()))?;
    Ok(Arguments {
        global,
        bundles,
        mode,
        list,
    })
}

/// What was chosen.
struct Choice {
    selection: Selection,
    link_dotfiles: bool,
}

impl Choice {
    fn of(preset: Option<&str>, plugins: BTreeSet<String>) -> Self {
        Self {
            selection: Selection {
                preset: preset.map(str::to_owned),
                plugins: plugins.into_iter().collect(),
            },
            link_dotfiles: false,
        }
    }
}

fn note(message: &str) {
    eprintln!("tx setup: {message}");
}

/// The wizard's choice; `None` when the person left it.
fn ask(bundles: &Bundles, presets: &Presets) -> Result<Option<Choice>, Failure> {
    // A preset that names a plugin without a bundle selects nothing here: it is not offered.
    let resolved: Vec<(String, BTreeSet<String>)> = presets
        .0
        .iter()
        .filter_map(|(name, preset)| Some((name.clone(), preset.resolve(bundles).ok()?)))
        .collect();
    let mut wizard = wizard::Wizard::new(bundles, &resolved, FULL, wizard::on_path);
    if !wizard::run(&mut wizard).map_err(failed)? {
        return Ok(None);
    }
    let mut choice = Choice::of(wizard.preset(), wizard.ticked.clone());
    choice.link_dotfiles = wizard.link_dotfiles;
    Ok(Some(choice))
}

/// What an upgrade selects: the recorded preset resolved again; a recorded list, or the list of
/// a preset that is gone, as it is without what has no bundle any more; and `full` for an
/// install that recorded nothing.
fn upgrade(
    recorded: Option<Selection>,
    bundles: &Bundles,
    presets: &Presets,
) -> Result<Choice, String> {
    let Some(recorded) = recorded else {
        return Ok(Choice::of(Some(FULL), presets.resolve(FULL, bundles)?));
    };
    if let Some(name) = &recorded.preset {
        if let Some((_, preset)) = presets.0.iter().find(|(known, _)| known == name) {
            return Ok(Choice::of(Some(name), preset.resolve(bundles)?));
        }
        // The user removed the preset's file: that never stops an upgrade.
        note(&format!(
            "preset {name:?} no longer exists: keeping its plugins"
        ));
    }
    let mut kept = BTreeSet::new();
    for id in recorded.plugins {
        if bundles.manifests.contains_key(&id) {
            kept.insert(id);
        } else {
            note(&format!(
                "{id} has no bundle in {}: dropped",
                bundles.directory.display()
            ));
        }
    }
    Ok(Choice::of(None, kept))
}

fn run(arguments: &[String]) -> Result<ExitCode, Failure> {
    let arguments = parse(arguments)?;
    let config = tx_plugin::setup::core_config(&arguments.global).map_err(failed)?;
    let directories = &config.directories;
    let (bundles, skipped) = Bundles::read(&arguments.bundles)
        .map_err(|error| failed(format!("{}: {error}", arguments.bundles.display())))?;
    let presets_directory = directories.config.join(PRESETS_DIR);
    let (presets, unread) = Presets::read(&presets_directory)
        .map_err(|error| failed(format!("{}: {error}", presets_directory.display())))?;
    for message in skipped.iter().chain(&unread) {
        note(message);
    }

    if arguments.list {
        let mut lines = Vec::new();
        for (name, preset) in &presets.0 {
            let ids: Vec<String> = preset
                .resolve(&bundles)
                .map_err(Failure::Failed)?
                .into_iter()
                .collect();
            lines.push(format!("{name}\t{}", ids.join(" ")));
        }
        for line in lines {
            println!("{line}");
        }
        return Ok(ExitCode::SUCCESS);
    }

    let shipped = directories.data.join(SHIPPED_PLUGINS_DIR);
    let full = || {
        presets
            .resolve(FULL, &bundles)
            .map(|plugins| Choice::of(Some(FULL), plugins))
    };
    let choice = match &arguments.mode {
        Mode::Preset(name) => presets
            .resolve(name, &bundles)
            .map(|plugins| Choice::of(Some(name), plugins)),
        Mode::Plugins(ids) => bundles.named(ids).map(|named| {
            let mut selected = named.clone();
            for id in &named {
                for message in bundles.require(id, &mut selected) {
                    note(&message);
                }
            }
            Choice::of(None, selected)
        }),
        Mode::Upgrade => {
            let recorded = Selection::read(&shipped).map_err(failed)?;
            upgrade(recorded, &bundles, &presets)
        }
        Mode::Ask if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() => {
            match ask(&bundles, &presets)? {
                Some(choice) => Ok(choice),
                // Left: nothing is written.
                None => return Ok(ExitCode::FAILURE),
            }
        }
        Mode::Ask => full(),
    }
    .map_err(Failure::Failed)?;

    choice.selection.write(&shipped).map_err(failed)?;
    if arguments.mode != Mode::Upgrade {
        let file = directories.config.join(tx_plugin::config::FILE_NAME);
        let selected = choice
            .selection
            .plugins
            .iter()
            .map(|id| &bundles.manifests[id]);
        seed::write(&file, &seed::text(selected))
            .map_err(|error| failed(format!("{}: {error}", file.display())))?;
    }
    for id in &choice.selection.plugins {
        println!("plugin\t{id}");
    }
    if choice.link_dotfiles {
        println!("option\tlink-dotfiles\ttrue");
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    let arguments: Vec<String> = match std::env::args_os()
        .skip(1)
        .map(std::ffi::OsString::into_string)
        .collect()
    {
        Ok(arguments) => arguments,
        Err(bad) => {
            note(&format!("argument is not UTF-8: {bad:?}"));
            return ExitCode::from(2);
        }
    };
    if arguments == [PROBE] {
        return ExitCode::SUCCESS;
    }
    match run(&arguments) {
        Ok(code) => code,
        Err(Failure::Usage(message)) => {
            note(&message);
            ExitCode::from(2)
        }
        Err(Failure::Failed(message)) => {
            note(&message);
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn the_flags_are_read_after_the_global_ones() {
        let arguments = parse(&words("--data-dir /d --bundles /b --plugins a,,b --list")).unwrap();
        assert_eq!(arguments.global.data_dir, Some(PathBuf::from("/d")));
        assert_eq!(arguments.bundles, PathBuf::from("/b"));
        assert_eq!(
            arguments.mode,
            Mode::Plugins(vec!["a".to_owned(), "b".to_owned()])
        );
        assert!(arguments.list);
        assert_eq!(parse(&words("--bundles /b")).unwrap().mode, Mode::Ask);
        assert_eq!(
            parse(&words("--upgrade --bundles /b")).unwrap().mode,
            Mode::Upgrade
        );
    }

    #[test]
    fn misuse_is_a_usage_error() {
        for (line, message) in [
            ("--list", "--bundles DIR is required"),
            ("--bundles", "--bundles needs a value"),
            ("--bundles /b --preset", "--preset needs a value"),
            (
                "--bundles /b --preset a --upgrade",
                "--preset, --plugins and --upgrade exclude each other",
            ),
            ("--bundles /b extra", "unexpected argument \"extra\""),
        ] {
            assert_eq!(
                parse(&words(line)).unwrap_err(),
                Failure::Usage(format!("{message}\n{USAGE}")),
                "{line}"
            );
        }
    }
}
