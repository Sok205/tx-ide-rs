//! The terminal loop: feed key presses to the [`Home`], keep its data fresh, return what it
//! chose. Data comes from a worker thread (git lookups and tmux calls never hold up a key or a
//! frame): the loop asks it for a fresh [`Context`] every [`REFRESH`], for a
//! [`Detail`](crate::model::Detail) as soon as an unfolded row lacks one, and for the git
//! lookups a context left provisional (a worktree's project and branch) right after that context
//! arrived; each reply replaces the home's context. A context never waits for git, so the first
//! paint waits at most [`FIRST_DATA`] for it; until it arrives the list says it is loading.
//! Every queued event is handled before a frame is drawn. A frame follows a key, a resize or a
//! reply at once; while a spinner shows or a focused backdrop animates, frames also come at its
//! rate, never more often however many events arrive. Unfocused, the backdrop's clock stops.
//! Raw mode and the alternate screen are restored on every exit path. Keys pass [`accept`]
//! first: no chords, and a home opened by an attach starts with a [`GRACE`] period.

use std::io;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture, Event,
    KeyCode, KeyEventKind, KeyModifiers,
};
use ratatui::crossterm::execute;
use tx_client::tmux::Ui;
use tx_core::{LlmEngineState, TxCore};

use crate::backdrop::{self, Backdrop};
use crate::data;
use crate::detail::detail;
use crate::model::{Action, AssistantState, Context, GRACE, Home, Outcome, accept};
use crate::project::ProjectCache;
use crate::ui::{self, FPS};

/// How often the data is gathered again.
pub const REFRESH: Duration = Duration::from_secs(1);
/// How long the first frame waits for the first context.
pub const FIRST_DATA: Duration = Duration::from_millis(60);

pub struct Options {
    /// Opened by an attach: the [`GRACE`] period applies.
    pub first: bool,
    /// The prefix+m popup: Ctrl-c closes it (on the floor it does nothing).
    pub popup: bool,
    /// The session the pane showed before the TUI came back; its row is selected once listed.
    pub returned_from: Option<String>,
    /// This open started `assistant warm`: until its record exists the header says `creating`.
    pub warming: bool,
}

enum Request {
    Context,
    /// The detail of the agent with this id.
    Detail(String),
    /// The git lookups the last context left due, then the context again.
    Projects,
}

/// A context, and whether git lookups are due for it (see [`ProjectCache::is_due`]).
struct Gathered {
    context: Context,
    lookups_due: bool,
}

type Reply = Result<Gathered, String>;

pub fn run(core: &TxCore, home: &mut Home, options: &Options) -> io::Result<Option<Action>> {
    std::thread::scope(|scope| {
        let (requests, worker_requests) = channel();
        let (worker_replies, replies) = channel();
        scope.spawn(move || worker(core, &worker_requests, &worker_replies));
        let catalogue = backdrop::catalogue();
        home.set_themed(catalogue.is_some());
        let mut theme = catalogue.and_then(|catalogue| catalogue.start);
        let mut backdrop = catalogue
            .zip(theme)
            .map(|(catalogue, index)| (catalogue.themes[index].1)());
        let mut terminal = ratatui::try_init()?;
        if backdrop.is_some() {
            execute!(io::stdout(), EnableMouseCapture, EnableFocusChange)?;
        }
        let result = event_loop(
            home,
            options,
            &mut terminal,
            &requests,
            &replies,
            &mut theme,
            &mut backdrop,
        );
        if backdrop.is_some() {
            let _ = execute!(io::stdout(), DisableMouseCapture, DisableFocusChange);
        }
        ratatui::restore();
        // Dropping `requests` ends the worker once its current request is done.
        result
    })
}

