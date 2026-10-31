//! `poc/install --dry-run` against a throwaway `$HOME`: the hook step acts on what matches, not
//! on formatting. A compact (minified) settings file with no old entries is reported as not
//! written; one with old entries lists each entry it would remove and keeps the foreign ones.
//! Dry runs execute nothing (no cargo, no tmux server), so this needs only `jq` on `PATH`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../install")
}

fn have(tool: &str) -> bool {
    Command::new("which")
        .arg(tool)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("txc-install")
        .tempdir_in("/tmp")
        .unwrap()
}

/// The script's output, with the ANSI colouring stripped.
fn dry_run(home: &Path) -> String {
    let output = Command::new("bash")
        .arg(script())
        .arg("--dry-run")
        .env("HOME", home)
        .env("CARGO_INSTALL_ROOT", home.join("cargo"))
        .env_remove("TX_IDE_HOME")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut plain = String::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find('\u{1b}') {
        plain.push_str(&rest[..start]);
        rest = &rest[start..];
        match rest.find('m') {
            Some(end) => rest = &rest[end + 1..],
            None => rest = "",
        }
    }
    plain.push_str(rest);
    plain
}

const FOREIGN: &str = "$HOME/.claude/hooks/require-worktree/check.sh";
const OURS_WORK: &str = "/Users/someone/.tx-ide/hooks/claude/work.sh";
const OURS_POST: &str = "/Users/someone/.tx-ide/hooks/claude/post.sh";
const OURS_STATUSLINE: &str = "bash /Users/someone/.tx-ide/statusline.sh";

#[test]
fn a_compact_file_without_old_entries_is_not_written() {
    if !have("jq") || !have("cargo") || !have("tmux") {
        eprintln!("jq, cargo or tmux not available; skipping");
        return;
    }
    let home = home();
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    std::fs::create_dir_all(home.path().join(".codex")).unwrap();
    let settings = home.path().join(".claude/settings.json");
    let compact = format!(
        "{{\"theme\":\"dark\",\"hooks\":{{\"PreToolUse\":[{{\"matcher\":\"\",\"hooks\":[{{\"type\":\"command\",\"command\":\"{FOREIGN}\"}}]}}]}}}}"
    );
    std::fs::write(&settings, &compact).unwrap();
    std::fs::write(home.path().join(".codex/hooks.json"), "{\"hooks\":{}}").unwrap();

    let output = dry_run(home.path());

    for file in [".claude/settings.json", ".codex/hooks.json"] {
        // The step's own line, not the header that names both files.
        let line = output
            .lines()
            .find(|line| line.trim_start().starts_with('→') && line.contains(file))
            .unwrap_or_else(|| panic!("a step line about {file} in:\n{output}"));
        assert!(
            line.contains("no old entries — not written"),
            "{file} untouched: {line}"
        );
    }
    assert!(!output.contains("would remove"), "{output}");
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), compact);
    let backups = std::fs::read_dir(home.path().join(".claude"))
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".bak.")
        })
        .count();
    assert_eq!(backups, 0, "a dry run writes no backup either");
}

#[test]
fn a_compact_file_with_old_entries_lists_what_goes_and_keeps_the_rest() {
    if !have("jq") || !have("cargo") || !have("tmux") {
        eprintln!("jq, cargo or tmux not available; skipping");
        return;
    }
    let home = home();
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    let settings = home.path().join(".claude/settings.json");
    let compact = format!(
        "{{\"statusLine\":{{\"type\":\"command\",\"command\":\"{OURS_STATUSLINE}\"}},\
         \"hooks\":{{\"PreToolUse\":[{{\"matcher\":\"\",\"hooks\":[\
         {{\"type\":\"command\",\"command\":\"{FOREIGN}\"}},\
         {{\"type\":\"command\",\"command\":\"{OURS_WORK}\"}}]}}],\
         \"Stop\":[{{\"hooks\":[{{\"type\":\"command\",\"command\":\"{OURS_POST}\"}}]}}]}}}}"
    );
    std::fs::write(&settings, &compact).unwrap();

    let output = dry_run(home.path());

    assert!(output.contains("would remove these entries"), "{output}");
    assert!(output.contains(&format!("- PreToolUse: {OURS_WORK}")), "{output}");
    assert!(output.contains(&format!("- Stop: {OURS_POST}")), "{output}");
    assert!(output.contains(&format!("- statusLine: {OURS_STATUSLINE}")), "{output}");
    assert!(!output.contains(&format!("- PreToolUse: {FOREIGN}")), "{output}");
    // The diff it prints keeps the foreign entry and drops the statusLine.
    assert!(output.contains(&format!("+            \"command\": \"{FOREIGN}\"")), "{output}");
    assert!(!output.contains("+  \"statusLine\""), "{output}");
    // A dry run changes nothing.
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), compact);
}
