//! tx-core plugin `tutor`: the vimtutor-style walkthrough. `txc tutor start` scaffolds a
//! project from a lesson pack and opens the `tutor-<lang>` view; `check` verifies the current
//! lesson and advances; `next` / `prev` / `goto` / `status` / `hint` / `reset` move around.
//! Port of main's `verbs/tutor.rs` and `tutor/`. Progress lives in the plugin's data dir,
//! `<data>/plugins/tutor/<lang>.json`; the packs ship as `tutor/…` resources and are
//! materialised under `<data>/plugins/tutor/packs/`.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use tx_client::tmux::{Ui, shell_quote};
use tx_client::views::{self, ViewKind, ViewRequest, default_shell};
use tx_core::{Agent, ListFilter, Plugin, PluginError, Registry, TxCore, Verb, VerbError, VerbIo};

pub mod bundle;
pub mod checks;
pub mod http;
pub mod keys;
pub mod pack;
pub mod progress;
pub mod project;
pub mod tx_check;

use checks::Context;
use pack::Pack;
use progress::Progress;

pub const PLUGIN_ID: &str = "tutor";
pub const DEFAULT_LANG: &str = "python";
/// Views are `tutor-<lang>`.
pub const VIEW_PREFIX: &str = "tutor-";

const KEYS_FILE: &str = "keys.txt";
const LESSON_LUA: &str = "lesson.lua";
/// The height of the cheat-sheet pane under the lesson.
const KEYS_LINES: u16 = 15;
/// Always needed besides the pack's own `requires`.
const BASE_REQUIRES: [&str; 2] = ["git", "nvim"];
const NVIM_COLORS: &str = "+set background=dark | colorscheme tokyonight-moon";

const USAGE: &str = "usage: txc tutor start [LANG] [--dir DIR] [--detach] | check | next | prev | goto N | status | hint | reset [--hard]   (all but start take --lang LANG)";

#[derive(Clone, Copy, Debug, Default)]
pub struct TutorPlugin;

impl Plugin for TutorPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.verb(Arc::new(TutorVerb));
        for (name, bytes) in bundle::FILES.iter().chain([&bundle::LESSON_LUA]) {
            r.resource(name, bytes)?;
        }
        Ok(())
    }
}

/// The plugin's store: `<data>/plugins/tutor/` (D1). Created by whoever writes into it.
pub fn data_dir(core: &TxCore) -> PathBuf {
    core.config().plugin_data_dir(PLUGIN_ID)
}

pub fn view_name(lang: &str) -> String {
    format!("{VIEW_PREFIX}{lang}")
}

/// The tutor view's nvim `--listen` socket.
fn socket_path(data: &Path, lang: &str) -> PathBuf {
    data.join(format!("{lang}.sock"))
}

#[derive(Debug, thiserror::Error)]
enum TutorError {
    #[error("no {0} tutorial in progress; run `txc tutor start`")]
    NotStarted(String),
    #[error("missing required program(s): {0}")]
    Missing(String),
    #[error("no lesson '{0}' (try `txc tutor status`)")]
    NoLesson(String),
    #[error("saved lesson '{0}' is not in the pack; run `txc tutor reset`")]
    StaleLesson(String),
    #[error(
        "a {lang} tutorial is already in progress in {}; run `txc tutor reset --hard` to start \
         over there, or delete {} to start somewhere else",
        dir.display(),
        path.display()
    )]
    AlreadyStarted {
        lang: String,
        dir: PathBuf,
        path: PathBuf,
    },
    #[error("HOME is not set; pass --dir")]
    NoHome,
}

