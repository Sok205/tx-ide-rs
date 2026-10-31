//! Re-executes `txc` detached: a new session (`setsid`), stdio on /dev/null, so the child
//! survives the caller's pane being killed and never sits on the caller's latency path.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// Spawns `exe <arguments> args…` with `env` added to the inherited environment and returns
/// without waiting. `arguments` are the core's `cli_arguments()`, so the child opens this core.
pub fn detach(
    exe: &Path,
    arguments: &[String],
    args: &[&str],
    env: &[(&str, &str)],
) -> std::io::Result<()> {
    let mut command = Command::new(exe);
    command
        .args(arguments)
        .args(args)
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: the closure runs in the forked child before exec and only calls setsid(2), which
    // is async-signal-safe and touches no parent memory.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_runs_detached_with_the_core_arguments_and_env() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-txc");
        let out = dir.path().join("out");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s|%s|%s' \"$*\" \"$MARK\" \"$(ps -o pgid= -p $$ | tr -d ' ')==$$\" > '{}'\n",
                out.display()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        detach(
            &script,
            &["--data-dir".to_owned(), "/st".to_owned()],
            &["_x", "op1"],
            &[("MARK", "m")],
        )
        .unwrap();
        let mut text = String::new();
        for _ in 0..100 {
            text = std::fs::read_to_string(&out).unwrap_or_default();
            if !text.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let mut parts = text.split('|');
        assert_eq!(parts.next(), Some("--data-dir /st _x op1"));
        assert_eq!(parts.next(), Some("m"));
        let sid = parts.next().unwrap();
        let (session, pid) = sid.split_once("==").unwrap();
        assert_eq!(
            session, pid,
            "the child leads its own session and process group"
        );
    }
}
