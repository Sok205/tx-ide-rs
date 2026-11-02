//! Gathering the home's [`Context`]: plugin status and the cheat sheet from the core's
//! registry. Reads only; another plugin is seen through its status, its resources and
//! `Agent.ext` (D3).

use std::path::PathBuf;

use tx_core::{CoreError, PluginStatus, TxCore};

use crate::model::{AssistantState, Context, KeyHint};

/// `# keys:` and the first prose comment of a key fragment, up to its first sentence; `None`
/// when the fragment declares no key.
pub fn key_hint(plugin: &str, text: &str) -> Option<KeyHint> {
    let mut keys = Vec::new();
    let mut prose: Vec<&str> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# keys:") {
            keys.extend(
                rest.split_whitespace()
                    .filter(|word| *word != "none")
                    .map(str::to_owned),
            );
            continue;
        }
        if ["#!", "# unset:", "# unhook:"]
            .iter()
            .any(|prefix| line.starts_with(prefix))
        {
            continue;
        }
        let words = line.strip_prefix('#').map(str::trim);
        match words {
            Some(words) if !words.is_empty() => prose.push(words),
            _ if !prose.is_empty() => break,
            _ => {}
        }
    }
    if keys.is_empty() {
        return None;
    }
    let joined = prose.join(" ");
    let mut text = joined.as_str();
    if let Some((_, rest)) = text
        .strip_prefix("tx key plugin `")
        .and_then(|rest| rest.split_once("`:"))
    {
        text = rest.trim_start();
    }
    let sentence = text.split(". ").next().unwrap_or_default();
    Some(KeyHint {
        plugin: plugin.to_owned(),
        keys,
        text: sentence.trim_end_matches('.').to_owned(),
    })
}

/// The plugin list and the cheat sheet from the core's registry; the sessions come in Task 7.
pub fn context(core: &TxCore) -> Result<Context, CoreError> {
    let plugins = core
        .plugin_status()
        .iter()
        .map(|(id, status)| (id.clone(), *status == PluginStatus::Active))
        .collect();
    let keys = core
        .resources("keys/")
        .iter()
        .filter_map(|resource| key_hint(&resource.plugin, &String::from_utf8_lossy(resource.bytes)))
        .collect();
    Ok(Context {
        plugins,
        keys,
        now: tx_core::events::now(),
        home_directory: std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default(),
        assistant: AssistantState::Absent,
        ..Context::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fragment_hint_is_its_keys_and_first_sentence() {
        let text = "#!/usr/bin/env bash\n# keys: t\n# tx key plugin `picker`: prefix+t opens the \
                    tx session picker in a popup. `tx pick` finds the\n# pane to nest into.\n\
                    #\n# The popup is wide.\nset -u\n";
        assert_eq!(
            key_hint("picker", text),
            Some(KeyHint {
                plugin: "picker".into(),
                keys: vec!["t".into()],
                text: "prefix+t opens the tx session picker in a popup".into(),
            })
        );
    }

    #[test]
    fn declarations_are_not_prose_and_several_keys_add_up() {
        let text = "#!/usr/bin/env bash\n# keys: s\n# unset: pane-border-format\n\
                    # unhook: after-new-window\n# keys: S\n# tx key plugin `labels`: prefix+s \
                    lists sessions\n# with their tags\nset -u\n";
        let hint = key_hint("labels", text).unwrap();
        assert_eq!(hint.keys, ["s", "S"]);
        assert_eq!(hint.text, "prefix+s lists sessions with their tags");
    }

    #[test]
    fn a_fragment_without_keys_has_no_hint() {
        assert_eq!(key_hint("x", "#!/bin/sh\n# keys: none\n# words\n"), None);
        assert_eq!(key_hint("x", "#!/bin/sh\n# words\n"), None);
    }

    #[test]
    fn the_shipped_fragment_has_a_hint() {
        let hint = key_hint("home", include_str!("../tmux/keys.tmux")).unwrap();
        assert_eq!(hint.keys, ["m"]);
        assert!(hint.text.starts_with("prefix+m opens"), "{}", hint.text);
    }
}
