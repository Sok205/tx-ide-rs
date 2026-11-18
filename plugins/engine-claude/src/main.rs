//! `tx-engine-claude`: the `engine-claude` plugin as its own program. It serves the Claude
//! engine to core (`--tx-engine`) and is the hook command of the agents it launches
//! (`--tx-hook <Event>`).

fn main() -> std::process::ExitCode {
    tx_plugin::sdk::main(vec![Box::new(tx_engine_claude::ClaudePlugin)])
}