fn failed(error: impl std::fmt::Display) -> VerbError {
    VerbError::Failed(error.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Start,
    Check,
    Next,
    Prev,
    Goto,
    Status,
    Hint,
    Reset,
}

#[derive(Debug, Default)]
struct Args {
    target: Option<String>,
    lang: Option<String>,
    dir: Option<String>,
    hard: bool,
    detach: bool,
}

fn parse_args(argv: &[String]) -> Option<(Action, Args)> {
    let (action, rest) = argv.split_first()?;
    let action = match action.as_str() {
        "start" => Action::Start,
        "check" => Action::Check,
        "next" => Action::Next,
        "prev" => Action::Prev,
        "goto" => Action::Goto,
        "status" => Action::Status,
        "hint" => Action::Hint,
        "reset" => Action::Reset,
        _ => return None,
    };
    let mut args = Args::default();
    let mut rest = rest.iter();
    while let Some(word) = rest.next() {
        match word.as_str() {
            "--lang" if action != Action::Start => args.lang = Some(rest.next()?.clone()),
            "--dir" if action == Action::Start => args.dir = Some(rest.next()?.clone()),
            "--detach" if action == Action::Start => args.detach = true,
            "--hard" if action == Action::Reset => args.hard = true,
            flag if flag.starts_with("--") => return None,
            _ if args.target.is_none() && matches!(action, Action::Start | Action::Goto) => {
                args.target = Some(word.clone());
            }
            _ => return None,
        }
    }
    if action == Action::Goto && args.target.is_none() {
        return None;
    }
    Some((action, args))
}

/// `txc tutor …`.
pub struct TutorVerb;

impl Verb for TutorVerb {
    fn name(&self) -> &'static str {
        "tutor"
    }

    fn usage(&self) -> &'static str {
        "tutor start [LANG] [--dir DIR] [--detach] | check | next | prev | goto N | status | hint | reset [--hard]   learn tx by building an HTTP server"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let Some((action, args)) = parse_args(argv) else {
            let _ = writeln!(io.stderr, "{USAGE}");
            return Ok(2);
        };
        let lang = match action {
            Action::Start => args.target.as_deref(),
            _ => args.lang.as_deref(),
        }
        .unwrap_or(DEFAULT_LANG);
        let tutor = Tutor::new(core);
        let pack = tutor.pack(lang)?;
        if action == Action::Start {
            return tutor.start(io, &pack, &args);
        }
        let path = Progress::path(&tutor.data, lang);
        let mut progress = Progress::load(&path)
            .map_err(|error| failed(format!("{}: {error}", path.display())))?
            .ok_or_else(|| failed(TutorError::NotStarted(lang.to_owned())))?;
        // `reset` must work even when `progress.current` no longer resolves in the pack (a
        // renumbered or renamed lesson): it is the only way out of that dead end.
        if action == Action::Reset {
            return tutor.reset(io, &pack, &mut progress, &path, args.hard);
        }
        let current = pack
            .index_of(&progress.current)
            .ok_or_else(|| failed(TutorError::StaleLesson(progress.current.clone())))?;
        let moved_to = match action {
            Action::Check => return tutor.check(io, &pack, &mut progress, &path, current),
            Action::Status => {
                write!(io.stdout, "{}", render_status(&pack, &progress, current))
                    .map_err(failed)?;
                return Ok(0);
            }
            Action::Hint => {
                match pack.solution_path(current) {
                    Some(solution) => {
                        let text = std::fs::read_to_string(&solution)
                            .map_err(|error| failed(format!("{}: {error}", solution.display())))?;
                        write!(io.stdout, "{text}").map_err(failed)?;
                    }
                    None => writeln!(
                        io.stdout,
                        "no hint for this lesson; re-read it, or ask your worker to explain"
                    )
                    .map_err(failed)?,
                }
                return Ok(0);
            }
            Action::Next => (current + 1).min(pack.lessons.len() - 1),
            Action::Prev => current.saturating_sub(1),
            Action::Goto => {
                let key = args.target.as_deref().unwrap_or_default();
                pack.resolve(key)
                    .ok_or_else(|| failed(TutorError::NoLesson(key.to_owned())))?
            }
            Action::Start | Action::Reset => unreachable!("handled above"),
        };
        progress.current = pack.lessons[moved_to].id.clone();
        tutor.save(&progress, &path)?;
        tutor.announce(io, &pack, moved_to)
    }
}

struct Tutor<'a> {
    core: &'a TxCore,
    ui: Ui,
    data: PathBuf,
}

impl<'a> Tutor<'a> {
    fn new(core: &'a TxCore) -> Self {
        Self {
            core,
            ui: Ui::from_core(core),
            data: data_dir(core),
        }
    }

    fn packs(&self) -> PathBuf {
        self.data.join("packs")
    }

    /// Materialises the active resources, then loads `<packs>/<lang>`.
    fn pack(&self, lang: &str) -> Result<Pack, VerbError> {
        let packs = self.packs();
        let resources = self.core.resources(bundle::PREFIX);
        bundle::materialise(&packs, resources.iter().map(|r| (r.name.as_str(), r.bytes)))
            .map_err(|error| failed(format!("{}: {error}", packs.display())))?;
        Pack::find(&packs, lang).map_err(failed)
    }

