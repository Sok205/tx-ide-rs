//! The installer's seams: the Python heredocs of `install`, `uninstall` and `setup/engines/*.sh`
//! as hidden verbs, so the bash entry points drive this binary and need no Python.
//!
//! * `_engines` — the adapter registry as `<engine> <binary>` lines (`install.sh`'s default set).
//! * `_claude-settings` — `claude.sh`'s settings.json surgery + the marker sidecar.
//! * `_codex-hooks` — `codex.sh`'s tx-owned hooks.json + the marked config.toml block.
//! * `_agy-template` — `antigravity.sh`'s per-worktree hooks.json template.
//! * `_statusline` — the C10 statusLine edit of `install` / its reversal in `uninstall`.
//!
//! Output bytes (ANSI included) are the heredocs'. Fixed quirks: Q9 (a missing parent on a
//! profile restore is one `tx _claude-settings: …` line, not a traceback), Q29 (a strip drops
//! the parent objects it emptied), Q33 (an uninstall that changes nothing does not rewrite).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use cordis::{BoxError, Component, Ctx};
use serde_json::{Map, Value, json};

use crate::app::{COMMANDS, Command, ENGINES, Visibility};
use crate::argparse::{Arg, Matches, Parser};
use crate::engines::EngineRegistry;
use crate::engines::adapter::realpath;
use crate::pyjson;

const G: &str = "\x1b[32m";
const Y: &str = "\x1b[33m";
const D: &str = "\x1b[2m";
const X: &str = "\x1b[0m";

pub struct InstallVerbs;

impl Component for InstallVerbs {
    fn name(&self) -> &str {
        "verbs.install"
    }
    fn inject(&self) -> &[&'static str] {
        &["engines", "commands"]
    }
    fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        let engines = ctx.get(ENGINES)?;
        let table = ctx.get(COMMANDS)?;
        table.register(ctx, Visibility::Hidden, Engines(engines));
        table.register(ctx, Visibility::Hidden, ClaudeSettings);
        table.register(ctx, Visibility::Hidden, CodexHooks);
        table.register(ctx, Visibility::Hidden, AgyTemplate);
        table.register(ctx, Visibility::Hidden, StatusLine);
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
enum InstallError {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{path}: invalid JSON: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("{0} is not a JSON object")]
    NotObject(String),
    #[error("cannot strip {key}: its parent '{part}' is missing from settings.json")]
    MissingParent { key: String, part: String },
    #[error("cannot restore {key}: the marker records no prior value")]
    MissingValue { key: String },
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> InstallError + '_ {
    move |source| InstallError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn parse(parser: &Parser, argv: &[String]) -> Result<Matches, i32> {
    parser.parse(argv).map_err(|exit| exit.emit())
}

fn arg(matches: &Matches, dest: &str) -> String {
    matches.get_one(dest).unwrap_or_default().to_owned()
}

// ----- file helpers ------------------------------------------------------------------------

fn read_json(path: &Path) -> Result<Value, InstallError> {
    let text = fs::read_to_string(path).map_err(io_err(path))?;
    serde_json::from_str(&text).map_err(|source| InstallError::Json {
        path: path.to_path_buf(),
        source,
    })
}

fn parent_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// `tempfile.mkstemp(dir, prefix, suffix=".tmp")` + write + `os.replace`; the temp is removed
/// on failure.
fn atomic_write(target: &Path, prefix: &str, text: &str) -> Result<(), InstallError> {
    use std::io::Write;
    let dir = parent_dir(target);
    let mut temp = tempfile::Builder::new()
        .prefix(prefix)
        .suffix(".tmp")
        .tempfile_in(&dir)
        .map_err(io_err(&dir))?;
    temp.write_all(text.as_bytes()).map_err(io_err(target))?;
    temp.persist(target)
        .map_err(|error| io_err(target)(error.error))?;
    Ok(())
}

fn pretty(value: &Value) -> String {
    pyjson::dumps_pretty(value) + "\n"
}

fn object_mut<'a>(value: &'a mut Value, what: &str) -> Result<&'a mut Map<String, Value>, InstallError> {
    value
        .as_object_mut()
        .ok_or_else(|| InstallError::NotObject(what.to_owned()))
}

/// Python truthiness of a JSON value (`if marker:`).
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64() != Some(0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
    }
}

/// `str(value)` for the marker fields the status report prints.
fn py_str(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".to_owned(),
        Some(Value::Bool(true)) => "True".to_owned(),
        Some(Value::Bool(false)) => "False".to_owned(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) if number.is_f64() => {
            pyjson::float_repr(number.as_f64().unwrap_or_default())
        }
        Some(other) => pyjson::dumps(other),
    }
}

/// `(value or {}).get(key)` — `None` for a missing key and for a non-object.
fn field<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    value.and_then(|value| value.as_object()).and_then(|map| map.get(key))
}

/// `.get(key)` treating JSON `null` like a missing key (`x.get(k) is not None`).
fn present<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    field(value, key).filter(|value| !value.is_null())
}

