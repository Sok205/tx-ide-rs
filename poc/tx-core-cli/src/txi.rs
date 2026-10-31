//! `txi`: tx's internal entry point. Machines run it, people do not: nothing here is listed in
//! `txc help`. The one verb today is `txi hook <Event>`, which the hooks an engine adapter wires
//! into its CLI run. It opens the same core as `txc` (the `TX_*_DIR` and socket every session carries).
//!
//! A hook must never fail the agent: anything that is not ours (outside a session, a malformed
//! id, an unknown record) exits 0, and the wired command caps every other failure at exit 1.

use std::process::ExitCode;

use tx_core::core::SESSION_ID_ENV;
use tx_core::{CoreError, LlmEngineHook, SessionId};
use tx_core_cli::args::Global;
use tx_core_cli::setup::{Failure, open};

const USAGE: &str = "\
usage: txi hook <Event>   (run by the hooks an engine wires) feed $TX_SESSION_ID's state; payload on stdin

txi is tx's internal entry point; people use txc.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        // Extra arguments are ignored, not refused: a hook must never fail over its own
        // command line.
        Some("hook") => match hook(args.get(1).cloned().unwrap_or_default()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("txi: {error}");
                ExitCode::from(1)
            }
        },
        _ => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn hook(name: String) -> Result<(), Failure> {
    let Some(id) = std::env::var(SESSION_ID_ENV)
        .ok()
        .and_then(|raw| raw.parse::<SessionId>().ok())
    else {
        return Ok(());
    };
    let core = open(&Global::default())?;
    let hook = LlmEngineHook {
        name,
        payload: read_payload(&mut std::io::stdin().lock()),
        at: tx_core::events::now(),
    };
    match core.hook(&id, hook) {
        Ok(_) | Err(CoreError::NotFound(_)) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// A hook's payload, tolerantly: `{}` for none, unreadable, bad JSON or a non-object. The hook
/// must never fail the agent over its own input.
fn read_payload(stdin: &mut dyn std::io::Read) -> serde_json::Value {
    let mut text = String::new();
    if stdin.read_to_string(&mut text).is_err() {
        return serde_json::json!({});
    }
    match serde_json::from_str(&text) {
        Ok(value @ serde_json::Value::Object(_)) => value,
        _ => serde_json::json!({}),
    }
}
