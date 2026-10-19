//! Adds a plugin's hook commands to the agents' hook files behind a sidecar that records exactly
//! what was added, so uninstall removes only that. Same file handling as `tx-hooks`' installer,
//! parametrised by the plugin's own [`Entries`] so plugins coexist. No foreign keys go into the
//! agents' files (Claude Code validates its settings schema).

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

/// What one plugin registers: the sidecar file recording it, the (agent event, our event)
/// pairs, and the hidden verb the agent runs. The verb `_history-ingest` takes no event
/// argument; neither does any entry whose event is empty.
#[derive(Clone, Copy, Debug)]
pub struct Entries {
    pub sidecar: &'static str,
    pub events: &'static [(&'static str, &'static str)],
    pub verb: &'static str,
}

const NO_EVENT_VERB: &str = "_history-ingest";

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{}: not valid JSON: {source}", path.display())]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("{0} is not a JSON object")]
    NotObject(String),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> InstallError + '_ {
    move |source| InstallError::Io {
        path: path.to_owned(),
        source,
    }
}

/// One agent's hook file. Claude nests under `hooks.<Event>[].hooks[]`; so does Codex
/// (`hooks.json` root is `{"hooks": {...}}`), so both share one shape.
struct Target {
    file: PathBuf,
    sidecar: PathBuf,
    label: &'static str,
}

fn targets(home: &Path, sidecar: &str, only_existing: bool) -> Vec<Target> {
    let mut out = vec![Target {
        file: home.join(".claude/settings.json"),
        sidecar: home.join(".claude").join(sidecar),
        label: "claude",
    }];
    if !only_existing || home.join(".codex").is_dir() {
        out.push(Target {
            file: home.join(".codex/hooks.json"),
            sidecar: home.join(".codex").join(sidecar),
            label: "codex",
        });
    }
    out
}

fn read_json(path: &Path) -> Result<Map<String, Value>, InstallError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(source) => {
            return Err(InstallError::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    let value = serde_json::from_str::<Value>(&text).map_err(|source| InstallError::Json {
        path: path.to_owned(),
        source,
    })?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(InstallError::NotObject(path.display().to_string())),
    }
}

/// Atomic replace that writes through a symlink (dotfile managers) and keeps the original's
/// permissions (`settings.json` may be 0600 and hold API keys).
fn write_json(path: &Path, value: &Value) -> Result<(), InstallError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io(dir))?;
    }
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let mut text = serde_json::to_string_pretty(value).expect("JSON value");
    text.push('\n');
    let tmp = target.with_extension("tmp");
    std::fs::write(&tmp, text).map_err(io(&tmp))?;
    if let Ok(meta) = std::fs::metadata(&target) {
        std::fs::set_permissions(&tmp, meta.permissions()).map_err(io(&tmp))?;
    }
    std::fs::rename(&tmp, &target).map_err(io(&target))
}

fn backup(path: &Path) -> Result<Option<PathBuf>, InstallError> {
    if !path.exists() {
        return Ok(None);
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut backup = PathBuf::from(format!("{}.bak.{ts}", path.display()));
    let mut n = 0;
    while backup.exists() {
        n += 1;
        backup = PathBuf::from(format!("{}.bak.{ts}-{n}", path.display()));
    }
    std::fs::copy(path, &backup).map_err(io(&backup))?;
    Ok(Some(backup))
}

/// POSIX single-quoting: safe for any path (spaces, `$`, embedded `'`).
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The hook commands. Claude Code reads exit 2 as "block the agent", so a `txc` that cannot
/// even start (a broken `plugins.yaml`, the plugin disabled, usage) is capped at exit 1: a
/// non-blocking error, never a blocked prompt or stop.
fn commands(exe: &Path, entries: &Entries) -> Vec<(&'static str, String)> {
    let exe = shell_quote(&exe.display().to_string());
    entries
        .events
        .iter()
        .map(|(agent_event, event)| {
            let verb = entries.verb;
            let command = if event.is_empty() || verb == NO_EVENT_VERB {
                format!("{exe} {verb} || exit 1")
            } else {
                format!("{exe} {verb} {event} || exit 1")
            };
            (*agent_event, command)
        })
        .collect()
}

fn blocks<'a>(
    root: &'a mut Map<String, Value>,
    event: &str,
) -> Result<&'a mut Vec<Value>, InstallError> {
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| InstallError::NotObject("hooks".into()))?;
    let blocks = hooks
        .entry(event.to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    blocks
        .as_array_mut()
        .ok_or_else(|| InstallError::NotObject(format!("hooks.{event}")))
}