fn entries(value: Option<&Value>) -> Vec<(String, Value)> {
    value
        .and_then(Value::as_object)
        .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

// ----- `_engines` --------------------------------------------------------------------------

struct Engines(Rc<std::cell::RefCell<EngineRegistry>>);

impl Command for Engines {
    fn name(&self) -> &'static str {
        "_engines"
    }
    fn summary(&self) -> &'static str {
        "Internal: list the registered engines as `<engine> <binary>` — the installer's seam."
    }
    fn run(&self, _argv: &[String]) -> Result<i32, BoxError> {
        let registry = self.0.borrow();
        let mut engines = registry.registered();
        engines.sort_by_key(|engine| engine.as_str());
        for engine in engines {
            if let Some(adapter) = registry.get(engine) {
                println!("{} {}", engine.as_str(), adapter.binary());
            }
        }
        Ok(0)
    }
}

// ----- `_claude-settings` ------------------------------------------------------------------

/// Claude event → the shim (under `<home>/hooks/claude/`) tx owns for it.
const CLAUDE_EVENTS: [(&str, &str); 12] = [
    ("SessionStart", "start"),
    ("UserPromptSubmit", "pre"),
    ("PreToolUse", "work"),
    ("PostToolUse", "work"),
    ("PostToolUseFailure", "work"),
    ("SubagentStart", "work"),
    ("PreCompact", "work"),
    ("Stop", "post"),
    ("StopFailure", "post"),
    ("PermissionRequest", "post"),
    ("Notification", "notify"),
    ("SessionEnd", "end"),
];

/// The settings keys tx owns beside the hooks (see `setup/engines/claude.sh` for the rationale).
/// Keys are dotted paths; install records each key's prior value, uninstall restores it.
const CONTEXT_PROFILE: &str = r#"{
  "disableWorkflows": true,
  "disableArtifact": true,
  "includeGitInstructions": false,
  "skillOverrides": {
    "artifact-design": "off",
    "artifact-diagramming": "off",
    "artifact-capabilities": "off",
    "claude-in-chrome": "off",
    "init": "off",
    "fewer-permission-prompts": "off",
    "code-review": "user-invocable-only",
    "security-review": "user-invocable-only",
    "simplify": "user-invocable-only",
    "dataviz": "user-invocable-only",
    "claude-api": "user-invocable-only",
    "update-config": "user-invocable-only",
    "keybindings-help": "user-invocable-only",
    "schedule": "user-invocable-only",
    "loop": "user-invocable-only",
    "run": "user-invocable-only"
  },
  "permissions.deny": ["ReportFindings", "ListAgents"]
}"#;

struct ClaudeSettings;

impl Command for ClaudeSettings {
    fn name(&self) -> &'static str {
        "_claude-settings"
    }
    fn summary(&self) -> &'static str {
        "Internal: claude.sh's settings.json surgery (hooks, context profile, marker sidecar)."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = Parser::new("tx _claude-settings")
            .description(self.summary())
            .arg(Arg::positional("op").choices(&["install", "uninstall", "status"]))
            .arg(Arg::option("--settings").required())
            .arg(Arg::option("--home").required())
            .arg(Arg::option("--stamp").required())
            .arg(Arg::option("--session-closed").required())
            .arg(Arg::option("--sandbox").flag())
            .arg(Arg::option("--dry-run").flag())
            .arg(Arg::option("--no-context-profile").flag());
        let matches = match parse(&parser, argv) {
            Ok(matches) => matches,
            Err(code) => return Ok(code),
        };
        let settings = arg(&matches, "settings");
        let home = arg(&matches, "home");
        let marker_path = if matches.get_flag("sandbox") {
            PathBuf::from(format!("{settings}.tx-managed.json"))
        } else {
            Path::new(&home).join("claude-managed.json")
        };
        let surgery = Surgery {
            real: realpath(Path::new(&settings)),
            events: CLAUDE_EVENTS
                .iter()
                .map(|(event, shim)| (*event, format!("{home}/hooks/claude/{shim}.sh")))
                .collect(),
            home,
            marker_path,
            stamp: arg(&matches, "stamp"),
            session_closed: arg(&matches, "session_closed"),
            apply_profile: !matches.get_flag("no_context_profile"),
            profile: serde_json::from_str(CONTEXT_PROFILE)?,
        };
        let op = arg(&matches, "op");
        surgery.run(&op, matches.get_flag("dry_run"))?;
        Ok(0)
    }
}

type Plan = Vec<(String, String)>;

struct Surgery {
    real: PathBuf,
    home: String,
    marker_path: PathBuf,
    stamp: String,
    session_closed: String,
    apply_profile: bool,
    profile: Value,
    events: Vec<(&'static str, String)>,
}

impl Surgery {
    fn run(&self, op: &str, dry_run: bool) -> Result<(), InstallError> {
        let mut data = if self.real.exists() {
            read_json(&self.real)?
        } else {
            json!({})
        };
        object_mut(&mut data, "settings.json")?;
        let before = data.clone();

        if op == "status" {
            self.status(&data)?;
            return Ok(());
        }
        let (plan, marker) = if op == "install" {
            let (plan, marker) = self.install(&mut data)?;
            (plan, Some(marker))
        } else {
            match self.uninstall(&mut data)? {
                Some(plan) => (plan, None),
                None => {
                    println!("  {Y}!{X} no _tx_ide_managed marker — settings.json left alone");
                    return Ok(());
                }
            }
        };
        for (event, action) in &plan {
            println!("  {G}→{X} {event:<18} {action}");
        }
        let name = self
            .real
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if dry_run {
            println!("  {D}(dry-run — {name} and the marker sidecar unchanged){X}");
            return Ok(());
        }
        // Q33: an uninstall whose plan changes nothing leaves settings.json (and its .bak) alone.
        if marker.is_none() && pretty(&data) == pretty(&before) {
            println!(
                "  {G}✓{X} {name} unchanged — not rewritten  {D}({}){X}",
                self.real.display()
            );
        } else {
            self.write(&data, &before)?;
            println!("  {G}✓{X} {name} written  {D}({}){X}", self.real.display());
            let backup = self.backup_path();
            if backup.exists() {
                println!("  {D}backup: {}{X}", backup.display());
            }
        }
        match marker {
            Some(marker) => {
                self.write_marker(&marker)?;
                println!(
                    "  {G}✓{X} marker sidecar written  {D}({}){X}",
                    self.marker_path.display()
                );
            }
            None if self.marker_path.exists() => {
                fs::remove_file(&self.marker_path).map_err(io_err(&self.marker_path))?;
                println!(
                    "  {G}✓{X} marker sidecar removed  {D}({}){X}",
                    self.marker_path.display()
                );
            }
            None => {}
        }
        Ok(())
    }

