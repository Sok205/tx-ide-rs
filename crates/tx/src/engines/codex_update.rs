//! Run the official standalone Codex installer outside Codex's interactive session
//! (codex_update.py).
//!
//! The installer verifies releases and switches `current`; tx only schedules it and keeps its log.
//! The log's `flock` spans the detached process, and its mtime limits attempts to four-hour
//! intervals. The detached child is the current `tx` binary re-executed as `tx` [`UPDATE_VERB`]
//! (the reference re-ran this module under the Python interpreter); the CLI routes that verb to
//! [`run_update`].

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::session::py_repr_str;

pub const INSTALLER_URL: &str = "https://chatgpt.com/codex/install.sh";
pub const CHECK_INTERVAL_SECONDS: u64 = 4 * 60 * 60;
pub const UPDATE_CHECK_CONFIGURATION: &str = "check_for_update_on_startup=false";
const UPDATE_CHECK_KEY: &str = "check_for_update_on_startup";
/// The hidden verb the detached child runs (`tx _codex-update`).
pub const UPDATE_VERB: &str = "_codex-update";
pub const LOG_FILE_NAME: &str = "update.log";

const CURL_TIMEOUT_SECONDS: u64 = 60;
const INSTALLER_TIMEOUT_SECONDS: u64 = 600;

/// Scheduling the update check failed; the text is Python's `OSError` shape
/// (`[Errno 17] File exists: '<path>'`), which the spawn warning line prints.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ScheduleError(String);

/// `str(OSError)`: `[Errno N] <strerror>: '<filename>'`.
pub(crate) fn py_os_error(error: &io::Error, filename: Option<&Path>) -> String {
    let Some(code) = error.raw_os_error() else {
        return error.to_string();
    };
    let text = io::Error::from_raw_os_error(code).to_string();
    let strerror = text
        .strip_suffix(&format!(" (os error {code})"))
        .unwrap_or(&text);
    match filename {
        Some(path) => format!(
            "[Errno {code}] {strerror}: {}",
            py_repr_str(&path.to_string_lossy())
        ),
        None => format!("[Errno {code}] {strerror}"),
    }
}

/// The installer's visible symlink through `current` (never a pinned release), or `None`.
/// `launch_path` is the launch env's `PATH`, `process_path` the parent's.
pub fn standalone_executable(
    binary: &str,
    launch_path: Option<&str>,
    process_path: Option<&str>,
    codex_home: &Path,
) -> Option<PathBuf> {
    let executable = absolute(&which(binary, launch_path.or(process_path))?);
    let current = absolute(codex_home).join("packages/standalone/current");
    if !executable.is_symlink() || !current.is_symlink() {
        return None;
    }
    let link = std::fs::read_link(&executable).ok()?;
    let target = normpath(&executable.parent()?.join(link));
    if target != current.join("bin/codex") && target != current.join("codex") {
        return None;
    }
    let releases = super::adapter::realpath(&current.parent()?.join("releases"));
    super::adapter::realpath(&executable)
        .starts_with(releases)
        .then_some(executable)
}

/// `shutil.which(cmd, path=path)` on POSIX.
fn which(binary: &str, path: Option<&str>) -> Option<PathBuf> {
    let candidates: Vec<PathBuf> = if binary.contains('/') {
        vec![PathBuf::from(binary)]
    } else {
        let path = path.unwrap_or(DEFAULT_SEARCH_PATH);
        if path.is_empty() {
            return None;
        }
        let mut seen: Vec<&str> = Vec::new();
        path.split(':')
            .filter(|dir| {
                let fresh = !seen.contains(dir);
                seen.push(dir);
                fresh
            })
            .map(|dir| {
                if dir.is_empty() {
                    PathBuf::from(binary)
                } else {
                    Path::new(dir).join(binary)
                }
            })
            .collect()
    };
    candidates
        .into_iter()
        .find(|candidate| is_executable(candidate))
}

#[cfg(target_os = "macos")]
const DEFAULT_SEARCH_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
#[cfg(not(target_os = "macos"))]
const DEFAULT_SEARCH_PATH: &str = "/bin:/usr/bin";

