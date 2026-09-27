//! `tutor`: the vimtutor-style walkthrough — scaffold a project from a pack, open the tutor view,
//! check the current lesson and move between lessons.

use std::cell::RefCell;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::rc::Rc;
use std::time::Duration;

use cordis::{BoxError, Component, Ctx};

use crate::app::{COMMANDS, Command, ENGINES, ENV, Env, HOME, SERVICE, TMUX, Visibility};
use crate::argparse::{Arg, Matches, Parser};
use crate::engines::EngineRegistry;
use crate::service::SessionService;
use crate::shlex::shlex_quote;
use crate::spawn::SpawnSpec;
use crate::storage::{Home, expand_user};
use crate::tmux::Tmux;
use crate::tutor::checks::{self, Context};
use crate::tutor::pack::Pack;
use crate::tutor::progress::Progress;
use crate::tutor::{project, socket_path};
use crate::verbs::common::{default_shell, repo_root};

const ACTIONS: [&str; 8] = ["start", "check", "next", "prev", "goto", "status", "hint", "reset"];
const DEFAULT_LANG: &str = "python";
/// Always needed besides the pack's own `requires`.
const BASE_REQUIRES: [&str; 2] = ["git", "nvim"];

#[derive(Debug, thiserror::Error)]
enum TutorError {
    #[error("no {0} tutorial in progress; run `tx tutor start`")]
    NotStarted(String),
    #[error("missing required program(s): {0}")]
    Missing(String),
    #[error("no lesson '{0}' (try `tx tutor status`)")]
    NoLesson(String),
    #[error("saved lesson '{0}' is not in the pack; run `tx tutor reset`")]
    StaleLesson(String),
    #[error(
        "a {lang} tutorial is already in progress in {}; run `tx tutor reset --hard` to start \
         over there, or delete {} to start somewhere else",
        dir.display(),
        path.display()
    )]
    AlreadyStarted {
        lang: String,
        dir: PathBuf,
        path: PathBuf,
    },
}

struct Deps {
    service: Rc<SessionService>,
    tmux: Rc<Tmux>,
    env: Rc<Env>,
    home: Rc<Home>,
    engines: Rc<RefCell<EngineRegistry>>,
}

pub struct TutorVerbs;

impl Component for TutorVerbs {
    fn name(&self) -> &str {
        "verbs.tutor"
    }
    fn inject(&self) -> &[&'static str] {
        &["commands", "service", "tmux", "env", "home", "engines"]
    }
    fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        let deps = Rc::new(Deps {
            service: ctx.get(SERVICE)?,
            tmux: ctx.get(TMUX)?,
            env: ctx.get(ENV)?,
            home: ctx.get(HOME)?,
            engines: ctx.get(ENGINES)?,
        });
        ctx.get(COMMANDS)?
            .register(ctx, Visibility::Public, Tutor(deps));
        Ok(())
    }
}

struct Tutor(Rc<Deps>);