    fn backup_path(&self) -> PathBuf {
        PathBuf::from(format!("{}.bak.{}", self.real.display(), self.stamp))
    }

    fn write(&self, data: &Value, before: &Value) -> Result<(), InstallError> {
        if self.real.exists() {
            let backup = self.backup_path();
            fs::write(&backup, pretty(before)).map_err(io_err(&backup))?;
        }
        atomic_write(&self.real, ".tx-settings.", &pretty(data))
    }

    fn write_marker(&self, marker: &Value) -> Result<(), InstallError> {
        let dir = parent_dir(&self.marker_path);
        fs::create_dir_all(&dir).map_err(io_err(&dir))?;
        atomic_write(&self.marker_path, ".tx-managed.", &pretty(marker))
    }

    fn load_marker(&self, data: &Value) -> Result<Option<Value>, InstallError> {
        if self.marker_path.exists() {
            return read_json(&self.marker_path).map(Some);
        }
        Ok(data.get("_tx_ide_managed").cloned())
    }

    fn install(&self, data: &mut Value) -> Result<(Plan, Value), InstallError> {
        let existing = self.load_marker(data)?;
        let existing = existing.as_ref();
        let prior_commands = field(existing, "hook_commands");
        let mut plan = Plan::new();
        for (event, new_command) in &self.events {
            let new_value = Value::String(new_command.clone());
            let target = present(prior_commands, event);
            if target == Some(&new_value) {
                plan.push((event.to_string(), "already current".into()));
                continue;
            }
            let repointed = match target {
                Some(target) => set_commands(data, event, target, &new_value),
                None => 0,
            };
            if let (true, Some(target)) = (repointed > 0, target) {
                plan.push((
                    event.to_string(),
                    format!("repoint  {}  →  {new_command}", py_str(Some(target))),
                ));
            } else if count_entries(data, event, &new_value) > 0 {
                plan.push((event.to_string(), "already current".into()));
            } else {
                let block = json!({"matcher": "", "hooks": [
                    {"type": "command", "command": new_command, "timeout": 10, "async": true}]});
                let root = object_mut(data, "settings.json")?;
                let hooks = root
                    .entry("hooks")
                    .or_insert_with(|| Value::Object(Map::new()));
                let hooks = object_mut(hooks, "settings.json hooks")?;
                let blocks = hooks
                    .entry(event.to_string())
                    .or_insert_with(|| Value::Array(Vec::new()));
                match blocks.as_array_mut() {
                    Some(blocks) => blocks.push(block),
                    None => return Err(InstallError::NotObject(format!("hooks.{event}"))),
                }
                plan.push((event.to_string(), format!("add      {new_command}")));
            }
        }

        // Keep the ORIGINAL pre-tx marker across re-installs: "ours" = a marker whose recorded
        // commands point at this home's hooks/ shims.
        let ours_prefix = format!("{}/hooks/", self.home);
        let existing_is_ours = truthy(existing)
            && entries(field(existing, "hook_commands"))
                .iter()
                .any(|(_, command)| py_str(Some(command)).starts_with(&ours_prefix));
        let previous = if existing_is_ours {
            field(existing, "previous").cloned().unwrap_or(Value::Null)
        } else {
            existing.cloned().unwrap_or(Value::Null)
        };

        let (context_profile, profile_previous) = if self.apply_profile {
            let previous = self.install_profile(data, existing, &mut plan)?;
            (self.profile.clone(), previous)
        } else {
            plan.push((
                "context profile".into(),
                "skipped (--no-context-profile)".into(),
            ));
            (
                field(existing, "context_profile").cloned().unwrap_or(Value::Null),
                field(existing, "context_profile_previous")
                    .cloned()
                    .unwrap_or(Value::Null),
            )
        };

        let root = object_mut(data, "settings.json")?;
        if root.shift_remove("_tx_ide_managed").is_some() {
            plan.push((
                "marker".into(),
                "migrated out of settings.json → sidecar".into(),
            ));
        }

        let hook_commands: Map<String, Value> = self
            .events
            .iter()
            .map(|(event, command)| (event.to_string(), Value::String(command.clone())))
            .collect();
        let mut marker = Map::new();
        marker.insert("version".into(), json!(3));
        marker.insert("mode".into(), json!("coexist"));
        marker.insert("home".into(), json!(self.home));
        marker.insert("hook_commands".into(), Value::Object(hook_commands));
        marker.insert("context_profile".into(), context_profile);
        marker.insert("context_profile_previous".into(), profile_previous);
        marker.insert(
            "statusLine_command".into(),
            field(existing, "statusLine_command")
                .cloned()
                .unwrap_or(Value::Null),
        );
        marker.insert("tmux_session_closed".into(), json!(self.session_closed));
        marker.insert("previous".into(), previous);
        Ok((plan, Value::Object(marker)))
    }

