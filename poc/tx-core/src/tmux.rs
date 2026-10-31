//! tx's own tmux server. Every call is `tmux -S <socket> -f <conf> …`, so the user's default
//! server is never touched and `~/.tmux.conf` is never loaded. Sessions are addressed by exact
//! name (`=name`) when created and by their `@tx-id` option afterwards: `list_live` is the only
//! liveness signal and yields the `$N` / `%N` targets every other call uses.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::agent::AgentId;

/// The session option that links a tmux session to its record.
pub const TX_ID_OPTION: &str = "@tx-id";

#[derive(Debug, thiserror::Error)]
pub enum TmuxError {
    #[error("cannot run {binary}: {source}")]
    Spawn {
        binary: String,
        #[source]
        source: io::Error,
    },
    #[error("tmux {args} failed: {stderr}")]
    Failed { args: String, stderr: String },
    /// `list-sessions` failed for a reason other than "no server": the answer is unknown, not
    /// "nothing is live". Callers must write nothing.
    #[error("cannot inspect tmux server: {stderr}")]
    InspectionFailed { stderr: String },
}

/// Where a live session executes, as server-local targets. Never stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Live {
    /// `$N`
    pub session_target: String,
    /// `%N`, the active pane of the session's current window
    pub pane_target: String,
}

/// One `list-sessions` scan: every live session that carries a `@tx-id`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveTable {
    by_id: HashMap<AgentId, Live>,
}

const LIST_FORMAT: &str = "#{@tx-id}\t#{session_id}\t#{pane_id}";

