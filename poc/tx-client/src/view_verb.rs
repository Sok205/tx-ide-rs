//! `txc ui view open|ls|close`: the views store from a shell. `ls` first closes the views whose
//! agent is gone (decision V3: orphans go on listing, not through events).

use std::path::PathBuf;

use tx_core::{SessionId, TxCore, VerbError, VerbIo};

use crate::tmux::Ui;
use crate::views::{self, ViewKind, ViewRequest};

const USAGE: &str = "usage: txc ui view open <name> --kind <kind> [--parent <agent>] [--cwd <dir>] [--tag <tag>]… [--bound <value>] [-- <program> [<argument>…]]\n\
                     \x20      txc ui view ls\n\
                     \x20      txc ui view close <name>";

#[derive(Debug, PartialEq, Eq)]
struct OpenArguments {
    name: String,
    kind: ViewKind,
    parent: Option<String>,
    working_directory: Option<PathBuf>,
    tags: Vec<String>,
    bound: Option<String>,
    command: Vec<String>,
}

/// `None` on a malformed command line (the caller prints the usage).
fn parse_open(argv: &[String]) -> Option<OpenArguments> {
    let (name, mut rest) = argv.split_first()?;
    if name.starts_with("--") {
        return None;
    }
    let mut kind = None;
    let mut parent = None;
    let mut working_directory = None;
    let mut tags = Vec::new();
    let mut bound = None;
    let mut command = Vec::new();
    while let Some((flag, after)) = rest.split_first() {
        if flag == "--" {
            command = after.to_vec();
            break;
        }
        let (value, after) = after.split_first()?;
        match flag.as_str() {
            "--kind" => kind = Some(value.parse().ok()?),
            "--parent" => parent = Some(value.clone()),
            "--cwd" => working_directory = Some(PathBuf::from(value)),
            "--tag" => tags.push(value.clone()),
            "--bound" => bound = Some(value.clone()),
            _ => return None,
        }
        rest = after;
    }
    Some(OpenArguments {
        name: name.clone(),
        kind: kind?,
        parent,
        working_directory,
        tags,
        bound,
        command,
    })
}

fn failed(error: impl std::fmt::Display) -> VerbError {
    VerbError::Failed(error.to_string())
}

fn usage(io: &mut VerbIo<'_>) -> Result<i32, VerbError> {
    let _ = writeln!(io.stderr, "{USAGE}");
    Ok(2)
}

pub fn run(core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
    let ui = Ui::from_core(core);
    match argv.split_first() {
        Some((sub, rest)) if sub == "open" => match parse_open(rest) {
            Some(arguments) => open(core, &ui, io, arguments),
            None => usage(io),
        },
        Some((sub, [])) if sub == "ls" => ls(core, &ui, io),
        Some((sub, [name])) if sub == "close" => close(&ui, io, name),
        _ => usage(io),
    }
}

fn open(
    core: &TxCore,
    ui: &Ui,
    io: &mut VerbIo<'_>,
    arguments: OpenArguments,
) -> Result<i32, VerbError> {
    let parent: Option<SessionId> = match &arguments.parent {
        Some(token) => Some(core.resolve(token)?),
        None => None,
    };
    let working_directory = match arguments.working_directory {
        Some(directory) => std::path::absolute(&directory)
            .map_err(|error| failed(format!("{}: {error}", directory.display())))?,
        None => std::env::current_dir().map_err(failed)?,
    };
    let (program, program_arguments) = match arguments.command.split_first() {
        Some((program, program_arguments)) => (program.clone(), program_arguments.to_vec()),
        None => (views::default_shell(), Vec::new()),
    };
    let view = views::open(
        ui,
        &ViewRequest {
            name: &arguments.name,
            kind: arguments.kind,
            program: &program,
            arguments: &program_arguments,
            working_directory: &working_directory,
            tags: &arguments.tags,
            parent_id: parent.as_ref(),
            bound: arguments.bound.as_deref(),
        },
    )
    .map_err(failed)?;
    writeln!(io.stdout, "{}", view.name).map_err(failed)?;
    Ok(0)
}

/// One view per line: `name kind parent cwd tags bound`, tab-separated; the parent is the
/// agent's name (every remaining parent is known to core once the orphans are closed).
fn ls(core: &TxCore, ui: &Ui, io: &mut VerbIo<'_>) -> Result<i32, VerbError> {
    for closed in views::close_orphans(ui, core).map_err(failed)? {
        let _ = writeln!(io.stderr, "closed orphaned view '{closed}'");
    }
    for view in views::list(ui).map_err(failed)? {
        let parent = match &view.parent_id {
            Some(id) => core.get_session(id)?.name,
            None => String::new(),
        };
        writeln!(
            io.stdout,
            "{}\t{}\t{parent}\t{}\t{}\t{}",
            view.name,
            view.kind,
            view.working_directory.display(),
            view.tags.join(","),
            view.bound.unwrap_or_default()
        )
        .map_err(failed)?;
    }
    Ok(0)
}

fn close(ui: &Ui, io: &mut VerbIo<'_>, name: &str) -> Result<i32, VerbError> {
    match views::close(ui, name) {
        Ok(()) => Ok(0),
        Err(error) => {
            let _ = writeln!(io.stderr, "txc ui view close: {error}");
            Ok(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn open_parses_every_flag_and_the_command_after_the_dashes() {
        let parsed = parse_open(&words(&[
            "qa-shell", "--kind", "shell", "--parent", "qa-agent", "--cwd", "/tmp/repo", "--tag",
            "qa", "--tag", "x", "--bound", "a50727bb", "--", "zsh", "-l", "--kind",
        ]))
        .unwrap();
        assert_eq!(
            parsed,
            OpenArguments {
                name: "qa-shell".to_owned(),
                kind: ViewKind::Shell,
                parent: Some("qa-agent".to_owned()),
                working_directory: Some(PathBuf::from("/tmp/repo")),
                tags: words(&["qa", "x"]),
                bound: Some("a50727bb".to_owned()),
                command: words(&["zsh", "-l", "--kind"]),
            }
        );
        let minimal = parse_open(&words(&["s", "--kind", "editor"])).unwrap();
        assert_eq!(minimal.kind, ViewKind::Editor);
        assert!(minimal.command.is_empty());
    }

    #[test]
    fn open_rejects_a_missing_kind_an_unknown_kind_or_flag_and_a_flagless_name() {
        for bad in [
            &["s"][..],
            &["s", "--kind"],
            &["s", "--kind", "agent"],
            &["s", "--kind", "shell", "--size", "3"],
            &["--kind", "shell"],
            &[],
        ] {
            assert!(parse_open(&words(bad)).is_none(), "{bad:?}");
        }
    }
}
