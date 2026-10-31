//! Removes the global agent hooks older txc versions installed (the chat plugin's chat-id
//! capture and the history plugin's transcript mirror), driven by the sidecar each install wrote next to the agent's hook
//! file: it records exactly what was added and the backup taken before the first write, so only
//! tx's own entries go. Run by `txc doctor --fix-hooks`.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// The sidecars the retired installers wrote, one per plugin.
pub const RETIRED_SIDECARS: [&str; 2] = [".tx-chat-managed.json", ".tx-history-managed.json"];

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

fn targets(home: &Path, sidecar: &str) -> [Target; 2] {
    [
        Target {
            file: home.join(".claude/settings.json"),
            sidecar: home.join(".claude").join(sidecar),
            label: "claude",
        },
        Target {
            file: home.join(".codex/hooks.json"),
            sidecar: home.join(".codex").join(sidecar),
            label: "codex",
        },
    ]
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
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = target.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        let mut file = std::fs::File::create(&tmp).map_err(io(&tmp))?;
        file.write_all(text.as_bytes()).map_err(io(&tmp))?;
        if let Ok(meta) = std::fs::metadata(&target) {
            file.set_permissions(meta.permissions()).map_err(io(&tmp))?;
        }
        file.sync_all().map_err(io(&tmp))?;
        std::fs::rename(&tmp, &target).map_err(io(&target))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Removes the hook entries a sidecar recorded from both agents' hook files, then the sidecar.
/// The file is restored from the first install's backup when nothing else changed since (the
/// backup itself is kept); otherwise only the recorded blocks go. No sidecar: nothing to do, so
/// a second run is a no-op.
pub fn uninstall(home: &Path, sidecar: &str, out: &mut dyn Write) -> Result<(), InstallError> {
    let mut any = false;
    for target in targets(home, sidecar) {
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
        let _ = writeln!(out, "nothing to do: no {sidecar} under {}", home.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CHAT: &str = RETIRED_SIDECARS[0];
    const HISTORY: &str = RETIRED_SIDECARS[1];

    fn home() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("txc-home")
            .tempdir_in("/tmp")
            .unwrap()
    }

    fn read(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn block(command: &str) -> Value {
        json!({"matcher": "", "hooks": [{"type": "command", "command": command, "timeout": 10}]})
    }

    /// What an old install left: `file` with `ours` appended per event, a backup of the
    /// original, and the sidecar recording both.
    fn old_install(file: &Path, sidecar: &str, ours: &[(&str, &str)]) {
        let original = std::fs::read_to_string(file).unwrap_or_default();
        let backup = file.with_extension(format!("json.bak.{}", sidecar.len()));
        let backup_value = if original.is_empty() {
            Value::Null
        } else {
            std::fs::write(&backup, &original).unwrap();
            json!(backup.display().to_string())
        };
        let mut root: Map<String, Value> = if original.is_empty() {
            Map::new()
        } else {
            serde_json::from_str(&original).unwrap()
        };
        let hooks = root.entry("hooks").or_insert_with(|| json!({}));
        for (event, command) in ours {
            hooks
                .as_object_mut()
                .unwrap()
                .entry(*event)
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .unwrap()
                .push(block(command));
        }
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, serde_json::to_string_pretty(&root).unwrap()).unwrap();
        let added: Vec<Value> = ours
            .iter()
            .map(|(event, command)| json!({"event": event, "command": command}))
            .collect();
        std::fs::write(
            file.with_file_name(sidecar),
            json!({"version": 1, "backup": backup_value, "added": added}).to_string(),
        )
        .unwrap();
    }

    const CHAT_STOP: &str = "'/opt/txc' _capture stop || exit 1";
    const INGEST: &str = "'/opt/txc' _ingest || exit 1";

    #[test]
    fn restores_the_backup_byte_identical_and_keeps_it() {
        let home = home();
        let settings = home.path().join(".claude/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        let text = "{\n  \"theme\": \"dark\"\n}\n";
        std::fs::write(&settings, text).unwrap();
        old_install(&settings, CHAT, &[("Stop", CHAT_STOP)]);
        uninstall(home.path(), CHAT, &mut Vec::new()).unwrap();
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), text);
        assert!(!home.path().join(".claude").join(CHAT).exists());
        let backups = std::fs::read_dir(home.path().join(".claude"))
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("settings.json.bak.")
            })
            .count();
        assert_eq!(backups, 1, "the backup is kept");
    }

    #[test]
    fn keeps_foreign_hooks_changed_since_install() {
        let home = home();
        let settings = home.path().join(".claude/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, json!({"theme": "dark"}).to_string()).unwrap();
        old_install(&settings, CHAT, &[("Stop", CHAT_STOP)]);
        let mut changed = read(&settings);
        changed["hooks"]["Stop"]
            .as_array_mut()
            .unwrap()
            .insert(0, block("say done"));
        std::fs::write(&settings, changed.to_string()).unwrap();
        uninstall(home.path(), CHAT, &mut Vec::new()).unwrap();
        assert_eq!(
            read(&settings),
            json!({"theme": "dark", "hooks": {"Stop": [block("say done")]}})
        );
    }

    #[test]
    fn keeps_permissions_and_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let home = home();
        let claude = home.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        let real = home.path().join("dotfiles-settings.json");
        std::os::unix::fs::symlink(&real, claude.join("settings.json")).unwrap();
        old_install(&claude.join("settings.json"), CHAT, &[("Stop", CHAT_STOP)]);
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        uninstall(home.path(), CHAT, &mut Vec::new()).unwrap();
        let link = claude.join("settings.json").symlink_metadata().unwrap();
        assert!(link.file_type().is_symlink(), "the symlink survives");
        assert_eq!(
            std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(read(&real), json!({}));
    }

    #[test]
    fn without_a_sidecar_it_is_a_noop_so_a_second_run_is_too() {
        let home = home();
        let hooks = home.path().join(".codex/hooks.json");
        old_install(&hooks, HISTORY, &[("Stop", INGEST)]);
        let mut out = Vec::new();
        uninstall(home.path(), HISTORY, &mut out).unwrap();
        assert_eq!(read(&hooks), json!({}));
        let mut again = Vec::new();
        uninstall(home.path(), HISTORY, &mut again).unwrap();
        assert!(String::from_utf8(again).unwrap().contains("nothing to do"));
        assert_eq!(read(&hooks), json!({}));
    }

    #[test]
    fn one_sidecar_leaves_the_other_plugins_entries() {
        let home = home();
        let settings = home.path().join(".claude/settings.json");
        old_install(&settings, CHAT, &[("Stop", CHAT_STOP)]);
        old_install(
            &settings,
            HISTORY,
            &[("Stop", INGEST), ("SessionEnd", INGEST)],
        );
        uninstall(home.path(), CHAT, &mut Vec::new()).unwrap();
        assert_eq!(
            read(&settings)["hooks"],
            json!({"Stop": [block(INGEST)], "SessionEnd": [block(INGEST)]})
        );
        uninstall(home.path(), HISTORY, &mut Vec::new()).unwrap();
        assert_eq!(read(&settings), json!({}));
    }

    #[test]
    fn refuses_a_hook_file_that_is_not_an_object() {
        let home = home();
        let settings = home.path().join(".claude/settings.json");
        old_install(&settings, CHAT, &[("Stop", CHAT_STOP)]);
        std::fs::write(&settings, "[1]").unwrap();
        let error = uninstall(home.path(), CHAT, &mut Vec::new()).unwrap_err();
        assert!(matches!(error, InstallError::NotObject(_)), "{error}");
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), "[1]");
    }
}
