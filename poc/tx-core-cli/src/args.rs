//! Hand-rolled argument parsing: no dependency, exact error texts, exit code 2 on misuse.

use std::fmt;
use std::path::PathBuf;

use tx_core::Lifecycle;

pub const USAGE: &str = "\
usage: txc [--config-dir DIR] [--data-dir DIR] [--state-dir DIR] [--cache-dir DIR] [--run-dir DIR] [--socket PATH]
           [--plugins FILE] [--profile-dir DIR]... [--skill-dir DIR]... [--env KEY=VALUE]... <verb> ...

verbs:
  create NAME --cwd DIR --engine ID --prompt T [--role R]... [--option KEY=VALUE]... [--model M]
                [--effort E] [--parent ID] [--tag T]... [--env KEY=VALUE]...
                                                                    start an agent; prints its id. It runs in its own
                                                                    worktree of the checkout DIR (name NAME, else NAME-2 …).
                                                                    --model M and --effort E are --option model=M / effort=E;
                                                                    the engine validates every option
  fork SOURCE --prompt T [--name NAME] [--tag T]... [--option KEY=VALUE]... [--model M] [--effort E]
                                                                    a new agent continuing SOURCE's conversation in a branch,
                                                                    in its own worktree at SOURCE's committed HEAD (uncommitted
                                                                    changes are not carried over); SOURCE's engine, roles and
                                                                    engine options unless an option says otherwise; name NAME,
                                                                    else SOURCE-fork; tags default to SOURCE's; prints its id
  ls [--parent ID] [--tag T] [--lifecycle running|exited|stopped] [--archived] [--json]
                                                                    id, name, lifecycle, cwd per line;
                                                                    --archived: the archived sessions instead;
                                                                    --json: one array of {id, name, state,
                                                                    working_directory, tags}, running only
                                                                    unless --lifecycle says otherwise
  show TARGET                                                       the record as JSON
  rename TARGET NAME
  tag TARGET [TAG]...                                               replace the tag list
  send [--via NAME] TARGET [--] TEXT...                             type a message into the session; as the
                                                                    agent this runs in, else as you from the
                                                                    view this runs in, else as you from NAME
  refresh [TARGET]                                                  mark vanished sessions exited
  where TARGET                                                      socket, session target, pane target
  attach TARGET                                                     attach the terminal to the session
  stop TARGET                                                       exit the agent (its engine's own way, else kill);
                                                                    waits for its process to be gone
  interrupt TARGET                                                  abort the agent's running turn
  respawn TARGET                                                    start an ended agent again: same record, worktree
                                                                    and conversation; prints its id and name
  archive TARGET                                                    stop it if running, then hide it from ls; nothing
                                                                    is deleted, and respawn brings it back
  engines                                                           registered engine ids
  plugins                                                           id and active|disabled per plugin
  migrate [--from DIR]                                              move a version-1 home (default
                                                                    $TX_CORE_HOME, else ~/.tx-core) into
                                                                    the directories below, once

TARGET is a session id or name. Files live in five directories named after the application,
$TX_APPNAME, else tx-ide; each is its flag, else $TX_<NAME>_DIR (every session carries these),
else the XDG variable, else the XDG default:
  config  $XDG_CONFIG_HOME/<app>, ~/.config/<app>           plugins.yaml, roles/ (your own)
  data    $XDG_DATA_HOME/<app>, ~/.local/share/<app>        records, log, plugin data, roles/ (shipped)
  state   $XDG_STATE_HOME/<app>, ~/.local/state/<app>       locks, profiles
  cache   $XDG_CACHE_HOME/<app>, ~/.cache/<app>
  run     $XDG_RUNTIME_DIR/<app>, $TMPDIR/<app>.<user>      tmux.sock
Roles resolve from --profile-dir, else <data>/roles (written by `ui install`), then <config>/roles;
skills from --skill-dir, else skills/ under those two.
Plugins are switched in <config>/plugins.yaml (see --plugins).
";

#[derive(Debug, PartialEq, Eq)]
pub struct UsageError(pub String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Global {
    pub config_dir: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
    pub run_dir: Option<PathBuf>,
    pub socket: Option<PathBuf>,
    pub profile_dirs: Vec<PathBuf>,
    pub skill_dirs: Vec<PathBuf>,
    pub env: Vec<(String, String)>,
    pub plugins: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verb {
    Create {
        name: String,
        cwd: PathBuf,
        parent: Option<String>,
        tags: Vec<String>,
        env: Vec<(String, String)>,
        engine: String,
        roles: Vec<String>,
        /// Engine options in command-line order (`--option KEY=VALUE`, `--model`, `--effort`);
        /// a repeated key is settled by the engine's options object, last one wins.
        options: Vec<(String, String)>,
        prompt: String,
    },
    Fork {
        source: String,
        name: Option<String>,
        prompt: String,
        tags: Vec<String>,
        /// Engine options in command-line order, as `Create` carries them.
        options: Vec<(String, String)>,
    },
    Ls {
        parent: Option<String>,
        tag: Option<String>,
        lifecycle: Option<Lifecycle>,
        archived: bool,
        json: bool,
    },
    Show {
        target: String,
    },
    Rename {
        target: String,
        name: String,
    },
    Tag {
        target: String,
        tags: Vec<String>,
    },
    Send {
        target: String,
        text: String,
        /// Where the operator typed, when the CLI cannot tell (an nvim buffer).
        via: Option<String>,
    },
    Refresh {
        target: Option<String>,
    },
    Where {
        target: String,
    },
    Attach {
        target: String,
    },
    Stop {
        target: String,
    },
    Interrupt {
        target: String,
    },
    Respawn {
        target: String,
    },
    Archive {
        target: String,
    },
    Engines,
    Plugins,
    Migrate {
        from: Option<PathBuf>,
    },
    /// Anything the built-ins do not know: looked up among the plugin verbs at run time.
    Plugin {
        name: String,
        argv: Vec<String>,
    },
    Help,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub global: Global,
    pub verb: Verb,
}

fn usage(text: impl Into<String>) -> UsageError {
    UsageError(text.into())
}

/// `--model M` / `--effort E` as the engine option they stand for; an empty value is a misuse
/// (the engine would otherwise see a key with nothing in it).
fn option_value(key: &str, flag: &str, value: &str) -> Result<(String, String), UsageError> {
    if value.is_empty() {
        return Err(usage(format!("{flag} needs a non-empty value")));
    }
    Ok((key.to_owned(), value.to_owned()))
}

/// `KEY=VALUE` of `--option`; the key is a bare name and the value is not empty.
fn option_pair(text: &str) -> Result<(String, String), UsageError> {
    let (key, value) = text
        .split_once('=')
        .ok_or_else(|| usage(format!("--option expects KEY=VALUE, got {text:?}")))?;
    if key.is_empty()
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(usage(format!(
            "--option key {key:?} is not a bare name ([A-Za-z0-9_-]+)"
        )));
    }
    if value.is_empty() {
        return Err(usage(format!("--option {key} needs a non-empty value")));
    }
    Ok((key.to_owned(), value.to_owned()))
}

fn env_pair(text: &str) -> Result<(String, String), UsageError> {
    text.split_once('=')
        .filter(|(key, _)| !key.is_empty())
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .ok_or_else(|| usage(format!("--env expects KEY=VALUE, got {text:?}")))
}

/// `--via NAME` goes verbatim into the envelope's `session="…"` attribute, so it may not be
/// empty or hold a character that would end or forge the attribute or the tag.
fn via_name(text: &str) -> Result<String, UsageError> {
    if text.is_empty()
        || text
            .chars()
            .any(|character| matches!(character, '"' | '<' | '>' | '&') || character.is_control())
    {
        return Err(usage(format!(
            "--via needs a name without quotes, <, >, & or control characters, got {text:?}"
        )));
    }
    Ok(text.to_owned())
}

/// A cursor over argv with "flag needs a value" bookkeeping.
struct Cursor<'a> {
    args: &'a [String],
    index: usize,
}

impl<'a> Cursor<'a> {
    fn peek(&self) -> Option<&'a str> {
        self.args.get(self.index).map(String::as_str)
    }

    fn next(&mut self) -> Option<&'a str> {
        let arg = self.peek()?;
        self.index += 1;
        Some(arg)
    }

    fn value(&mut self, flag: &str) -> Result<&'a str, UsageError> {
        self.next()
            .ok_or_else(|| usage(format!("{flag} needs a value")))
    }

    fn rest(&mut self) -> Vec<String> {
        let rest = self.args[self.index..].to_vec();
        self.index = self.args.len();
        rest
    }
}

