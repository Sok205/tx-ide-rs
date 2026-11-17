//! Hand-rolled argument parsing: no dependency, exact error texts, exit code 2 on misuse.

use std::path::PathBuf;
use std::time::Duration;

use tx_core::{CoreError, EngineKind};
pub use tx_plugin::args::{Global, UsageError};
use tx_plugin::args::{env_pair, parse_global};

pub const USAGE: &str = "\
usage: tx [--config-dir DIR] [--data-dir DIR] [--state-dir DIR] [--cache-dir DIR] [--run-dir DIR] [--socket PATH]
           [--config FILE] [--profile-dir DIR]... [--skill-dir DIR]... [--plugin-dir DIR]... [--env KEY=VALUE]...
           <verb> ...

verbs:
  create NAME --cwd DIR --prompt T [--engine ENGINE] [--role R]... [--option KEY=VALUE]... [--model M]
                [--effort E] [--parent ID] [--tag T]... [--env KEY=VALUE]...
                [--agent A [--skill S]... [--mcp M]... [--tool T]...] [--engine-args ARGS]
                                                                    start an agent; prints its id. It runs in its own
                                                                    worktree of the checkout DIR (name NAME, else NAME-2 …).
                                                                    --model M and --effort E are --option model=M / effort=E;
                                                                    effort is 1-5 (or the engine's own word);
                                                                    the engine validates every option. A flag not given
                                                                    comes from `agent:` in the config file.
                                                                    --agent A runs the agent definition A (see
                                                                    `definitions`): its prompt, tools, MCP servers and
                                                                    skills, plus the --skill/--mcp/--tool extras, all kept
                                                                    on the record and resolved again at every respawn;
                                                                    with --agent, the roles are the --roles given, else
                                                                    the definition's `roles:`.
                                                                    --engine-args ARGS: arguments passed to the engine
                                                                    after tx's own flags, so they win (`--engine-args
                                                                    --chrome`: Claude in Chrome); split like a shell
                                                                    line, quotes kept together; the option `args`, kept
                                                                    for respawn and fork
  fork SOURCE --prompt T [--name NAME] [--tag T]... [--option KEY=VALUE]... [--model M] [--effort E]
                [--engine-args ARGS]
                                                                    a new agent continuing SOURCE's conversation in a branch,
                                                                    in its own worktree at SOURCE's committed HEAD (uncommitted
                                                                    changes are not carried over); SOURCE's engine, roles and
                                                                    engine options unless an option says otherwise; name NAME,
                                                                    else SOURCE-fork; tags default to SOURCE's; prints its id;
                                                                    --engine-args ARGS replaces SOURCE's, as on create
  ls [--parent ID] [--tag T] [--running|--ended] [--json]
                                                                    id, name, state, working directory per line
                                                                    of the running agents (the default, --running),
                                                                    or of the ended ones (--ended);
                                                                    --json: one array of {id, name, state,
                                                                    idle_reason, working_directory, tags}, where
                                                                    idle_reason says why an idle agent is idle
                                                                    (finished, waiting on the operator, or null)
  show TARGET                                                       the record as JSON
  rename TARGET NAME
  tag TARGET [TAG]...                                               replace the tag list
  send [--via NAME] TARGET [--] TEXT...                             type a message into the session; as the
                                                                    agent this runs in, else as you from the
                                                                    view this runs in, else as you from NAME
  where TARGET                                                      socket, session target, pane target
  attach TARGET                                                     attach the terminal to the session
  stop TARGET                                                       mark the agent ended and kill its tmux session
  interrupt TARGET                                                  abort the agent's running turn
  respawn TARGET                                                    start an ended agent again: same record, worktree
                                                                    and conversation; prints its id and name
  gc [--older-than DURATION] [--dry-run]                            delete the build output (`worktree.build_outputs`)
                                                                    in the worktrees of agents ended longer ago than
                                                                    DURATION (default 1d; 30m, 2h, 90s, 1.5d, bare
                                                                    minutes); size freed per worktree, then the total.
                                                                    Worktree, branch, record and conversation stay, so
                                                                    respawn works. --dry-run: the same, nothing deleted
  engines                                                           the kinds of the active engines, one per line
  definitions [agents|skills|mcp|tools]                             name and description of each definition, every kind
                                                                    or one; yours (<config>/definitions) win by name over
                                                                    the shipped ones (<data>/definitions). tools (only
                                                                    when asked): the names `tools` can use, from the
                                                                    engines, and each MCP server's tools
  check [--engine ENGINE | --plugins] [NAME|ID]...                  validate the definitions (agents, skills, MCP servers),
                                                                    all or those named, and with no name the installed
                                                                    plugins too; path:line per problem, exit 1;
                                                                    --engine: also what ENGINE adds to each agent and what
                                                                    it carries that no flag removes; --plugins [ID]...:
                                                                    only the plugins, all or those named (manifests, bins,
                                                                    resources, clashes, requires, the config, the shipped
                                                                    stamp), warnings apart
  plugins [--json]                                                  id and status per plugin, and why for one in error:
                                                                    active | disabled | error | missing (an entry of the
                                                                    config file that names no plugin); --json: one array
                                                                    of {id, status, detail, origin, directory, description,
                                                                    tier, requires, needs, verbs, engine}, origin being
                                                                    shipped, user or built-in
  update [--bundles DIR --binaries DIR [--link]]                    bring this machine up to this tx: run the update steps
                                                                    it has not had yet (<data>/update-version), then each
                                                                    plugin's updater (the IDE's tmux config, keys, shipped
                                                                    roles and definitions); one line per step, and whether
                                                                    running agents need a respawn. Other verbs warn while
                                                                    one is pending. --bundles, --binaries: lay the shipped
                                                                    plugins of DIR out under <data>/shipped, their programs
                                                                    taken from DIR/<id>/bin or the binaries DIR; --link:
                                                                    as links, not copies

TARGET is a session id or name. Files live in five directories named after the application,
$TX_APPNAME, else tx-ide; each is its flag, else $TX_<NAME>_DIR (every session carries these),
else the XDG variable, else the XDG default:
  config  $XDG_CONFIG_HOME/<app>, ~/.config/<app>           config.yaml, roles/, definitions/ (your own)
  data    $XDG_DATA_HOME/<app>, ~/.local/share/<app>        records, log, plugin data, roles/, definitions/ (shipped)
  state   $XDG_STATE_HOME/<app>, ~/.local/state/<app>       locks, profiles
  cache   $XDG_CACHE_HOME/<app>, ~/.cache/<app>
  run     $XDG_RUNTIME_DIR/<app>, $TMPDIR/<app>.<user>      tmux.sock
Roles resolve from --profile-dir, else <data>/roles (written by `tx update`), then <config>/roles;
skills from --skill-dir, else <data>/definitions/skills (written by `tx update`), then
<config>/definitions/skills; plugins are the directories under --plugin-dir, else <data>/shipped
(written by `tx update`), then <config>/plugins (your own).
A flag left out is read from its variable: $TX_PROFILE_PATH, $TX_SKILL_PATH, $TX_PLUGIN_PATH
(colon-separated), $TX_CONFIG_FILE, $TX_SESSION_ENV (a JSON object, for --env).
The config file is <config>/config.yaml (--config FILE overrides); every key is optional:
  agent:                        defaults for create; a flag given replaces its key
    engine: claude              an engine kind (see `engines`)
    roles: []                   empty: the definition's `roles`; any --role replaces the list
    definition: developer       the agent definition without --agent (null: roles alone)
    model: claude-opus-5-5      null: the engine's own default
    effort: 3                   1-5 (or the engine's own word); null: the engine's own default
  worktree:
    build_outputs: [target]     paths in a worktree `gc` may delete; relative, no `..`, not tracked by git
  plugins:                      one entry per plugin id (see `plugins`)
    <id>: { enabled: true, settings: {...} }
";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verb {
    Create {
        name: String,
        cwd: PathBuf,
        parent: Option<String>,
        tags: Vec<String>,
        env: Vec<(String, String)>,
        /// `None`: the config file's `agent.engine`.
        engine: Option<EngineKind>,
        /// Empty: the config file's `agent.roles`, or with `--agent` the definition's `roles:`.
        roles: Vec<String>,
        /// `--agent A` and its `--skill`/`--mcp`/`--tool` extras. Boxed: it is the one large
        /// field, and most invocations have none.
        definition: Option<Box<tx_core::DefinitionChoice>>,
        /// Engine options in command-line order (`--option KEY=VALUE`, `--model`, `--effort`);
        /// a repeated key is settled by the engine's options object, last one wins.
        options: Vec<(String, String)>,
        /// `--engine-args ARGS`, split: arguments the engine appends after its own flags.
        engine_args: Vec<String>,
        prompt: String,
    },
    Fork {
        source: String,
        name: Option<String>,
        prompt: String,
        tags: Vec<String>,
        /// Engine options in command-line order, as `Create` carries them.
        options: Vec<(String, String)>,
        /// `--engine-args ARGS`, as `Create` carries them.
        engine_args: Vec<String>,
    },
    Ls {
        parent: Option<String>,
        tag: Option<String>,
        /// `--ended`: the ended agents instead of the running ones.
        ended: bool,
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
    /// Delete the configured build output in the worktrees of agents ended longer ago than
    /// `older_than`.
    Gc {
        older_than: Duration,
        dry_run: bool,
    },
    Engines,
    /// `plugins [--json]`: every plugin and its status.
    Plugins {
        json: bool,
    },
    /// `update`: the pending update steps, then every plugin's updater. Its arguments are
    /// `update`'s own to read.
    Update {
        arguments: Vec<String>,
    },
    /// `check [--engine E | --plugins] [NAME]...`: validate the definitions, all of them or those
    /// named, and with no name the plugins too; with an engine, also what it adds or cannot
    /// remove. With `--plugins`, only the plugins, and `names` are plugin ids.
    Check {
        engine: Option<EngineKind>,
        plugins: bool,
        names: Vec<String>,
    },
    /// `definitions [agents|skills|mcp]`: list them, every kind or one.
    Definitions {
        kind: Option<DefinitionKind>,
    },
    /// Anything the built-ins do not know: looked up among the plugin verbs at run time.
    Plugin {
        name: String,
        argv: Vec<String>,
    },
    Help,
}

/// What `tx definitions` lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefinitionKind {
    Agents,
    Skills,
    Mcp,
    /// Only when asked for: it starts the engines and the MCP servers to ask them.
    Tools,
}

impl DefinitionKind {
    pub const ALL: [DefinitionKind; 3] = [
        DefinitionKind::Agents,
        DefinitionKind::Skills,
        DefinitionKind::Mcp,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            DefinitionKind::Agents => "agents",
            DefinitionKind::Skills => "skills",
            DefinitionKind::Mcp => "mcp",
            DefinitionKind::Tools => "tools",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub global: Global,
    pub verb: Verb,
}

fn usage(text: impl Into<String>) -> UsageError {
    UsageError(text.into())
}

/// `--engine ENGINE`: the name of an engine kind; anything that cannot be one is a misuse.
/// Whether an active engine provides the kind is settled when something is launched with it.
fn engine_kind(text: &str) -> Result<EngineKind, UsageError> {
    text.parse()
        .map_err(|error: CoreError| usage(format!("--engine: {error}")))
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

/// `--engine-args ARGS` as the words a shell would split it into: `--chrome -c "a b"` is three.
fn engine_args_words(text: &str) -> Result<Vec<String>, UsageError> {
    shlex::split(text).ok_or_else(|| usage(format!("--engine-args: unbalanced quotes in {text:?}")))
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
    let (global, index) = parse_global(args)?;
    let mut cursor = Cursor { args, index };
    let verb = match cursor.next() {
        None => return Err(usage("missing verb")),
        Some("help" | "--help" | "-h") => Verb::Help,
        Some("engines") => Verb::Engines,
        Some("plugins") => {
            let json = cursor.peek() == Some("--json");
            if json {
                cursor.next();
            }
            Verb::Plugins { json }
        }
        Some("update") => Verb::Update {
            arguments: cursor.rest(),
        },
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
        Some("gc") => parse_gc(&mut cursor)?,
        Some("check") => parse_check(&mut cursor)?,
        Some("definitions") => Verb::Definitions {
            kind: match cursor.next() {
                None => None,
                Some(text) => Some(
                    DefinitionKind::ALL
                        .into_iter()
                        .chain([DefinitionKind::Tools])
                        .find(|kind| kind.as_str() == text)
                        .ok_or_else(|| {
                            usage(format!(
                                "definitions lists agents, skills, mcp or tools, got {text:?}"
                            ))
                        })?,
                ),
            },
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

/// `check [--engine E | --plugins] [NAME]...`.
fn parse_check(cursor: &mut Cursor<'_>) -> Result<Verb, UsageError> {
    let mut engine = None;
    let mut plugins = false;
    loop {
        match cursor.peek() {
            Some("--engine") => {
                cursor.next();
                engine = Some(engine_kind(cursor.value("--engine")?)?);
            }
            Some("--plugins") => {
                cursor.next();
                plugins = true;
            }
            _ => break,
        }
    }
    if plugins && engine.is_some() {
        return Err(usage("--plugins takes no --engine"));
    }
    Ok(Verb::Check {
        engine,
        plugins,
        names: cursor.rest(),
    })
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
    let mut engine: Option<EngineKind> = None;
    let mut roles = Vec::new();
    let mut options = Vec::new();
    let mut prompt = None;
    let mut agent = None;
    let mut extras = tx_core::DefinitionChoice::default();
    let mut engine_args = Vec::new();
    while let Some(flag) = cursor.next() {
        match flag {
            "--cwd" => cwd = Some(cursor.value(flag)?.into()),
            "--agent" => agent = Some(cursor.value(flag)?.to_owned()),
            "--skill" => extras.skills.push(cursor.value(flag)?.to_owned()),
            "--mcp" => extras.mcp_servers.push(cursor.value(flag)?.to_owned()),
            "--tool" => extras.tools.push(cursor.value(flag)?.to_owned()),
            "--parent" => parent = Some(cursor.value(flag)?.to_owned()),
            "--tag" => tags.push(cursor.value(flag)?.to_owned()),
            "--env" => env.push(env_pair(cursor.value(flag)?)?),
            "--engine" => engine = Some(engine_kind(cursor.value(flag)?)?),
            "--role" => roles.push(cursor.value(flag)?.to_owned()),
            "--model" => options.push(option_value("model", flag, cursor.value(flag)?)?),
            "--effort" => options.push(option_value("effort", flag, cursor.value(flag)?)?),
            "--option" => options.push(option_pair(cursor.value(flag)?)?),
            "--prompt" => prompt = Some(cursor.value(flag)?.to_owned()),
            "--engine-args" => engine_args.extend(engine_args_words(cursor.value(flag)?)?),
            other => return Err(usage(format!("create: unexpected argument {other:?}"))),
        }
    }
    let cwd = cwd.ok_or_else(|| usage("create needs --cwd DIR"))?;
    let prompt = prompt.ok_or_else(|| usage("create needs --prompt T"))?;
    let definition = match agent {
        Some(agent) => Some(Box::new(tx_core::DefinitionChoice { agent, ..extras })),
        None if extras == tx_core::DefinitionChoice::default() => None,
        None => return Err(usage("--skill, --mcp and --tool extend an --agent")),
    };
    Ok(Verb::Create {
        name,
        cwd,
        parent,
        tags,
        env,
        engine,
        roles,
        definition,
        options,
        engine_args,
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
    let mut engine_args = Vec::new();
    while let Some(flag) = cursor.next() {
        match flag {
            "--name" => name = Some(cursor.value(flag)?.to_owned()),
            "--prompt" => prompt = Some(cursor.value(flag)?.to_owned()),
            "--tag" => tags.push(cursor.value(flag)?.to_owned()),
            "--model" => options.push(option_value("model", flag, cursor.value(flag)?)?),
            "--effort" => options.push(option_value("effort", flag, cursor.value(flag)?)?),
            "--option" => options.push(option_pair(cursor.value(flag)?)?),
            "--engine-args" => engine_args.extend(engine_args_words(cursor.value(flag)?)?),
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
        engine_args,
    })
}

fn parse_ls(cursor: &mut Cursor<'_>) -> Result<Verb, UsageError> {
    let mut parent = None;
    let mut tag = None;
    let mut ended = false;
    let mut json = false;
    while let Some(flag) = cursor.next() {
        match flag {
            "--parent" => parent = Some(cursor.value(flag)?.to_owned()),
            "--tag" => tag = Some(cursor.value(flag)?.to_owned()),
            "--running" => ended = false,
            "--ended" => ended = true,
            "--json" => json = true,
            other => return Err(usage(format!("ls: unexpected argument {other:?}"))),
        }
    }
    Ok(Verb::Ls {
        parent,
        tag,
        ended,
        json,
    })
}

/// `gc` without `--older-than`: agents ended more than a day ago.
pub const GC_OLDER_THAN: Duration = Duration::from_secs(24 * 60 * 60);

fn parse_gc(cursor: &mut Cursor<'_>) -> Result<Verb, UsageError> {
    let mut older_than = GC_OLDER_THAN;
    let mut dry_run = false;
    while let Some(flag) = cursor.next() {
        match flag {
            "--older-than" => {
                let raw = cursor.value(flag)?;
                let minutes = tx_timeline::verbs::parse_duration_minutes(raw).ok_or_else(|| {
                    usage(format!(
                        "--older-than takes a duration such as 30m, 2h or 1d, got {raw:?}"
                    ))
                })?;
                older_than = Duration::from_secs_f64(minutes * 60.0);
            }
            "--dry-run" => dry_run = true,
            other => return Err(usage(format!("gc: unexpected argument {other:?}"))),
        }
    }
    Ok(Verb::Gc {
        older_than,
        dry_run,
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
    fn config_flag_and_unknown_verbs_go_to_plugins() {
        let invocation = parse_str("--config p.yaml frob a b").unwrap();
        assert_eq!(
            invocation.global.config,
            Some(Path::new("p.yaml").to_owned())
        );
        assert_eq!(
            invocation.verb,
            Verb::Plugin {
                name: "frob".into(),
                argv: vec!["a".into(), "b".into()]
            }
        );
        assert_eq!(
            parse_str("plugins").unwrap().verb,
            Verb::Plugins { json: false }
        );
        assert_eq!(
            parse_str("plugins --json").unwrap().verb,
            Verb::Plugins { json: true }
        );
        for bad in ["plugins --yaml", "plugins --json x", "plugins x"] {
            assert!(
                parse_str(bad)
                    .unwrap_err()
                    .0
                    .contains("unexpected argument"),
                "{bad}"
            );
        }
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
        let inv = parse_str("--plugin-dir /shipped --plugin-dir /mine ls").unwrap();
        assert_eq!(
            inv.global.plugin_dirs,
            [PathBuf::from("/shipped"), PathBuf::from("/mine")]
        );
        let inv = parse_str("ls").unwrap();
        assert!(inv.global.state_dir.is_none() && inv.global.env.is_empty());
        assert!(inv.global.plugin_dirs.is_empty());
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
            definition,
            options,
            engine_args,
            prompt,
        } = inv.verb
        else {
            panic!()
        };
        assert_eq!(engine_args, Vec::<String>::new());
        assert_eq!(definition, None);
        assert_eq!(name, "w1");
        assert_eq!(cwd, Path::new("/w"));
        assert_eq!(parent.as_deref(), Some("p-1"));
        assert_eq!(tags, ["a", "b"]);
        assert_eq!(env, [("K".to_owned(), "V".to_owned())]);
        assert_eq!(engine, Some(EngineKind::CLAUDE));
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
    fn create_splits_engine_args_like_a_shell() {
        let argv = |args: &[&str]| args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        let create = |engine_args: &str| {
            parse(&argv(&[
                "create",
                "w",
                "--cwd",
                "/w",
                "--engine-args",
                engine_args,
                "--prompt",
                "go",
            ]))
        };
        let Verb::Create {
            engine_args,
            prompt,
            ..
        } = create(r#"--chrome -c "a b""#).unwrap().verb
        else {
            panic!()
        };
        assert_eq!(prompt, "go");
        assert_eq!(engine_args, ["--chrome", "-c", "a b"]);
        let error = create(r#"-c "a"#).unwrap_err();
        assert!(error.0.contains("unbalanced quotes"), "{error}");
        let error = parse_str("create w --cwd /w --prompt go --engine-args").unwrap_err();
        assert!(error.0.contains("--engine-args needs a value"), "{error}");
    }

    #[test]
    fn create_with_an_agent_definition_and_extras() {
        let Verb::Create { definition, .. } = parse_str(
            "create w --cwd /w --prompt go --agent pm --skill s1 --mcp probe --tool WebFetch --skill s2",
        )
        .unwrap()
        .verb
        else {
            panic!()
        };
        assert_eq!(
            definition,
            Some(Box::new(tx_core::DefinitionChoice {
                agent: "pm".into(),
                skills: vec!["s1".into(), "s2".into()],
                mcp_servers: vec!["probe".into()],
                tools: vec!["WebFetch".into()],
            }))
        );
        assert_eq!(
            parse_str("create w --cwd /w --prompt go --skill s")
                .unwrap_err()
                .0,
            "--skill, --mcp and --tool extend an --agent"
        );
    }

    #[test]
    fn create_without_engine_or_roles_leaves_them_to_the_config_file() {
        let Verb::Create { engine, roles, .. } =
            parse_str("create w --cwd /w --prompt go").unwrap().verb
        else {
            panic!()
        };
        assert_eq!(engine, None);
        assert!(roles.is_empty());
    }

    #[test]
    fn empty_model_and_effort_values_are_a_misuse() {
        for (flag, needle) in [
            ("--effort", "--effort needs a non-empty value"),
            ("--model", "--model needs a non-empty value"),
        ] {
            let args: Vec<String> = ["create", "n", "--cwd", "/w", "--engine", "claude", flag, ""]
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
                engine_args: vec![],
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
                engine_args: vec![],
            }
        );
        let Verb::Fork { engine_args, .. } =
            parse_str("fork src --prompt go --engine-args --chrome")
                .unwrap()
                .verb
        else {
            panic!()
        };
        assert_eq!(engine_args, ["--chrome"]);
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
            ("create n --cwd /w", "--prompt"),
            (
                "create n --cwd /w --engine claude --option model",
                "KEY=VALUE",
            ),
            ("create n --cwd /w --engine claude --option =x", "bare name"),
            (
                "create n --cwd /w --engine claude --option a.b=x",
                "bare name",
            ),
            (
                "create n --cwd /w --engine claude --option model=",
                "needs a non-empty value",
            ),
            (
                "create n --cwd /w --engine claude --role",
                "--role needs a value",
            ),
            (
                "create n --cwd /w --engine claude --prompt go --resume x",
                "--resume",
            ),
            (
                "create n --cwd /w --engine claude --model m",
                "create needs --prompt T",
            ),
            ("create n --cwd /w --bogus --engine claude", "--bogus"),
            ("create n extra --cwd /w --engine claude", "extra"),
        ] {
            let error = parse_str(line).unwrap_err();
            assert!(error.0.contains(needle), "{line}: {error}");
        }
    }

    #[test]
    fn other_verbs() {
        assert_eq!(
            parse_str("ls --parent p --tag t --ended").unwrap().verb,
            Verb::Ls {
                parent: Some("p".into()),
                tag: Some("t".into()),
                ended: true,
                json: false
            }
        );
        assert!(matches!(
            parse_str("ls --running").unwrap().verb,
            Verb::Ls {
                ended: false,
                json: false,
                ..
            }
        ));
        assert!(matches!(
            parse_str("ls").unwrap().verb,
            Verb::Ls { ended: false, .. }
        ));
        assert!(parse_str("ls --archived").is_err());
        assert!(matches!(
            parse_str("ls --json").unwrap().verb,
            Verb::Ls {
                ended: false,
                json: true,
                ..
            }
        ));
        assert!(matches!(
            parse_str("ls --json --ended").unwrap().verb,
            Verb::Ls {
                ended: true,
                json: true,
                ..
            }
        ));
        assert!(parse_str("ls --lifecycle running").is_err());
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
            parse_str("gc").unwrap().verb,
            Verb::Gc {
                older_than: GC_OLDER_THAN,
                dry_run: false
            }
        );
        assert_eq!(
            parse_str("gc --older-than 2h --dry-run").unwrap().verb,
            Verb::Gc {
                older_than: Duration::from_secs(2 * 60 * 60),
                dry_run: true
            }
        );
        for bad in [
            "gc --older-than",
            "gc --older-than soon",
            "gc --older-than 0",
            "gc x",
        ] {
            assert!(parse_str(bad).is_err(), "{bad}");
        }
        assert_eq!(parse_str("engines").unwrap().verb, Verb::Engines);
        assert_eq!(
            parse_str("update").unwrap().verb,
            Verb::Update { arguments: vec![] }
        );
        // `update` reads its own arguments.
        assert_eq!(
            parse_str("update --bundles b now").unwrap().verb,
            Verb::Update {
                arguments: vec!["--bundles".into(), "b".into(), "now".into()]
            }
        );
        assert_eq!(
            parse_str("check pm linear").unwrap().verb,
            Verb::Check {
                engine: None,
                plugins: false,
                names: vec!["pm".into(), "linear".into()]
            }
        );
        assert_eq!(
            parse_str("check --engine codex pm").unwrap().verb,
            Verb::Check {
                engine: Some(EngineKind::CODEX),
                plugins: false,
                names: vec!["pm".into()]
            }
        );
        assert_eq!(
            parse_str("check --plugins mockup nope").unwrap().verb,
            Verb::Check {
                engine: None,
                plugins: true,
                names: vec!["mockup".into(), "nope".into()]
            }
        );
        assert_eq!(
            parse_str("check --plugins").unwrap().verb,
            Verb::Check {
                engine: None,
                plugins: true,
                names: vec![]
            }
        );
        for bad in [
            "check --plugins --engine claude",
            "check --engine claude --plugins",
        ] {
            assert_eq!(
                parse_str(bad).unwrap_err().0,
                "--plugins takes no --engine",
                "{bad}"
            );
        }
        assert_eq!(
            parse_str("definitions mcp").unwrap().verb,
            Verb::Definitions {
                kind: Some(DefinitionKind::Mcp)
            }
        );
        assert_eq!(
            parse_str("definitions").unwrap().verb,
            Verb::Definitions { kind: None }
        );
        assert!(parse_str("definitions roles").is_err());
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