/// Answers every request with the whole context, the details it gathered so far included, and
/// whether git lookups are due. A context that cannot be read is the last reply.
fn worker(core: &TxCore, requests: &Receiver<Request>, replies: &Sender<Reply>) {
    let ui = Ui::from_core(core);
    let mut cache = ProjectCache::new();
    let mut context = Context::default();
    for request in requests {
        let regather = match request {
            Request::Context => true,
            Request::Projects => {
                cache.look_up_due();
                true
            }
            Request::Detail(id) => {
                // An agent whose record went away gets no detail; the next context drops its row.
                if let Ok(agent) = core
                    .resolve(&id)
                    .and_then(|agent_id| core.get_agent(&agent_id))
                {
                    context.details.insert(id, detail(core, &mut cache, &agent));
                }
                false
            }
        };
        if regather {
            match data::context(core, &ui, &mut cache, Some(&context)) {
                Ok(fresh) => context = fresh,
                Err(error) => {
                    let _ = replies.send(Err(error.to_string()));
                    return;
                }
            }
        }
        let gathered = Gathered {
            context: context.clone(),
            lookups_due: cache.is_due(),
        };
        if replies.send(Ok(gathered)).is_err() {
            return;
        }
    }
}

/// The loop's side of the worker: what is asked and not yet answered.
struct Data<'a> {
    requests: &'a Sender<Request>,
    replies: &'a Receiver<Reply>,
    outstanding: usize,
    refreshed: Instant,
    /// The detail last asked for, so a row is asked for once.
    asked: Option<String>,
    returned_from: Option<String>,
    warming: bool,
}

impl Data<'_> {
    fn ask(&mut self, request: Request) -> io::Result<()> {
        self.requests
            .send(request)
            .map_err(|_| io::Error::other("the data worker stopped"))?;
        self.outstanding += 1;
        Ok(())
    }

    /// Replaces the home's context; the first one selects the row the pane returned from.
    /// Asks for the lookups a context left due, unless another reply is still to come.
    fn apply(&mut self, home: &mut Home, reply: Reply) -> io::Result<()> {
        self.outstanding -= 1;
        let Gathered {
            mut context,
            lookups_due,
        } = reply.map_err(io::Error::other)?;
        if lookups_due && self.outstanding == 0 {
            self.ask(Request::Projects)?;
        }
        if self.warming
            && matches!(
                context.assistant,
                AssistantState::Absent | AssistantState::Stopped { .. }
            )
        {
            context.assistant = AssistantState::Creating;
        }
        home.replace_context(context);
        if let Some(name) = self.returned_from.take() {
            home.select_session(&name);
        }
        Ok(())
    }

    /// Takes every reply that arrived; whether any did.
    fn receive(&mut self, home: &mut Home) -> io::Result<bool> {
        let mut received = false;
        loop {
            match self.replies.try_recv() {
                Ok(reply) => {
                    self.apply(home, reply)?;
                    received = true;
                }
                Err(TryRecvError::Empty) => return Ok(received),
                Err(TryRecvError::Disconnected) => {
                    return Err(io::Error::other("the data worker stopped"));
                }
            }
        }
    }

    /// Asks for the detail an unfolded row lacks, and for fresh data once [`REFRESH`] passed.
    fn request(&mut self, home: &Home) -> io::Result<()> {
        let wanted = home.needs_detail();
        if wanted != self.asked {
            if let Some(id) = &wanted {
                self.ask(Request::Detail(id.clone()))?;
            }
            self.asked = wanted;
        }
        if self.outstanding == 0 && self.refreshed.elapsed() >= REFRESH {
            self.ask(Request::Context)?;
            self.refreshed = Instant::now();
        }
        Ok(())
    }
}

/// Whether a `◐` spinner shows: an agent or the assistant is starting.
fn spinning(home: &Home) -> bool {
    home.context
        .agents
        .iter()
        .any(|agent| agent.state == LlmEngineState::Starting)
        || matches!(
            home.context.assistant,
            AssistantState::Creating | AssistantState::Starting
        )
}

