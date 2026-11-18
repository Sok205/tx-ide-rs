//! `tx-engine-codex`: the `engine-codex` plugin as its own program. It serves the Codex engine
//! to core (`--tx-engine`) and is the hook command of the agents it launches
//! (`--tx-hook <Event>`).

fn main() -> std::process::ExitCode {
    tx_plugin::sdk::main(vec![Box::new(tx_engine_codex::CodexPlugin)])
}