    fn save(&self, progress: &Progress, path: &Path) -> Result<(), VerbError> {
        progress
            .save(path)
            .map_err(|error| failed(format!("{}: {error}", path.display())))
    }

    fn start(&self, io: &mut VerbIo<'_>, pack: &Pack, args: &Args) -> Result<i32, VerbError> {
        // A conflicting `--dir` is refused before anything else: a tutorial in progress
        // elsewhere must not be silently split across two dirs.
        let path = Progress::path(&self.data, &pack.lang);
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let requested = match args.dir.as_deref() {
            Some(dir) => Some(absolute_dir(dir, home.as_deref())?),
            None => None,
        };
        let saved = Progress::load(&path)
            .map_err(|error| failed(format!("{}: {error}", path.display())))?;
        if let (Some(saved), Some(dir)) = (&saved, &requested)
            && dir != &saved.dir
        {
            return Err(failed(TutorError::AlreadyStarted {
                lang: pack.lang.clone(),
                dir: saved.dir.clone(),
                path,
            }));
        }
        let missing: Vec<&str> = BASE_REQUIRES
            .iter()
            .copied()
            .chain(pack.meta.requires.iter().map(String::as_str))
            .filter(|program| !on_path(program))
            .collect();
        if !missing.is_empty() {
            return Err(failed(TutorError::Missing(missing.join(", "))));
        }
        if !on_path("claude") {
            let _ = writeln!(
                io.stderr,
                "txc tutor: `claude` is not on PATH; lessons that need an agent worker will not \
                 pass; skip them with `txc tutor goto N`"
            );
        }
        let mut progress = match saved {
            Some(saved) => saved,
            None => Progress {
                dir: match requested {
                    Some(dir) => dir,
                    None => home
                        .ok_or_else(|| failed(TutorError::NoHome))?
                        .join("tx-tutor")
                        .join(&pack.lang),
                },
                current: pack.lessons[0].id.clone(),
                passed: Vec::new(),
            },
        };
        if pack.index_of(&progress.current).is_none() {
            progress.current = pack.lessons[0].id.clone();
        }
        if !project::is_tutor_project(&progress.dir) {
            project::scaffold(&pack.skeleton(), &progress.dir, &pack.lang).map_err(failed)?;
            writeln!(io.stdout, "Scaffolded {}", progress.dir.display()).map_err(failed)?;
        }
        self.save(&progress, &path)?;

        let view = view_name(&pack.lang);
        if !self.ui.has_session(&view) {
            let current = pack.index_of(&progress.current).unwrap_or(0);
            if let Err(error) = self.build_view(pack, &view, &progress.dir, current) {
                // Half-built view: close it so a re-run of `start` rebuilds it cleanly.
                let _ = views::close(&self.ui, &view);
                return Err(error);
            }
            writeln!(io.stdout, "{view} view created.").map_err(failed)?;
        }
        if self.ui.inside() {
            self.ui.switch_client(&view).map_err(failed)?;
            return Ok(0);
        }
        if args.detach || !std::io::stdin().is_terminal() {
            writeln!(
                io.stdout,
                "{view} is ready: tmux -S {} attach -t ={view}",
                shell_quote(&self.core.socket_path().to_string_lossy())
            )
            .map_err(failed)?;
            return Ok(0);
        }
        let _ = io.stdout.flush();
        Err(failed(self.ui.attach_exec(&format!("={view}"))))
    }

