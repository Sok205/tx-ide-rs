//! The `on-event` program of a plugin that is not hosted in this process, as an [`EventSink`]:
//! started once per event it subscribes to, with the event as one JSON line on its input.

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::external::{ExecContext, Launcher};
use super::index::{Activation, Index};
use super::manifest::{Events, ManifestOnEvent};
use crate::core::TxCore;
use crate::events::{Event, EventSink};

/// The role of a traced start: the `on-event` program of a plugin, started for one event.
pub const TRACE_SINK: &str = "sink";

/// The longest pause between two looks at a sink that has not ended yet.
const LONGEST_PAUSE: Duration = Duration::from_millis(25);

/// The `on-event` program of one active plugin. For an event of a type it subscribes to, it
/// starts the program in a process group of its own, writes the event's JSON form and a newline
/// to its input and closes it, discards its output, lets its errors through, and waits for it
/// up to the timeout. Then the whole group is killed and named on stderr. Nothing it does fails
/// the operation that emitted the event: its exit code is ignored.
pub struct ExecSink {
    launcher: Launcher,
    program: PathBuf,
    args: Vec<String>,
    events: Events,
    timeout: u64,
}

impl ExecSink {
    pub fn new(launcher: Launcher, on_event: &ManifestOnEvent) -> Self {
        Self {
            program: launcher.program(&on_event.bin),
            args: on_event.args.clone(),
            events: on_event.events.clone(),
            timeout: on_event.timeout,
            launcher,
        }
    }

    /// Waits for `child` until the deadline: `true` when it ended in time.
    fn ended_in_time(&self, child: &mut Child) -> bool {
        let deadline = Instant::now() + Duration::from_secs(self.timeout);
        let mut pause = Duration::from_millis(1);
        loop {
            // An error here means the child cannot be waited for any more: it is not ours to
            // kill then.
            if !matches!(child.try_wait(), Ok(None)) {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            std::thread::sleep(pause.min(deadline - now));
            pause = (pause * 2).min(LONGEST_PAUSE);
        }
    }
}

impl EventSink for ExecSink {
    fn on_event(&self, event: &Event) {
        if !self.events.includes(event.type_) {
            return;
        }
        let spawned = self
            .launcher
            .command(TRACE_SINK, &self.program, &self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .process_group(0)
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                eprintln!(
                    "tx: plugin {}: on-event cannot run {}: {error}",
                    self.launcher.id(),
                    self.program.display()
                );
                return;
            }
        };
        let mut line = serde_json::to_string(event).expect("an event always has a JSON form");
        line.push('\n');
        let mut input = child.stdin.take().expect("the input is piped");
        // A thread of its own, so that a program that never reads its input cannot hold the
        // emitter past the timeout. It ends when the line is written or the program is gone.
        std::thread::spawn(move || {
            let _ = input.write_all(line.as_bytes());
        });
        if self.ended_in_time(&mut child) {
            return;
        }
        let group = libc::pid_t::try_from(child.id()).expect("a process id fits pid_t");
        // SAFETY: kill(2) takes plain integers; a negative pid names the process group the
        // program was started as the leader of, which is still ours since it was not reaped.
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
        let _ = child.wait();
        eprintln!(
            "tx: plugin {}: on-event timed out after {}s",
            self.launcher.id(),
            self.timeout
        );
    }
}

