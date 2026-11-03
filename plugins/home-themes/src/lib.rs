//! tx-core plugin `home-themes`: installs the backdrop theme named by `settings.theme` (default:
//! the first of `THEMES`) into the home TUI.

use tx_core::{Plugin, PluginError, Registry};
use tx_home::backdrop::{Constructor, install};

pub mod koi;

pub const PLUGIN_ID: &str = "home-themes";

/// The only place theme names appear; the first is the default.
pub const THEMES: &[(&str, Constructor)] = &[("koi", koi::pond)];

fn theme(settings: &serde_json::Value) -> Result<Constructor, String> {
    let Some(name) = settings.get("theme") else {
        return Ok(THEMES[0].1);
    };
    name.as_str()
        .and_then(|name| THEMES.iter().find(|(known, _)| *known == name))
        .map(|(_, constructor)| *constructor)
        .ok_or_else(|| {
            let known = THEMES.iter().map(|(name, _)| *name).collect::<Vec<_>>();
            format!(
                "settings.theme must be one of: {}, got {name}",
                known.join(", ")
            )
        })
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HomeThemesPlugin;

impl Plugin for HomeThemesPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        let constructor = theme(r.settings()).map_err(|reason| PluginError::Register {
            plugin: PLUGIN_ID.to_owned(),
            reason,
        })?;
        install(constructor);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn is_pond(constructor: Constructor) -> bool {
        std::ptr::fn_addr_eq(constructor, koi::pond as Constructor)
    }

    #[test]
    fn no_setting_picks_the_first_theme() {
        assert!(is_pond(theme(&Value::Null).unwrap()));
        assert!(is_pond(theme(&json!({})).unwrap()));
    }

    #[test]
    fn a_known_name_picks_that_theme() {
        assert!(is_pond(theme(&json!({"theme": "koi"})).unwrap()));
    }

    #[test]
    fn an_unknown_name_lists_the_known_ones() {
        assert_eq!(
            theme(&json!({"theme": "nope"})).err().unwrap(),
            r#"settings.theme must be one of: koi, got "nope""#
        );
    }

    #[test]
    fn a_non_string_is_refused() {
        assert!(theme(&json!({"theme": 3})).err().unwrap().contains("got 3"));
    }
}