fn has_command(blocks: &[Value], command: &str) -> bool {
    blocks.iter().any(|block| {
        block
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|hooks| {
                hooks
                    .iter()
                    .any(|h| h.get("command").and_then(Value::as_str) == Some(command))
            })
    })
}

pub fn install(
    home: &Path,
    exe: &Path,
    entries: &Entries,
    dry_run: bool,
    out: &mut dyn Write,
) -> Result<(), InstallError> {
    for target in targets(home, entries.sidecar, true) {
        let mut root = read_json(&target.file)?;
        let mut added = Vec::new();
        for (event, command) in commands(exe, entries) {
            let blocks = blocks(&mut root, event)?;
            if has_command(blocks, &command) {
                continue;
            }
            blocks.push(json!({"matcher": "", "hooks": [
                {"type": "command", "command": command, "timeout": 10}]}));
            added.push(json!({"event": event, "command": command}));
        }
        if added.is_empty() {
            let _ = writeln!(out, "{}: already current", target.label);
            continue;
        }
        for entry in &added {
            let _ = writeln!(
                out,
                "{}: add {}  {}",
                target.label,
                entry["event"].as_str().unwrap_or(""),
                entry["command"].as_str().unwrap_or("")
            );
        }
        if dry_run {
            let _ = writeln!(out, "{}: dry-run, nothing written", target.label);
            continue;
        }
        // The sidecar remembers the first install's backup so uninstall can restore it.
        let mut sidecar = read_json(&target.sidecar)?;
        if sidecar.is_empty() {
            let backup = backup(&target.file)?;
            sidecar.insert("version".into(), json!(1));
            sidecar.insert(
                "backup".into(),
                backup
                    .map(|b| json!(b.display().to_string()))
                    .unwrap_or(Value::Null),
            );
            sidecar.insert("added".into(), json!([]));
        }
        if let Some(list) = sidecar.get_mut("added").and_then(Value::as_array_mut) {
            list.extend(added);
        }
        write_json(&target.file, &Value::Object(root))?;
        write_json(&target.sidecar, &Value::Object(sidecar))?;
        let _ = writeln!(out, "{}: written {}", target.label, target.file.display());
    }
    Ok(())
}

