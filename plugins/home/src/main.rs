//! `tx-home`: the `home` plugin as its own program. It hosts `home-themes` too: the themes hook
//! into the home in-process, so they are registered here, when that plugin is installed and
//! enabled.

fn main() -> std::process::ExitCode {
    tx_plugin::sdk::main(vec![
        Box::new(tx_home::HomePlugin),
        Box::new(tx_home_themes::HomeThemesPlugin),
    ])
}
