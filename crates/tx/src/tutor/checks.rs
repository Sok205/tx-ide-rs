//! Runs one lesson's checks and renders the ✓ / ✗ report.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::session::Session;
use crate::tutor::http::{self, Server};
use crate::tutor::pack::{Check, CommandCheck, FileCheck};
use crate::tutor::tx_check;

const OUTPUT_TAIL: usize = 20;

pub struct Context<'a> {
    /// The main checkout every `http` / `file` / `command` check runs in.
    pub project: &'a Path,
    /// The pack's server command.
    pub run: &'a [String],
    /// The `http` check's server-readiness timeout (pack.toml `ready_timeout_seconds`, default 5 s).
    pub ready_timeout: Duration,
    pub sessions: &'a dyn Fn() -> Result<Vec<Session>, String>,
    pub confirm: &'a mut dyn FnMut(&str) -> bool,
}

pub struct Report {
    pub results: Vec<(String, Result<(), String>)>,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.results.iter().all(|(_, result)| result.is_ok())
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for (label, result) in &self.results {
            match result {
                Ok(()) => {
                    let _ = writeln!(out, "  ✓ {label}");
                }
                Err(reason) => {
                    let _ = writeln!(out, "  ✗ {label}");
                    for line in reason.lines() {
                        let _ = writeln!(out, "      {line}");
                    }
                }
            }
        }
        out
    }
}

pub fn run(checks: &[Check], ctx: &mut Context<'_>) -> Report {
    let mut server: Option<Result<Server, String>> = None;
    let mut sessions: Option<Result<Vec<Session>, String>> = None;
    let mut results = Vec::with_capacity(checks.len());
    for check in checks {
        let outcome = match check {
            Check::Http(check) => {
                match server.get_or_insert_with(|| Server::start(ctx.run, ctx.project, Some(ctx.ready_timeout))) {
                    Ok(server) => http::request(server.port, &check.method, &check.path, check.body.as_deref())
                        .map_err(|error| format!("request failed: {error}\n{}", server.stderr_tail()))
                        .and_then(|response| http::evaluate(check, &response)),
                    Err(error) => Err(error.clone()),
                }
            }
            Check::Tx(check) => match sessions.get_or_insert_with(|| (ctx.sessions)()) {
                Ok(list) => tx_check::evaluate(check, list),
                Err(error) => Err(error.clone()),
            },
            Check::File(check) => file(ctx.project, check),
            Check::Command(check) => command(ctx.project, check),
            Check::Confirm(check) => {
                if (ctx.confirm)(&check.prompt) {
                    Ok(())
                } else {
                    Err("not confirmed".to_owned())
                }
            }
        };
        results.push((check.label().to_owned(), outcome));
    }
    Report { results }
}

fn file(project: &Path, check: &FileCheck) -> Result<(), String> {
    let shown = check.path.display();
    let text = std::fs::read_to_string(project.join(&check.path))
        .map_err(|_| format!("{shown} does not exist"))?;
    match &check.contains {
        Some(needle) if !text.contains(needle.as_str()) => {
            Err(format!("{shown} does not contain '{needle}'"))
        }
        _ => Ok(()),
    }
}

fn command(project: &Path, check: &CommandCheck) -> Result<(), String> {
    let (program, args) = check.argv.split_first().ok_or("empty argv")?;
    let output = Command::new(program)
        .args(args)
        .current_dir(project)
        .output()
        .map_err(|error| format!("could not run {program}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<&str> = text.lines().collect();
    Err(format!(
        "`{}` exited with {}\n{}",
        check.argv.join(" "),
        output.status,
        lines[lines.len().saturating_sub(OUTPUT_TAIL)..].join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::tutor::pack::{CommandCheck, ConfirmCheck, FileCheck, SessionFilter, TxCheck};

    fn file(path: &str, contains: Option<&str>) -> Check {
        Check::File(FileCheck { label: format!("file {path}"), path: path.into(), contains: contains.map(Into::into) })
    }

    #[test]
    fn runs_every_check_and_reports_each() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("server.py"), "def main(): pass\n").unwrap();
        let fetched = Cell::new(0);
        let sessions = || {
            fetched.set(fetched.get() + 1);
            Ok(Vec::new())
        };
        let mut asked = Vec::new();
        let mut confirm = |prompt: &str| {
            asked.push(prompt.to_owned());
            true
        };
        let tx = |label: &str| Check::Tx(TxCheck { label: label.into(), session: SessionFilter::default(), count: 1, absent: false });
        let checks = [
            file("server.py", Some("def main")),
            file("server.py", Some("class")),
            file("missing.py", None),
            Check::Command(CommandCheck { label: "true".into(), argv: vec!["true".into()] }),
            Check::Command(CommandCheck { label: "false".into(), argv: vec!["sh".into(), "-c".into(), "echo bad; exit 2".into()] }),
            Check::Confirm(ConfirmCheck { label: "moved".into(), prompt: "Did you move?".into() }),
            tx("one"),
            tx("two"),
        ];
        let run_cmd: Vec<String> = Vec::new();
        let report = run(&checks, &mut Context { project: dir.path(), run: &run_cmd, ready_timeout: Duration::from_secs(5), sessions: &sessions, confirm: &mut confirm });
        let outcomes: Vec<bool> = report.results.iter().map(|(_, result)| result.is_ok()).collect();
        assert_eq!(outcomes, [true, false, false, true, false, true, false, false]);
        assert!(!report.passed());
        assert_eq!(fetched.get(), 1, "sessions are fetched once");
        assert_eq!(asked, ["Did you move?"]);
        let rendered = report.render();
        assert!(rendered.contains("  ✓ file server.py\n"), "{rendered}");
        assert!(rendered.contains("  ✗ file server.py\n      server.py does not contain 'class'\n"), "{rendered}");
        assert!(rendered.contains("bad"), "{rendered}");
    }

    #[test]
    fn no_checks_pass() {
        let sessions = || -> Result<Vec<Session>, String> { Ok(Vec::new()) };
        let mut confirm = |_: &str| false;
        let report = run(&[], &mut Context { project: Path::new("."), run: &[], ready_timeout: Duration::from_secs(5), sessions: &sessions, confirm: &mut confirm });
        assert!(report.passed());
    }

    #[test]
    fn a_server_that_cannot_start_fails_every_http_check() {
        let http: Check = toml::from_str::<crate::tutor::pack::HttpCheck>("label = \"root\"\npath = \"/\"\nstatus = 200")
            .map(Check::Http)
            .unwrap();
        let sessions = || -> Result<Vec<Session>, String> { Ok(Vec::new()) };
        let mut confirm = |_: &str| true;
        let run_cmd = ["sh".to_owned(), "-c".to_owned(), "echo nope >&2; exit 1".to_owned()];
        let report = run(&[http.clone(), http], &mut Context { project: Path::new("."), run: &run_cmd, ready_timeout: Duration::from_secs(5), sessions: &sessions, confirm: &mut confirm });
        assert!(report.results.iter().all(|(_, result)| result.as_ref().is_err_and(|e| e.contains("nope"))));
    }
}
