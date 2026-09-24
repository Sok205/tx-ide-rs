//! Differential test: `tx::argparse` against CPython 3.14's real `argparse`.
//!
//! Every parser below mirrors an `add_argument` sequence from `lib/tx/cli.py`. Each (parser, argv,
//! COLUMNS) case is run through one `python3.14` process (building the same parser from the same
//! JSON spec, with the real `tx.cli` type callbacks); its stdout / stderr / exit code / namespace
//! are the expected values the Rust parser must reproduce byte for byte.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Map, Value as Json, json};
use tx::argparse::{Arg, Parser, Value};

const PYTHON_LIB: &str = "/Users/Sok205/PycharmProjects/tx-ide/lib";
const COLUMNS: &[usize] = &[200, 80, 60, 45, 30];

const LONG_ROLE_HELP: &str = "role file(s) to inject additively into the agent's system prompt \
(COMMON is always included first). NAME resolves to user-agents/NAME.md (replaces) or \
agents/NAME.md, plus user-agents/NAME.local.md (extends)";

fn specs() -> Vec<Json> {
    let group_help = "explicit effort-group override (default: derived at read time)";
    vec![
        json!({"prog": "tx spawn", "description": "Spawn a detached tmux session (--tag mandatory).", "args": [
            {"name": "name"},
            {"flags": ["--tag"], "required": true},
            {"flags": ["--group"], "type": "group", "help": group_help},
            {"flags": ["--cwd"]},
            {"flags": ["--cmd"], "help": "a full, hand-written launch command (shell / nvim / other, or an explicit agent command); cannot combine with --prompt/--model/--effort"},
            {"flags": ["--engine"], "choices": ["claude", "codex", "antigravity"], "help": "build the launch command for this agent engine via its adapter (default: claude). With --cmd, declares the engine to stamp on the record (the command stays yours; the engine is never inferred from it)"},
            {"flags": ["--prompt"], "help": "initial/priming prompt for an --engine agent spawn (auto-submits in the TUI)"},
            {"flags": ["--model"], "help": "model override for an --engine agent spawn"},
            {"flags": ["--effort"], "int_choices": [1, 6], "metavar": "{1,2,3,4,5}", "help": "reasoning-effort tier for an --engine agent spawn (default: 3)"},
            {"flags": ["--role"], "action": "append", "metavar": "NAME[,NAME…]", "help": LONG_ROLE_HELP},
            {"flags": ["--read-only"], "action": "store_true", "help": "run an engine-built agent in a tx worktree with repository edits blocked"},
            {"flags": ["--chrome"], "action": "store_true", "help": "grant the engine's browser-automation tooling (off by default — on claude it costs ~1,300 tokens of standing system prompt per session)"},
            {"flags": ["--env"], "action": "append", "type": "env"},
        ]}),
        json!({"prog": "tx spawn-nvim", "description": "Spawn a detached nvim companion (--diff opens a diffview; --open opens a file).", "args": [
            {"name": "name"},
            {"flags": ["--tag"], "required": true, "help": "scope tag(s), comma-separated — a companion takes the same tag as the session it belongs to, so the pair surfaces together in the operator's filters"},
            {"flags": ["--group"], "type": "group", "help": group_help},
            {"flags": ["--cwd"], "help": "working directory (default: the firing pane's)"},
            {"flags": ["--diff"], "nargs": "?", "const": "main", "help": "open a diffview of the worktree against BASE (default: main) — prefer the merge-base over a branch name, which shows commits you lack as deletions once the branch moves ahead"},
            {"flags": ["--open"], "help": "open FILE on startup"},
            {"flags": ["--env"], "action": "append", "type": "env"},
        ]}),
        json!({"prog": "tx spawn-view", "description": "Spawn a detached view session (a live @tx_view tmux home, not a store record).", "args": [
            {"name": "name"}, {"flags": ["--cwd"]}, {"flags": ["--cmd"]},
            {"flags": ["--env"], "action": "append", "type": "env"},
        ]}),
        json!({"prog": "tx ls", "description": "List current (live) sessions (a single PROCESSES listing; views live in tmux).", "args": []}),
        json!({"prog": "tx show", "description": "Print a session record as JSON (by id or name).", "args": [{"name": "target"}]}),
        json!({"prog": "tx history", "description": "List past (EXITED / ARCHIVED) sessions — filter by --tag / --cwd / --since / --until.", "args": [
            {"flags": ["--tag"], "help": "only sessions carrying this tag"},
            {"flags": ["--cwd"], "help": "only sessions whose cwd contains this substring"},
            {"flags": ["--since"], "metavar": "YYYY-MM-DD", "help": "ended on or after this date"},
            {"flags": ["--until"], "metavar": "YYYY-MM-DD", "help": "ended on or before this date"},
        ]}),
        json!({"prog": "tx chat", "description": "Inspect a session's chats: `chat ls <session>` lists its ChatRefs + bundle paths.", "args": [
            {"name": "subcommand", "choices": ["ls"], "help": "ls — list the session's chats"},
            {"name": "session"},
        ]}),
        json!({"prog": "tx resume", "description": "Re-spawn a past session + reattach its chat (claude --resume); collision-safe (§7).", "args": [
            {"name": "target", "help": "the past session to resume (id or name)"},
            {"flags": ["--as"], "dest": "new_name", "metavar": "NAME", "help": "spawn under a new name (required on a live-name clash)"},
            {"flags": ["--cwd"], "metavar": "DIR", "help": "override the cwd (required if the stored cwd is gone — C8)"},
        ]}),
        json!({"prog": "tx tag", "description": "Read or set a session's tags (comma-separated).", "args": [
            {"name": "name"}, {"name": "tags", "nargs": "?"},
        ]}),
        json!({"prog": "tx group", "description": "Read or set a session's effort-group override (--clear returns to derived).", "args": [
            {"name": "name"},
            {"name": "group", "nargs": "?", "help": "the explicit group to set"},
            {"flags": ["--clear"], "action": "store_true", "help": "drop the override — back to derived"},
        ]}),
        json!({"prog": "tx rename", "description": "Rename a session's display name (record; a process leaves its tmux id untouched).", "args": [
            {"name": "name"}, {"name": "new_name"},
        ]}),
        json!({"prog": "tx send-message", "description": "Peer-message another agent session (delivered in a <from-agent> envelope).", "args": [
            {"name": "target", "help": "the recipient's display name, as `tx ls` prints it"},
            {"name": "body", "help": "single-line message — escape literal newlines as \\n; the recipient receives it wrapped in a <from-agent session='<your-name>'> envelope"},
        ]}),
        json!({"prog": "tx artifact create", "args": [
            {"name": "file"}, {"flags": ["--title"]},
            {"flags": ["--group"], "type": "group", "help": "explicit effort-group override (default: derived from the creator)"},
        ]}),
        json!({"prog": "tx artifact modify", "args": [
            {"name": "id"}, {"name": "file", "nargs": "?"}, {"flags": ["--changes"]},
        ]}),
        json!({"prog": "tx artifact ls", "args": [
            {"flags": ["--session"], "help": "only artifacts this session created or touched"},
        ]}),
        json!({"prog": "tx artifact diff", "args": [
            {"name": "id"}, {"name": "rev_a", "nargs": "?", "type": "int"}, {"name": "rev_b", "nargs": "?", "type": "int"},
        ]}),
        json!({"prog": "tx artifact open", "args": [
            {"name": "id"},
            {"flags": ["--tag"], "help": "tags for the nvim view (overrides the invoker's tags)"},
            {"flags": ["--cwd"], "help": "working directory for the view (default: the artifact's dir)"},
        ]}),
        json!({"prog": "tx artifact doctor", "args": [
            {"flags": ["--repair"], "action": "store_true", "help": "remove orphan rev files so a retried modify can claim the slot (run when quiescent)"},
        ]}),
        json!({"prog": "tx sync", "description": "Manual archive sync of the reproducible corpus (push/pull/status) — never hot-path.", "args": [
            {"name": "action", "choices": ["push", "pull", "status"]},
            {"flags": ["--remote"], "metavar": "PATH", "help": "a local filesystem remote (archive dir / S3 dogfood proxy)"},
            {"flags": ["--s3"], "metavar": "BUCKET[/PREFIX]", "help": "select the S3 backend (deferred — reports 'not implemented')"},
        ]}),
        json!({"prog": "tx start", "description": "Ensure the Views home base + tx-assistant exist, then attach Views.", "args": [
            {"flags": ["-r", "--restart"], "action": "store_true", "help": "kill an existing tx-assistant first so warmup recreates it"},
        ]}),
        json!({"prog": "tx attach", "description": "Open the interactive session picker (fzf).", "args": [
            {"flags": ["-f", "--filter"], "dest": "query", "default": "", "metavar": "QUERY", "help": "pre-fill the search with QUERY"},
            {"flags": ["-j", "--jump"], "action": "store_true", "help": "Enter focuses the existing pane hosting the session instead of nest-attaching here (popup-friendly)"},
            {"flags": ["--host"], "nargs": "?", "const": "personal", "metavar": "ALIAS", "help": "attach a session on remote ssh ALIAS (default 'personal') by running that host's own tx picker over ssh"},
            {"flags": ["--all"], "dest": "mix", "action": "store_true", "help": "(unsupported) merged local+remote picker — use --host ALIAS"},
        ]}),
        json!({"prog": "tx fork", "description": "Fork a session's chat into a NEW session that starts with the full history (§4).", "args": [
            {"name": "source", "help": "the session to fork (id or name)"},
            {"name": "new_name", "nargs": "?", "help": "name for the fork (default <source>-fork)"},
            {"flags": ["--read-only"], "action": "store_true", "help": "create the new fork in a tx worktree with repository edits blocked"},
            {"flags": ["--group"], "type": "group", "help": "explicit effort-group override (default: derived via the source)"},
        ]}),
        json!({"prog": "tx handover", "description": "Distill a session's chat into a focused brief for a NEW worker session (§5).", "args": [
            {"name": "source", "help": "the session to hand over from (id or name)"},
            {"name": "task", "help": "the task to distill a brief for"},
            {"name": "new_name", "nargs": "?", "help": "name for the worker (default <source>-handover)"},
            {"flags": ["--self-catch-up"], "action": "store_true", "help": "skip the distiller — the worker reads the source bundle itself (CHD1)"},
            {"flags": ["--read-only"], "action": "store_true", "help": "create the new worker in a tx worktree with repository edits blocked"},
        ]}),
        json!({"prog": "tx rollover", "description": "Rotate a session onto a fresh chat in the SAME pane (context exhausted) (§6).", "args": [
            {"name": "session", "nargs": "?", "help": "the session to roll over (default: the one you are in)"},
            {"flags": ["--self-catch-up"], "action": "store_true", "help": "skip the distiller — the successor reads the bundle itself (CHD1)"},
        ]}),
        json!({"prog": "tx _chat-op-finish", "description": "Internal: complete a handover/rollover from its op-spec (idempotent, CHD5).", "args": [
            {"name": "op_id"},
        ]}),
        json!({"prog": "tx migrate", "description": "Upgrade $TX_IDE_HOME records to the current schemas (sessions v3→v6, artifacts v1→v2).", "args": []}),
        // Synthetic stress parser: a very long prog forces the "prog on its own line" usage branch.
        json!({"prog": "tx a-deliberately-very-long-program-name-for-wrapping", "description": "Word-wrap stress: hyphenated-compound-words, em--dashes, a-b-c, x--y, supercalifragilisticexpialidocious-and-then-some-more-text, ____, 12-34, don't.", "args": [
            {"name": "alpha", "help": "supercalifragilisticexpialidocioussupercalifragilisticexpialidocious long token"},
            {"flags": ["--an-option-with-a-really-long-name"], "metavar": "SOME_LONG_METAVAR", "help": "wraps after the long invocation header"},
            {"flags": ["--x"], "help": "   "},
            {"flags": ["--z"], "help": "e-mail ab-cd-ef x-y-z abc-def-ghi a--b word--word, --flag ---triple end-- -lead trail- ab1-cd 1-2-3 it's--fine \"q\"--r ok?--yes co-op re--do a-b-c-d-e-f-g-h-i-j-k-l-m-n-o-p-q-r-s-t-u-v-w-x-y-z ----- -- - done"},
            {"flags": ["--y"], "help": ""},
        ]}),
    ]
}