fn is_executable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    if std::fs::metadata(path).map_or(true, |m| m.is_dir()) {
        return false;
    }
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c_path` is a valid NUL-terminated string that outlives the call.
    unsafe { libc::access(c_path.as_ptr(), libc::X_OK) == 0 }
}

/// `Path.absolute()`: relative paths joined onto the process cwd, no normalisation.
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// `os.path.normpath` for an absolute path: `.` dropped, `..` popped lexically.
fn normpath(path: &Path) -> PathBuf {
    let mut normal = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::Normal(name) => normal.push(name),
            Component::ParentDir => {
                normal.pop();
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    normal
}

/// Make centrally-managed update behaviour authoritative: drop any
/// `check_for_update_on_startup` override and insert ours right after the binary.
pub fn disable_startup_update_check(tokens: &[String]) -> Vec<String> {
    let Some((binary, rest)) = tokens.split_first() else {
        return Vec::new();
    };
    let is_update_key = |value: &str| value.split('=').next() == Some(UPDATE_CHECK_KEY);
    let mut filtered = vec![
        binary.clone(),
        "-c".to_owned(),
        UPDATE_CHECK_CONFIGURATION.to_owned(),
    ];
    let mut index = 0;
    while index < rest.len() {
        let token = rest[index].as_str();
        if (token == "-c" || token == "--config")
            && rest
                .get(index + 1)
                .is_some_and(|value| is_update_key(value))
        {
            index += 2;
            continue;
        }
        if token.strip_prefix("--config=").is_some_and(is_update_key) {
            index += 1;
            continue;
        }
        filtered.push(token.to_owned());
        index += 1;
    }
    filtered
}

/// Everything [`schedule_update`] needs to launch the detached child.
#[derive(Clone, Copy, Debug)]
pub struct UpdateRequest<'a> {
    pub executable: &'a Path,
    pub codex_home: &'a Path,
    /// The session's launch env (layered over the inherited process env for the child).
    pub environment: &'a [(String, String)],
    /// `$TX_IDE_HOME/codex-update`.
    pub state_directory: &'a Path,
    /// The running `tx` binary, re-executed as `tx _codex-update`.
    pub tx_executable: &'a Path,
}

/// Claim a due check without waiting; the child inherits the locked log as stdout/stderr.
pub fn schedule_update(request: &UpdateRequest<'_>) -> Result<(), ScheduleError> {
    let state_directory = request.state_directory;
    std::fs::create_dir_all(state_directory)
        .map_err(|error| ScheduleError(py_os_error(&error, Some(state_directory))))?;
    let log_path = state_directory.join(LOG_FILE_NAME);
    let mut log = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(&log_path)
        .map_err(|error| ScheduleError(py_os_error(&error, Some(&log_path))))?;
    let os_error = |error: io::Error| ScheduleError(py_os_error(&error, None));
    match try_lock(&log) {
        Ok(true) => {}
        Ok(false) => return Ok(()),
        Err(error) => return Err(os_error(error)),
    }
    let last_check = log.metadata().map_err(os_error)?;
    if last_check.len() > 0 && !is_due(last_check.modified().map_err(os_error)?) {
        return Ok(());
    }
    log.set_len(0).map_err(os_error)?;
    let header = format!(
        "[{}] Checking for Codex updates\n",
        utc_timestamp(SystemTime::now())
    );
    log.write_all(header.as_bytes()).map_err(os_error)?;
    let child_log = |log: &File| log.try_clone().map_err(os_error);
    let mut command = Command::new(request.tx_executable);
    command
        .arg(UPDATE_VERB)
        .stdin(Stdio::null())
        .stdout(child_log(&log)?)
        .stderr(child_log(&log)?)
        .envs(request.environment.iter().map(|(k, v)| (k, v)))
        .env("CODEX_HOME", absolute(request.codex_home))
        .env(
            "CODEX_INSTALL_DIR",
            request.executable.parent().unwrap_or(Path::new("")),
        )
        .env("CODEX_NON_INTERACTIVE", "1");
    // SAFETY: `setsid` is async-signal-safe and touches no parent state (start_new_session).
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    if let Err(error) = command.spawn() {
        let message = py_os_error(&error, Some(request.tx_executable));
        // Best effort: the log is only diagnostics once the spawn itself failed.
        let _ = writeln!(log, "Update failed: {message}");
        return Err(ScheduleError(message));
    }
    Ok(())
}

/// `flock(LOCK_EX | LOCK_NB)`: `Ok(false)` when another process holds it.
fn try_lock(file: &File) -> io::Result<bool> {
    // SAFETY: the fd is owned by `file`, which outlives the call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::WouldBlock {
        Ok(false)
    } else {
        Err(error)
    }
}

/// Whether a log last written at `modified` is older than the check interval (`time.time() -
/// st_mtime >= interval`; a future mtime is not due).
fn is_due(modified: SystemTime) -> bool {
    SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age.as_secs_f64() >= CHECK_INTERVAL_SECONDS as f64)
}

