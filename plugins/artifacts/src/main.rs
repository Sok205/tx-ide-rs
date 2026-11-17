//! `tx-artifacts`: the `artifacts` plugin as its own program.

fn main() -> std::process::ExitCode {
    tx_plugin::sdk::main(vec![Box::new(tx_artifacts::ArtifactsPlugin)])
}