fn event_loop(
    home: &mut Home,
    options: &Options,
    terminal: &mut ratatui::DefaultTerminal,
    requests: &Sender<Request>,
    replies: &Receiver<Reply>,
    theme: &mut Option<usize>,
    backdrop: &mut Option<Box<dyn Backdrop>>,
) -> io::Result<Option<Action>> {
    let started = Instant::now();
    let mut clock = Clock::new(started);
    let mut data = Data {
        requests,
        replies,
        outstanding: 0,
        refreshed: started,
        asked: None,
        returned_from: options.returned_from.clone(),
        warming: options.warming,
    };
    data.ask(Request::Context)?;
    match replies.recv_timeout(FIRST_DATA) {
        Ok(reply) => data.apply(home, reply)?,
        Err(RecvTimeoutError::Timeout) => {}
        Err(RecvTimeoutError::Disconnected) => {
            return Err(io::Error::other("the data worker stopped"));
        }
    }
    let mut changed = true;
    let mut last_frame = started;
    loop {
        changed |= data.receive(home)?;
        changed |= home.expire_notice(Instant::now());
        data.request(home)?;
        let animating = clock.running() && backdrop.is_some();
        let interval = match &backdrop {
            Some(backdrop) if animating => {
                Duration::from_millis(1000 / backdrop.frames_per_second())
            }
            _ => Duration::from_millis(1000 / FPS),
        };
        let periodic = animating || spinning(home);
        if changed || (periodic && last_frame.elapsed() >= interval) {
            last_frame = Instant::now();
            // A coarse spinner clock, so it turns at 4 Hz whatever the frame rate.
            let spinner = (started.elapsed().as_millis() / 250) as usize;
            terminal.draw(|f| {
                ui::draw_frame(
                    home,
                    spinner,
                    backdrop
                        .as_deref_mut()
                        .map(|backdrop| backdrop as &mut dyn Backdrop),
                    clock.elapsed(last_frame),
                    f.area(),
                    f.buffer_mut(),
                );
            })?;
            changed = false;
        }
        let mut wait = if periodic {
            (last_frame + interval).saturating_duration_since(Instant::now())
        } else {
            interval
        };
        // Every queued event is handled before the next frame, so a backlog costs one frame.
        while event::poll(wait)? {
            wait = Duration::ZERO;
            let key = match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => key,
                Event::Mouse(event) => {
                    if let Some(backdrop) = backdrop.as_deref_mut() {
                        backdrop.mouse(event);
                    }
                    continue;
                }
                Event::FocusLost => {
                    clock.pause(Instant::now());
                    continue;
                }
                Event::FocusGained => {
                    clock.resume(Instant::now());
                    changed = true;
                    continue;
                }
                Event::Resize(..) => {
                    changed = true;
                    continue;
                }
                _ => continue,
            };
            if options.popup
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && key.code == KeyCode::Char('c')
            {
                return Ok(None);
            }
            let in_grace = options.first && started.elapsed() < GRACE;
            let Some(code) = accept(key.code, key.modifiers, in_grace) else {
                continue;
            };
            changed = true;
            match home.key(code) {
                Outcome::Stay => {}
                Outcome::Close => return Ok(None),
                Outcome::Run(action) => return Ok(Some(action)),
                Outcome::NextTheme => {
                    let catalogue = backdrop::catalogue().expect("`b` needs a catalogue");
                    let was_on = backdrop.is_some();
                    *theme = backdrop::next(*theme, catalogue.themes.len());
                    let chosen = theme.map(|index| &catalogue.themes[index]);
                    *backdrop = chosen.map(|(_, constructor)| constructor());
                    match modes_change(was_on, backdrop.is_some()) {
                        Some(true) => {
                            execute!(io::stdout(), EnableMouseCapture, EnableFocusChange)?;
                        }
                        Some(false) => {
                            execute!(io::stdout(), DisableMouseCapture, DisableFocusChange)?;
                        }
                        None => {}
                    }
                    let name = chosen.map(|(name, _)| *name);
                    (catalogue.remember)(name);
                    home.notify(format!("theme: {}", name.unwrap_or("off")), Instant::now());
                    clock = Clock::new(Instant::now());
                }
            }
        }
    }
}