fn group_value(value: &str) -> Result<String, String> {
    if value.is_empty() {
        return Err("a group cannot be empty".to_string());
    }
    Ok(value.to_string())
}

fn env_pair(value: &str) -> Result<String, String> {
    if !value.contains('=') {
        return Err(format!("--env expects KEY=VALUE, got '{value}'"));
    }
    Ok(value.to_string())
}

fn str_field<'a>(spec: &'a Json, key: &str) -> Option<&'a str> {
    spec.get(key).and_then(Json::as_str)
}

fn build(spec: &Json, columns: usize) -> Parser {
    let mut parser = Parser::new(str_field(spec, "prog").expect("prog")).columns(columns);
    if let Some(description) = str_field(spec, "description") {
        parser = parser.description(description);
    }
    for arg_spec in spec["args"].as_array().expect("args") {
        let mut arg = match arg_spec.get("flags") {
            Some(flags) => {
                let flags: Vec<&str> = flags
                    .as_array()
                    .expect("flags")
                    .iter()
                    .filter_map(Json::as_str)
                    .collect();
                Arg::options(&flags)
            }
            None => Arg::positional(str_field(arg_spec, "name").expect("name")),
        };
        match str_field(arg_spec, "action") {
            Some("store_true") => arg = arg.flag(),
            Some("append") => arg = arg.append(),
            _ => {}
        }
        if str_field(arg_spec, "nargs") == Some("?") {
            arg = arg.optional();
        }
        if arg_spec.get("required") == Some(&json!(true)) {
            arg = arg.required();
        }
        if let Some(choices) = arg_spec.get("choices").and_then(Json::as_array) {
            let choices: Vec<&str> = choices.iter().filter_map(Json::as_str).collect();
            arg = arg.choices(&choices);
        }
        if let Some(range) = arg_spec.get("int_choices") {
            let (start, end) = (
                range[0].as_i64().expect("start"),
                range[1].as_i64().expect("end"),
            );
            arg = arg.int_choices(start..end);
        }
        match str_field(arg_spec, "type") {
            Some("int") => arg = arg.int(),
            Some("group") => arg = arg.value_parser(group_value),
            Some("env") => arg = arg.value_parser(env_pair),
            _ => {}
        }
        if let Some(metavar) = str_field(arg_spec, "metavar") {
            arg = arg.metavar(metavar);
        }
        if let Some(dest) = str_field(arg_spec, "dest") {
            arg = arg.dest(dest);
        }
        if let Some(default) = str_field(arg_spec, "default") {
            arg = arg.default(default);
        }
        if let Some(constant) = str_field(arg_spec, "const") {
            arg = arg.constant(constant);
        }
        if let Some(help) = str_field(arg_spec, "help") {
            arg = arg.help(help);
        }
        parser = parser.arg(arg);
    }
    parser
}

