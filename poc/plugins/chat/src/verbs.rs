//! `chat ls <session>`: the session's current conversation, asked from its engine
//! (`TxCore::conversation`). `doctor --fix-hooks`: removes the global hook entries the
//! retired chat-id capture and transcript mirror installed, and only those.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use tx_core::{Conversation, CoreError, TxCore, Verb, VerbError, VerbIo};

use crate::hooks_cleanup::{RETIRED_SIDECARS, uninstall};

const CHAT_USAGE: &str = "usage: txc chat ls <session>\n";
const DOCTOR_USAGE: &str = "usage: txc doctor --fix-hooks\n";

pub fn verbs() -> Vec<Arc<dyn Verb>> {
    vec![Arc::new(Chat), Arc::new(Doctor)]
}

fn say(out: &mut dyn Write, text: &str) -> Result<(), VerbError> {
    out.write_all(text.as_bytes())
        .map_err(|error| VerbError::Failed(format!("write: {error}")))
}

/// `chat ls`: the conversation line for a session called `name`.
pub fn render_conversation(name: &str, conversation: Option<&Conversation>) -> String {
    match conversation {
        Some(conversation) => format!(
            "{name} — conversation {}\n  transcript {}",
            conversation.id,
            conversation.transcript_path.display()
        ),
        None => format!("{name} — no conversation yet"),
    }
}

struct Chat;

impl Verb for Chat {
    fn name(&self) -> &'static str {
        "chat"
    }

    fn usage(&self) -> &'static str {
        "chat ls <session>   a session's current conversation"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [verb, token] = argv else {
            say(io.stderr, CHAT_USAGE)?;
            return Ok(2);
        };
        if verb != "ls" {
            say(io.stderr, CHAT_USAGE)?;
            return Ok(2);
        }
        let id = match core.resolve(token) {
            Ok(id) => id,
            Err(CoreError::NotFound(_)) => {
                say(
                    io.stderr,
                    &format!("chat ls: session '{token}' not found\n"),
                )?;
                return Ok(1);
            }
            Err(error) => return Err(error.into()),
        };
        let session = core.get_session(&id)?;
        let conversation = core.conversation(&id)?;
        say(
            io.stdout,
            &format!(
                "{}\n",
                render_conversation(&session.name, conversation.as_ref())
            ),
        )?;
        Ok(0)
    }
}

struct Doctor;

impl Verb for Doctor {
    fn name(&self) -> &'static str {
        "doctor"
    }

    fn usage(&self) -> &'static str {
        "doctor --fix-hooks   remove the global agent hooks older versions installed"
    }

    fn run(&self, _core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        if argv != ["--fix-hooks"] {
            say(io.stderr, DOCTOR_USAGE)?;
            return Ok(2);
        }
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| VerbError::Failed("HOME is not set".into()))?;
        for sidecar in RETIRED_SIDECARS {
            uninstall(&home, sidecar, io.stdout)
                .map_err(|error| VerbError::Failed(error.to_string()))?;
        }
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_known_and_an_unknown_conversation() {
        let conversation = Conversation {
            id: "c1".into(),
            transcript_path: PathBuf::from("/t/c1.jsonl"),
        };
        assert_eq!(
            render_conversation("alice", Some(&conversation)),
            "alice — conversation c1\n  transcript /t/c1.jsonl"
        );
        assert_eq!(
            render_conversation("alice", None),
            "alice — no conversation yet"
        );
    }
}
