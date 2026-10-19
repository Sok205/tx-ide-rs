//! Hand-rolled argument parsing: no dependency, exact error texts, exit code 2 on misuse.

use std::fmt;
use std::path::PathBuf;

use tx_core::{Effort, Lifecycle};

pub const USAGE: &str = "\
usage: txc [--state-dir DIR] [--socket PATH] [--plugins FILE] [--profile-dir DIR]... [--skill-dir DIR]... [--env KEY=VALUE]... <verb> ...

verbs:
  create NAME --cwd DIR (--engine ID [--role R]... [--model M] [--effort E] [--prompt T] [--resume ID] | --exec PROG [ARG]...)
                [--parent ID] [--tag T]... [--env KEY=VALUE]...     start a session; prints its id
  ls [--parent ID] [--tag T] [--lifecycle running|exited|stopped]   id, name, lifecycle, cwd per line
  show TARGET                                                       the record as JSON
  rename TARGET NAME
  tag TARGET [TAG]...                                               replace the tag list
  send TARGET [--] TEXT...                                          type a message into the session
  refresh [TARGET]                                                  mark vanished sessions exited
  where TARGET                                                      socket, session target, pane target
  attach TARGET                                                     attach the terminal to the session
  stop TARGET
  forget TARGET                                                     drop the record; leave tmux alone
  engines                                                           registered engine ids
  plugins                                                           id and active|disabled per plugin