pub fn parse(args: &[String]) -> Result<Invocation, UsageError> {
    let mut cursor = Cursor { args, index: 0 };
    let mut global = Global::default();
    loop {
        match cursor.peek() {
            Some("--config-dir") => {
                cursor.next();
                global.config_dir = Some(cursor.value("--config-dir")?.into());
            }
            Some("--data-dir") => {
                cursor.next();
                global.data_dir = Some(cursor.value("--data-dir")?.into());
            }
            Some("--state-dir") => {
                cursor.next();
                global.state_dir = Some(cursor.value("--state-dir")?.into());
            }
            Some("--cache-dir") => {
                cursor.next();
                global.cache_dir = Some(cursor.value("--cache-dir")?.into());
            }
            Some("--run-dir") => {
                cursor.next();
                global.run_dir = Some(cursor.value("--run-dir")?.into());
            }
            Some("--socket") => {
                cursor.next();
                global.socket = Some(cursor.value("--socket")?.into());
            }
            Some("--profile-dir") => {
                cursor.next();
                global
                    .profile_dirs
                    .push(cursor.value("--profile-dir")?.into());
            }
            Some("--skill-dir") => {
                cursor.next();
                global.skill_dirs.push(cursor.value("--skill-dir")?.into());
            }
            Some("--env") => {
                cursor.next();
                global.env.push(env_pair(cursor.value("--env")?)?);
            }
            Some("--plugins") => {
                cursor.next();
                global.plugins = Some(cursor.value("--plugins")?.into());
            }
            _ => break,
        }
    }
    let verb = match cursor.next() {
        None => return Err(usage("missing verb")),
        Some("help" | "--help" | "-h") => Verb::Help,
        Some("engines") => Verb::Engines,
        Some("plugins") => Verb::Plugins,
        Some("migrate") => {
            let from = match cursor.peek() {
                Some("--from") => {
                    cursor.next();
                    Some(cursor.value("--from")?.into())
                }
                _ => None,
            };
            Verb::Migrate { from }
        }
        Some("create") => parse_create(&mut cursor)?,
        Some("fork") => parse_fork(&mut cursor)?,
        Some("ls") => parse_ls(&mut cursor)?,
        Some("show") => Verb::Show {
            target: target(&mut cursor, "show")?,
        },
        Some("rename") => {
            let target = target(&mut cursor, "rename")?;
            let name = cursor
                .next()
                .ok_or_else(|| usage("rename needs a new name"))?
                .to_owned();
            Verb::Rename { target, name }
        }
        Some("tag") => {
            let target = target(&mut cursor, "tag")?;
            Verb::Tag {
                target,
                tags: cursor.rest(),
            }
        }
        Some("send") => {
            let via = match cursor.peek() {
                Some("--via") => {
                    cursor.next();
                    Some(via_name(cursor.value("--via")?)?)
                }
                _ => None,
            };
            let target = target(&mut cursor, "send")?;
            if cursor.peek() == Some("--") {
                cursor.next();
            }
            let words = cursor.rest();
            if words.is_empty() {
                return Err(usage("send needs the message text"));
            }
            Verb::Send {
                target,
                text: words.join(" "),
                via,
            }
        }
        Some("refresh") => Verb::Refresh {
            target: cursor.next().map(str::to_owned),
        },
        Some("where") => Verb::Where {
            target: target(&mut cursor, "where")?,
        },
        Some("attach") => Verb::Attach {
            target: target(&mut cursor, "attach")?,
        },
        Some("stop") => Verb::Stop {
            target: target(&mut cursor, "stop")?,
        },
        Some("interrupt") => Verb::Interrupt {
            target: target(&mut cursor, "interrupt")?,
        },
        Some("respawn") => Verb::Respawn {
            target: target(&mut cursor, "respawn")?,
        },
        Some("archive") => Verb::Archive {
            target: target(&mut cursor, "archive")?,
        },
        Some(other) => Verb::Plugin {
            name: other.to_owned(),
            argv: cursor.rest(),
        },
    };
    if let Some(extra) = cursor.peek() {
        return Err(usage(format!("unexpected argument {extra:?}")));
    }
    Ok(Invocation { global, verb })
}