/// The sink step of [`TxCore::plug_external`]: an [`ExecSink`] for every active plugin with
/// `on-event` that is not hosted here, subscribed in plugin order after the sinks registered
/// in-process. Without such a plugin nothing is subscribed.
pub(super) fn plug_sinks(
    core: &mut TxCore,
    index: &Index,
    activation: &Activation,
    context: &Arc<ExecContext>,
    hosted_ids: &[&str],
) {
    let sinks: Vec<ExecSink> = activation
        .active(index)
        .filter(|(state, _, _)| !hosted_ids.contains(&state.id.as_str()))
        .filter_map(|(state, installed, manifest)| {
            let on_event = manifest.on_event.as_ref()?;
            let launcher = Launcher::new(context, core.config(), state, installed);
            Some(ExecSink::new(launcher, on_event))
        })
        .collect();
    for sink in sinks {
        core.subscribe(Box::new(sink));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentId;
    use crate::config::{CoreConfig, Directories};
    use crate::events::EventType;
    use crate::plugin::PluginStatus;
    use crate::plugin::index::{Installed, PluginState};
    use serde_json::Value;
    use std::os::unix::fs::PermissionsExt;

    /// A plugin `log-events` in a temporary directory whose `on-event` program is the shell
    /// script `bin/sink`, tracing into `trace`.
    struct Fixture {
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new(script: &str) -> Self {
            let fixture = Self {
                dir: tempfile::tempdir().unwrap(),
            };
            let program = fixture.plugin().join("bin/sink");
            std::fs::create_dir_all(program.parent().unwrap()).unwrap();
            std::fs::write(&program, format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            fixture
        }

        fn plugin(&self) -> PathBuf {
            self.dir.path().join("log-events")
        }

        fn read(&self, file: &str) -> String {
            std::fs::read_to_string(self.dir.path().join(file)).unwrap_or_default()
        }

        fn sink(&self, events: Events, timeout: u64) -> ExecSink {
            let context = Arc::new(ExecContext {
                environment: vec![("SINK_OUT".to_owned(), self.dir.path().display().to_string())],
                trace: Some(self.dir.path().join("trace")),
            });
            let config = CoreConfig::new(Directories::under(self.dir.path().join("home")));
            let state = PluginState {
                id: "log-events".to_owned(),
                status: PluginStatus::Active,
                settings: Value::Null,
                installed: Some(0),
            };
            let installed = Installed {
                id: "log-events".to_owned(),
                directory: self.plugin(),
                root: self.dir.path().to_owned(),
                manifest: Err("not looked at".to_owned()),
            };
            let on_event = ManifestOnEvent {
                events,
                bin: "sink".to_owned(),
                args: Vec::new(),
                timeout,
            };
            ExecSink::new(
                Launcher::new(&context, &config, &state, &installed),
                &on_event,
            )
        }
    }

    fn event(type_: EventType, detail: &str) -> Event {
        Event::new(type_, AgentId::mint(), "worker", None, detail)
    }

    #[test]
    fn a_subscribed_event_reaches_the_program_as_one_line_with_its_plugin_named() {
        let fixture = Fixture::new(
            r#"cat >> "$SINK_OUT/events"; echo "$TX_PLUGIN_ID" >> "$SINK_OUT/ids"; echo noise"#,
        );
        let sink = fixture.sink(Events::Only(vec![EventType::Renamed]), 5);
        let renamed = event(EventType::Renamed, &"y".repeat(100_000));
        sink.on_event(&renamed);
        sink.on_event(&event(EventType::Tagged, "t"));
        let lines = fixture.read("events");
        assert_eq!(lines.lines().count(), 1);
        assert_eq!(
            serde_json::from_str::<Event>(lines.trim_end()).unwrap(),
            renamed
        );
        assert_eq!(fixture.read("ids"), "log-events\n");
        assert_eq!(
            fixture.read("trace"),
            format!(
                "sink\tlog-events\t{}\n",
                fixture.plugin().join("bin/sink").display()
            )
        );
    }

    #[test]
    fn a_program_past_its_timeout_is_killed_with_its_children() {
        let fixture = Fixture::new(r#"sleep 60 & echo "$!" > "$SINK_OUT/child"; wait"#);
        // Three seconds, so that a loaded machine still starts the child before the deadline.
        let sink = fixture.sink(Events::All, 3);
        let started = Instant::now();
        sink.on_event(&event(EventType::Renamed, "old"));
        assert!(started.elapsed() < Duration::from_secs(4));
        let child: libc::pid_t = fixture.read("child").trim().parse().unwrap();
        // SAFETY: signal 0 only asks whether the process exists.
        let alive = || unsafe { libc::kill(child, 0) } == 0;
        let deadline = Instant::now() + Duration::from_secs(2);
        while alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!alive(), "the sink's own child outlived the timeout");
    }

    #[test]
    fn a_program_that_never_reads_its_input_still_ends_at_the_timeout() {
        let fixture = Fixture::new("sleep 60");
        let sink = fixture.sink(Events::All, 1);
        let started = Instant::now();
        sink.on_event(&event(EventType::Tagged, &"z".repeat(1_000_000)));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_program_that_cannot_start_or_fails_is_no_failure() {
        let fixture = Fixture::new("exit 9");
        fixture
            .sink(Events::All, 1)
            .on_event(&event(EventType::Stopped, ""));
        std::fs::remove_file(fixture.plugin().join("bin/sink")).unwrap();
        fixture
            .sink(Events::All, 1)
            .on_event(&event(EventType::Stopped, ""));
        assert_eq!(fixture.read("trace").lines().count(), 2);
    }
}
