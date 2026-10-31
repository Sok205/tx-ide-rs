//! `CoreConfig`: everything the library needs, handed in by the caller. The library never reads
//! the process environment.

use std::path::PathBuf;

use crate::error::CoreError;

/// The longest tmux socket path `open` accepts. `sun_path` holds 104 bytes on macOS and 108 on
/// Linux, and tmux fails opaquely near the limit; 100 leaves room for the NUL and some slack.
pub const MAX_SOCKET_PATH_BYTES: usize = 100;

pub const SOCKET_FILE: &str = "tmux.sock";
pub const CONF_FILE: &str = "tmux.conf";

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CoreConfig {
    /// Records, log, locks, profiles and (by default) the tmux socket live here.
    pub state_dir: PathBuf,
    /// Overrides `<state_dir>/tmux.sock` when that path would be too long.
    pub tmux_socket: Option<PathBuf>,
    /// Base environment for every session, applied before the per-session env.
    pub env: Vec<(String, String)>,
    /// Role search dirs, in order; a later dir overrides an earlier one.
    pub profile_dirs: Vec<PathBuf>,
    /// Skill search dirs, in order; a later dir overrides an earlier one.
    pub skill_dirs: Vec<PathBuf>,
    /// The argv prefix the hooks an engine wires run (`[txc, "chat", "hook"]`). Empty: none.
    pub hook_command: Vec<String>,
}

impl CoreConfig {
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            tmux_socket: None,
            env: Vec::new(),
            profile_dirs: Vec::new(),
            skill_dirs: Vec::new(),
            hook_command: Vec::new(),
        }
    }

    pub fn tmux_socket(mut self, socket: impl Into<PathBuf>) -> Self {
        self.tmux_socket = Some(socket.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn profile_dirs<I, P>(mut self, dirs: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.profile_dirs.extend(dirs.into_iter().map(Into::into));
        self
    }

    pub fn skill_dirs<I, P>(mut self, dirs: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.skill_dirs.extend(dirs.into_iter().map(Into::into));
        self
    }

    pub fn hook_command<I, S>(mut self, words: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.hook_command = words.into_iter().map(Into::into).collect();
        self
    }

    pub fn socket_path(&self) -> PathBuf {
        self.tmux_socket
            .clone()
            .unwrap_or_else(|| self.state_dir.join(SOCKET_FILE))
    }

    pub fn tmux_conf_path(&self) -> PathBuf {
        self.state_dir.join(CONF_FILE)
    }

    /// Fails before any file is touched when the socket path cannot fit a `sockaddr_un` or is
    /// not absolute (a relative socket would move with the caller's cwd and be exported into
    /// sessions as such).
    pub fn check_socket_path(&self) -> Result<(), CoreError> {
        let socket = self.socket_path();
        if socket.as_os_str().len() > MAX_SOCKET_PATH_BYTES {
            return Err(CoreError::SocketPathTooLong(socket));
        }
        if !socket.is_absolute() {
            return Err(CoreError::SocketPathNotAbsolute(socket));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn socket_defaults_under_state_dir() {
        let config = CoreConfig::new("/tmp/txc");
        assert_eq!(config.socket_path(), PathBuf::from("/tmp/txc/tmux.sock"));
        assert_eq!(config.tmux_conf_path(), PathBuf::from("/tmp/txc/tmux.conf"));
    }

    #[test]
    fn explicit_socket_wins() {
        let mut config = CoreConfig::new("/tmp/txc");
        config.tmux_socket = Some("/tmp/s.sock".into());
        assert_eq!(config.socket_path(), PathBuf::from("/tmp/s.sock"));
        assert_eq!(config.tmux_conf_path(), PathBuf::from("/tmp/txc/tmux.conf"));
    }

    #[test]
    fn empty_by_default() {
        let config = CoreConfig::new("/x");
        assert!(config.env.is_empty());
        assert!(config.profile_dirs.is_empty());
        assert!(config.skill_dirs.is_empty());
        assert!(config.hook_command.is_empty());
        assert_eq!(config.state_dir, Path::new("/x"));
    }

    /// A state dir whose `tmux.sock` lands at exactly `len` bytes.
    fn state_dir_for_socket_len(len: usize) -> PathBuf {
        let dir = format!("/{}", "d".repeat(len - "/".len() - "/tmux.sock".len()));
        let config = CoreConfig::new(&dir);
        assert_eq!(config.socket_path().as_os_str().len(), len);
        PathBuf::from(dir)
    }

    #[test]
    fn socket_path_of_100_bytes_is_ok() {
        let config = CoreConfig::new(state_dir_for_socket_len(MAX_SOCKET_PATH_BYTES));
        assert!(config.check_socket_path().is_ok());
    }

    #[test]
    fn socket_path_of_101_bytes_is_too_long() {
        let config = CoreConfig::new(state_dir_for_socket_len(MAX_SOCKET_PATH_BYTES + 1));
        let error = config.check_socket_path().unwrap_err();
        let CoreError::SocketPathTooLong(path) = &error else {
            panic!("{error:?}")
        };
        assert_eq!(*path, config.socket_path());
        assert!(error.to_string().contains("101 bytes"), "{error}");
    }

    #[test]
    fn relative_or_empty_socket_is_refused() {
        for bad in ["", "rel.sock", "."] {
            let config = CoreConfig::new("/abs").tmux_socket(bad);
            assert!(
                matches!(
                    config.check_socket_path(),
                    Err(CoreError::SocketPathNotAbsolute(_))
                ),
                "{bad:?}"
            );
        }
        assert!(CoreConfig::new("rel").check_socket_path().is_err());
    }

    #[test]
    fn explicit_long_socket_is_checked_too() {
        let mut config = CoreConfig::new("/short");
        config.tmux_socket = Some(PathBuf::from(format!("/{}", "s".repeat(120))));
        assert!(matches!(
            config.check_socket_path(),
            Err(CoreError::SocketPathTooLong(_))
        ));
    }
}