fn target(cursor: &mut Cursor<'_>, verb: &str) -> Result<String, UsageError> {
    cursor
        .next()
        .map(str::to_owned)
        .ok_or_else(|| usage(format!("{verb} needs a session id or name")))
}

fn parse_create(cursor: &mut Cursor<'_>) -> Result<Verb, UsageError> {
    let name = cursor
        .next()
        .filter(|arg| !arg.starts_with("--"))
        .ok_or_else(|| usage("create needs a session name"))?
        .to_owned();
    let mut cwd: Option<PathBuf> = None;
    let mut parent = None;
    let mut tags = Vec::new();
    let mut env = Vec::new();
    let mut engine: Option<String> = None;
    let mut roles = Vec::new();
    let mut options = Vec::new();
    let mut prompt = None;
    while let Some(flag) = cursor.next() {
        match flag {
            "--cwd" => cwd = Some(cursor.value(flag)?.into()),
            "--parent" => parent = Some(cursor.value(flag)?.to_owned()),
            "--tag" => tags.push(cursor.value(flag)?.to_owned()),
            "--env" => env.push(env_pair(cursor.value(flag)?)?),
            "--engine" => engine = Some(cursor.value(flag)?.to_owned()),
            "--role" => roles.push(cursor.value(flag)?.to_owned()),
            "--model" => options.push(option_value("model", flag, cursor.value(flag)?)?),
            "--effort" => options.push(option_value("effort", flag, cursor.value(flag)?)?),
            "--option" => options.push(option_pair(cursor.value(flag)?)?),
            "--prompt" => prompt = Some(cursor.value(flag)?.to_owned()),
            other => return Err(usage(format!("create: unexpected argument {other:?}"))),
        }
    }
    let cwd = cwd.ok_or_else(|| usage("create needs --cwd DIR"))?;
    let engine = engine.ok_or_else(|| usage("create needs --engine ID"))?;
    let prompt = prompt.ok_or_else(|| usage("--prompt is required with --engine"))?;
    Ok(Verb::Create {
        name,
        cwd,
        parent,
        tags,
        env,
        engine,
        roles,
        options,
        prompt,
    })
}

