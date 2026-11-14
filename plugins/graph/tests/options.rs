use tx_graph::verbs::parse_options;

fn parse(words: &[&str]) -> Result<tx_graph::verbs::Options, String> {
    let argv: Vec<String> = words.iter().map(|word| (*word).to_owned()).collect();
    parse_options(&argv)
}

#[test]
fn snapshot_flags_need_snapshot() {
    for flag in ["--cols", "--rows", "--now"] {
        assert_eq!(
            parse(&[flag, "50"]).unwrap_err(),
            format!("{flag} needs --snapshot")
        );
        assert!(parse(&[flag, "50", "--snapshot"]).is_ok(), "{flag}");
    }
    assert_eq!(
        parse(&["--rows", "9", "--cols", "50"]).unwrap_err(),
        "--rows needs --snapshot",
        "the first one is named"
    );
    assert!(parse(&["--tag", "x", "--last", "2h"]).is_ok());
}