/// argv cases for one parser: generic shapes plus per-option variants.
fn cases(spec: &Json) -> Vec<Vec<String>> {
    let args = spec["args"].as_array().expect("args");
    let required_positionals: Vec<String> = args
        .iter()
        .filter(|a| a.get("flags").is_none() && str_field(a, "nargs").is_none())
        .enumerate()
        .map(
            |(i, a)| match a.get("choices").and_then(|c| c[0].as_str()) {
                Some(choice) => choice.to_string(),
                None => format!("p{i}"),
            },
        )
        .collect();
    let required_options: Vec<String> = args
        .iter()
        .filter(|a| a.get("required") == Some(&json!(true)))
        .flat_map(|a| {
            [
                a["flags"][0].as_str().expect("flag").to_string(),
                "t".to_string(),
            ]
        })
        .collect();
    let base: Vec<String> = required_positionals
        .iter()
        .chain(&required_options)
        .cloned()
        .collect();
    let with = |extra: &[&str]| -> Vec<String> {
        base.iter()
            .cloned()
            .chain(extra.iter().map(|s| s.to_string()))
            .collect()
    };
    let owned = |items: &[&str]| -> Vec<String> { items.iter().map(|s| s.to_string()).collect() };

    let mut out = vec![
        vec![],
        owned(&["-h"]),
        owned(&["--help"]),
        owned(&["--he"]),
        owned(&["--h"]),
        owned(&["--help=x"]),
        base.clone(),
        with(&["-h"]),
        with(&["--bogus"]),
        with(&["--bogus=1"]),
        with(&["-x"]),
        with(&["-"]),
        with(&["-1"]),
        with(&["-.5"]),
        with(&["- 1"]),
        with(&["--"]),
        with(&["--", "-x"]),
        with(&["--", "--", "y"]),
        with(&["extra1", "extra2", "extra3"]),
        with(&["--effort", "3"]),
        owned(&["--", "a", "b"]),
        owned(&["a", "--", "b", "c"]),
        owned(&["a", "b", "c", "d", "e"]),
        owned(&["-h", "--bogus"]),
        owned(&["--bogus", "-h"]),
        owned(&["weird\tvalue", "it's", "x\"y", "a'b\"c", "\u{7}\u{a0}é"]),
    ];
    for arg in args {
        let Some(flags) = arg.get("flags").and_then(Json::as_array) else {
            // positional: bad value for typed / choice positionals
            out.push(owned(&["bogus", "x", "y"]));
            continue;
        };
        for flag in flags.iter().filter_map(Json::as_str) {
            let value = match (arg.get("choices"), arg.get("int_choices")) {
                (Some(choices), _) => choices[0].as_str().unwrap_or("v").to_string(),
                (_, Some(_)) => "2".to_string(),
                _ => "k=v".to_string(),
            };
            let joined = format!("{flag}={value}");
            let empty = format!("{flag}=");
            out.push(with(&[flag]));
            out.push(with(&[flag, &value]));
            out.push(with(&[&joined]));
            out.push(with(&[&empty]));
            out.push(with(&[flag, ""]));
            out.push(with(&[flag, "-x"]));
            out.push(with(&[flag, "-5"]));
            out.push(with(&[flag, "--"]));
            out.push(with(&[flag, &value, flag, "second=2"]));
            out.push(with(&[flag, "bad value", "tail"]));
            for bad in [
                "0",
                "6",
                "x",
                " 4 ",
                "+3",
                "1_0",
                "0x3",
                "003",
                "²",
                "\u{7}",
                "\u{a0}",
                "é",
                "it's",
                "a'b\"c",
                "\\",
                "\u{200b}",
                "\u{e000}",
                "\u{1f600}",
                "\u{85}",
                "1__0",
                "_1",
                "\u{feff}",
            ] {
                out.push(with(&[flag, bad]));
            }
            if flag.starts_with("--") && flag.len() > 3 {
                let abbrev = &flag[..flag.len() - 1];
                out.push(with(&[abbrev, &value]));
                out.push(with(&[&format!("{abbrev}={value}")]));
                out.push(with(&[&flag[..3], &value]));
            } else if !flag.starts_with("--") {
                out.push(with(&[&format!("{flag}{value}")]));
                out.push(with(&[&format!("{flag}j")]));
                out.push(with(&[&format!("{flag}r")]));
                out.push(with(&[&format!("{flag}=x")]));
                out.push(with(&[&format!("{flag}-j")]));
                out.push(with(&["-jf", "q"]));
                out.push(with(&["-jfq"]));
                out.push(with(&["-jx"]));
            }
        }
    }
    // positionals with odd values
    out.push(owned(&["p0", "notanint", "3"]));
    out.push(owned(&["p0", "1", "notanint"]));
    out.push(owned(&["p0", "1"]));
    out.push(owned(&["p0", "-1", "-2"]));
    out.push(owned(&["bogus"]));
    out.push(owned(&["ls", "s", "--clear"]));
    out.push(owned(&["n", "--clear", "g"]));
    out.push(owned(&["n", "g", "--clear"]));
    out.push(owned(&["n", "", "--clear"]));
    out
}

