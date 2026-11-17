//! The SDK of a plugin written in Rust. Its crate keeps its `Plugin` with `register`, its
//! `Verb`s, `Updater` and engine, and gains a program whose whole `main` is [`main`]:
//!
//! ```ignore
//! fn main() -> std::process::ExitCode {
//!     tx_plugin::sdk::main(vec![Box::new(tx_kill::KillPlugin)])
//! }
//! ```
//!
//! A plugin program is not `tx`: where it needs a command line that starts with `tx`, it asks
//! [`tx_bin`].

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;

use tx_core::{Plugin, VerbIo};

use crate::args::{Global, parse_global};
use crate::setup::{Failure, Installation, process_environment};

/// The absolute path of the `tx` that ran this program: the only way a plugin runs tx again.
pub const TX_BIN_ENV: &str = "TX_BIN";
/// Set to `1` when `--tx-update` is to retire the plugin instead of updating it.
pub const RETIRE_ENV: &str = "TX_PLUGIN_RETIRE";
/// What `--tx-update` prints on a line of its own when an updater asks for the running agents
/// to be respawned.
pub const RESPAWN_LINE: &str = "tx-update: respawn-agents";

/// The entries of a plugin program that are not verbs.
const PROBE: &str = "--tx-probe";
const UPDATE: &str = "--tx-update";
const ENGINE: &str = "--tx-engine";
const HOOK: &str = "--tx-hook";

static TX_BIN: OnceLock<PathBuf> = OnceLock::new();

/// Names the `tx` binary for this process. `tx` calls it first thing with its own path, `txi`
/// with its sibling `tx`; a plugin program never does, and is told through `$TX_BIN`. The first
/// call wins.
pub fn set_tx_bin(path: impl Into<PathBuf>) {
    let _ = TX_BIN.set(path.into());
}

/// The `tx` binary: what [`set_tx_bin`] named, else `$TX_BIN`.
pub fn try_tx_bin() -> Option<PathBuf> {
    try_tx_bin_in(&process_environment)
}

pub(crate) fn try_tx_bin_in(environment: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    TX_BIN
        .get()
        .cloned()
        .or_else(|| environment(TX_BIN_ENV).map(PathBuf::from))
}

/// The `tx` binary, for a command line that starts with it (a key, a popup, a hook, a view,
/// another plugin's verb, a built-in). `std::env::current_exe()` is right only for the program's
/// own verbs.
///
/// # Panics
/// When there is none: the program was started neither by `tx` nor from one of its sessions.
/// It fails by name rather than guessing a `tx` on `PATH`.
pub fn tx_bin() -> PathBuf {
    try_tx_bin().expect("TX_BIN is not set: this program was not started by tx")
}

/// The `main` of a plugin program that hosts `hosted`. It reads `[global flags] <first>
/// [rest…]`, the global flags being `tx`'s own:
///
/// - `first` is a verb of a hosted plugin: the core is resolved as `tx` would (flags, then
///   variables, then defaults) and opened, and the verb runs with the process's standard
///   streams; its code is the exit code. So the program can be run directly for its own verbs,
///   without `tx` in front.
/// - `--tx-update`: the core is opened and the updaters the hosted plugins registered run; with
///   `TX_PLUGIN_RETIRE=1` their `retire` runs instead, whether or not the plugin is enabled.
///   [`RESPAWN_LINE`] is printed when one asks for a respawn.
/// - `--tx-engine`: the engine a hosted plugin registers is served on standard input and
///   output, one request and one response per line, until the input ends (see
///   [`crate::engine::serve`]). No core is opened.
/// - `--tx-hook <Event>`: the hook entry of the hosted engine, what the hooks it wires run when
///   its manifest names this program under `hook:` (see [`crate::engine::hook`]). It exits 0
///   for a hook that is not tx's and 1, never more, when it fails: a hook must not block the
///   agent.
/// - `--tx-probe`: exit 0 at once.
/// - anything else: `<program>: not a verb of this plugin: <first>`, exit 2.
///
/// A hosted plugin is registered only when it is active in this installation, with the settings
/// `config.yaml` gives it; every other active plugin's verb is in the core as a proxy. What the
/// program was asked wrongly is reported under its own name; what goes wrong in tx (the core,
/// `config.yaml`, a verb that fails) as `tx: <error>`, exit 1, or 2 for misuse, as `tx` does.
pub fn main(hosted: Vec<Box<dyn Plugin>>) -> ExitCode {
    let mut words = std::env::args_os();
    let program = words
        .next()
        .as_deref()
        .map(Path::new)
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let arguments: Result<Vec<String>, OsString> = words.map(OsString::into_string).collect();
    match arguments {
        Ok(arguments) => ExitCode::from(run(&program, &arguments, hosted)),
        Err(bad) => {
            eprintln!("{program}: argument is not UTF-8: {bad:?}");
            ExitCode::from(2)
        }
    }
}

