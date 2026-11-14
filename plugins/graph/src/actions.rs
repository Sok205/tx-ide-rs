//! Requests carried out through `tx` verbs (plugin rule D3), never through core directly. A
//! verb runs on its own thread so the view keeps drawing; its outcome comes back as one line
//! for the footer.
//!
//! The commands drop `TX_SESSION_ID`: run from an agent's pane, `tx send` would otherwise speak
//! as that agent. What the graph sends, the operator sends.

use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};

use tx_core::TxCore;

use crate::app::Request;

/// What a fork made from the graph is first told, as the prefix+f fork says it.
pub const FORK_PROMPT: &str = "You are a fork of {source}. Introduce yourself.";

/// This `tx` binary with the core's own directory flags.
#[derive(Clone, Debug)]
pub struct Tx {
    program: PathBuf,
    arguments: Vec<String>,
}

impl Tx {
    pub fn current(core: &TxCore) -> io::Result<Self> {
        Ok(Self {
            program: std::env::current_exe()?,
            arguments: core.config().cli_arguments().to_vec(),
        })
    }

    pub fn program(&self) -> String {
        self.program.to_string_lossy().into_owned()
    }

    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    pub fn command(&self, words: &[String]) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.arguments)
            .args(words)
            .env_remove("TX_SESSION_ID")
            .stdin(Stdio::null());
        command
    }
}

/// The verb and its arguments for a request `tx` carries out alone; `None` for the ones that
/// need tmux (focus, artifacts).
pub fn words(request: &Request) -> Option<Vec<String>> {
    let words: Vec<&str> = match request {
        Request::Send { id, text, .. } => vec!["send", id.as_str(), "--", text],
        Request::Fork {
            id,
            name,
            fork_name,
        } => {
            let prompt = FORK_PROMPT.replace("{source}", name);
            let mut words = vec![
                "fork".to_owned(),
                id.as_str().to_owned(),
                "--prompt".to_owned(),
                prompt,
            ];
            if let Some(fork_name) = fork_name {
                words.extend(["--name".to_owned(), fork_name.clone()]);
            }
            return Some(words);
        }
        Request::Stop { id, .. } => vec!["stop", id.as_str()],
        Request::Respawn { id, .. } => vec!["respawn", id.as_str()],
        Request::Focus { .. } | Request::Artifacts { .. } => return None,
    };
    Some(words.into_iter().map(str::to_owned).collect())
}

/// The footer line while a request runs, and once it succeeded.
fn lines(request: &Request) -> (String, String) {
    match request {
        Request::Send { name, .. } => (format!("sending to {name}…"), format!("sent to {name}")),
        Request::Fork { name, .. } => (format!("forking {name}…"), format!("forked {name}")),
        Request::Stop { name, .. } => (format!("stopping {name}…"), format!("stopped {name}")),
        Request::Respawn { name, .. } => {
            (format!("respawning {name}…"), format!("respawned {name}"))
        }
        Request::Focus { name } | Request::Artifacts { name, .. } => (name.clone(), name.clone()),
    }
}

/// The last non-empty line of a failed verb's stderr.
fn failure(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .rfind(|line| !line.trim().is_empty())
        .unwrap_or("failed")
        .to_owned()
}

/// Runs verbs in the background; each finished one leaves a footer line in [`Runner::finished`].
pub struct Runner {
    tx: Tx,
    sender: Sender<String>,
    receiver: Receiver<String>,
}

impl Runner {
    pub fn new(tx: Tx) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            tx,
            sender,
            receiver,
        }
    }

    /// Starts `request`; the line to show while it runs.
    pub fn start(&self, request: &Request, words: Vec<String>) -> String {
        let (running, done) = lines(request);
        let mut command = self.tx.command(&words);
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let line = match command.output() {
                Ok(output) if output.status.success() => done,
                Ok(output) => failure(&output.stderr),
                Err(error) => error.to_string(),
            };
            // The view may be gone; nobody is left to tell.
            let _ = sender.send(line);
        });
        running
    }

    /// The line of a verb that finished since the last call.
    pub fn finished(&self) -> Option<String> {
        self.receiver.try_recv().ok()
    }
}