/// `time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())`.
fn utc_timestamp(at: SystemTime) -> String {
    let seconds = at
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Howard Hinnant's days-since-epoch → (year, month, day).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The detached child (`tx _codex-update`): download the installer, then run it. Prints the
/// outcome line (stdout is the locked log) and returns the exit code.
pub fn run_update() -> i32 {
    match download_and_install() {
        Ok(()) => {
            println!("Update succeeded");
            0
        }
        Err(message) => {
            println!("Update failed: {message}");
            1
        }
    }
}

fn download_and_install() -> Result<(), String> {
    let directory = tempfile::Builder::new()
        .prefix("tx-codex-update-")
        .tempdir()
        .map_err(|error| py_os_error(&error, None))?;
    let installer = directory.path().join("install.sh");
    // Download separately: a curl | sh pipeline can hide download failures.
    let installer_arg = installer.to_string_lossy().into_owned();
    run_checked(
        &[
            "curl",
            "-fsSL",
            "--connect-timeout",
            "10",
            "--max-time",
            "45",
            INSTALLER_URL,
            "-o",
            &installer_arg,
        ],
        CURL_TIMEOUT_SECONDS,
    )?;
    run_checked(&["sh", &installer_arg], INSTALLER_TIMEOUT_SECONDS)
}

/// `subprocess.run(argv, check=True, timeout=…)`, errors in Python's wording.
fn run_checked(argv: &[&str], timeout_seconds: u64) -> Result<(), String> {
    let shown = format!(
        "[{}]",
        argv.iter()
            .map(|arg| py_repr_str(arg))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut child = Command::new(argv[0])
        .args(&argv[1..])
        .spawn()
        .map_err(|error| py_os_error(&error, Some(Path::new(argv[0]))))?;
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| py_os_error(&error, None))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "Command '{shown}' timed out after {timeout_seconds} seconds"
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    check_status(&shown, status)
}

fn check_status(shown: &str, status: ExitStatus) -> Result<(), String> {
    if let Some(signal) = status.signal() {
        return Err(format!(
            "Command '{shown}' died with {}.",
            signal_repr(signal)
        ));
    }
    match status.code() {
        Some(0) => Ok(()),
        Some(code) => Err(format!(
            "Command '{shown}' returned non-zero exit status {code}."
        )),
        None => Err(format!("Command '{shown}' returned an unknown status.")),
    }
}

/// `repr(signal.Signals(n))`.
fn signal_repr(signal: i32) -> String {
    let name = match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGTRAP => "SIGTRAP",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        _ => return format!("<Signals: {signal}>"),
    };
    format!("<Signals.{name}: {signal}>")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    #[test]
    fn disable_startup_update_check_rewrites() {
        // Expected values from the Python reference.
        let cases: &[(&[&str], &[&str])] = &[
            (
                &["codex", "-m", "x"],
                &["codex", "-c", UPDATE_CHECK_CONFIGURATION, "-m", "x"],
            ),
            (
                &["codex", "-c", "check_for_update_on_startup=true", "-m", "x"],
                &["codex", "-c", UPDATE_CHECK_CONFIGURATION, "-m", "x"],
            ),
            (
                &["codex", "--config=check_for_update_on_startup=true"],
                &["codex", "-c", UPDATE_CHECK_CONFIGURATION],
            ),
            (
                &["codex", "--config", "check_for_update_on_startup"],
                &["codex", "-c", UPDATE_CHECK_CONFIGURATION],
            ),
            (
                &["codex", "-c", "other=1", "-c"],
                &[
                    "codex",
                    "-c",
                    UPDATE_CHECK_CONFIGURATION,
                    "-c",
                    "other=1",
                    "-c",
                ],
            ),
            (
                &["codex", "-c", "-c", "check_for_update_on_startup=1"],
                &["codex", "-c", UPDATE_CHECK_CONFIGURATION, "-c"],
            ),
        ];
        for (tokens, expected) in cases {
            assert_eq!(
                disable_startup_update_check(&strings(tokens)),
                strings(expected),
                "{tokens:?}"
            );
        }
    }

    #[test]
    fn python_os_error_text() {
        let error = io::Error::from_raw_os_error(libc::EEXIST);
        assert_eq!(
            py_os_error(&error, Some(Path::new("/h/codex-update"))),
            "[Errno 17] File exists: '/h/codex-update'"
        );
        assert_eq!(
            py_os_error(&io::Error::from_raw_os_error(libc::ENOENT), None),
            "[Errno 2] No such file or directory"
        );
    }

    #[test]
    fn utc_timestamps() {
        let at = |seconds: u64| utc_timestamp(UNIX_EPOCH + Duration::from_secs(seconds));
        assert_eq!(at(0), "1970-01-01T00:00:00Z");
        assert_eq!(at(951_825_599), "2000-02-29T11:59:59Z");
        assert_eq!(at(1_790_000_000), "2026-09-21T14:13:20Z");
    }

    #[test]
    fn subprocess_error_wording() {
        let shown = "['sh', '-c', 'exit 3']";
        assert_eq!(
            check_status(shown, ExitStatus::from_raw(3 << 8)).unwrap_err(),
            "Command '['sh', '-c', 'exit 3']' returned non-zero exit status 3."
        );
        assert_eq!(
            check_status(shown, ExitStatus::from_raw(9)).unwrap_err(),
            "Command '['sh', '-c', 'exit 3']' died with <Signals.SIGKILL: 9>."
        );
        assert_eq!(
            run_checked(&["sleep", "5"], 0).unwrap_err(),
            "Command '['sleep', '5']' timed out after 0 seconds"
        );
        assert_eq!(
            run_checked(&["nonexistent-xyz-tx"], 5).unwrap_err(),
            "[Errno 2] No such file or directory: 'nonexistent-xyz-tx'"
        );
        assert_eq!(run_checked(&["true"], 5), Ok(()));
    }

    struct Layout {
        _dir: tempfile::TempDir,
        base: PathBuf,
        codex_home: PathBuf,
        bin: PathBuf,
    }

    fn layout() -> Layout {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let codex_home = base.join("cx");
        let release = codex_home.join("packages/standalone/releases/1.0");
        std::fs::create_dir_all(release.join("bin")).unwrap();
        for path in [release.join("bin/codex"), release.join("codex")] {
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
        }
        std::os::unix::fs::symlink(
            "releases/1.0",
            codex_home.join("packages/standalone/current"),
        )
        .unwrap();
        let bin = base.join("bin");
        std::fs::create_dir(&bin).unwrap();
        Layout {
            _dir: dir,
            base,
            codex_home,
            bin,
        }
    }

    #[test]
    fn standalone_executable_detection() {
        let l = layout();
        let path = l.bin.to_str().unwrap();
        assert_eq!(
            standalone_executable("codex", Some(path), None, &l.codex_home),
            None
        );
        std::os::unix::fs::symlink(
            l.codex_home.join("packages/standalone/current/bin/codex"),
            l.bin.join("codex"),
        )
        .unwrap();
        assert_eq!(
            standalone_executable("codex", Some(path), Some("/usr/bin"), &l.codex_home),
            Some(l.bin.join("codex"))
        );
        assert_eq!(
            standalone_executable("codex", None, Some(path), &l.codex_home),
            Some(l.bin.join("codex"))
        );
        assert_eq!(
            standalone_executable("codex", Some(""), Some(path), &l.codex_home),
            None
        );
        // A pinned release (not through `current`) is not managed.
        std::fs::remove_file(l.bin.join("codex")).unwrap();
        std::os::unix::fs::symlink(
            l.codex_home.join("packages/standalone/releases/1.0/codex"),
            l.bin.join("codex"),
        )
        .unwrap();
        assert_eq!(
            standalone_executable("codex", Some(path), None, &l.codex_home),
            None
        );
        // `current` pointing outside `releases/`.
        let outside = l.base.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("codex"), "").unwrap();
        std::fs::set_permissions(
            outside.join("codex"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let current = l.codex_home.join("packages/standalone/current");
        std::fs::remove_file(&current).unwrap();
        std::os::unix::fs::symlink(&outside, &current).unwrap();
        std::fs::remove_file(l.bin.join("codex")).unwrap();
        std::os::unix::fs::symlink(current.join("codex"), l.bin.join("codex")).unwrap();
        assert_eq!(
            standalone_executable("codex", Some(path), None, &l.codex_home),
            None
        );
    }

    #[test]
    fn schedule_update_claims_writes_the_header_and_respects_interval_and_lock() {
        let l = layout();
        let state = l.base.join("state");
        let marker = l.base.join("child-ran");
        // A stand-in for `tx`: records its argv + env, prints `ok`.
        let fake_tx = l.base.join("fake-tx");
        std::fs::write(
            &fake_tx,
            format!(
                "#!/bin/sh\necho \"$1 $CODEX_HOME $CODEX_INSTALL_DIR $CODEX_NON_INTERACTIVE $EXTRA\" > {}\necho ok\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            &fake_tx,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let executable = l.bin.join("codex");
        let env = vec![("EXTRA".to_owned(), "e".to_owned())];
        let request = UpdateRequest {
            executable: &executable,
            codex_home: &l.codex_home,
            environment: &env,
            state_directory: &state,
            tx_executable: &fake_tx,
        };
        schedule_update(&request).unwrap();
        let log = state.join(LOG_FILE_NAME);
        let wait_for = |check: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !check() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(check());
        };
        wait_for(&|| std::fs::read_to_string(&log).unwrap().ends_with("ok\n"));
        let text = std::fs::read_to_string(&log).unwrap();
        let (header, rest) = text.split_once('\n').unwrap();
        assert!(header.starts_with('[') && header.ends_with("Z] Checking for Codex updates"));
        assert_eq!(
            header.len(),
            "[2026-01-01T00:00:00Z] Checking for Codex updates".len()
        );
        assert_eq!(rest, "ok\n");
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            format!(
                "{UPDATE_VERB} {} {} 1 e\n",
                l.codex_home.display(),
                l.bin.display()
            )
        );

        // Fresh non-empty log → no second run.
        std::fs::remove_file(&marker).unwrap();
        wait_for(&|| try_lock(&File::open(&log).unwrap()).unwrap());
        schedule_update(&request).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(!marker.exists());
        assert_eq!(std::fs::read_to_string(&log).unwrap(), text);

        // Held lock → skipped even when due.
        let holder = File::open(&log).unwrap();
        assert!(try_lock(&holder).unwrap());
        std::fs::write(&log, "").unwrap();
        schedule_update(&request).unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "");
        drop(holder);

        // Empty log → runs.
        schedule_update(&request).unwrap();
        wait_for(&|| marker.exists());
    }

    #[test]
    fn schedule_update_on_a_file_state_dir_is_an_errno_17() {
        let l = layout();
        let state = l.base.join("codex-update");
        std::fs::write(&state, "not a dir").unwrap();
        let executable = l.bin.join("codex");
        let error = schedule_update(&UpdateRequest {
            executable: &executable,
            codex_home: &l.codex_home,
            environment: &[],
            state_directory: &state,
            tx_executable: Path::new("/bin/true"),
        })
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("[Errno 17] File exists: '{}'", state.display())
        );
    }
}