impl Command for Tutor {
    fn name(&self) -> &'static str {
        "tutor"
    }
    fn summary(&self) -> &'static str {
        "Learn tx-ide by building an HTTP server, vimtutor-style: start, check, next, prev, goto, \
         status, hint, reset."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let parser = Parser::new("tx tutor")
            .description(self.summary())
            .arg(Arg::positional("action").choices(&ACTIONS).help(
                "start — scaffold the project and open the tutor view; check — verify the current \
                 lesson and advance; next / prev / goto N — move; status — list lessons; hint — \
                 print the current lesson's solution; reset — back to lesson 1",
            ))
            .arg(
                Arg::positional("target")
                    .optional()
                    .help("start: the pack (default: python); goto: a lesson number or id"),
            )
            .arg(Arg::option("--lang").help("the pack other actions act on (default: python)"))
            .arg(Arg::option("--dir").help("start: the project directory (default: ~/tx-tutor/<lang>)"))
            .arg(Arg::option("--hard").flag().help("reset: also delete and re-scaffold the project"));
        let args = match parser.parse(argv) {
            Ok(args) => args,
            Err(exit) => return Ok(exit.emit()),
        };
        let deps = &self.0;
        let action = args.get_one("action").unwrap_or_default();
        let lang = match action {
            "start" => args.get_one("target"),
            _ => args.get_one("lang"),
        }
        .unwrap_or(DEFAULT_LANG);
        let pack = Pack::find(&repo_root(&deps.env), lang)?;
        if action == "start" {
            return deps.start(&pack, &args);
        }
        let path = Progress::path(&deps.home, lang);
        let mut progress =
            Progress::load(&path)?.ok_or_else(|| TutorError::NotStarted(lang.to_owned()))?;
        // `reset` must work even when `progress.current` no longer resolves in the pack (a
        // renumbered/renamed lesson) — it is the only way out of that dead end.
        if action == "reset" {
            if args.get_flag("hard") {
                if !confirm(&format!("Delete {} and start over?", progress.dir.display())) {
                    return Ok(1);
                }
                if project::is_tutor_project(&progress.dir) {
                    std::fs::remove_dir_all(&progress.dir)?;
                }
                project::scaffold(&pack.skeleton(), &progress.dir, &pack.lang)?;
            }
            progress.passed.clear();
            progress.current = pack.lessons[0].id.clone();
            progress.save(&path)?;
            deps.announce(&pack, 0);
            return Ok(0);
        }
        let current = pack
            .index_of(&progress.current)
            .ok_or_else(|| TutorError::StaleLesson(progress.current.clone()))?;
        let moved_to = match action {
            "check" => return deps.check(&pack, &mut progress, &path, current),
            "status" => {
                print!("{}", render_status(&pack, &progress, current));
                return Ok(0);
            }
            "hint" => {
                match pack.solution_path(current) {
                    Some(solution) => print!("{}", std::fs::read_to_string(solution)?),
                    None => println!(
                        "no hint for this lesson — re-read it, or ask your worker to explain"
                    ),
                }
                return Ok(0);
            }
            "next" => (current + 1).min(pack.lessons.len() - 1),
            "prev" => current.saturating_sub(1),
            "goto" => {
                let key = args.get_one("target").unwrap_or_default();
                pack.resolve(key)
                    .ok_or_else(|| TutorError::NoLesson(key.to_owned()))?
            }
            _ => unreachable!("argparse restricts the action"),
        };
        progress.current = pack.lessons[moved_to].id.clone();
        progress.save(&path)?;
        deps.announce(&pack, moved_to);
        Ok(0)
    }
}

impl Deps {
    fn start(&self, pack: &Pack, args: &Matches) -> Result<i32, BoxError> {
        // Check for a conflicting `--dir` before anything else (requires, tmux, scaffold): a
        // saved tutorial in progress elsewhere must not be silently split across two dirs.
        let path = Progress::path(&self.home, &pack.lang);
        let home_dir = self.env.var("HOME").map(Path::new);
        // A relative `--dir` is relative to the caller's cwd, not wherever `tx` happens to run
        // from later — absolutize it now so it is used consistently below and when saved.
        let requested = args.get_one("dir").map(|dir| expand_user(dir, home_dir)).map(|dir| {
            if dir.is_relative() {
                self.env.cwd.join(dir)
            } else {
                dir
            }
        });
        let saved = Progress::load(&path)?;
        if let (Some(saved), Some(dir)) = (&saved, &requested)
            && dir != &saved.dir
        {
            return Err(TutorError::AlreadyStarted {
                lang: pack.lang.clone(),
                dir: saved.dir.clone(),
                path: path.clone(),
            }
            .into());
        }

        let missing: Vec<&str> = BASE_REQUIRES
            .iter()
            .copied()
            .chain(pack.meta.requires.iter().map(String::as_str))
            .filter(|program| !on_path(&self.env, program))
            .collect();
        if !missing.is_empty() {
            return Err(TutorError::Missing(missing.join(", ")).into());
        }
        if !on_path(&self.env, "claude") {
            eprintln!(
                "tx tutor: `claude` is not on PATH — lessons that need an agent worker will not \
                 pass; skip them with `tx tutor goto N`"
            );
        }
        let mut progress = match saved {
            Some(saved) => saved,
            None => Progress {
                dir: requested.unwrap_or_else(|| {
                    expand_user(&format!("~/tx-tutor/{}", pack.lang), home_dir)
                }),
                current: pack.lessons[0].id.clone(),
                passed: Vec::new(),
            },
        };
        if pack.index_of(&progress.current).is_none() {
            progress.current = pack.lessons[0].id.clone();
        }
        if !project::is_tutor_project(&progress.dir) {
            project::scaffold(&pack.skeleton(), &progress.dir, &pack.lang)?;
            println!("Scaffolded {}", progress.dir.display());
        }
        progress.save(&path)?;

        let view = format!("tutor-{}", pack.lang);
        if !self.tmux.has_session(&view) {
            let socket = socket_path(&self.home, &pack.lang);
            let _ = std::fs::remove_file(&socket);
            let lesson = pack.lesson_path(pack.index_of(&progress.current).unwrap_or(0));
            let nvim = format!(
                "nvim --listen {} {}",
                shlex_quote(&socket.to_string_lossy()),
                shlex_quote(&lesson.to_string_lossy())
            );
            let cwd = progress.dir.to_string_lossy().into_owned();
            let spec = SpawnSpec::for_view(&view, cwd.clone(), nvim, &self.engines.borrow());
            self.service.spawn_view(spec)?;
            if let Err(error) = self.tmux.split_window(&view, &cwd, &default_shell(&self.env)) {
                // Half-built view: kill it so a re-run of `start` rebuilds it cleanly.
                self.tmux.kill_session(&view);
                return Err(error.into());
            }
            println!("{view} view created.");
        }
        if self.env.var("TMUX").is_some_and(|value| !value.is_empty()) {
            self.tmux.switch_client(&view)?;
            Ok(0)
        } else {
            let _ = std::io::stdout().flush();
            Ok(self.tmux.attach_session(&view)?)
        }
    }

