//! GitHub through `gh`: the repository of a checkout, the issues and labels of a repository
//! and the few mutations the board makes. `gh` holds the operator's login; the board holds no
//! token.

use std::path::Path;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

/// The fields `gh issue list --json` is asked for; [`Issue`] reads exactly these.
pub const FIELDS: &str =
    "number,title,state,labels,assignees,milestone,url,body,createdAt,updatedAt,author";
/// Open issues are the board; closed ones only its Done column, the most recently updated.
pub const OPEN_LIMIT: &str = "200";
pub const CLOSED_LIMIT: &str = "20";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Repository {
    pub owner: String,
    pub name: String,
}

impl Repository {
    /// `owner/name`.
    pub fn parse(slug: &str) -> Option<Self> {
        let (owner, name) = slug.trim().split_once('/')?;
        let word = |text: &str| {
            !text.is_empty()
                && text
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
        };
        (word(owner) && word(name)).then(|| Self {
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }

    /// `git@github.com:owner/name.git`, `ssh://git@github.com/owner/name.git`,
    /// `https://github.com/owner/name(.git)`; `None` for a remote that is not on GitHub.
    pub fn from_remote_url(url: &str) -> Option<Self> {
        let url = url.trim();
        let path = url
            .strip_prefix("git@github.com:")
            .or_else(|| url.strip_prefix("ssh://git@github.com/"))
            .or_else(|| url.strip_prefix("https://github.com/"))
            .or_else(|| url.strip_prefix("http://github.com/"))?;
        let path = path.trim_end_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        Self::parse(path)
    }

    /// The repository `origin` of the checkout holding `directory` points at.
    pub fn of_checkout(directory: &Path) -> Option<Self> {
        let url = git(directory, &["remote", "get-url", "origin"])?;
        Self::from_remote_url(&url)
    }

    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// Trimmed stdout of `git -C <directory> <arguments>`; `None` on any failure or empty output.
pub fn git(directory: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// The top of the git checkout holding `directory`, if it is in one.
pub fn checkout_of(directory: &Path) -> Option<std::path::PathBuf> {
    git(directory, &["rev-parse", "--show-toplevel"]).map(std::path::PathBuf::from)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Label {
    pub name: String,
    /// Six hex digits, as GitHub keeps it (`d73a4a`).
    #[serde(default)]
    pub color: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct User {
    pub login: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Milestone {
    pub title: String,
}

/// One issue as `gh issue list --json` prints it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    /// `OPEN` or `CLOSED`.
    pub state: String,
    #[serde(default)]
    pub labels: Vec<Label>,
    #[serde(default)]
    pub assignees: Vec<User>,
    #[serde(default)]
    pub milestone: Option<Milestone>,
    pub url: String,
    #[serde(default)]
    pub body: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    #[serde(default)]
    pub author: User,
}

impl Issue {
    pub fn is_open(&self) -> bool {
        self.state == "OPEN"
    }

    pub fn assigned_to(&self, login: &str) -> bool {
        self.assignees.iter().any(|user| user.login == login)
    }

    pub fn has_label(&self, name: &str) -> bool {
        self.labels.iter().any(|label| label.name == name)
    }
}

/// One label of the repository, as `gh label list --json` prints it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct RepoLabel {
    pub name: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub description: String,
}

/// `d73a4a` (GitHub's label colour, six hex digits, `#` optional) as channels.
pub fn rgb_of(hex: &str) -> Option<(u8, u8, u8)> {
    let hex = hex.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let channel = |index: usize| u8::from_str_radix(&hex[index..index + 2], 16).ok();
    Some((channel(0)?, channel(2)?, channel(4)?))
}

/// What one fetch brings back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fetched {
    pub issues: Vec<Issue>,
    pub labels: Vec<RepoLabel>,
}

/// Why `gh` could not answer: the board tells the operator what to do about the first two and
/// keeps its last frame on the third.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// `gh` is not on `PATH`.
    NotInstalled,
    /// `gh` runs but is not signed in; the line it printed.
    NotLoggedIn(String),
    /// Anything else, as `gh` said it.
    Failed(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::NotInstalled => f.write_str("gh is not installed (brew install gh)"),
            Failure::NotLoggedIn(_) => f.write_str("gh is not signed in (gh auth login)"),
            Failure::Failed(reason) => f.write_str(reason),
        }
    }
}

/// Trimmed stdout of `gh <arguments>`; the failure carries `gh`'s last stderr line.
pub fn gh(arguments: &[&str]) -> Result<String, Failure> {
    let output = Command::new("gh")
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => Failure::NotInstalled,
            _ => Failure::Failed(format!("gh: {error}")),
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("failed")
            .trim()
            .to_owned();
        if stderr.contains("gh auth login") || stderr.to_lowercase().contains("not logged in") {
            return Err(Failure::NotLoggedIn(reason));
        }
        return Err(Failure::Failed(format!(
            "gh {}: {reason}",
            arguments.first().unwrap_or(&"")
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// `gh` as a plain message, for the footer.
fn text(failure: Failure) -> String {
    failure.to_string()
}

pub fn parse_issues(json: &str) -> Result<Vec<Issue>, String> {
    serde_json::from_str(json).map_err(|error| format!("gh issue list: {error}"))
}

fn parsed(json: &str) -> Result<Vec<Issue>, Failure> {
    parse_issues(json).map_err(Failure::Failed)
}

/// The open issues (newest activity first) followed by the recently closed ones.
pub fn fetch_issues(repository: &Repository) -> Result<Vec<Issue>, Failure> {
    let slug = repository.slug();
    let mut issues = Vec::new();
    for (state, limit) in [("open", OPEN_LIMIT), ("closed", CLOSED_LIMIT)] {
        let json = gh(&[
            "issue", "list", "--repo", &slug, "--state", state, "--limit", limit, "--json", FIELDS,
        ])?;
        issues.extend(parsed(&json)?);
    }
    Ok(issues)
}

/// The repository's labels, by name.
pub fn fetch_labels(repository: &Repository) -> Result<Vec<RepoLabel>, Failure> {
    let json = gh(&[
        "label",
        "list",
        "--repo",
        &repository.slug(),
        "--limit",
        "200",
        "--json",
        "name,color,description",
    ])?;
    let mut labels: Vec<RepoLabel> = serde_json::from_str(&json)
        .map_err(|error| Failure::Failed(format!("gh label list: {error}")))?;
    labels.sort_by_key(|label| label.name.to_lowercase());
    Ok(labels)
}

/// The issues and the labels.
pub fn fetch(repository: &Repository) -> Result<Fetched, Failure> {
    Ok(Fetched {
        issues: fetch_issues(repository)?,
        labels: fetch_labels(repository)?,
    })
}

/// The login `gh` is signed in as.
pub fn viewer() -> Result<String, String> {
    gh(&["api", "user", "--jq", ".login"]).map_err(text)
}

pub fn close(repository: &Repository, number: u64) -> Result<(), String> {
    gh(&[
        "issue",
        "close",
        &number.to_string(),
        "--repo",
        &repository.slug(),
    ])
    .map(drop)
    .map_err(text)
}

pub fn reopen(repository: &Repository, number: u64) -> Result<(), String> {
    gh(&[
        "issue",
        "reopen",
        &number.to_string(),
        "--repo",
        &repository.slug(),
    ])
    .map(drop)
    .map_err(text)
}

pub fn assign(repository: &Repository, number: u64, login: &str, on: bool) -> Result<(), String> {
    let flag = if on {
        "--add-assignee"
    } else {
        "--remove-assignee"
    };
    gh(&[
        "issue",
        "edit",
        &number.to_string(),
        "--repo",
        &repository.slug(),
        flag,
        login,
    ])
    .map(drop)
    .map_err(text)
}

/// `gh issue edit <n> --add-label a,b --remove-label c`; nothing to change is no call.
pub fn edit_labels(
    repository: &Repository,
    number: u64,
    add: &[String],
    remove: &[String],
) -> Result<(), String> {
    if add.is_empty() && remove.is_empty() {
        return Ok(());
    }
    let number = number.to_string();
    let slug = repository.slug();
    let (added, removed) = (add.join(","), remove.join(","));
    let mut arguments = vec!["issue", "edit", &number, "--repo", &slug];
    if !add.is_empty() {
        arguments.extend(["--add-label", &added]);
    }
    if !remove.is_empty() {
        arguments.extend(["--remove-label", &removed]);
    }
    gh(&arguments).map(drop).map_err(text)
}

pub fn open_in_browser(repository: &Repository, number: u64) -> Result<(), String> {
    gh(&[
        "issue",
        "view",
        &number.to_string(),
        "--repo",
        &repository.slug(),
        "--web",
    ])
    .map(drop)
    .map_err(text)
}

/// `gh issue comment <n>` / `gh issue create` on the caller's terminal: `gh` opens the editor.
pub fn interactive(repository: &Repository, arguments: &[&str]) -> Result<(), String> {
    let status = Command::new("gh")
        .args(arguments)
        .args(["--repo", &repository.slug()])
        .status()
        .map_err(|error| format!("gh: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "gh {}: exit {}",
            arguments.join(" "),
            status.code().unwrap_or(1)
        ))
    }
}

/// Seconds since the epoch of an ISO 8601 UTC stamp (`2026-11-20T16:38:34Z`), `None` for
/// anything else.
pub fn epoch_of(stamp: &str) -> Option<f64> {
    let (date, time) = stamp.split_once('T')?;
    let time = time.strip_suffix('Z')?;
    let mut date = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let mut time = time.split(':').map(str::parse::<i64>);
    let (hour, minute, second) = (time.next()?.ok()?, time.next()?.ok()?, time.next()?.ok()?);
    // days from civil (Howard Hinnant)
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some((days * 86_400 + hour * 3600 + minute * 60 + second) as f64)
}

/// `3m`, `2h`, `5d`, `3w`: how long ago `stamp` was at `now`.
pub fn age(stamp: &str, now: f64) -> String {
    let Some(then) = epoch_of(stamp) else {
        return "?".to_owned();
    };
    let seconds = (now - then).max(0.0);
    let minutes = seconds / 60.0;
    if minutes < 1.0 {
        "now".to_owned()
    } else if minutes < 60.0 {
        format!("{}m", minutes as u64)
    } else if minutes < 1440.0 {
        format!("{}h", (minutes / 60.0) as u64)
    } else if minutes < 1440.0 * 14.0 {
        format!("{}d", (minutes / 1440.0) as u64)
    } else {
        format!("{}w", (minutes / (1440.0 * 7.0)) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repositories_from_remote_urls() {
        for url in [
            "git@github.com:tx-devs/tx-ide.git",
            "git@github.com:tx-devs/tx-ide",
            "ssh://git@github.com/tx-devs/tx-ide.git",
            "https://github.com/tx-devs/tx-ide.git",
            "https://github.com/tx-devs/tx-ide/",
        ] {
            assert_eq!(
                Repository::from_remote_url(url).map(|repository| repository.slug()),
                Some("tx-devs/tx-ide".to_owned()),
                "{url}"
            );
        }
        assert_eq!(Repository::from_remote_url("git@gitlab.com:a/b.git"), None);
        assert_eq!(Repository::parse("a"), None);
        assert_eq!(Repository::parse("a/b c"), None);
    }

    #[test]
    fn stamps_become_epochs_and_ages() {
        assert_eq!(epoch_of("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(epoch_of("2026-11-20T16:38:34Z"), Some(1_795_192_714.0));
        assert_eq!(epoch_of("yesterday"), None);
        let now = 1_795_192_714.0;
        assert_eq!(age("2026-11-20T16:38:34Z", now), "now");
        assert_eq!(age("2026-11-20T16:00:00Z", now), "38m");
        assert_eq!(age("2026-11-20T10:00:00Z", now), "6h");
        assert_eq!(age("2026-11-17T10:00:00Z", now), "3d");
        assert_eq!(age("2026-10-17T10:00:00Z", now), "4w");
        assert_eq!(age("nope", now), "?");
    }

    #[test]
    fn failures_say_what_to_do() {
        assert_eq!(
            Failure::NotInstalled.to_string(),
            "gh is not installed (brew install gh)"
        );
        assert_eq!(
            Failure::NotLoggedIn("x".to_owned()).to_string(),
            "gh is not signed in (gh auth login)"
        );
        assert_eq!(Failure::Failed("boom".to_owned()).to_string(), "boom");
    }

    #[test]
    fn label_colours_are_six_hex_digits() {
        assert_eq!(rgb_of("d73a4a"), Some((0xd7, 0x3a, 0x4a)));
        assert_eq!(rgb_of("#A2EEEF"), Some((0xa2, 0xee, 0xef)));
        assert_eq!(rgb_of("red"), None);
        assert_eq!(rgb_of(""), None);
    }

    #[test]
    fn issues_read_what_gh_prints() {
        let json = r#"[{"number":1,"title":"t","state":"OPEN","labels":[{"name":"bug","color":"d73a4a"}],
          "assignees":[{"login":"me","name":"Me"}],"milestone":null,"url":"u","body":"b",
          "createdAt":"2026-11-20T16:38:34Z","updatedAt":"2026-11-20T16:54:16Z","author":{"login":"me","is_bot":false}}]"#;
        let issues = parse_issues(json).unwrap();
        assert_eq!(issues.len(), 1);
        assert!(issues[0].is_open());
        assert!(issues[0].assigned_to("me"));
        assert!(issues[0].has_label("bug"));
    }
}