struct Case {
    parser: usize,
    columns: usize,
    argv: Vec<String>,
}

fn value_json(value: &Value) -> Json {
    match value {
        Value::None => Json::Null,
        Value::Bool(flag) => json!(flag),
        Value::Str(text) => json!(text),
        Value::Int(number) => json!(number),
        Value::List(items) => json!(items),
    }
}

const PYTHON_DRIVER: &str = r#"
import argparse, contextlib, io, json, os, sys
from tx.cli import _group_value, _env_pair

TYPES = {"int": int, "group": _group_value, "env": _env_pair}
payload = json.load(sys.stdin)

def build(spec):
    parser = argparse.ArgumentParser(prog=spec["prog"], description=spec.get("description"))
    for arg in spec["args"]:
        names = arg.get("flags") or [arg["name"]]
        kwargs = {k: arg[k] for k in ("action", "nargs", "required", "choices", "metavar",
                                      "dest", "default", "const", "help") if k in arg}
        if "type" in arg:
            kwargs["type"] = TYPES[arg["type"]]
        if "int_choices" in arg:
            kwargs["type"] = int
            kwargs["choices"] = range(*arg["int_choices"])
        parser.add_argument(*names, **kwargs)
    return parser

results = []
for case in payload["cases"]:
    os.environ["COLUMNS"] = str(case["columns"])
    parser = build(payload["specs"][case["parser"]])
    out, err = io.StringIO(), io.StringIO()
    namespace, code = None, None
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        try:
            namespace = vars(parser.parse_args(case["argv"]))
        except SystemExit as exit:
            code = exit.code
    results.append({"stdout": out.getvalue(), "stderr": err.getvalue(), "code": code,
                    "namespace": namespace})