    fn check(
        &self,
        pack: &Pack,
        progress: &mut Progress,
        path: &Path,
        current: usize,
    ) -> Result<i32, BoxError> {
        let lesson = &pack.lessons[current];
        println!("Checking lesson {}/{}: {}", current + 1, pack.lessons.len(), lesson.id);
        for program in &lesson.requires {
            if !on_path(&self.env, program) {
                eprintln!(
                    "tx tutor: this lesson needs `{program}`, which is not on PATH; skip this \
                     lesson with `tx tutor next`"
                );
            }
        }
        let sessions = || self.service.reconcile().map_err(|error| error.to_string());
        let mut ask = confirm;
        let report = checks::run(
            &lesson.checks,
            &mut Context {
                project: &progress.dir,
                run: &pack.meta.run,
                ready_timeout: pack
                    .meta
                    .ready_timeout_seconds
                    .map_or(crate::tutor::http::DEFAULT_READY_TIMEOUT, Duration::from_secs),
                sessions: &sessions,
                confirm: &mut ask,
            },
        );
        print!("{}", report.render());
        if !report.passed() {
            println!("Not yet — fix the ✗ items and run `tx tutor check` again.");
            return Ok(1);
        }
        progress.mark_passed(&lesson.id);
        match pack.lessons.get(current + 1) {
            Some(next) => {
                progress.current = next.id.clone();
                progress.save(path)?;
                self.announce(pack, current + 1);
            }
            None => {
                progress.save(path)?;
                println!("🎉 You finished the {} tutorial.", pack.meta.name);
            }
        }
        Ok(0)
    }

    /// Print the lesson line and show the lesson in the tutor view's nvim, if it is listening.
    fn announce(&self, pack: &Pack, index: usize) {
        println!("Lesson {}/{}: {}", index + 1, pack.lessons.len(), pack.lessons[index].id);
        let socket = socket_path(&self.home, &pack.lang);
        if socket.exists() {
            let _ = Process::new("nvim")
                .arg("--server")
                .arg(&socket)
                .arg("--remote")
                .arg(pack.lesson_path(index))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn render_status(pack: &Pack, progress: &Progress, current: usize) -> String {
    pack.lessons
        .iter()
        .enumerate()
        .map(|(index, lesson)| {
            let marker = if index == current {
                "▶"
            } else if progress.is_passed(&lesson.id) {
                "✓"
            } else {
                " "
            };
            format!("{marker} {:>2}. {}\n", index + 1, lesson.id)
        })
        .collect()
}

fn on_path(env: &Env, program: &str) -> bool {
    env.var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|dir| !dir.is_empty())
        .any(|dir| is_executable(&PathBuf::from(dir).join(program)))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// `y` / `yes` on stdin; anything else (or EOF) is no.
fn confirm(prompt: &str) -> bool {
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().lock().read_line(&mut answer);
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}