/// [`main`] for the program called `program` with `arguments`: the exit code.
fn run(program: &str, arguments: &[String], hosted: Vec<Box<dyn Plugin>>) -> u8 {
    let (global, first) = match parse_global(arguments) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("{program}: {error}");
            return 2;
        }
    };
    let result = match arguments.get(first).map(String::as_str) {
        None => {
            eprintln!("{program}: missing verb");
            return 2;
        }
        Some(PROBE) => return 0,
        Some(ENGINE) => crate::engine::serve_hosted(program, &global, &hosted),
        Some(HOOK) => {
            let event = arguments.get(first + 1).cloned().unwrap_or_default();
            return hook_exit(crate::engine::hook_hosted(&global, hosted, event));
        }
        Some(UPDATE) => {
            let retire = std::env::var_os(RETIRE_ENV).is_some_and(|value| value == "1");
            update(&global, hosted, retire)
        }
        Some(name) => verb(program, &global, hosted, name, &arguments[first + 1..]),
    };
    match result {
        Ok(code) => code,
        Err(Failure::Usage(error)) => {
            eprintln!("tx: {error}");
            2
        }
        Err(error) => {
            eprintln!("tx: {error}");
            1
        }
    }
}

/// The exit code of the hook entry: 0, and 1 for a failure of any kind. Never 2, which the
/// engine's CLI may read as "block the agent".
fn hook_exit(result: Result<(), Failure>) -> u8 {
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("tx: {error}");
            1
        }
    }
}

/// Runs the verb `name` when it is a verb of an active hosted plugin: one the manifest of the
/// plugin declares, the activation gave it, and the plugin registers.
fn verb(
    program: &str,
    global: &Global,
    hosted: Vec<Box<dyn Plugin>>,
    name: &str,
    argv: &[String],
) -> Result<u8, Failure> {
    let installation = Installation::read(global)?;
    let ours = installation
        .activation
        .verbs
        .get(name)
        .is_some_and(|verb| hosted.iter().any(|plugin| plugin.id() == verb.plugin));
    let core = if ours {
        Some(installation.open_hosted(global, hosted)?)
    } else {
        None
    };
    let Some((core, verb)) = core.and_then(|core| {
        let verb = core.verb(name)?;
        Some((core, verb))
    }) else {
        eprintln!("{program}: not a verb of this plugin: {name}");
        return Ok(2);
    };
    let (mut stdin, mut stdout, mut stderr) = (
        std::io::stdin().lock(),
        std::io::stdout().lock(),
        std::io::stderr().lock(),
    );
    let mut io = VerbIo {
        stdin: &mut stdin,
        stdout: &mut stdout,
        stderr: &mut stderr,
    };
    let code = verb.run(&core, &mut io, argv).map_err(Failure::Verb)?;
    Ok(u8::try_from(code).unwrap_or(1))
}