json.dump(results, sys.stdout)
"#;

fn run_python(specs: &[Json], cases: &[Case]) -> Vec<Json> {
    let payload = json!({
        "specs": specs,
        "cases": cases.iter().map(|c| json!({"parser": c.parser, "columns": c.columns, "argv": c.argv})).collect::<Vec<_>>(),
    });
    assert!(
        Path::new(PYTHON_LIB).join("tx/cli.py").is_file(),
        "reference tx.cli not found under {PYTHON_LIB}"
    );
    let mut child = Command::new("python3.14")
        .arg("-c")
        .arg(PYTHON_DRIVER)
        .env("PYTHONPATH", PYTHON_LIB)
        .env("NO_COLOR", "1")
        .env("PYTHON_COLORS", "0")
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("python3.14 must be installed for the argparse differential test");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.to_string().as_bytes())
        .expect("write payload");
    let output = child.wait_with_output().expect("python3.14 run");
    assert!(output.status.success(), "python driver failed");
    serde_json::from_slice(&output.stdout).expect("python JSON")
}

fn assert_no_differences(failures: &[String], total: usize) {
    assert!(
        failures.is_empty(),
        "{} of {total} cases differ; first ones:\n{}",
        failures.len(),
        failures
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn argparse_matches_cpython_314() {
    let specs = specs();
    let mut all = Vec::new();
    for (parser, spec) in specs.iter().enumerate() {
        for argv in cases(spec) {
            for &columns in COLUMNS {
                all.push(Case {
                    parser,
                    columns,
                    argv: argv.clone(),
                });
            }
        }
    }
    assert_no_differences(&differences(&specs, &all), all.len());
}

/// `-h` for every parser at every width from 1 to 130: exercises usage wrapping and the
/// textwrap port (hyphen / em-dash chunking, long-word breaking) far beyond the fixed widths.
#[test]
fn help_wrapping_matches_cpython_314_at_every_width() {
    let specs = specs();
    let all: Vec<Case> = (0..specs.len())
        .flat_map(|parser| {
            (1..=130).map(move |columns| Case {
                parser,
                columns,
                argv: vec!["-h".to_string()],
            })
        })
        .collect();
    assert_no_differences(&differences(&specs, &all), all.len());
}

/// Runs every case through CPython and through the Rust port; returns the differing cases.
fn differences(specs: &[Json], all: &[Case]) -> Vec<String> {
    let mut expected = run_python(specs, all);
    assert_eq!(expected.len(), all.len());
    for want in &mut expected {
        if let Some(stderr) = want["stderr"].as_str() {
            want["stderr"] = Json::String(quote_choices(stderr));
        }
    }
    let mut failures = Vec::new();
    for (case, want) in all.iter().zip(&expected) {
        let parser = build(&specs[case.parser], case.columns);
        let got = match parser.parse(&case.argv) {
            Ok(matches) => {
                let namespace: Map<String, Json> = matches
                    .iter()
                    .map(|(dest, value)| (dest.to_string(), value_json(value)))
                    .collect();
                json!({"stdout": "", "stderr": "", "code": null, "namespace": namespace})
            }
            Err(exit) => {
                json!({"stdout": exit.stdout, "stderr": exit.stderr, "code": exit.code, "namespace": null})
            }
        };
        if &got != want {
            failures.push(format!(
                "prog={:?} COLUMNS={} argv={:?}\n  want: {want}\n  got:  {got}",
                parser.prog(),
                case.columns,
                case.argv
            ));
        }
    }
    failures
}

/// The one deliberate difference from the local CPython: the contract pins `(choose from '1', '2')`
/// (quoted items) where CPython 3.14.4 prints `(choose from 1, 2)`.
fn quote_choices(stderr: &str) -> String {
    const OPEN: &str = " (choose from ";
    let Some(start) = stderr.find(OPEN) else {
        return stderr.to_string();
    };
    let list_start = start + OPEN.len();
    let Some(len) = stderr[list_start..].find(")\n") else {
        return stderr.to_string();
    };
    let items: Vec<String> = stderr[list_start..list_start + len]
        .split(", ")
        .map(|item| {
            if item.starts_with('\'') {
                item.to_string()
            } else {
                format!("'{item}'")
            }
        })
        .collect();
    format!(
        "{}{}{}",
        &stderr[..list_start],
        items.join(", "),
        &stderr[list_start + len..]
    )
}

#[test]
fn post_parse_error_matches_parser_error() {
    let spec = &specs()[0];
    let expected = {
        let driver = r#"
import argparse, contextlib, io, json, os, sys
os.environ["COLUMNS"] = "80"
p = argparse.ArgumentParser(prog="tx spawn", description="d")
p.add_argument("name")
p.add_argument("--tag", required=True)
err = io.StringIO()
with contextlib.redirect_stderr(err):
    try:
        p.error("--tag requires at least one value")
    except SystemExit as e:
        code = e.code
json.dump({"stderr": err.getvalue(), "code": code}, sys.stdout)
"#;
        let output = Command::new("python3.14")
            .arg("-c")
            .arg(driver)
            .env("NO_COLOR", "1")
            .output()
            .expect("python3.14");
        serde_json::from_slice::<Json>(&output.stdout).expect("json")
    };
    let parser = Parser::new(str_field(spec, "prog").expect("prog"))
        .description("d")
        .columns(80)
        .arg(Arg::positional("name"))
        .arg(Arg::option("--tag").required());
    let exit = parser.error("--tag requires at least one value");
    assert_eq!(json!({"stderr": exit.stderr, "code": exit.code}), expected);
    assert!(exit.stdout.is_empty());
}