fn parse_fork(cursor: &mut Cursor<'_>) -> Result<Verb, UsageError> {
    let source = cursor
        .next()
        .filter(|arg| !arg.starts_with("--"))
        .ok_or_else(|| usage("fork needs a source session id or name"))?
        .to_owned();
    let mut name = None;
    let mut prompt = None;
    let mut tags = Vec::new();
    let mut options = Vec::new();
    while let Some(flag) = cursor.next() {
        match flag {
            "--name" => name = Some(cursor.value(flag)?.to_owned()),
            "--prompt" => prompt = Some(cursor.value(flag)?.to_owned()),
            "--tag" => tags.push(cursor.value(flag)?.to_owned()),
            "--model" => options.push(option_value("model", flag, cursor.value(flag)?)?),
            "--effort" => options.push(option_value("effort", flag, cursor.value(flag)?)?),
            "--option" => options.push(option_pair(cursor.value(flag)?)?),
            other => return Err(usage(format!("fork: unexpected argument {other:?}"))),
        }
    }
    let prompt = prompt.ok_or_else(|| usage("fork needs --prompt T"))?;
    Ok(Verb::Fork {
        source,
        name,
        prompt,
        tags,
        options,
    })
}

fn parse_ls(cursor: &mut Cursor<'_>) -> Result<Verb, UsageError> {
    let mut parent = None;
    let mut tag = None;
    let mut lifecycle = None;
    let mut archived = false;
    let mut json = false;
    while let Some(flag) = cursor.next() {
        match flag {
            "--parent" => parent = Some(cursor.value(flag)?.to_owned()),
            "--tag" => tag = Some(cursor.value(flag)?.to_owned()),
            "--lifecycle" => {
                let text = cursor.value(flag)?;
                lifecycle = Some(
                    text.parse::<Lifecycle>()
                        .map_err(|error| usage(error.to_string()))?,
                );
            }
            "--archived" => archived = true,
            "--json" => json = true,
            other => return Err(usage(format!("ls: unexpected argument {other:?}"))),
        }
    }
    // The JSON listing is for pickers of live agents (nvim's tx-ask).
    if json && lifecycle.is_none() {
        lifecycle = Some(Lifecycle::Running);
    }
    Ok(Verb::Ls {
        parent,
        tag,
        lifecycle,
        archived,
        json,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn parse_str(line: &str) -> Result<Invocation, UsageError> {
        parse(
            &line
                .split(' ')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn plugins_flag_and_unknown_verbs_go_to_plugins() {
        let invocation = parse_str("--plugins p.yaml frob a b").unwrap();
        assert_eq!(
            invocation.global.plugins,
            Some(Path::new("p.yaml").to_owned())
        );
        assert_eq!(
            invocation.verb,
            Verb::Plugin {
                name: "frob".into(),
                argv: vec!["a".into(), "b".into()]
            }
        );
        assert_eq!(parse_str("plugins").unwrap().verb, Verb::Plugins);
        assert_eq!(
            parse_str("migrate").unwrap().verb,
            Verb::Migrate { from: None }
        );
        assert_eq!(
            parse_str("migrate --from /old").unwrap().verb,
            Verb::Migrate {
                from: Some("/old".into())
            }
        );
        assert!(parse_str("migrate --from").is_err());
        assert!(parse_str("migrate /old").is_err());
    }

    #[test]
    fn every_directory_has_a_flag() {
        let inv = parse_str(
            "--config-dir /c --data-dir /d --state-dir /s --cache-dir /k --run-dir /r ls",
        )
        .unwrap();
        assert_eq!(inv.global.config_dir.as_deref(), Some(Path::new("/c")));
        assert_eq!(inv.global.data_dir.as_deref(), Some(Path::new("/d")));
        assert_eq!(inv.global.state_dir.as_deref(), Some(Path::new("/s")));
        assert_eq!(inv.global.cache_dir.as_deref(), Some(Path::new("/k")));
        assert_eq!(inv.global.run_dir.as_deref(), Some(Path::new("/r")));
    }

    #[test]
    fn global_options_in_any_order() {
        let inv = parse_str("--state-dir /s --socket /k.sock --profile-dir /p1 --profile-dir /p2 --skill-dir /sk --env A=1 --env B=x=y ls").unwrap();
        assert_eq!(inv.global.state_dir.as_deref(), Some(Path::new("/s")));
        assert_eq!(inv.global.socket.as_deref(), Some(Path::new("/k.sock")));
        assert_eq!(
            inv.global.profile_dirs,
            [PathBuf::from("/p1"), PathBuf::from("/p2")]
        );
        assert_eq!(inv.global.skill_dirs, [PathBuf::from("/sk")]);
        assert_eq!(
            inv.global.env,
            [
                ("A".to_owned(), "1".to_owned()),
                ("B".to_owned(), "x=y".to_owned())
            ]
        );
        assert!(matches!(inv.verb, Verb::Ls { .. }));
        let inv = parse_str("ls").unwrap();
        assert!(inv.global.state_dir.is_none() && inv.global.env.is_empty());
    }

    #[test]
    fn create_agent() {
        let inv = parse_str("create w1 --cwd /w --engine claude --role dev --role ops --model opus --effort xhigh --option model=sonnet --option thinking=on --prompt go --parent p-1 --tag a --tag b --env K=V").unwrap();
        let Verb::Create {
            name,
            cwd,
            parent,
            tags,
            env,
            engine,
            roles,
            options,
            prompt,
        } = inv.verb
        else {
            panic!()
        };
        assert_eq!(name, "w1");
        assert_eq!(cwd, Path::new("/w"));
        assert_eq!(parent.as_deref(), Some("p-1"));
        assert_eq!(tags, ["a", "b"]);
        assert_eq!(env, [("K".to_owned(), "V".to_owned())]);
        assert_eq!(engine, "claude");
        assert_eq!(roles, ["dev", "ops"]);
        assert_eq!(
            options,
            [
                ("model".to_owned(), "opus".to_owned()),
                ("effort".to_owned(), "xhigh".to_owned()),
                ("model".to_owned(), "sonnet".to_owned()),
                ("thinking".to_owned(), "on".to_owned()),
            ]
        );
        assert_eq!(prompt, "go");
    }

    #[test]
    fn empty_model_and_effort_values_are_a_misuse() {
        for (flag, needle) in [
            ("--effort", "--effort needs a non-empty value"),
            ("--model", "--model needs a non-empty value"),
        ] {
            let args: Vec<String> = ["create", "n", "--cwd", "/w", "--engine", "e", flag, ""]
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
            let error = parse(&args).unwrap_err();
            assert!(error.0.contains(needle), "{flag}: {error}");
        }
    }

    #[test]
    fn fork() {
        assert_eq!(
            parse_str("fork src --prompt go").unwrap().verb,
            Verb::Fork {
                source: "src".into(),
                name: None,
                prompt: "go".into(),
                tags: vec![],
                options: vec![],
            }
        );
        assert_eq!(
            parse_str(
                "fork src --name twig --tag a --prompt go --tag b --model m --option k=v --effort low"
            )
            .unwrap()
            .verb,
            Verb::Fork {
                source: "src".into(),
                name: Some("twig".into()),
                prompt: "go".into(),
                tags: vec!["a".into(), "b".into()],
                options: vec![
                    ("model".into(), "m".into()),
                    ("k".into(), "v".into()),
                    ("effort".into(), "low".into()),
                ],
            }
        );
        for (line, needle) in [
            ("fork", "source"),
            ("fork --prompt go", "source"),
            ("fork src", "--prompt"),
            ("fork src --name n", "--prompt"),
            ("fork src --prompt", "--prompt needs a value"),
            ("fork src --prompt go --cwd /w", "--cwd"),
            ("fork src --prompt go --option novalue", "KEY=VALUE"),
            ("fork src --prompt go --model", "--model needs a value"),
            ("fork src extra --prompt go", "extra"),
        ] {
            let error = parse_str(line).unwrap_err();
            assert!(error.0.contains(needle), "{line}: {error}");
        }
    }

    #[test]
    fn create_usage_errors() {
        for (line, needle) in [
            ("create", "name"),
            ("create n", "--cwd"),
            ("create n --cwd /w", "--engine"),
            ("create n --cwd /w --prompt go", "--engine"),
            ("create n --cwd /w --engine e --option model", "KEY=VALUE"),
            ("create n --cwd /w --engine e --option =x", "bare name"),
            ("create n --cwd /w --engine e --option a.b=x", "bare name"),
            (
                "create n --cwd /w --engine e --option model=",
                "needs a non-empty value",
            ),
            (
                "create n --cwd /w --engine e --role",
                "--role needs a value",
            ),
            (
                "create n --cwd /w --engine e --prompt go --resume x",
                "--resume",
            ),
            (
                "create n --cwd /w --engine e --model m",
                "--prompt is required with --engine",
            ),
            ("create n --cwd /w --bogus --engine e", "--bogus"),
            ("create n extra --cwd /w --engine e", "extra"),
        ] {
            let error = parse_str(line).unwrap_err();
            assert!(error.0.contains(needle), "{line}: {error}");
        }
    }

    #[test]
    fn other_verbs() {
        assert_eq!(
            parse_str("ls --parent p --tag t --lifecycle exited")
                .unwrap()
                .verb,
            Verb::Ls {
                parent: Some("p".into()),
                tag: Some("t".into()),
                lifecycle: Some(Lifecycle::Exited),
                archived: false,
                json: false
            }
        );
        assert!(matches!(
            parse_str("ls --archived").unwrap().verb,
            Verb::Ls { archived: true, .. }
        ));
        assert!(matches!(
            parse_str("ls --json").unwrap().verb,
            Verb::Ls {
                lifecycle: Some(Lifecycle::Running),
                json: true,
                ..
            }
        ));
        assert!(matches!(
            parse_str("ls --json --lifecycle stopped").unwrap().verb,
            Verb::Ls {
                lifecycle: Some(Lifecycle::Stopped),
                json: true,
                ..
            }
        ));
        assert!(parse_str("ls --lifecycle gone").is_err());
        assert_eq!(
            parse_str("show x").unwrap().verb,
            Verb::Show { target: "x".into() }
        );
        assert_eq!(
            parse_str("rename x y").unwrap().verb,
            Verb::Rename {
                target: "x".into(),
                name: "y".into()
            }
        );
        assert_eq!(
            parse_str("tag x").unwrap().verb,
            Verb::Tag {
                target: "x".into(),
                tags: vec![]
            }
        );
        assert_eq!(
            parse_str("tag x a b").unwrap().verb,
            Verb::Tag {
                target: "x".into(),
                tags: vec!["a".into(), "b".into()]
            }
        );
        assert_eq!(
            parse_str("send x hello big world").unwrap().verb,
            Verb::Send {
                target: "x".into(),
                text: "hello big world".into(),
                via: None,
            }
        );
        assert_eq!(
            parse_str("send x -- --flag").unwrap().verb,
            Verb::Send {
                target: "x".into(),
                text: "--flag".into(),
                via: None,
            }
        );
        assert_eq!(
            parse_str("send --via plan.md x hi").unwrap().verb,
            Verb::Send {
                target: "x".into(),
                text: "hi".into(),
                via: Some("plan.md".into()),
            }
        );
        assert_eq!(
            parse_str("send x --via plan.md").unwrap().verb,
            Verb::Send {
                target: "x".into(),
                text: "--via plan.md".into(),
                via: None,
            }
        );
        assert!(parse_str("send --via").unwrap_err().0.contains("--via"));
        for bad in ["", "a\"b", "<x>", "a&b", "a\nb"] {
            let args = vec![
                "send".to_owned(),
                "--via".to_owned(),
                bad.to_owned(),
                "x".to_owned(),
                "hi".to_owned(),
            ];
            assert!(parse(&args).unwrap_err().0.contains("--via"), "{bad:?}");
        }
        assert_eq!(
            parse_str("refresh").unwrap().verb,
            Verb::Refresh { target: None }
        );
        assert_eq!(
            parse_str("refresh x").unwrap().verb,
            Verb::Refresh {
                target: Some("x".into())
            }
        );
        assert_eq!(
            parse_str("where x").unwrap().verb,
            Verb::Where { target: "x".into() }
        );
        assert_eq!(
            parse_str("attach x").unwrap().verb,
            Verb::Attach { target: "x".into() }
        );
        assert_eq!(
            parse_str("stop x").unwrap().verb,
            Verb::Stop { target: "x".into() }
        );
        assert_eq!(
            parse_str("interrupt x").unwrap().verb,
            Verb::Interrupt { target: "x".into() }
        );
        assert_eq!(
            parse_str("respawn x").unwrap().verb,
            Verb::Respawn { target: "x".into() }
        );
        assert_eq!(
            parse_str("archive x").unwrap().verb,
            Verb::Archive { target: "x".into() }
        );
        assert_eq!(parse_str("engines").unwrap().verb, Verb::Engines);
        assert_eq!(parse_str("help").unwrap().verb, Verb::Help);
        assert_eq!(parse_str("--help").unwrap().verb, Verb::Help);
        assert_eq!(parse_str("").unwrap_err().0, "missing verb");
        // Unknown verbs parse as plugin candidates; dispatch rejects them (exit 2).
        assert!(
            matches!(parse_str("bogus").unwrap().verb, Verb::Plugin { ref name, .. } if name == "bogus")
        );
        assert!(parse_str("show").unwrap_err().0.contains("show"));
        assert!(parse_str("show a b").unwrap_err().0.contains("b"));
        assert!(parse_str("send x").unwrap_err().0.contains("text"));
        assert!(parse_str("rename x").unwrap_err().0.contains("name"));
        assert!(
            parse_str("--state-dir")
                .unwrap_err()
                .0
                .contains("--state-dir")
        );
        assert!(
            parse_str("--env NOEQ ls")
                .unwrap_err()
                .0
                .contains("KEY=VALUE")
        );
    }
}