    /// Apply each profile key, recording its prior value the FIRST time tx touches it.
    fn install_profile(
        &self,
        data: &mut Value,
        existing: Option<&Value>,
        plan: &mut Plan,
    ) -> Result<Value, InstallError> {
        let mut previous: Map<String, Value> = entries(field(existing, "context_profile_previous"))
            .into_iter()
            .collect();
        for (key, wanted) in entries(Some(&self.profile)) {
            let current = path_get(data, &key).cloned();
            if !previous.contains_key(&key) {
                let record = match &current {
                    None => json!({"absent": true}),
                    Some(value) => json!({ "value": value }),
                };
                previous.insert(key.clone(), record);
            }
            if current.as_ref() == Some(&wanted) {
                plan.push((key, "already current".into()));
                continue;
            }
            path_set(data, &key, wanted)?;
            let action = if current.is_some() { "set" } else { "add" };
            plan.push((key, action.into()));
        }
        Ok(Value::Object(previous))
    }

    fn uninstall(&self, data: &mut Value) -> Result<Option<Plan>, InstallError> {
        let marker = self.load_marker(data)?;
        if !truthy(marker.as_ref()) {
            return Ok(None);
        }
        let marker = marker.as_ref();
        let previous = present(marker, "previous").cloned();
        let prior_commands = field(previous.as_ref(), "hook_commands");
        let mut plan = Plan::new();
        for (event, command) in entries(field(marker, "hook_commands")) {
            if count_entries(data, &event, &command) == 0 {
                plan.push((event, "missing (drift) — skipped".into()));
                continue;
            }
            match present(prior_commands, &event) {
                Some(restore_to) => {
                    set_commands(data, &event, &command, restore_to);
                    plan.push((
                        event,
                        format!(
                            "restore  {}  →  {}",
                            py_str(Some(&command)),
                            py_str(Some(restore_to))
                        ),
                    ));
                }
                None => {
                    strip(data, &event, &command);
                    plan.push((event, format!("strip    {}", py_str(Some(&command)))));
                }
            }
        }
        for (key, record) in entries(field(marker, "context_profile_previous")) {
            if truthy(field(Some(&record), "absent")) {
                path_del(data, &key)?;
                plan.push((key, "strip    (was absent before tx)".into()));
            } else {
                let value = field(Some(&record), "value")
                    .cloned()
                    .ok_or_else(|| InstallError::MissingValue { key: key.clone() })?;
                let shown: String = pyjson::dumps(&value).chars().take(40).collect();
                path_set(data, &key, value)?;
                plan.push((key, format!("restore  → {shown}")));
            }
        }
        let root = object_mut(data, "settings.json")?;
        match previous {
            Some(previous) => {
                root.insert("_tx_ide_managed".into(), previous);
            }
            None => {
                root.shift_remove("_tx_ide_managed");
            }
        }
        Ok(Some(plan))
    }

    fn status(&self, data: &Value) -> Result<(), InstallError> {
        let marker = self.load_marker(data)?;
        if !truthy(marker.as_ref()) {
            println!("  {Y}!{X} no _tx_ide_managed marker — Claude integration not installed");
            return Ok(());
        }
        let marker = marker.as_ref();
        println!(
            "  marker: version={}  mode={}  home={}",
            py_str(field(marker, "version")),
            py_str(field(marker, "mode")),
            py_str(field(marker, "home"))
        );
        if self.marker_path.exists() {
            println!(
                "    {:<18} {D}{}{X}",
                "location",
                self.marker_path.display()
            );
        } else {
            println!(
                "    {:<18} {Y}LEGACY — embedded in settings.json (Claude ≥2.1.257 rejects the whole file; re-run install to migrate){X}",
                "location"
            );
        }
        for (event, command) in entries(field(marker, "hook_commands")) {
            let flag = if count_entries(data, &event, &command) > 0 {
                format!("{G}in sync{X}")
            } else {
                format!("{Y}DRIFT — not in settings{X}")
            };
            println!("    {event:<18} {flag}");
            println!("    {:<18} {D}{}{X}", "", py_str(Some(&command)));
        }
        let session_closed = field(marker, "tmux_session_closed");
        if truthy(session_closed) {
            println!(
                "    {:<18} {D}(tmux global) {}{X}",
                "session-closed",
                py_str(session_closed)
            );
        }
        let owned = field(marker, "context_profile");
        if !truthy(owned) {
            println!("    {:<18} {D}not managed{X}", "context profile");
            return Ok(());
        }
        for (key, wanted) in entries(owned) {
            let flag = if path_get(data, &key) == Some(&wanted) {
                format!("{G}in sync{X}")
            } else {
                format!("{Y}DRIFT — settings differ{X}")
            };
            println!("    {key:<18} {flag}");
        }
        Ok(())
    }
}

/// Every hook entry under `event` — the match-by-marker join (only these are ever touched).
fn hook_entries<'a>(data: &'a mut Value, event: &str) -> impl Iterator<Item = &'a mut Value> {
    data.get_mut("hooks")
        .and_then(|hooks| hooks.get_mut(event))
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
        .filter_map(|block| block.get_mut("hooks").and_then(Value::as_array_mut))
        .flatten()
}