    /// Top left: the lesson in nvim; under it the key cheat sheet; right: a shell over a
    /// spare shell (the viewer). Focus ends on the lesson.
    fn build_view(
        &self,
        pack: &Pack,
        view: &str,
        project: &Path,
        lesson: usize,
    ) -> Result<(), VerbError> {
        let socket = socket_path(&self.data, &pack.lang);
        let _ = std::fs::remove_file(&socket);
        let nvim = lesson_nvim_argv(
            &socket,
            &pack.dir.join("lessons"),
            &self.packs().join(LESSON_LUA),
            &format!("={view}:.{{top-right}}"),
            &self.ui.argv(&[]),
            &pack.lesson_path(lesson),
        );
        views::open(
            &self.ui,
            &ViewRequest {
                name: view,
                kind: ViewKind::Tutor,
                program: &nvim[0],
                arguments: &nvim[1..],
                working_directory: project,
                tags: &[],
                parent_id: None,
                bound: None,
            },
        )
        .map_err(failed)?;
        // `-c` is format-expanded by tmux, so a literal `#` must be doubled.
        let cwd = project.to_string_lossy().replace('#', "##");
        let shell = default_shell();
        let session = format!("={view}:");
        self.ui
            .run(&["split-window", "-h", "-t", &session, "-c", &cwd, &shell])
            .map_err(failed)?;
        let top_right = format!("={view}:.{{top-right}}");
        self.ui
            .run(&[
                "split-window",
                "-v",
                "-l",
                "50%",
                "-t",
                &top_right,
                "-c",
                &cwd,
                &shell,
            ])
            .map_err(failed)?;
        let keys_source = self.packs().join(KEYS_FILE);
        let text = std::fs::read_to_string(&keys_source)
            .map_err(|error| failed(format!("{}: {error}", keys_source.display())))?;
        let prefix = self.ui.quiet(&["show-options", "-gv", "prefix"]).1;
        let rendered = self.data.join(KEYS_FILE);
        std::fs::write(&rendered, keys::render(&text, Some(prefix.trim())))
            .map_err(|error| failed(format!("{}: {error}", rendered.display())))?;
        let show = keys_pane_command(&self.ui.argv(&[]), &rendered);
        let top_left = format!("={view}:.{{top-left}}");
        self.ui
            .run(&[
                "split-window",
                "-v",
                "-l",
                &KEYS_LINES.to_string(),
                "-t",
                &top_left,
                "-c",
                &cwd,
                &show,
            ])
            .map_err(failed)?;
        self.ui.select_pane(&top_left);
        Ok(())
    }

    fn reset(
        &self,
        io: &mut VerbIo<'_>,
        pack: &Pack,
        progress: &mut Progress,
        path: &Path,
        hard: bool,
    ) -> Result<i32, VerbError> {
        if hard {
            let prompt = format!("Delete {} and start over?", progress.dir.display());
            if !confirm(io, &prompt) {
                return Ok(1);
            }
            if project::is_tutor_project(&progress.dir) {
                std::fs::remove_dir_all(&progress.dir)
                    .map_err(|error| failed(format!("{}: {error}", progress.dir.display())))?;
            }
            project::scaffold(&pack.skeleton(), &progress.dir, &pack.lang).map_err(failed)?;
        }
        progress.passed.clear();
        progress.current = pack.lessons[0].id.clone();
        self.save(progress, path)?;
        self.announce(io, pack, 0)
    }

    fn check(
        &self,
        io: &mut VerbIo<'_>,
        pack: &Pack,
        progress: &mut Progress,
        path: &Path,
        current: usize,
    ) -> Result<i32, VerbError> {
        let lesson = &pack.lessons[current];
        writeln!(
            io.stdout,
            "Checking lesson {}/{}: {}",
            current + 1,
            pack.lessons.len(),
            lesson.id
        )
        .map_err(failed)?;
        for program in lesson.requires.iter().filter(|program| !on_path(program)) {
            let _ = writeln!(
                io.stderr,
                "txc tutor: this lesson needs `{program}`, which is not on PATH; skip this \
                 lesson with `txc tutor next`"
            );
        }
        let sessions = || all_sessions(self.core).map_err(|error| error.to_string());
        let report = {
            let mut ask = |prompt: &str| confirm(io, prompt);
            checks::run(
                &lesson.checks,
                &mut Context {
                    project: &progress.dir,
                    run: &pack.meta.run,
                    ready_timeout: pack
                        .meta
                        .ready_timeout_seconds
                        .map_or(http::DEFAULT_READY_TIMEOUT, Duration::from_secs),
                    sessions: &sessions,
                    confirm: &mut ask,
                },
            )
        };
        write!(io.stdout, "{}", report.render()).map_err(failed)?;
        if !report.passed() {
            writeln!(
                io.stdout,
                "Not yet: fix the ✗ items and run `txc tutor check` again."
            )
            .map_err(failed)?;
            return Ok(1);
        }
        progress.mark_passed(&lesson.id);
        self.save(progress, path)?;
        match pack.lessons.get(current + 1) {
            Some(next) => {
                progress.current = next.id.clone();
                self.save(progress, path)?;
                self.announce(io, pack, current + 1)
            }
            None => {
                writeln!(io.stdout, "You finished the {} tutorial.", pack.meta.name)
                    .map_err(failed)?;
                Ok(0)
            }
        }
    }

