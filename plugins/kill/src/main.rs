//! `tx-kill`: the `kill` plugin as its own program.

fn main() -> std::process::ExitCode {
    tx_plugin::sdk::main(vec![Box::new(tx_kill::KillPlugin)])
}