fn count_entries(data: &Value, event: &str, command: &Value) -> usize {
    data.get("hooks")
        .and_then(|hooks| hooks.get(event))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|block| block.get("hooks").and_then(Value::as_array))
        .flatten()
        .filter(|entry| entry.get("command") == Some(command))
        .count()
}

/// Repoint every entry whose command is `from`; returns how many matched.
fn set_commands(data: &mut Value, event: &str, from: &Value, to: &Value) -> usize {
    let mut count = 0;
    for entry in hook_entries(data, event) {
        if entry.get("command") == Some(from) {
            entry["command"] = to.clone();
            count += 1;
        }
    }
    count
}

/// Remove the matching tx entry, dropping a block / event / `hooks` that becomes empty.
fn strip(data: &mut Value, event: &str, command: &Value) {
    let Some(hooks) = data.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    let blocks = hooks
        .get(event)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut kept_blocks = Vec::new();
    for mut block in blocks {
        let kept: Vec<Value> = block
            .get("hooks")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|entry| entry.get("command") != Some(command))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if !kept.is_empty() {
            block["hooks"] = Value::Array(kept);
            kept_blocks.push(block);
        }
    }
    if kept_blocks.is_empty() {
        hooks.shift_remove(event);
    } else {
        hooks.insert(event.to_owned(), Value::Array(kept_blocks));
    }
    if hooks.is_empty()
        && let Some(root) = data.as_object_mut()
    {
        root.shift_remove("hooks");
    }
}

fn path_get<'a>(data: &'a Value, key: &str) -> Option<&'a Value> {
    key.split('.')
        .try_fold(data, |node, part| node.as_object()?.get(part))
}

fn path_set(data: &mut Value, key: &str, value: Value) -> Result<(), InstallError> {
    let parts: Vec<&str> = key.split('.').collect();
    let (leaf, parents) = parts.split_last().expect("split yields at least one part");
    let mut node = data;
    for part in parents {
        node = object_mut(node, key)?
            .entry(part.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    object_mut(node, key)?.insert(leaf.to_string(), value);
    Ok(())
}

/// Pop the leaf, then drop every parent object the pop left empty (Q29). A missing parent is an
/// error (Q9) — nothing has been written at that point.
fn path_del(data: &mut Value, key: &str) -> Result<(), InstallError> {
    fn remove(node: &mut Map<String, Value>, parts: &[&str], key: &str) -> Result<bool, InstallError> {
        let [head, rest @ ..] = parts else {
            return Ok(false);
        };
        if rest.is_empty() {
            return Ok(node.shift_remove(*head).is_some());
        }
        let child = node
            .get_mut(*head)
            .ok_or_else(|| InstallError::MissingParent {
                key: key.to_owned(),
                part: head.to_string(),
            })?;
        let child = object_mut(child, key)?;
        let removed = remove(child, rest, key)?;
        if removed && child.is_empty() {
            node.shift_remove(*head);
        }
        Ok(removed)
    }
    let parts: Vec<&str> = key.split('.').collect();
    remove(object_mut(data, "settings.json")?, &parts, key)?;
    Ok(())
}

// ----- `_codex-hooks` ----------------------------------------------------------------------

const CODEX_EVENTS: [(&str, &str); 9] = [
    ("SessionStart", "start"),
    ("UserPromptSubmit", "pre"),
    ("PreToolUse", "work"),
    ("PostToolUse", "work"),
    ("PreCompact", "work"),
    ("PostCompact", "work"),
    ("SubagentStart", "work"),
    ("Stop", "post"),
    ("PermissionRequest", "post"),
];
const CODEX_MARK_BEGIN: &str = "# === BEGIN tx-ide (codex) ===";
const CODEX_MARK_END: &str = "# === END tx-ide (codex) ===";
const CODEX_BLOCK_BODY: &str = "[tui]\nstatus_line = [\"model\", \"reasoning\", \"project-name\", \"context-used\"]\nstatus_line_use_colors = true\n";

struct CodexHooks;

impl Command for CodexHooks {
    fn name(&self) -> &'static str {
        "_codex-hooks"
    }
    fn summary(&self) -> &'static str {
        "Internal: codex.sh's tx-owned hooks.json + the marked config.toml [tui] block."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = Parser::new("tx _codex-hooks")
            .description(self.summary())
            .arg(Arg::positional("op").choices(&["install", "uninstall", "status"]))
            .arg(Arg::option("--hooks-json").required())
            .arg(Arg::option("--config-toml").required())
            .arg(Arg::option("--update-log").required())
            .arg(Arg::option("--home").required())
            .arg(Arg::option("--stamp").required())
            .arg(Arg::option("--dry-run").flag());
        let matches = match parse(&parser, argv) {
            Ok(matches) => matches,
            Err(code) => return Ok(code),
        };
        let home = arg(&matches, "home");
        let hooks: Map<String, Value> = CODEX_EVENTS
            .iter()
            .map(|(event, shim)| {
                let command = format!("{home}/hooks/codex/{shim}.sh");
                (
                    event.to_string(),
                    json!([{"hooks": [{"type": "command", "command": command}]}]),
                )
            })
            .collect();
        let codex = Codex {
            hooks_json: arg(&matches, "hooks_json"),
            config_toml: arg(&matches, "config_toml"),
            update_log: arg(&matches, "update_log"),
            stamp: arg(&matches, "stamp"),
            dry_run: matches.get_flag("dry_run"),
            desired: json!({ "hooks": hooks }),
        };
        match arg(&matches, "op").as_str() {
            "install" => {
                codex.install_hooks_json()?;
                codex.install_config_block()?;
            }
            "uninstall" => {
                codex.uninstall_config_block()?;
                codex.uninstall_hooks_json()?;
            }
            _ => codex.status()?,
        }
        Ok(0)
    }
}