    /// Prints the lesson line and shows the lesson in the view's nvim, if it is listening.
    fn announce(&self, io: &mut VerbIo<'_>, pack: &Pack, index: usize) -> Result<i32, VerbError> {
        writeln!(
            io.stdout,
            "Lesson {}/{}: {}",
            index + 1,
            pack.lessons.len(),
            pack.lessons[index].id
        )
        .map_err(failed)?;
        let socket = socket_path(&self.data, &pack.lang);
        if socket.exists() {
            let _ = Command::new("nvim")
                .arg("--server")
                .arg(&socket)
                .arg("--remote")
                .arg(pack.lesson_path(index))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        Ok(0)
    }
}

/// Every record after a refresh, so lifecycles are current. `refresh_all` returns only the
/// records it changed; the check needs all of them.
fn all_sessions(core: &TxCore) -> Result<Vec<Agent>, tx_core::CoreError> {
    core.refresh_all()?;
    core.list_agents(&ListFilter::new())?
        .iter()
        .map(|id| core.get_agent(id))
        .collect()
}

/// `~` expanded against `home`; a relative dir is taken against the caller's cwd now, so the
/// saved path does not move with a later caller.
fn absolute_dir(dir: &str, home: Option<&Path>) -> Result<PathBuf, VerbError> {
    let expanded = match (dir.strip_prefix('~'), home) {
        (Some(""), Some(home)) => home.to_path_buf(),
        (Some(rest), Some(home)) if rest.starts_with('/') => home.join(&rest[1..]),
        _ => PathBuf::from(dir),
    };
    std::path::absolute(&expanded).map_err(|error| failed(format!("{dir}: {error}")))
}

/// The lesson pane, as argv: nvim listening on the tutor socket so `check` / `next` can switch
/// lessons. `lesson.lua` makes lesson buffers diagnostics-free and lets Enter type a lesson
/// command into the view's shell pane through `tmux_argv` (the core's server).
fn lesson_nvim_argv(
    socket: &Path,
    lessons: &Path,
    lua: &Path,
    shell: &str,
    tmux_argv: &[std::ffi::OsString],
    lesson: &Path,
) -> Vec<String> {
    let tmux: Vec<String> = std::iter::once("tmux".to_owned())
        .chain(
            tmux_argv
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned()),
        )
        .map(|arg| format!("[[{arg}]]"))
        .collect();
    // `--cmd`, not `+cmd`: the autocmd must exist before the first lesson's BufEnter fires.
    let globals = format!(
        "lua vim.g.tx_tutor_lessons = [[{}]]; vim.g.tx_tutor_shell = [[{shell}]]; vim.g.tx_tutor_tmux = {{ {} }}",
        lessons.join("*").to_string_lossy(),
        tmux.join(", ")
    );
    let load = format!("luafile {}", lua.to_string_lossy().replace(' ', "\\ "));
    vec![
        "nvim".to_owned(),
        "--listen".to_owned(),
        socket.to_string_lossy().into_owned(),
        NVIM_COLORS.to_owned(),
        "--cmd".to_owned(),
        globals,
        "--cmd".to_owned(),
        load,
        lesson.to_string_lossy().into_owned(),
    ]
}