impl LiveTable {
    /// Parses `list-sessions -F LIST_FORMAT` output. Sessions without a `@tx-id` are not ours.
    /// Two sessions with one id (a `new-session -t` clone) keep the first; the table is a map,
    /// never a guess.
    pub fn parse(out: &str) -> Result<Self, TmuxError> {
        let mut by_id = HashMap::new();
        for line in out.split('\n').filter(|line| !line.is_empty()) {
            let mut fields = line.split('\t');
            let (Some(id), Some(session), Some(pane), None) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                return Err(TmuxError::InspectionFailed {
                    stderr: format!("unexpected list-sessions line: {line:?}"),
                });
            };
            // No `@tx-id`, or one core never minted: not ours.
            let Ok(id) = id.parse::<AgentId>() else {
                continue;
            };
            by_id.entry(id).or_insert_with(|| Live {
                session_target: session.to_owned(),
                pane_target: pane.to_owned(),
            });
        }
        Ok(Self { by_id })
    }

    pub fn get(&self, id: &AgentId) -> Option<&Live> {
        self.by_id.get(id)
    }

    pub fn contains(&self, id: &AgentId) -> bool {
        self.by_id.contains_key(id)
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

/// The argument sequence that makes `send-keys -l` type `text` verbatim: the text minus its
/// trailing semicolons (if non-empty), then `\;` for each one. Empty text sends nothing.
pub(crate) fn literal_chunks(text: &str) -> Vec<&str> {
    let head = text.trim_end_matches(';');
    let mut chunks = Vec::with_capacity(1 + text.len() - head.len());
    if !head.is_empty() {
        chunks.push(head);
    }
    chunks.extend(std::iter::repeat_n(
        ESCAPED_SEMICOLON,
        text.len() - head.len(),
    ));
    chunks
}

const ESCAPED_SEMICOLON: &str = "\\;";

/// An argument that survives tmux's command-sequence parse verbatim: a bare trailing `;` ends
/// the command, and a trailing `\;` loses one backslash. So a trailing `;` preceded by `m`
/// backslashes is sent with `m + 1` (measured against tmux 3.6a; see the integration test).
pub(crate) fn escape_trailing_semicolon(arg: &str) -> String {
    let Some(head) = arg.strip_suffix(';') else {
        return arg.to_owned();
    };
    let stem = head.trim_end_matches('\\');
    let backslashes = head.len() - stem.len();
    format!("{stem}{};", "\\".repeat(backslashes + 1))
}

/// An exact session target: tmux otherwise prefix-matches names.
pub fn exact(name: &str) -> String {
    format!("={name}")
}

/// The exact session form for commands whose `-t` is pane-typed (`set-option`).
fn exact_for_pane_target(name: &str) -> String {
    format!("={name}:")
}

/// tmux's two ways of saying "there is no server behind this socket". Any other failure means
/// the socket is unusable and the answer is unknown.
pub(crate) fn is_no_server(stderr: &str) -> bool {
    let stderr = stderr.trim_end();
    stderr.starts_with("no server running on ")
        || (stderr.starts_with("error connecting to ")
            && stderr.ends_with("(No such file or directory)"))
}

/// The server quit while answering (`exit-empty` after the last session ended): it has no
/// sessions, same as no server.
pub(crate) fn server_quit(stderr: &str) -> bool {
    stderr.trim_end() == "server exited unexpectedly"
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tmux {
    binary: OsString,
    socket: PathBuf,
    conf: PathBuf,
}

impl Tmux {
    pub fn new(
        binary: impl Into<OsString>,
        socket: impl Into<PathBuf>,
        conf: impl Into<PathBuf>,
    ) -> Self {
        Self {
            binary: binary.into(),
            socket: socket.into(),
            conf: conf.into(),
        }
    }

    pub fn binary(&self) -> &OsStr {
        &self.binary
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn conf(&self) -> &Path {
        &self.conf
    }

    /// The full argument list of one call: `-S <socket> -f <conf>` then `args`. `-f` only
    /// matters to the call that starts the server, so every call carries it.
    pub fn argv(&self, args: &[OsString]) -> Vec<OsString> {
        let mut argv = Vec::with_capacity(args.len() + 4);
        argv.push("-S".into());
        argv.push(self.socket.as_os_str().to_owned());
        argv.push("-f".into());
        argv.push(self.conf.as_os_str().to_owned());
        argv.extend_from_slice(args);
        argv
    }

    fn output(&self, args: &[OsString]) -> Result<std::process::Output, TmuxError> {
        Command::new(&self.binary)
            .args(self.argv(args))
            .stdin(Stdio::null())
            .output()
            .map_err(|source| TmuxError::Spawn {
                binary: self.binary.to_string_lossy().into_owned(),
                source,
            })
    }

    /// A call that must succeed.
    fn run(&self, args: &[OsString]) -> Result<String, TmuxError> {
        let output = self.output(args)?;
        if !output.status.success() {
            return Err(TmuxError::Failed {
                args: args
                    .iter()
                    .map(|arg| arg.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// tmux parses its argv as a command sequence: an argument ending in `;` ends the command
    /// (so a `Process` arg `x;` followed by `kill-server` would run `kill-server` as a tmux
    /// command), and a trailing `\;` loses one backslash. Every user-controlled argument is
    /// escaped accordingly.
    pub(crate) fn new_session_args(
        name: &str,
        cwd: &Path,
        program: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> Vec<OsString> {
        let mut argv: Vec<OsString> = vec![
            "new-session".into(),
            "-d".into(),
            "-s".into(),
            name.into(),
            "-c".into(),
            cwd.as_os_str().to_owned(),
        ];
        for (key, value) in env {
            argv.push("-e".into());
            argv.push(escape_trailing_semicolon(&format!("{key}={value}")).into());
        }
        argv.push("--".into());
        argv.push(escape_trailing_semicolon(program).into());
        argv.extend(
            args.iter()
                .map(|arg| OsString::from(escape_trailing_semicolon(arg))),
        );
        argv
    }

    /// A detached session named exactly `name`, running `program args…` in `cwd` with `env`
    /// added to the server's environment. Starts the server (with `conf`) if needed.
    pub fn new_session(
        &self,
        name: &str,
        cwd: &Path,
        program: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<(), TmuxError> {
        self.run(&Self::new_session_args(name, cwd, program, args, env))
            .map(drop)
    }

    /// Links the session named exactly `name` to `id`. Session scope only.
    pub fn set_tx_id(&self, name: &str, id: &AgentId) -> Result<(), TmuxError> {
        self.run(&[
            "set-option".into(),
            "-t".into(),
            exact_for_pane_target(name).into(),
            TX_ID_OPTION.into(),
            id.as_str().into(),
        ])
        .map(drop)
    }

    /// `Ok(false)` when there is no such session (already gone); any other failure is an error.
    pub fn kill_session(&self, target: &str) -> Result<bool, TmuxError> {
        let args: [OsString; 3] = ["kill-session".into(), "-t".into(), target.into()];
        match self.run(&args) {
            Ok(_) => Ok(true),
            Err(TmuxError::Failed { stderr, .. })
                if stderr.starts_with("can't find session") || is_no_server(&stderr) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    /// The pid of the process `pane` runs (`#{pane_pid}`); `None` when the pane is already
    /// gone.
    pub fn pane_pid(&self, pane: &str) -> Result<Option<u32>, TmuxError> {
        let args: [OsString; 5] = [
            "display-message".into(),
            "-p".into(),
            "-t".into(),
            pane.into(),
            "#{pane_pid}".into(),
        ];
        match self.run(&args) {
            Ok(out) => out
                .trim()
                .parse()
                .map(Some)
                .map_err(|_| TmuxError::InspectionFailed {
                    stderr: format!("unexpected pane_pid: {out:?}"),
                }),
            Err(TmuxError::Failed { stderr, .. })
                if stderr.starts_with("can't find pane") || is_no_server(&stderr) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub fn rename_session(&self, target: &str, new_name: &str) -> Result<(), TmuxError> {
        self.run(&[
            "rename-session".into(),
            "-t".into(),
            target.into(),
            "--".into(),
            new_name.into(),
        ])
        .map(drop)
    }

    /// Types `text` verbatim into `pane` (`-l`: no key-name lookup, `--`: no option parsing).
    /// tmux still reads a trailing `;` as its command separator and drops it, and `\;` is
    /// unescaped only at the end of an argument; so the text is sent without its trailing
    /// semicolons, then one `\;` per semicolon stripped.
    pub fn send_keys_literal(&self, pane: &str, text: &str) -> Result<(), TmuxError> {
        for chunk in literal_chunks(text) {
            self.run(&[
                "send-keys".into(),
                "-t".into(),
                pane.into(),
                "-l".into(),
                "--".into(),
                chunk.into(),
            ])?;
        }
        Ok(())
    }

    pub fn send_enter(&self, pane: &str) -> Result<(), TmuxError> {
        self.send_key(pane, "Enter")
    }

    /// Presses one key, by tmux's name for it (`Enter`, `Escape`, `C-c`).
    pub fn send_key(&self, pane: &str, key: &str) -> Result<(), TmuxError> {
        self.run(&["send-keys".into(), "-t".into(), pane.into(), key.into()])
            .map(drop)
    }

    /// The only liveness signal. No server (or one quitting) → an empty table; any other failure →
    /// `InspectionFailed`, never an empty table.
    pub fn list_live(&self) -> Result<LiveTable, TmuxError> {
        let output = self.output(&["list-sessions".into(), "-F".into(), LIST_FORMAT.into()])?;
        if output.status.success() {
            return LiveTable::parse(&String::from_utf8_lossy(&output.stdout));
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if is_no_server(&stderr) || server_quit(&stderr) {
            return Ok(LiveTable::default());
        }
        Err(TmuxError::InspectionFailed { stderr })
    }

    pub fn is_live(&self, id: &AgentId) -> Result<Option<Live>, TmuxError> {
        Ok(self.list_live()?.by_id.remove(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(id: &str) -> AgentId {
        id.parse().unwrap()
    }
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::process::Command;

    fn tmux() -> Tmux {
        Tmux::new("tmux", "/s/tmux.sock", "/s/tmux.conf")
    }

    fn strs(argv: &[OsString]) -> Vec<&str> {
        argv.iter().map(|a| a.to_str().unwrap()).collect()
    }

    #[test]
    fn every_call_is_bound_to_the_socket_and_conf() {
        let argv = tmux().argv(&["list-sessions".into()]);
        assert_eq!(
            strs(&argv),
            ["-S", "/s/tmux.sock", "-f", "/s/tmux.conf", "list-sessions"]
        );
        assert_eq!(tmux().socket(), Path::new("/s/tmux.sock"));
        assert_eq!(tmux().conf(), Path::new("/s/tmux.conf"));
        assert_eq!(tmux().binary(), "tmux");
    }

    #[test]
    fn new_session_argv_shape() {
        let argv = Tmux::new_session_args(
            "-w",
            Path::new("/tmp/w"),
            "-prog",
            &["-a".into(), "x y".into()],
            &[("A".into(), "1".into()), ("B".into(), "two words".into())],
        );
        assert_eq!(
            strs(&argv),
            [
                "new-session",
                "-d",
                "-s",
                "-w",
                "-c",
                "/tmp/w",
                "-e",
                "A=1",
                "-e",
                "B=two words",
                "--",
                "-prog",
                "-a",
                "x y"
            ]
        );
    }

    #[test]
    fn literal_chunks_keep_trailing_semicolons() {
        assert_eq!(literal_chunks("a"), ["a"]);
        assert_eq!(literal_chunks("a;"), ["a", "\\;"]);
        assert_eq!(literal_chunks("a;;"), ["a", "\\;", "\\;"]);
        assert_eq!(literal_chunks(";"), ["\\;"]);
        assert_eq!(literal_chunks("a\\;"), ["a\\", "\\;"]);
        assert_eq!(literal_chunks("a; "), ["a; "]);
        assert_eq!(literal_chunks("a\\"), ["a\\"]);
        assert_eq!(literal_chunks(""), Vec::<&str>::new());
    }

    #[test]
    fn trailing_semicolons_in_command_args_are_escaped() {
        assert_eq!(escape_trailing_semicolon("a"), "a");
        assert_eq!(escape_trailing_semicolon("a;b"), "a;b");
        assert_eq!(escape_trailing_semicolon(r"end\"), r"end\");
        assert_eq!(escape_trailing_semicolon("a;"), r"a\;");
        assert_eq!(escape_trailing_semicolon(r"a\;"), r"a\\;");
        assert_eq!(escape_trailing_semicolon(";"), r"\;");
        assert_eq!(escape_trailing_semicolon(""), "");
        let argv = Tmux::new_session_args(
            "n",
            Path::new("/"),
            "prog;",
            &["x;".into(), "kill-server".into()],
            &[("V".into(), "a;".into())],
        );
        let args = strs(&argv);
        assert_eq!(args[7], r"V=a\;");
        assert_eq!(&args[9..], [r"prog\;", r"x\;", "kill-server"]);
    }

    #[test]
    fn targets_are_exact_names() {
        assert_eq!(exact("w"), "=w");
        assert_eq!(exact("-w"), "=-w");
        assert_eq!(exact("3"), "=3");
        assert_eq!(exact_for_pane_target("3"), "=3:");
    }

    /// `exit-empty` races the next client: killing the last session can leave a `list-sessions`
    /// connected to a server that quits before answering. No server, so no live sessions.
    #[test]
    fn list_live_reads_a_server_that_quits_while_answering_as_empty() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("tmux");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho 'server exited unexpectedly' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let tmux = Tmux::new(&fake, dir.path().join("s"), dir.path().join("c"));
        assert_eq!(tmux.list_live().unwrap(), LiveTable::default());
    }

    #[test]
    fn no_server_phrases() {
        assert!(is_no_server("no server running on /tmp/x.sock"));
        assert!(is_no_server(
            "error connecting to /tmp/x.sock (No such file or directory)"
        ));
        assert!(is_no_server("no server running on /tmp/x.sock\n"));
        assert!(!is_no_server(
            "error connecting to /tmp/x (Socket operation on non-socket)"
        ));
        assert!(!is_no_server(
            "error connecting to /tmp/x (Permission denied)"
        ));
        assert!(!is_no_server(
            "error connecting to /tmp/x (Connection refused)"
        ));
        assert!(!is_no_server(""));
        assert!(!is_no_server("usage: list-sessions [-F format]"));
    }

    #[test]
    fn live_table_parses_rows() {
        let table = LiveTable::parse("id-1\t$0\t%0\nid-2\t$3\t%7\n").unwrap();
        assert_eq!(table.len(), 2);
        let live = table.get(&sid("id-2")).unwrap();
        assert_eq!(live.session_target, "$3");
        assert_eq!(live.pane_target, "%7");
        assert!(table.get(&sid("id-9")).is_none());
        assert!(LiveTable::parse("").unwrap().is_empty());
        assert!(LiveTable::parse("\n\n").unwrap().is_empty());
    }

    #[test]
    fn live_table_ignores_sessions_without_tx_id() {
        let table = LiveTable::parse("\t$0\t%0\nid-2\t$3\t%7\n").unwrap();
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn live_table_keeps_the_first_of_duplicate_ids() {
        let table = LiveTable::parse("dup\t$0\t%0\ndup\t$1\t%1\n").unwrap();
        assert_eq!(table.len(), 1);
        assert_eq!(table.get(&sid("dup")).unwrap().session_target, "$0");
    }

    #[test]
    fn live_table_skips_foreign_ids_and_rejects_short_rows() {
        let table = LiveTable::parse("a b\t$0\t%0\nnot/ours\t$1\t%1\nok\t$2\t%2\n").unwrap();
        assert_eq!(table.len(), 1);
        assert_eq!(table.get(&sid("ok")).unwrap().pane_target, "%2");
        let error = LiveTable::parse("id-1\t$0\n").unwrap_err();
        assert!(
            matches!(error, TmuxError::InspectionFailed { .. }),
            "{error:?}"
        );
        let error = LiveTable::parse("id-1\t$0\t%0\textra\n").unwrap_err();
        assert!(
            matches!(error, TmuxError::InspectionFailed { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn error_text() {
        let error = TmuxError::Failed {
            args: "kill-session -t =x".into(),
            stderr: "can't find session: x".into(),
        };
        assert_eq!(
            error.to_string(),
            "tmux kill-session -t =x failed: can't find session: x"
        );
        let error = TmuxError::InspectionFailed {
            stderr: "error connecting to /d (Socket operation on non-socket)".into(),
        };
        assert!(
            error
                .to_string()
                .starts_with("cannot inspect tmux server: ")
        );
    }

    #[test]
    fn missing_binary_is_a_spawn_error() {
        let tmux = Tmux::new("/nonexistent/tmux", "/tmp/x.sock", "/dev/null");
        let error = tmux.list_live().unwrap_err();
        assert!(matches!(error, TmuxError::Spawn { .. }), "{error:?}");
    }

    // ----- against a real, private server ------------------------------------------------

    /// A private server under /tmp (short socket path); killed on drop.
    struct Server {
        dir: tempfile::TempDir,
        tmux: Tmux,
    }

    impl Server {
        fn start() -> Option<Self> {
            Command::new("tmux").arg("-V").output().ok()?;
            let dir = tempfile::Builder::new()
                .prefix("txc")
                .tempdir_in("/tmp")
                .ok()?;
            let conf = dir.path().join("tmux.conf");
            std::fs::write(&conf, "set -g @from-conf yes\n").ok()?;
            let tmux = Tmux::new("tmux", dir.path().join("tmux.sock"), conf);
            Some(Self { dir, tmux })
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = Command::new("tmux")
                .args(self.tmux.argv(&["kill-server".into()]))
                .output();
        }
    }

    macro_rules! server {
        () => {
            match Server::start() {
                Some(server) => server,
                None => {
                    eprintln!("tmux not available; skipping");
                    return;
                }
            }
        };
    }

    #[test]
    fn no_server_means_no_live_sessions() {
        let server = server!();
        assert!(server.tmux.list_live().unwrap().is_empty());
        assert!(server.tmux.is_live(&sid("x")).unwrap().is_none());
    }

    #[test]
    fn session_lifecycle_against_private_server() {
        let server = server!();
        let tmux = &server.tmux;
        let out = server.dir.path().join("env.txt");
        let id = sid("b0000000-0000-4000-8000-000000000001");
        tmux.new_session(
            "-w",
            server.dir.path(),
            "sh",
            &[
                "-c".into(),
                format!("echo \"$TX_A|$TX_B\" > {}; sleep 30", out.display()),
            ],
            &[
                ("TX_A".into(), "1".into()),
                ("TX_B".into(), "two words".into()),
            ],
        )
        .unwrap();
        tmux.set_tx_id("-w", &id).unwrap();
        let live = tmux.is_live(&id).unwrap().expect("live by @tx-id");
        assert!(live.session_target.starts_with('$'), "{live:?}");
        assert!(live.pane_target.starts_with('%'), "{live:?}");
        assert_eq!(tmux.list_live().unwrap().len(), 1);
        for _ in 0..50 {
            if out.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert_eq!(std::fs::read_to_string(&out).unwrap().trim(), "1|two words");

        tmux.rename_session(&live.session_target, "-renamed")
            .unwrap();
        assert!(tmux.rename_session("=-w", "x").is_err());
        assert_eq!(tmux.is_live(&id).unwrap().unwrap(), live);

        assert!(tmux.kill_session(&live.session_target).unwrap());
        assert!(!tmux.kill_session(&live.session_target).unwrap());
        assert!(!tmux.kill_session("=nope").unwrap());
        assert!(tmux.is_live(&id).unwrap().is_none());
    }

    #[test]
    fn command_args_and_env_with_trailing_semicolons_arrive_verbatim() {
        let server = server!();
        let tmux = &server.tmux;
        let out = server.dir.path().join("args.txt");
        tmux.new_session(
            "semi",
            server.dir.path(),
            "sh",
            &[
                "-c".into(),
                format!("printf '%s|' \"$@\" \"$V\" > {}; sleep 30", out.display()),
                "sh".into(),
                "a;".into(),
                "a\\;".into(),
                ";".into(),
                "end\\".into(),
                "kill-server".into(),
            ],
            &[("V".into(), "v;".into())],
        )
        .unwrap();
        for _ in 0..50 {
            if out.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "a;|a\\;|;|end\\|kill-server|v;|"
        );
        assert_eq!(
            tmux.list_live().unwrap().len(),
            0,
            "server still up, no @tx-id yet"
        );
    }

    #[test]
    fn conf_is_loaded_and_home_conf_is_not() {
        let server = server!();
        let tmux = &server.tmux;
        tmux.new_session("a", Path::new("/"), "sleep", &["30".into()], &[])
            .unwrap();
        let output = Command::new("tmux")
            .args(tmux.argv(&["show-options".into(), "-gv".into(), "@from-conf".into()]))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "yes");
    }

    #[test]
    fn duplicate_name_is_a_failed_call() {
        let server = server!();
        let tmux = &server.tmux;
        tmux.new_session("a", Path::new("/"), "sleep", &["30".into()], &[])
            .unwrap();
        let error = tmux
            .new_session("a", Path::new("/"), "sleep", &["30".into()], &[])
            .unwrap_err();
        assert!(
            matches!(&error, TmuxError::Failed { stderr, .. } if stderr.contains("duplicate session")),
            "{error:?}"
        );
    }

    #[test]
    fn send_keys_reach_the_pane() {
        let server = server!();
        let tmux = &server.tmux;
        let out = server.dir.path().join("typed.txt");
        let id = sid("k");
        tmux.new_session(
            "k",
            Path::new("/"),
            "sh",
            &[
                "-c".into(),
                format!("head -n 1 > {}; sleep 30", out.display()),
            ],
            &[],
        )
        .unwrap();
        tmux.set_tx_id("k", &id).unwrap();
        let live = tmux.is_live(&id).unwrap().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        tmux.send_keys_literal(&live.pane_target, "-n ; Enter wörld")
            .unwrap();
        tmux.send_keys_literal(&live.pane_target, "|a;").unwrap();
        tmux.send_keys_literal(&live.pane_target, "|b\\;;").unwrap();
        tmux.send_keys_literal(&live.pane_target, "").unwrap();
        tmux.send_enter(&live.pane_target).unwrap();
        for _ in 0..50 {
            if out.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "-n ; Enter wörld|a;|b\\;;\n"
        );
        assert!(tmux.send_enter("%999").is_err());
    }

    #[test]
    fn unusable_socket_is_an_inspection_failure() {
        let server = server!();
        let dir = server.dir.path().join("sock-dir");
        std::fs::create_dir(&dir).unwrap();
        let tmux = Tmux::new("tmux", &dir, server.tmux.conf());
        let error = tmux.list_live().unwrap_err();
        assert!(
            matches!(&error, TmuxError::InspectionFailed { stderr } if stderr.contains("non-socket")),
            "{error:?}"
        );
        let error = tmux.is_live(&sid("x")).unwrap_err();
        assert!(matches!(error, TmuxError::InspectionFailed { .. }));
    }

    #[test]
    fn tx_id_is_session_scoped() {
        let server = server!();
        let tmux = &server.tmux;
        tmux.new_session("a", Path::new("/"), "sleep", &["30".into()], &[])
            .unwrap();
        tmux.new_session("b", Path::new("/"), "sleep", &["30".into()], &[])
            .unwrap();
        tmux.set_tx_id("a", &sid("id-a")).unwrap();
        let table = tmux.list_live().unwrap();
        assert_eq!(table.len(), 1);
        assert!(table.get(&sid("id-a")).is_some());
        let output = Command::new("tmux")
            .args(tmux.argv(&["show-options".into(), "-gv".into(), "@tx-id".into()]))
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(tmux.set_tx_id("nope", &sid("z")).is_err());
        let _ = PathBuf::new();
    }
}
