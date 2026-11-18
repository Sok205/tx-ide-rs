//! `tx-tutor`: the `tutor` plugin as its own program.

fn main() -> std::process::ExitCode {
    tx_plugin::sdk::main(vec![Box::new(tx_tutor::TutorPlugin)])
}
