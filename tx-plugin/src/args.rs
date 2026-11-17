//! The global flags every program that runs as tx accepts before its first word.

use std::fmt;
use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
pub struct UsageError(pub String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Global {
    pub config_dir: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
    pub run_dir: Option<PathBuf>,
    pub socket: Option<PathBuf>,
    pub profile_dirs: Vec<PathBuf>,
    pub skill_dirs: Vec<PathBuf>,
    pub env: Vec<(String, String)>,
    pub config: Option<PathBuf>,
}

/// `KEY=VALUE` of `--env`; the key is not empty.
pub fn env_pair(text: &str) -> Result<(String, String), UsageError> {
    text.split_once('=')
        .filter(|(key, _)| !key.is_empty())
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .ok_or_else(|| UsageError(format!("--env expects KEY=VALUE, got {text:?}")))
}

/// The global flags at the start of `arguments`, and the index of the first word after them.
pub fn parse_global(arguments: &[String]) -> Result<(Global, usize), UsageError> {
    let mut global = Global::default();
    let mut index = 0;
    while let Some(flag) = arguments.get(index).map(String::as_str) {
        let value = || {
            arguments
                .get(index + 1)
                .map(String::as_str)
                .ok_or_else(|| UsageError(format!("{flag} needs a value")))
        };
        match flag {
            "--config-dir" => global.config_dir = Some(value()?.into()),
            "--data-dir" => global.data_dir = Some(value()?.into()),
            "--state-dir" => global.state_dir = Some(value()?.into()),
            "--cache-dir" => global.cache_dir = Some(value()?.into()),
            "--run-dir" => global.run_dir = Some(value()?.into()),
            "--socket" => global.socket = Some(value()?.into()),
            "--profile-dir" => global.profile_dirs.push(value()?.into()),
            "--skill-dir" => global.skill_dirs.push(value()?.into()),
            "--env" => global.env.push(env_pair(value()?)?),
            "--config" => global.config = Some(value()?.into()),
            _ => break,
        }
        index += 2;
    }
    Ok((global, index))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn the_index_is_the_first_word_after_the_flags() {
        let (global, index) = parse_global(&words(
            "--data-dir /d --env A=1 --skill-dir /s --env B=2 ls --data-dir x",
        ))
        .unwrap();
        assert_eq!(index, 8);
        assert_eq!(global.data_dir, Some(PathBuf::from("/d")));
        assert_eq!(global.skill_dirs, [PathBuf::from("/s")]);
        assert_eq!(
            global.env,
            [
                ("A".to_owned(), "1".to_owned()),
                ("B".to_owned(), "2".to_owned())
            ]
        );
    }

    #[test]
    fn no_flags_is_index_zero() {
        assert_eq!(parse_global(&words("ls")).unwrap(), (Global::default(), 0));
        assert_eq!(parse_global(&[]).unwrap(), (Global::default(), 0));
    }

    #[test]
    fn a_flag_without_its_value_is_a_usage_error() {
        for flag in ["--config-dir", "--profile-dir", "--env", "--config"] {
            assert_eq!(
                parse_global(&words(flag)).unwrap_err(),
                UsageError(format!("{flag} needs a value"))
            );
        }
    }

    #[test]
    fn env_needs_a_key() {
        for text in ["novalue", "=x"] {
            assert_eq!(
                parse_global(&["--env".to_owned(), text.to_owned()]).unwrap_err(),
                UsageError(format!("--env expects KEY=VALUE, got {text:?}"))
            );
        }
    }
}