struct Codex {
    hooks_json: String,
    config_toml: String,
    update_log: String,
    stamp: String,
    dry_run: bool,
    desired: Value,
}

impl Codex {
    fn backup(&self, real: &Path, text: &str) -> Result<(), InstallError> {
        let path = PathBuf::from(format!("{}.bak.{}", real.display(), self.stamp));
        fs::write(&path, text).map_err(io_err(&path))
    }

    /// `json.load`, with anything unreadable or malformed as `None`.
    fn load_json(real: &Path) -> Option<Value> {
        let bytes = fs::read(real).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn read_text(real: &Path) -> Result<String, InstallError> {
        if !real.exists() {
            return Ok(String::new());
        }
        fs::read_to_string(real).map_err(io_err(real))
    }

    fn install_hooks_json(&self) -> Result<(), InstallError> {
        let hooks_json = &self.hooks_json;
        let real = realpath(Path::new(hooks_json));
        if Self::load_json(&real).as_ref() == Some(&self.desired) {
            println!("  {G}→{X} hooks.json already current  {D}{hooks_json}{X}");
            return Ok(());
        }
        let exists = real.exists();
        let action = if exists {
            format!("replaced (backup .bak.{})", self.stamp)
        } else {
            "written".to_owned()
        };
        if self.dry_run {
            println!("  {D}would write {hooks_json} ({action}){X}");
            return Ok(());
        }
        if exists {
            let old = fs::read(&real).map_err(io_err(&real))?;
            self.backup(&real, &String::from_utf8_lossy(&old))?;
        }
        let dir = parent_dir(&real);
        fs::create_dir_all(&dir).map_err(io_err(&dir))?;
        atomic_write(&real, ".tx-codex.", &pretty(&self.desired))?;
        println!("  {G}→{X} hooks.json {action}  {D}{hooks_json}{X}");
        Ok(())
    }

    fn uninstall_hooks_json(&self) -> Result<(), InstallError> {
        let hooks_json = &self.hooks_json;
        let real = realpath(Path::new(hooks_json));
        if !real.exists() {
            println!("  {D}hooks.json (already absent) {hooks_json}{X}");
            return Ok(());
        }
        if Self::load_json(&real).as_ref() != Some(&self.desired) {
            println!(
                "  {Y}→{X} hooks.json present but not ours (drift) — left alone  {D}{hooks_json}{X}"
            );
            return Ok(());
        }
        if self.dry_run {
            println!("  {D}would remove {hooks_json}{X}");
            return Ok(());
        }
        fs::remove_file(&real).map_err(io_err(&real))?;
        println!("  {G}→{X} hooks.json removed  {D}{hooks_json}{X}");
        Ok(())
    }

    fn install_config_block(&self) -> Result<(), InstallError> {
        let config_toml = &self.config_toml;
        let real = realpath(Path::new(config_toml));
        let content = Self::read_text(&real)?;
        if content.contains(CODEX_MARK_BEGIN) {
            println!("  {G}→{X} config.toml [tui] block already present  {D}{config_toml}{X}");
            return Ok(());
        }
        if content
            .lines()
            .any(|line| line.trim_start().starts_with("[tui]"))
        {
            println!(
                "  {Y}!{X} config.toml already has a [tui] table — our marked block would DUPLICATE it; review {config_toml}"
            );
        }
        if self.dry_run {
            println!("  {D}would append the marked [tui] block to {config_toml}{X}");
            return Ok(());
        }
        if !content.is_empty() {
            self.backup(&real, &content)?;
        }
        let appended = format!("{content}\n{CODEX_MARK_BEGIN}\n{CODEX_BLOCK_BODY}{CODEX_MARK_END}\n");
        atomic_write(&real, ".tx-codex.", &appended)?;
        let tail = if content.is_empty() {
            String::new()
        } else {
            format!("  {D}(backup .bak.{}){X}", self.stamp)
        };
        println!("  {G}→{X} config.toml [tui] block appended  {D}{config_toml}{X}{tail}");
        Ok(())
    }

    fn uninstall_config_block(&self) -> Result<(), InstallError> {
        let config_toml = &self.config_toml;
        let real = realpath(Path::new(config_toml));
        if !real.exists() {
            println!("  {D}config.toml (absent) {config_toml}{X}");
            return Ok(());
        }
        let content = fs::read_to_string(&real).map_err(io_err(&real))?;
        if !content.contains(CODEX_MARK_BEGIN) {
            println!("  {D}config.toml — no tx-ide block {config_toml}{X}");
            return Ok(());
        }
        let stripped = strip_codex_block(&content);
        if self.dry_run {
            println!("  {D}would strip the marked [tui] block from {config_toml}{X}");
            return Ok(());
        }
        self.backup(&real, &content)?;
        atomic_write(&real, ".tx-codex.", &stripped)?;
        println!(
            "  {G}→{X} config.toml [tui] block stripped  {D}{config_toml}{X}  {D}(backup .bak.{}){X}",
            self.stamp
        );
        Ok(())
    }

    fn status(&self) -> Result<(), InstallError> {
        let hooks_json = &self.hooks_json;
        let real = realpath(Path::new(hooks_json));
        let flag = if !real.exists() {
            format!("{Y}absent{X}")
        } else if Self::load_json(&real).as_ref() == Some(&self.desired) {
            format!("{G}ours{X}")
        } else {
            format!("{Y}present but not ours (drift){X}")
        };
        println!("  hooks.json:              {flag}  {D}{hooks_json}{X}");

        let config_toml = &self.config_toml;
        let content = Self::read_text(&realpath(Path::new(config_toml)))?;
        let flag = if content.contains(CODEX_MARK_BEGIN) {
            format!("{G}present{X}")
        } else {
            format!("{Y}absent{X}")
        };
        println!("  config.toml [tui] block: {flag}  {D}{config_toml}{X}");
        let note = if content.contains("[hooks.state]") {
            format!("  {Y}(config has a [hooks.state] table — not written by us){X}")
        } else {
            String::new()
        };
        println!("  [hooks.state] trust:     {D}none — bypass-first (verification §4){X}{note}");

        let update_log = &self.update_log;
        println!("  automatic update log:    {D}{update_log}{X}");
        let log = Path::new(update_log);
        if log.exists() {
            let text = fs::read(log).map_err(io_err(log))?;
            let text = String::from_utf8_lossy(&text);
            let lines: Vec<&str> = text.split_inclusive('\n').collect();
            for line in &lines[lines.len().saturating_sub(10)..] {
                println!("    {}", line.trim_end());
            }
        } else {
            println!("    {D}not checked yet{X}");
        }
        Ok(())
    }
}

/// `re.sub(r"\n?BEGIN.*?END\n", "", content, count=1, flags=DOTALL)`.
fn strip_codex_block(content: &str) -> String {
    let Some(begin) = content.find(CODEX_MARK_BEGIN) else {
        return content.to_owned();
    };
    let after_begin = begin + CODEX_MARK_BEGIN.len();
    let closing = format!("{CODEX_MARK_END}\n");
    let Some(end) = content[after_begin..].find(&closing) else {
        return content.to_owned();
    };
    let end = after_begin + end + closing.len();
    let start = if content[..begin].ends_with('\n') {
        begin - 1
    } else {
        begin
    };
    format!("{}{}", &content[..start], &content[end..])
}

// ----- `_agy-template` ---------------------------------------------------------------------

struct AgyTemplate;

impl Command for AgyTemplate {
    fn name(&self) -> &'static str {
        "_agy-template"
    }
    fn summary(&self) -> &'static str {
        "Internal: write antigravity.sh's per-worktree hooks.json template."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = Parser::new("tx _agy-template")
            .description(self.summary())
            .arg(Arg::option("--home").required());
        let matches = match parse(&parser, argv) {
            Ok(matches) => matches,
            Err(code) => return Ok(code),
        };
        let dir = format!("{}/hooks/antigravity", arg(&matches, "home"));
        let shim = |name: &str| format!("{dir}/{name}.sh");
        let flat = |command: String| json!([{"type": "command", "command": command}]);
        let grouped = |command: String| {
            json!([{"matcher": "*", "hooks": [{"type": "command", "command": command}]}])
        };
        let mut events = Map::new();
        events.insert("SessionStart".into(), flat(shim("start")));
        events.insert("PreInvocation".into(), flat(shim("pre")));
        events.insert("PreToolUse".into(), grouped(shim("work")));
        events.insert("PostToolUse".into(), grouped(shim("work")));
        events.insert("PostInvocation".into(), flat(shim("work")));
        events.insert("Stop".into(), flat(shim("post")));
        let template = json!({ "tx-ide": events });
        let path = PathBuf::from(format!("{dir}/hooks.json.template"));
        fs::write(&path, pretty(&template)).map_err(io_err(&path))?;
        Ok(0)
    }
}