/// The cheat-sheet pane: redraws on every resize (the view is born detached at 80x24 and grows
/// on attach), holding its height through the core's server.
fn keys_pane_command(tmux_argv: &[std::ffi::OsString], rendered: &Path) -> String {
    let tmux: Vec<String> = std::iter::once("tmux".to_owned())
        .chain(
            tmux_argv
                .iter()
                .map(|arg| shell_quote(&arg.to_string_lossy())),
        )
        .collect();
    let script = format!(
        "draw() {{ [ \"$(tput lines)\" -ne {KEYS_LINES} ] && \
         {} resize-pane -t \"$TMUX_PANE\" -y {KEYS_LINES}; clear; cat \"$0\"; }}; \
         trap draw WINCH; draw; \
         while :; do sleep 3600 & wait $!; done",
        tmux.join(" ")
    );
    format!(
        "sh -c {} {}",
        shell_quote(&script),
        shell_quote(&rendered.to_string_lossy())
    )
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

fn on_path(program: &str) -> bool {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|dir| !dir.is_empty())
        .any(|dir| is_executable(&Path::new(dir).join(program)))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// `y` / `yes` on the verb's stdin; anything else (or EOF) is no. Reads byte by byte so a
/// second prompt gets the next line, not whatever a buffer swallowed.
fn confirm(io: &mut VerbIo<'_>, prompt: &str) -> bool {
    let _ = write!(io.stdout, "{prompt} [y/N] ");
    let _ = io.stdout.flush();
    let mut answer = Vec::new();
    let mut byte = [0u8; 1];
    while let Ok(1) = io.stdin.read(&mut byte) {
        if byte[0] == b'\n' {
            break;
        }
        answer.push(byte[0]);
    }
    let _ = writeln!(io.stdout);
    matches!(
        String::from_utf8_lossy(&answer)
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "y" | "yes"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn args_parse_per_action() {
        let (action, args) =
            parse_args(&argv(&["start", "python", "--dir", "/p", "--detach"])).unwrap();
        assert_eq!(action, Action::Start);
        assert_eq!(
            (args.target.as_deref(), args.dir.as_deref(), args.detach),
            (Some("python"), Some("/p"), true)
        );
        let (action, args) = parse_args(&argv(&["goto", "3", "--lang", "python"])).unwrap();
        assert_eq!(
            (action, args.target.as_deref(), args.lang.as_deref()),
            (Action::Goto, Some("3"), Some("python"))
        );
        assert!(parse_args(&argv(&["reset", "--hard"])).unwrap().1.hard);
        for bad in [
            &["fly"][..],
            &[],
            &["goto"],
            &["check", "extra"],
            &["check", "--hard"],
            &["start", "--lang", "python"],
            &["next", "--dir", "/p"],
            &["start", "a", "b"],
        ] {
            assert!(parse_args(&argv(bad)).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn the_lesson_opens_in_nvim_listening_on_the_tutor_socket() {
        let tmux = ["-S", "/s/tmux.sock", "-f", "/s/tmux.conf"].map(std::ffi::OsString::from);
        let argv = lesson_nvim_argv(
            Path::new("/d/python.sock"),
            Path::new("/r/my pack/lessons"),
            Path::new("/r/my pack/lesson.lua"),
            "=tutor-python:.{top-right}",
            &tmux,
            Path::new("/r/my pack/lessons/01.md"),
        );
        assert_eq!(
            argv,
            [
                "nvim",
                "--listen",
                "/d/python.sock",
                "+set background=dark | colorscheme tokyonight-moon",
                "--cmd",
                "lua vim.g.tx_tutor_lessons = [[/r/my pack/lessons/*]]; \
                 vim.g.tx_tutor_shell = [[=tutor-python:.{top-right}]]; \
                 vim.g.tx_tutor_tmux = { [[tmux]], [[-S]], [[/s/tmux.sock]], [[-f]], [[/s/tmux.conf]] }",
                "--cmd",
                "luafile /r/my\\ pack/lesson.lua",
                "/r/my pack/lessons/01.md",
            ]
        );
    }

    #[test]
    fn the_keys_pane_resizes_through_the_cores_server() {
        let tmux = ["-S", "/s/tmux.sock", "-f", "/s/tmux.conf"].map(std::ffi::OsString::from);
        let command = keys_pane_command(&tmux, Path::new("/d/keys.txt"));
        assert!(command.starts_with("sh -c '"), "{command}");
        assert!(
            command.contains(
                r"tmux '\''-S'\'' '\''/s/tmux.sock'\'' '\''-f'\'' '\''/s/tmux.conf'\'' resize-pane"
            ),
            "{command}"
        );
        assert!(command.ends_with(" '/d/keys.txt'"), "{command}");
    }

    #[test]
    fn a_relative_or_tilde_dir_becomes_absolute() {
        let home = Path::new("/home/u");
        assert_eq!(
            absolute_dir("~/t", Some(home)).unwrap(),
            Path::new("/home/u/t")
        );
        assert_eq!(absolute_dir("~", Some(home)).unwrap(), home);
        assert_eq!(
            absolute_dir("rel", Some(home)).unwrap(),
            std::env::current_dir().unwrap().join("rel")
        );
    }
}