/// What to do with mouse capture and focus reporting when the backdrop goes from `before` to
/// `after`: `Some(true)` turns them on, `Some(false)` off, `None` leaves them.
fn modes_change(before: bool, after: bool) -> Option<bool> {
    (before != after).then_some(after)
}

/// The backdrop's time: it stands still while the pane is unfocused.
struct Clock {
    started: Instant,
    /// Time spent unfocused before the current focus.
    paused: Duration,
    lost: Option<Instant>,
}

impl Clock {
    fn new(started: Instant) -> Self {
        Self {
            started,
            paused: Duration::ZERO,
            lost: None,
        }
    }

    fn running(&self) -> bool {
        self.lost.is_none()
    }

    fn pause(&mut self, now: Instant) {
        self.lost.get_or_insert(now);
    }

    fn resume(&mut self, now: Instant) {
        if let Some(lost) = self.lost.take() {
            self.paused += now - lost;
        }
    }

    fn elapsed(&self, now: Instant) -> Duration {
        self.lost.unwrap_or(now) - self.started - self.paused
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AgentRow;

    fn agent(state: LlmEngineState) -> AgentRow {
        AgentRow {
            id: "id".into(),
            name: "a".into(),
            tag: String::new(),
            state,
            age_at: 0.0,
            branch: None,
            chats: 0,
            project: 0,
            is_assistant: false,
        }
    }

    #[test]
    fn only_a_starting_agent_or_assistant_spins() {
        let mut home = Home::new(Context::default());
        assert!(!spinning(&home));
        home.context.agents = vec![agent(LlmEngineState::Idle)];
        assert!(!spinning(&home));
        home.context.agents = vec![agent(LlmEngineState::Starting)];
        assert!(spinning(&home));
        home.context.agents.clear();
        home.context.assistant = AssistantState::Creating;
        assert!(spinning(&home));
    }

    #[test]
    fn cycling_to_off_and_back_toggles_the_terminal_modes() {
        assert_eq!(modes_change(true, true), None);
        assert_eq!(modes_change(true, false), Some(false));
        assert_eq!(modes_change(false, false), None);
        assert_eq!(modes_change(false, true), Some(true));
    }

    #[test]
    fn the_backdrop_clock_stands_still_while_unfocused() {
        let started = Instant::now();
        let at = |seconds| started + Duration::from_secs(seconds);
        let mut clock = Clock::new(started);
        assert_eq!(clock.elapsed(at(2)), Duration::from_secs(2));
        clock.pause(at(2));
        clock.pause(at(3));
        assert!(!clock.running());
        assert_eq!(clock.elapsed(at(10)), Duration::from_secs(2));
        clock.resume(at(10));
        clock.resume(at(11));
        assert!(clock.running());
        assert_eq!(clock.elapsed(at(12)), Duration::from_secs(4));
    }

    #[test]
    fn the_worker_answers_with_the_details_it_gathered() {
        let (requests, worker_requests) = channel();
        let (worker_replies, replies) = channel();
        let dir = tempfile::Builder::new()
            .prefix("tx")
            .tempdir_in("/tmp")
            .unwrap();
        let core = TxCore::open(tx_core::CoreConfig::new(tx_core::Directories::under(
            dir.path().join("state"),
        )))
        .unwrap();
        let core = &core;
        std::thread::scope(|scope| {
            scope.spawn(move || worker(core, &worker_requests, &worker_replies));
            requests.send(Request::Context).unwrap();
            let gathered = replies.recv().unwrap().unwrap();
            assert!(gathered.context.agents.is_empty());
            assert!(!gathered.lookups_due);
            requests.send(Request::Detail("nosuch".into())).unwrap();
            assert!(replies.recv().unwrap().unwrap().context.details.is_empty());
            requests.send(Request::Projects).unwrap();
            assert!(!replies.recv().unwrap().unwrap().lookups_due);
            drop(requests);
        });
    }
}