// ----- `_statusline` -----------------------------------------------------------------------

struct StatusLine;

impl Command for StatusLine {
    fn name(&self) -> &'static str {
        "_statusline"
    }
    fn summary(&self) -> &'static str {
        "Internal: point settings.json's statusLine at $TX_IDE_HOME (install) or remove ours (uninstall)."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = Parser::new("tx _statusline")
            .description(self.summary())
            .arg(Arg::positional("op").choices(&["install", "uninstall"]))
            .arg(Arg::option("--settings").required())
            .arg(Arg::option("--home").required())
            .arg(Arg::option("--stamp").required());
        let matches = match parse(&parser, argv) {
            Ok(matches) => matches,
            Err(code) => return Ok(code),
        };
        let settings = arg(&matches, "settings");
        let home = arg(&matches, "home");
        let stamp = arg(&matches, "stamp");
        let code = if arg(&matches, "op") == "install" {
            statusline_install(&settings, &home, &stamp)?
        } else {
            statusline_uninstall(&settings, &home, &stamp)?
        };
        Ok(code)
    }
}

fn statusline_install(settings: &str, home: &str, stamp: &str) -> Result<i32, InstallError> {
    const MODE: &str = "installed";
    let real = realpath(Path::new(settings));
    if !real.exists() {
        println!("  {Y}!{X} {settings} absent — run claude.sh install first");
        return Ok(1);
    }
    let mut data = read_json(&real)?;
    let before = data.clone();
    let status_command = format!("bash {home}/statusline.sh");
    let root = object_mut(&mut data, "settings.json")?;
    match root.get_mut("statusLine").and_then(Value::as_object_mut) {
        Some(existing) => {
            existing.insert("command".into(), json!(status_command));
        }
        None => {
            root.insert(
                "statusLine".into(),
                json!({"type": "command", "command": status_command}),
            );
        }
    }

    let marker_path = Path::new(home).join("claude-managed.json");
    let mut marker = read_json(&marker_path)?;
    let fields = object_mut(&mut marker, &marker_path.display().to_string())?;
    fields.insert("mode".into(), json!(MODE));
    fields.insert("statusLine_command".into(), json!(status_command));
    atomic_write(&marker_path, ".tx-managed.", &pretty(&marker))?;

    let backup = PathBuf::from(format!("{}.bak.{stamp}", real.display()));
    fs::write(&backup, pretty(&before)).map_err(io_err(&backup))?;
    atomic_write(&real, ".tx-settings.", &pretty(&data))?;
    println!("  {G}→{X} statusLine.command → {status_command}  {D}(marker mode={MODE}){X}");
    println!("  {D}backup: {}{X}", backup.display());
    Ok(0)
}