pub fn uninstall(home: &Path, entries: &Entries, out: &mut dyn Write) -> Result<(), InstallError> {
    let mut any = false;
    for target in targets(home, entries.sidecar, false) {
        if !target.sidecar.exists() {
            continue;
        }
        any = true;
        let sidecar = read_json(&target.sidecar)?;
        let added: Vec<(String, String)> = sidecar
            .get("added")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|e| {
                        Some((
                            e.get("event")?.as_str()?.to_owned(),
                            e.get("command")?.as_str()?.to_owned(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut root = read_json(&target.file)?;
        if let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) {
            for (event, command) in &added {
                if let Some(blocks) = hooks.get_mut(event).and_then(Value::as_array_mut) {
                    blocks.retain(|block| {
                        !block
                            .get("hooks")
                            .and_then(Value::as_array)
                            .is_some_and(|hooks| {
                                hooks.len() == 1
                                    && hooks[0].get("command").and_then(Value::as_str)
                                        == Some(command)
                            })
                    });
                    if blocks.is_empty() {
                        hooks.remove(event);
                    }
                }
            }
            if hooks.is_empty() {
                root.remove("hooks");
            }
        }
        // Byte-identical restore when nothing else changed since install: prefer the backup.
        let backup = sidecar
            .get("backup")
            .and_then(Value::as_str)
            .map(PathBuf::from);
        match backup.filter(|b| b.exists()) {
            Some(backup) if read_json(&backup)? == root => {
                std::fs::copy(&backup, &target.file).map_err(io(&target.file))?;
            }
            _ => write_json(&target.file, &Value::Object(root))?,
        }
        std::fs::remove_file(&target.sidecar).map_err(io(&target.sidecar))?;
        let _ = writeln!(
            out,
            "{}: removed {} hook(s) from {}",
            target.label,
            added.len(),
            target.file.display()
        );
    }
    if !any {
        let _ = writeln!(
            out,
            "nothing to do: no {} sidecar under {}",
            entries.sidecar,
            home.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CHAT: Entries = Entries {
        sidecar: ".tx-chat-managed.json",
        events: &[
            ("UserPromptSubmit", "prompt-submit"),
            ("Stop", "stop"),
            ("StopFailure", "stop"),
            ("SessionEnd", "session-end"),
        ],
        verb: "_chat-hook",
    };
    const HISTORY: Entries = Entries {
        sidecar: ".tx-history-managed.json",
        events: &[("Stop", "stop"), ("SessionEnd", "session-end")],
        verb: "_history-ingest",
    };

    fn home() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("txc-home")
            .tempdir_in("/tmp")
            .unwrap()
    }
    fn read(path: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }
    fn commands_of(settings: &serde_json::Value, event: &str) -> Vec<String> {
        settings["hooks"][event]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .flat_map(|b| b["hooks"].as_array().unwrap().clone())
                    .map(|h| h["command"].as_str().unwrap().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn commands_are_shell_quoted_and_never_exit_2() {
        let cmds = commands(Path::new("/Users/Jan Kowalski/it's/txc"), &CHAT);
        let (event, stop) = cmds.iter().find(|(e, _)| *e == "Stop").unwrap();
        assert_eq!(event, &"Stop");
        assert_eq!(
            stop,
            "'/Users/Jan Kowalski/it'\\''s/txc' _chat-hook stop || exit 1"
        );
    }

    #[test]
    fn history_ingest_takes_no_event_argument() {
        let cmds = commands(Path::new("/opt/txc"), &HISTORY);
        assert!(
            cmds.iter()
                .all(|(_, c)| c == "'/opt/txc' _history-ingest || exit 1"),
            "{cmds:?}"
        );
    }

    #[test]
    fn install_keeps_permissions_and_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let home = home();
        let claude = home.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        let real = home.path().join("dotfiles-settings.json");
        std::fs::write(&real, "{}").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&real, claude.join("settings.json")).unwrap();
        let mut out = Vec::new();
        install(home.path(), Path::new("/opt/txc"), &CHAT, false, &mut out).unwrap();
        assert!(
            claude
                .join("settings.json")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink survives"
        );
        assert_eq!(
            std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o600,
            "mode preserved on the real file"
        );
        assert!(read(&real)["hooks"]["Stop"].is_array());
    }

    #[test]
    fn install_creates_missing_settings() {
        let home = home();
        let mut out = Vec::new();
        install(home.path(), Path::new("/opt/txc"), &CHAT, false, &mut out).unwrap();
        let settings = read(&home.path().join(".claude/settings.json"));
        let stop = &settings["hooks"]["Stop"][0]["hooks"][0];
        assert_eq!(stop["type"], "command");
        assert_eq!(stop["command"], "'/opt/txc' _chat-hook stop || exit 1");
        assert_eq!(
            commands_of(&settings, "SessionEnd"),
            ["'/opt/txc' _chat-hook session-end || exit 1"]
        );
        assert_eq!(
            commands_of(&settings, "StopFailure"),
            ["'/opt/txc' _chat-hook stop || exit 1"]
        );
        assert!(home.path().join(".claude/.tx-chat-managed.json").exists());
        assert!(
            !home.path().join(".codex").exists(),
            "codex untouched when absent"
        );
    }

    #[test]
    fn install_is_idempotent_and_keeps_foreign_hooks() {
        let home = home();
        let claude = home.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        let before = json!({"theme": "dark", "hooks": {"Stop": [{"matcher": "", "hooks": [{"type": "command", "command": "say done"}]}]}});
        std::fs::write(claude.join("settings.json"), before.to_string()).unwrap();
        let mut out = Vec::new();
        install(home.path(), Path::new("/opt/txc"), &CHAT, false, &mut out).unwrap();
        install(home.path(), Path::new("/opt/txc"), &CHAT, false, &mut out).unwrap();
        let settings = read(&claude.join("settings.json"));
        assert_eq!(settings["theme"], "dark");
        assert_eq!(
            commands_of(&settings, "Stop"),
            ["say done", "'/opt/txc' _chat-hook stop || exit 1"],
            "foreign + ours, once"
        );
        let backups = std::fs::read_dir(&claude)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("settings.json.bak.")
            })
            .count();
        assert_eq!(backups, 1, "one backup, taken before the first write");
    }

    #[test]
    fn uninstall_restores_byte_identical() {
        let home = home();
        let claude = home.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        let text = "{\n  \"theme\": \"dark\",\n  \"hooks\": {\n    \"Stop\": [{\"matcher\": \"\", \"hooks\": [{\"type\": \"command\", \"command\": \"say done\"}]}]\n  }\n}\n";
        std::fs::write(claude.join("settings.json"), text).unwrap();
        let mut out = Vec::new();
        install(home.path(), Path::new("/opt/txc"), &CHAT, false, &mut out).unwrap();
        uninstall(home.path(), &CHAT, &mut out).unwrap();
        assert_eq!(
            std::fs::read_to_string(claude.join("settings.json")).unwrap(),
            text
        );
        assert!(!claude.join(".tx-chat-managed.json").exists());
    }

    #[test]
    fn uninstall_without_sidecar_is_a_noop() {
        let home = home();
        let mut out = Vec::new();
        uninstall(home.path(), &CHAT, &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("nothing to do"));
    }

    #[test]
    fn install_refuses_non_object() {
        let home = home();
        let claude = home.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(claude.join("settings.json"), "[1]").unwrap();
        let mut out = Vec::new();
        let error =
            install(home.path(), Path::new("/opt/txc"), &CHAT, false, &mut out).unwrap_err();
        assert!(matches!(error, InstallError::NotObject(_)), "{error}");
        assert_eq!(
            std::fs::read_to_string(claude.join("settings.json")).unwrap(),
            "[1]"
        );
    }

    #[test]
    fn dry_run_writes_nothing() {
        let home = home();
        let mut out = Vec::new();
        install(home.path(), Path::new("/opt/txc"), &CHAT, true, &mut out).unwrap();
        assert!(!home.path().join(".claude").exists());
        assert!(String::from_utf8(out).unwrap().contains("_chat-hook stop"));
    }

    #[test]
    fn codex_hooks_when_codex_dir_exists() {
        let home = home();
        std::fs::create_dir_all(home.path().join(".codex")).unwrap();
        let mut out = Vec::new();
        install(home.path(), Path::new("/opt/txc"), &CHAT, false, &mut out).unwrap();
        let hooks = read(&home.path().join(".codex/hooks.json"));
        assert_eq!(
            commands_of(&hooks, "Stop"),
            ["'/opt/txc' _chat-hook stop || exit 1"]
        );
        uninstall(home.path(), &CHAT, &mut out).unwrap();
        assert_eq!(read(&home.path().join(".codex/hooks.json")), json!({}));
    }

    #[test]
    fn coexists_with_another_sidecar() {
        let home = home();
        let claude = home.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        let mut out = Vec::new();
        install(home.path(), Path::new("/opt/txc"), &CHAT, false, &mut out).unwrap();
        install(
            home.path(),
            Path::new("/opt/txc"),
            &HISTORY,
            false,
            &mut out,
        )
        .unwrap();
        uninstall(home.path(), &CHAT, &mut out).unwrap();
        let settings = read(&claude.join("settings.json"));
        assert_eq!(
            commands_of(&settings, "Stop"),
            ["'/opt/txc' _history-ingest || exit 1"]
        );
        assert_eq!(
            commands_of(&settings, "SessionEnd"),
            ["'/opt/txc' _history-ingest || exit 1"]
        );
        assert!(settings["hooks"].get("UserPromptSubmit").is_none());
        assert!(claude.join(".tx-history-managed.json").exists());
        assert!(!claude.join(".tx-chat-managed.json").exists());
        uninstall(home.path(), &HISTORY, &mut out).unwrap();
        assert_eq!(read(&claude.join("settings.json")), json!({}));
    }
}