TARGET is a session id or name. State lives in $TX_CORE_HOME, else ~/.tx-core.
Roles resolve from --profile-dir, else <state-dir>/agents (written by `ui install`) and
<state-dir>/user-agents; skills from --skill-dir, else skills/ under those two.
Plugins are switched in <state-dir>/plugins.yaml (see --plugins).
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
    pub state_dir: Option<PathBuf>,
    pub socket: Option<PathBuf>,
    pub profile_dirs: Vec<PathBuf>,
    pub skill_dirs: Vec<PathBuf>,
    pub env: Vec<(String, String)>,
    pub plugins: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchSpec {
    Agent {
        engine: String,
        roles: Vec<String>,
        model: Option<String>,
        effort: Option<Effort>,
        prompt: Option<String>,
        resume: Option<String>,
    },
    Process {
        program: String,
        args: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verb {
    Create {
        name: String,
        cwd: PathBuf,
        parent: Option<String>,
        tags: Vec<String>,
        env: Vec<(String, String)>,
        launch: LaunchSpec,
    },
    Ls {
        parent: Option<String>,
        tag: Option<String>,
        lifecycle: Option<Lifecycle>,
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
    Forget {
        target: String,
    },
    Engines,
    Plugins,
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

fn env_pair(text: &str) -> Result<(String, String), UsageError> {
    text.split_once('=')
        .filter(|(key, _)| !key.is_empty())
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .ok_or_else(|| usage(format!("--env expects KEY=VALUE, got {text:?}")))
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
            Some("--state-dir") => {
                cursor.next();
                global.state_dir = Some(cursor.value("--state-dir")?.into());
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
        Some("create") => parse_create(&mut cursor)?,
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
        Some("forget") => Verb::Forget {
            target: target(&mut cursor, "forget")?,
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
    let mut model = None;
    let mut effort = None;
    let mut prompt = None;
    let mut resume = None;
    let mut exec: Option<(String, Vec<String>)> = None;
    while let Some(flag) = cursor.next() {
        match flag {
            "--cwd" => cwd = Some(cursor.value(flag)?.into()),
            "--parent" => parent = Some(cursor.value(flag)?.to_owned()),
            "--tag" => tags.push(cursor.value(flag)?.to_owned()),
            "--env" => env.push(env_pair(cursor.value(flag)?)?),
            "--engine" => engine = Some(cursor.value(flag)?.to_owned()),
            "--role" => roles.push(cursor.value(flag)?.to_owned()),
            "--model" => model = Some(cursor.value(flag)?.to_owned()),
            "--prompt" => prompt = Some(cursor.value(flag)?.to_owned()),
            "--resume" => resume = Some(cursor.value(flag)?.to_owned()),
            "--effort" => {
                let text = cursor.value(flag)?;
                effort = Some(
                    text.parse::<Effort>()
                        .map_err(|error| usage(error.to_string()))?,
                );
            }
            "--exec" => {
                let program = cursor
                    .next()
                    .ok_or_else(|| usage("--exec needs a program"))?;
                exec = Some((program.to_owned(), cursor.rest()));
            }
            other => return Err(usage(format!("create: unexpected argument {other:?}"))),
        }
    }
    let cwd = cwd.ok_or_else(|| usage("create needs --cwd DIR"))?;
    let launch = match (engine, exec) {
        (Some(engine), None) => LaunchSpec::Agent {
            engine,
            roles,
            model,
            effort,
            prompt,
            resume,
        },
        (None, Some((program, args))) => {
            if !roles.is_empty()
                || model.is_some()
                || effort.is_some()
                || prompt.is_some()
                || resume.is_some()
            {
                return Err(usage(
                    "--role, --model, --effort, --prompt and --resume belong to --engine, not --exec",
                ));
            }
            LaunchSpec::Process { program, args }
        }
        _ => return Err(usage("create needs exactly one of --engine or --exec")),
    };
    Ok(Verb::Create {
        name,
        cwd,
        parent,
        tags,
        env,
        launch,
    })
}

fn parse_ls(cursor: &mut Cursor<'_>) -> Result<Verb, UsageError> {
    let mut parent = None;
    let mut tag = None;
    let mut lifecycle = None;
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
            other => return Err(usage(format!("ls: unexpected argument {other:?}"))),
        }
    }
    Ok(Verb::Ls {
        parent,
        tag,
        lifecycle,
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
        let inv = parse_str("create w1 --cwd /w --engine claude --role dev --role ops --model opus --effort xhigh --prompt go --resume chat-1 --parent p-1 --tag a --tag b --env K=V").unwrap();
        let Verb::Create {
            name,
            cwd,
            parent,
            tags,
            env,
            launch,
        } = inv.verb
        else {
            panic!()
        };
        assert_eq!(name, "w1");
        assert_eq!(cwd, Path::new("/w"));
        assert_eq!(parent.as_deref(), Some("p-1"));
        assert_eq!(tags, ["a", "b"]);
        assert_eq!(env, [("K".to_owned(), "V".to_owned())]);
        assert_eq!(
            launch,
            LaunchSpec::Agent {
                engine: "claude".into(),
                roles: vec!["dev".into(), "ops".into()],
                model: Some("opus".into()),
                effort: Some(Effort::XHigh),
                prompt: Some("go".into()),
                resume: Some("chat-1".into()),
            }
        );
    }

    #[test]
    fn create_process_takes_the_rest_verbatim() {
        let inv = parse_str("create sh1 --cwd /w --tag t --exec sh -c --flag --cwd x").unwrap();
        let Verb::Create { launch, tags, .. } = inv.verb else {
            panic!()
        };
        assert_eq!(tags, ["t"]);
        assert_eq!(
            launch,
            LaunchSpec::Process {
                program: "sh".into(),
                args: vec!["-c".into(), "--flag".into(), "--cwd".into(), "x".into()],
            }
        );
    }

    #[test]
    fn create_usage_errors() {
        for (line, needle) in [
            ("create", "name"),
            ("create n", "--cwd"),
            ("create n --cwd /w", "--engine or --exec"),
            (
                "create n --cwd /w --engine e --exec sh",
                "--engine or --exec",
            ),
            ("create n --cwd /w --exec", "--exec needs a program"),
            ("create n --cwd /w --engine e --effort ultra", "effort"),
            (
                "create n --cwd /w --engine e --role",
                "--role needs a value",
            ),
            ("create n --cwd /w --model m --exec sh", "--model"),
            ("create n --cwd /w --resume x --exec sh", "--resume"),
            (
                "create n --cwd /w --engine e --resume",
                "--resume needs a value",
            ),
            ("create n --cwd /w --bogus --exec sh", "--bogus"),
            ("create n extra --cwd /w --exec sh", "extra"),
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
                lifecycle: Some(Lifecycle::Exited)
            }
        );
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
                text: "hello big world".into()
            }
        );
        assert_eq!(
            parse_str("send x -- --flag").unwrap().verb,
            Verb::Send {
                target: "x".into(),
                text: "--flag".into()
            }
        );
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
            parse_str("forget x").unwrap().verb,
            Verb::Forget { target: "x".into() }
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
