//! `tx update`: the core's pending update steps, then every active plugin's updater; and the
//! warning every other verb prints while a step is pending.

use std::io::Write;
use std::process::ExitCode;

use tx_core::Directories;
use tx_core::updates::{self, Applied};

use crate::args::Global;
use crate::setup::{Failure, core_config, open_with};

const RESPAWN: &str = "respawn running agents to pick this up";

/// The core's update steps first: opening the core creates `<data>/agents`, which would make a
/// fresh install look used. A failed step stops there. Then every active plugin's updater, in
/// manifest order, its output prefixed with its plugin id; a failed updater is named and the
/// others still run, since they are independent. Exit 1 on any failure.
pub fn run(global: &Global) -> Result<ExitCode, Failure> {
    let config = core_config(global)?;
    let mut out = std::io::stdout().lock();
    let mut respawn = false;
    let mut any = false;
    let result = updates::apply_pending(&config.directories, |applied: Applied| {
        any = true;
        respawn |= applied.outcome.respawn_agents;
        writeln!(out, "update {}: {}", applied.version, applied.description).ok();
    });
    if let Err(error) = result {
        if respawn {
            writeln!(out, "{RESPAWN}").ok();
        }
        return Err(Failure::Update(error));
    }
    if !any {
        writeln!(
            out,
            "update steps are at {}: nothing pending",
            updates::latest()
        )
        .ok();
    }
    let core = open_with(global, config)?;
    let mut failed = false;
    for (plugin, updater) in core.updaters() {
        let mut stdout = Vec::new();
        let mut stdin = std::io::empty();
        let mut stderr = std::io::stderr().lock();
        let mut io = tx_core::VerbIo {
            stdin: &mut stdin,
            stdout: &mut stdout,
            stderr: &mut stderr,
        };
        let result = updater.update(&core, &mut io);
        for line in String::from_utf8_lossy(&stdout).lines() {
            writeln!(out, "{plugin}: {line}").ok();
        }
        match result {
            Ok(outcome) => respawn |= outcome.respawn_agents,
            Err(error) => {
                eprintln!("tx: update: {plugin}: {error}");
                failed = true;
            }
        }
    }
    if respawn {
        writeln!(out, "{RESPAWN}").ok();
    }
    Ok(if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// On stderr, once: update steps this tx knows have not run here. Never refuses; a stamp that
/// cannot be read is named the same way.
pub fn warn_if_behind(directories: &Directories) {
    match updates::behind(directories) {
        Ok(None) => {}
        Ok(Some((current, latest))) => {
            let pending = latest - current;
            let steps = if pending == 1 { "step" } else { "steps" };
            eprintln!("tx: {pending} update {steps} pending: run tx update");
        }
        Err(error) => eprintln!("tx: {error}"),
    }
}
