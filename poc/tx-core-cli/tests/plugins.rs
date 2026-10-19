//! plugins.yaml drives what txc links; plugin verbs dispatch after the built-ins.

use std::path::PathBuf;
use std::process::{Command, Output};

struct Cli {
    dir: tempfile::TempDir,
}

impl Cli {
    fn new() -> Self {
        Self {
            dir: tempfile::Builder::new()
                .prefix("txc")
                .tempdir_in("/tmp")
                .unwrap(),
        }
    }
    fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }
    fn write_yaml(&self, text: &str) {
        std::fs::create_dir_all(self.state()).unwrap();
        std::fs::write(self.state().join("plugins.yaml"), text).unwrap();
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_txc"))
            .arg("--state-dir")
            .arg(self.state())
            .args(args)
            .env_remove("TMUX")
            .env_remove("TX_SESSION_ID")
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "txc {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
    fn fails(&self, args: &[&str], code: i32) -> String {
        let output = self.run(args);
        assert_eq!(
            output.status.code(),
            Some(code),
            "txc {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    }
}

#[test]
fn without_a_yaml_every_plugin_is_active() {
    let cli = Cli::new();
    assert_eq!(
        cli.ok(&["plugins"]),
        "engine-claude\tactive\nengine-codex\tactive\nhooks\tactive\nviews\tactive\n"
    );
    assert_eq!(cli.ok(&["engines"]), "claude\ncodex\n");
}

#[test]
fn a_disabled_engine_disappears() {
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  engine-codex: { enabled: false }\n");
    assert_eq!(
        cli.ok(&["plugins"]),
        "engine-claude\tactive\nengine-codex\tdisabled\nhooks\tactive\nviews\tactive\n"
    );
    assert_eq!(cli.ok(&["engines"]), "claude\n");
}

#[test]
fn unknown_id_is_a_usage_error_naming_the_key() {
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  engine-cloud: {}\n");
    let stderr = cli.fails(&["plugins"], 2);
    assert!(stderr.contains("engine-cloud"), "{stderr}");
}

#[test]
fn malformed_yaml_is_a_usage_error() {
    let cli = Cli::new();
    cli.write_yaml("plugins: [\n");
    let stderr = cli.fails(&["engines"], 2);
    assert!(stderr.contains("plugins.yaml"), "{stderr}");
}

#[test]
fn plugins_flag_overrides_the_default_file() {
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  engine-codex: { enabled: false }\n");
    let other = cli.dir.path().join("other.yaml");
    std::fs::write(&other, "plugins:\n  engine-claude: { enabled: false }\n").unwrap();
    assert_eq!(
        cli.ok(&["--plugins", other.to_str().unwrap(), "engines"]),
        "codex\n"
    );
}

#[test]
fn unknown_verb_still_exits_2() {
    let cli = Cli::new();
    let stderr = cli.fails(&["frobnicate"], 2);
    assert!(stderr.contains("unknown verb \"frobnicate\""), "{stderr}");
}

#[test]
fn help_lists_views_verbs() {
    let cli = Cli::new();
    let help = cli.ok(&["help"]);
    for verb in ["pick", "kill", "whoami"] {
        assert!(help.contains(verb), "{verb} missing from help:\n{help}");
    }
}

#[test]
fn disabled_views_hides_verbs() {
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  views: { enabled: false }\n");
    cli.fails(&["kill", "x"], 2);
    assert_eq!(
        cli.ok(&["plugins"]),
        "engine-claude\tactive\nengine-codex\tactive\nhooks\tactive\nviews\tdisabled\n"
    );
}