/// `--tx-update`: every updater the hosted plugins registered, with no input; what each says
/// goes to standard output as it is. A failed one is named and the others still run. Retiring,
/// the plugins are registered whether or not they are active, and `retire` runs instead.
fn update(global: &Global, hosted: Vec<Box<dyn Plugin>>, retire: bool) -> Result<u8, Failure> {
    let installation = Installation::read(global)?;
    let core = if retire {
        installation.open_retiring(global, hosted)?
    } else {
        installation.open_hosted(global, hosted)?
    };
    let mut respawn = false;
    let mut failed = false;
    for (plugin, updater) in core.updaters() {
        let result = {
            let (mut stdin, mut stdout, mut stderr) = (
                std::io::empty(),
                std::io::stdout().lock(),
                std::io::stderr().lock(),
            );
            let mut io = VerbIo {
                stdin: &mut stdin,
                stdout: &mut stdout,
                stderr: &mut stderr,
            };
            if retire {
                updater.retire(&core, &mut io)
            } else {
                updater.update(&core, &mut io)
            }
        };
        match result {
            Ok(outcome) => respawn |= outcome.respawn_agents,
            Err(error) => {
                eprintln!("tx: update: {plugin}: {error}");
                failed = true;
            }
        }
    }
    if respawn {
        println!("{RESPAWN_LINE}");
    }
    Ok(u8::from(failed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use tx_core::updates::Outcome;
    use tx_core::{PluginError, Registry, TxCore, Updater, Verb, VerbError};

    /// What the hosted plugin of these tests did, in order.
    type Seen = Arc<Mutex<Vec<String>>>;

    /// A plugin `hosted` with the verb `hosted`, which notes its settings, its arguments and
    /// whether the core knows the verb `other`, then exits with the number it was given; and an
    /// updater that notes whether it updated or retired.
    struct Hosted(Seen);

    struct HostedVerb(Seen, serde_json::Value);

    impl Verb for HostedVerb {
        fn name(&self) -> &'static str {
            "hosted"
        }
        fn usage(&self) -> &'static str {
            "hosted CODE   exit with CODE"
        }
        fn run(
            &self,
            core: &TxCore,
            _: &mut VerbIo<'_>,
            argv: &[String],
        ) -> Result<i32, VerbError> {
            self.0.lock().unwrap().push(format!(
                "verb {argv:?} settings {} other {} hosted {}",
                self.1,
                core.verb("other").is_some(),
                core.verb("hosted").is_some(),
            ));
            match argv.first().map(String::as_str) {
                Some("fail") => Err(VerbError::Failed("the verb failed".to_owned())),
                Some(code) => Ok(code.parse().unwrap()),
                None => Ok(0),
            }
        }
    }

    struct HostedUpdater(Seen);

    impl Updater for HostedUpdater {
        fn update(&self, _: &TxCore, _: &mut VerbIo<'_>) -> Result<Outcome, VerbError> {
            self.0.lock().unwrap().push("updated".to_owned());
            Ok(Outcome::default())
        }
        fn retire(&self, _: &TxCore, _: &mut VerbIo<'_>) -> Result<Outcome, VerbError> {
            self.0.lock().unwrap().push("retired".to_owned());
            Ok(Outcome::default())
        }
    }

    impl Plugin for Hosted {
        fn id(&self) -> &'static str {
            "hosted"
        }
        fn register(&self, registry: &mut Registry) -> Result<(), PluginError> {
            registry.verb(Arc::new(HostedVerb(
                Arc::clone(&self.0),
                registry.settings().clone(),
            )));
            registry.updater(Arc::new(HostedUpdater(Arc::clone(&self.0))));
            Ok(())
        }
    }

    /// An installation in a temporary directory: the five directories, one plugin root, and
    /// what the hosted plugin saw.
    struct Sandbox {
        dir: tempfile::TempDir,
        seen: Seen,
    }

    impl Sandbox {
        fn new() -> Self {
            let dir = tempfile::Builder::new()
                .prefix("txsdk")
                .tempdir_in("/tmp")
                .unwrap();
            Self {
                dir,
                seen: Seen::default(),
            }
        }

        /// Installs a plugin `id` whose manifest declares the verb `verb`.
        fn install(&self, id: &str, verb: &str) {
            let directory = self.dir.path().join("root").join(id);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join("plugin.yaml"),
                format!(
                    "api: 1\nid: {id}\ndescription: The {id} plugin.\nverbs:\n  - name: {verb}\n    \
                     usage: \"{verb}\"\n    bin: {id}\n    args: [{verb}]\n"
                ),
            )
            .unwrap();
        }

        fn write_yaml(&self, text: &str) {
            let home = self.dir.path().join("home");
            std::fs::create_dir_all(&home).unwrap();
            std::fs::write(home.join("config.yaml"), text).unwrap();
        }

        /// The global flags of this installation, then `words`.
        fn arguments(&self, words: &[&str]) -> Vec<String> {
            let home = self.dir.path().join("home");
            let mut arguments = Vec::new();
            for flag in [
                "--config-dir",
                "--data-dir",
                "--state-dir",
                "--cache-dir",
                "--run-dir",
            ] {
                arguments.push(flag.to_owned());
                arguments.push(home.to_string_lossy().into_owned());
            }
            arguments.push("--plugin-dir".to_owned());
            arguments.push(self.dir.path().join("root").to_string_lossy().into_owned());
            arguments.extend(words.iter().map(|word| (*word).to_owned()));
            arguments
        }

        fn global(&self) -> Global {
            parse_global(&self.arguments(&[])).unwrap().0
        }

        fn hosted(&self) -> Vec<Box<dyn Plugin>> {
            vec![Box::new(Hosted(Arc::clone(&self.seen)))]
        }

        /// The program `tx-hosted` run with `words` after the global flags: its exit code.
        fn run(&self, words: &[&str]) -> u8 {
            run("tx-hosted", &self.arguments(words), self.hosted())
        }

        fn seen(&self) -> Vec<String> {
            std::mem::take(&mut *self.seen.lock().unwrap())
        }
    }

    #[test]
    fn a_verb_of_an_active_hosted_plugin_runs_on_the_resolved_core() {
        let sandbox = Sandbox::new();
        sandbox.install("hosted", "hosted");
        sandbox.install("other", "other");
        sandbox.write_yaml("plugins:\n  hosted:\n    settings: { greeting: hi }\n");
        assert_eq!(sandbox.run(&["hosted", "7", "x y"]), 7);
        assert_eq!(
            sandbox.seen(),
            [r#"verb ["7", "x y"] settings {"greeting":"hi"} other true hosted true"#],
            "its settings, its arguments, and the other plugin's verb as a proxy"
        );
        assert_eq!(sandbox.run(&["hosted"]), 0);
        assert_eq!(
            sandbox.run(&["hosted", "fail"]),
            1,
            "a failed verb is exit 1"
        );
        assert_eq!(
            sandbox.run(&["hosted", "300"]),
            1,
            "a code no exit can carry"
        );
    }

    #[test]
    fn anything_that_is_not_a_hosted_verb_is_refused_and_nothing_is_opened() {
        let sandbox = Sandbox::new();
        sandbox.install("hosted", "hosted");
        sandbox.install("other", "other");
        for words in [
            &["other"][..],
            &["ls"],
            // The plugin of these tests registers no engine to serve.
            &["--tx-engine"],
            &["--tx-event"],
            &["--nope"],
            &[],
        ] {
            assert_eq!(sandbox.run(words), 2, "{words:?}");
        }
        assert!(sandbox.seen().is_empty());
        assert!(
            !sandbox.dir.path().join("home").exists(),
            "misuse created the directories"
        );
        // A bad global flag is the program's own misuse too.
        assert_eq!(
            run("tx-hosted", &["--data-dir".to_owned()], sandbox.hosted()),
            2
        );
    }

    #[test]
    fn a_hosted_plugin_that_is_not_active_here_has_no_verb() {
        let sandbox = Sandbox::new();
        // Not installed at all.
        assert_eq!(sandbox.run(&["hosted", "0"]), 2);
        // Installed and switched off.
        sandbox.install("hosted", "hosted");
        sandbox.write_yaml("plugins:\n  hosted: { enabled: false }\n");
        assert_eq!(sandbox.run(&["hosted", "0"]), 2);
        // Its verb lost to a plugin earlier in plugin order.
        sandbox.write_yaml("");
        sandbox.install("earlier", "hosted");
        assert_eq!(sandbox.run(&["hosted", "0"]), 2);
        assert!(sandbox.seen().is_empty());
    }

    #[test]
    fn a_verb_the_manifest_declares_and_the_plugin_does_not_register_is_refused() {
        let sandbox = Sandbox::new();
        sandbox.install("hosted", "promised");
        assert_eq!(sandbox.run(&["promised"]), 2);
        assert_eq!(
            sandbox.run(&["hosted", "0"]),
            2,
            "nor one it does not declare"
        );
    }

    /// What is exit 2 for a verb (a `config.yaml` that does not read, say) is 1 for a hook.
    /// (The entry itself reads the session's variable and standard input, so it is driven
    /// through `engine::hook_in` in that module's tests, never from here.)
    #[test]
    fn the_hook_entry_exits_0_or_1_whatever_failed() {
        assert_eq!(hook_exit(Ok(())), 0);
        let misuse = Failure::Usage(crate::args::UsageError("config.yaml: bad".to_owned()));
        assert_eq!(hook_exit(Err(misuse)), 1);
        assert_eq!(hook_exit(Err(Failure::Environment("bad".to_owned()))), 1);
    }

    #[test]
    fn the_probe_exits_at_once() {
        let sandbox = Sandbox::new();
        assert_eq!(
            run("tx-hosted", &["--tx-probe".to_owned()], sandbox.hosted()),
            0
        );
        assert_eq!(sandbox.run(&["--tx-probe"]), 0);
        assert!(!sandbox.dir.path().join("home").exists());
    }

    #[test]
    fn a_broken_config_file_is_misuse() {
        let sandbox = Sandbox::new();
        sandbox.install("hosted", "hosted");
        sandbox.write_yaml("plugins: [1, 2\n");
        assert_eq!(sandbox.run(&["hosted", "0"]), 2);
        assert_eq!(sandbox.run(&["--tx-update"]), 2);
    }

    #[test]
    fn update_runs_the_updaters_of_the_active_hosted_plugins() {
        let sandbox = Sandbox::new();
        sandbox.install("hosted", "hosted");
        assert_eq!(
            update(&sandbox.global(), sandbox.hosted(), false).unwrap(),
            0
        );
        assert_eq!(sandbox.seen(), ["updated"]);
        sandbox.write_yaml("plugins:\n  hosted: { enabled: false }\n");
        assert_eq!(
            update(&sandbox.global(), sandbox.hosted(), false).unwrap(),
            0
        );
        assert!(
            sandbox.seen().is_empty(),
            "a disabled plugin is not updated"
        );
    }

    #[test]
    fn retiring_registers_the_plugin_whether_or_not_it_is_active() {
        let sandbox = Sandbox::new();
        // Not installed any more: it left the selection.
        assert_eq!(
            update(&sandbox.global(), sandbox.hosted(), true).unwrap(),
            0
        );
        assert_eq!(sandbox.seen(), ["retired"]);
        // Installed and disabled.
        sandbox.install("hosted", "hosted");
        sandbox.write_yaml("plugins:\n  hosted: { enabled: false }\n");
        assert_eq!(
            update(&sandbox.global(), sandbox.hosted(), true).unwrap(),
            0
        );
        assert_eq!(sandbox.seen(), ["retired"]);
    }

    #[test]
    fn tx_bin_fails_by_name_when_no_tx_is_known() {
        let none = |_: &str| None;
        assert_eq!(try_tx_bin_in(&none), None);
        let told = |key: &str| (key == TX_BIN_ENV).then(|| OsString::from("/opt/tx/bin/tx"));
        assert_eq!(try_tx_bin_in(&told), Some(PathBuf::from("/opt/tx/bin/tx")));
    }
}
