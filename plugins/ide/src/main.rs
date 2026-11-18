//! `tx-ide`: the `ide` plugin as its own program: its verbs, and its updater (`--tx-update`).

fn main() -> std::process::ExitCode {
    tx_plugin::sdk::main(vec![Box::new(tx_ide::IdePlugin)])
}
