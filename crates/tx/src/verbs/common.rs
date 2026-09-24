//! The `cli.py` helpers several verbs share.

use std::fs::File;
use std::os::fd::AsRawFd;

use crate::app::Env;
use crate::tmux::Tmux;

/// `_split_tags`: comma-separated, empty parts dropped.
pub fn split_tags(raw: &str) -> Vec<String> {
    raw.split(',')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `_group_value`, the `type=` of every `--group`.
pub fn group_value(value: &str) -> Result<String, String> {
    if value.is_empty() {
        return Err("a group cannot be empty".to_owned());
    }
    Ok(value.to_owned())
}

/// `_env_pair`, the `type=` of `--env`.
pub fn env_pair(value: &str) -> Result<String, String> {
    if !value.contains('=') {
        return Err(format!("--env expects KEY=VALUE, got '{value}'"));
    }
    Ok(value.to_owned())
}

/// `_parse_env`: `["K=V", …]` → pairs in order, split on the first `=`; a repeated key keeps its
/// first position and takes the last value (dict semantics).
pub fn parse_env(pairs: &[String]) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = Vec::new();
    for pair in pairs {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match env.iter_mut().find(|(existing, _)| existing == key) {
            Some(slot) => slot.1 = value.to_owned(),
            None => env.push((key.to_owned(), value.to_owned())),
        }
    }
    env
}

/// `_default_shell`: `$SHELL`, else `zsh`.
pub fn default_shell(env: &Env) -> String {
    env.var("SHELL")
        .filter(|shell| !shell.is_empty())
        .unwrap_or("zsh")
        .to_owned()
}

/// `Command._default_cwd`, with Q20 fixed: the calling pane's path only when the caller is
/// inside tmux; outside, the caller's own cwd (the reference asked the server for "the current
/// pane", which is some unrelated pane).
pub fn default_cwd(tmux: &Tmux, env: &Env) -> String {
    let inside_tmux = env.var("TMUX").is_some_and(|value| !value.is_empty());
    inside_tmux
        .then(|| tmux.current_pane_path())
        .flatten()
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| env.cwd.to_string_lossy().into_owned())
}

/// `detect_term_cols`: the width of `/dev/tty` (a popup's pty inside tmux), else `$COLUMNS`, else 80.
pub fn detect_term_cols(env: &Env) -> usize {
    tty_columns().unwrap_or_else(|| crate::argparse::columns_from(env.var("COLUMNS"), || None))
}

fn tty_columns() -> Option<usize> {
    let tty = File::open("/dev/tty").ok()?;
    // SAFETY: TIOCGWINSZ fills a winsize struct we own; the fd is valid for the call's duration.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let status = unsafe { libc::ioctl(tty.as_raw_fd(), libc::TIOCGWINSZ, &mut size) };
    (status == 0 && size.ws_col > 0).then_some(usize::from(size.ws_col))
}

/// `_env_namew`: the picker's exported NAME width, default 18.
pub fn env_namew(env: &Env) -> i64 {
    env.var("NAMEW")
        .filter(|raw| !raw.is_empty() && raw.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(18)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_and_env_helpers_follow_cli_py() {
        // Expected values from lib/tx/cli.py (_split_tags / _parse_env) under python3.14.
        assert_eq!(split_tags(",a,,b,"), ["a", "b"]);
        assert!(split_tags(",").is_empty());
        let pairs = [
            "A=1".to_owned(),
            "B=x=y".to_owned(),
            "A=2".to_owned(),
            "C=".to_owned(),
        ];
        let env = parse_env(&pairs);
        let expected = [("A", "2"), ("B", "x=y"), ("C", "")];
        assert_eq!(env.len(), expected.len());
        for ((key, value), (want_key, want_value)) in env.iter().zip(expected) {
            assert_eq!((key.as_str(), value.as_str()), (want_key, want_value));
        }
        assert_eq!(
            env_pair("FOO"),
            Err("--env expects KEY=VALUE, got 'FOO'".to_owned())
        );
        assert_eq!(group_value(""), Err("a group cannot be empty".to_owned()));
    }
}