fn statusline_uninstall(settings: &str, home: &str, stamp: &str) -> Result<i32, InstallError> {
    let real = realpath(Path::new(settings));
    let mut data = read_json(&real)?;
    let before = data.clone();
    let marker_path = Path::new(home).join("claude-managed.json");
    let marker = if marker_path.exists() {
        Some(read_json(&marker_path)?)
    } else {
        data.get("_tx_ide_managed").cloned()
    };
    let status_command = field(marker.as_ref(), "statusLine_command").cloned();
    let root = object_mut(&mut data, "settings.json")?;
    let ours = truthy(status_command.as_ref())
        && root
            .get("statusLine")
            .and_then(Value::as_object)
            .is_some_and(|existing| existing.get("command") == status_command.as_ref());
    if !ours {
        println!("  {Y}→{X} statusLine not ours / absent — left alone");
        return Ok(0);
    }
    root.shift_remove("statusLine");
    let backup = PathBuf::from(format!("{}.bak.{stamp}", real.display()));
    fs::write(&backup, pretty(&before)).map_err(io_err(&backup))?;
    atomic_write(&real, ".tx-settings.", &pretty(&data))?;
    println!(
        "  {G}→{X} statusLine removed  {D}({}){X}",
        py_str(status_command.as_ref())
    );
    println!("  {D}backup: {}{X}", backup.display());
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_del_drops_the_parent_it_emptied() {
        let mut data = json!({"a": 1, "permissions": {"deny": ["x"]}});
        path_del(&mut data, "permissions.deny").unwrap();
        assert_eq!(data, json!({"a": 1}));
    }

    #[test]
    fn path_del_keeps_a_parent_with_siblings() {
        let mut data = json!({"permissions": {"allow": ["Bash"], "deny": ["x"]}});
        path_del(&mut data, "permissions.deny").unwrap();
        assert_eq!(data, json!({"permissions": {"allow": ["Bash"]}}));
    }

    #[test]
    fn path_del_missing_parent_is_an_error_naming_the_key() {
        let mut data = json!({"a": 1});
        let error = path_del(&mut data, "permissions.deny").unwrap_err();
        assert!(error.to_string().contains("permissions.deny"));
        assert_eq!(data, json!({"a": 1}));
    }

    #[test]
    fn path_set_creates_parents_and_keeps_order() {
        let mut data = json!({"z": 1});
        path_set(&mut data, "permissions.deny", json!(["x"])).unwrap();
        assert_eq!(pyjson::dumps(&data), r#"{"z": 1, "permissions": {"deny": ["x"]}}"#);
    }

    #[test]
    fn strip_codex_block_matches_the_reference_regex() {
        let block = format!("{CODEX_MARK_BEGIN}\n{CODEX_BLOCK_BODY}{CODEX_MARK_END}\n");
        assert_eq!(strip_codex_block(&format!("a\n\n{block}")), "a\n");
        assert_eq!(strip_codex_block(&block), "");
        assert_eq!(
            strip_codex_block(&format!("a\n\n{block}\n{block}")),
            format!("a\n\n{block}")
        );
        assert_eq!(strip_codex_block(&format!("a\n{CODEX_MARK_BEGIN}")), format!("a\n{CODEX_MARK_BEGIN}"));
    }

    #[test]
    fn strip_drops_emptied_blocks_events_and_hooks() {
        let command = json!("/h/post.sh");
        let mut data = json!({"hooks": {"Stop": [
            {"matcher": "", "hooks": [{"type": "command", "command": "/h/post.sh"}]}]}});
        strip(&mut data, "Stop", &command);
        assert_eq!(data, json!({}));
    }

    #[test]
    fn truthiness_follows_python() {
        assert!(!truthy(Some(&json!({}))));
        assert!(!truthy(Some(&Value::Null)));
        assert!(truthy(Some(&json!({"a": 1}))));
    }
}
