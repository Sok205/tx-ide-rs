//! tx-core plugin `home-themes`: installs the backdrop themes into the home TUI. It starts with
//! the theme last chosen with `t` (kept in the plugin's data directory), else `settings.theme`
//! (default: the first of `THEMES`).

use std::fs;
use std::path::Path;

use tx_core::{Plugin, PluginError, Registry};
use tx_home::backdrop::{Catalogue, Constructor, install};

pub mod abyss;
pub mod campfire;
pub mod common;
pub mod fireflies;
pub mod garden;
pub mod koi;
pub mod life;
pub mod rain;
pub mod snow;
pub mod stars;

pub const PLUGIN_ID: &str = "home-themes";

/// The only place theme names appear; the first is the default.
pub const THEMES: &[(&str, Constructor)] = &[
    ("koi", koi::pond),
    ("stars", stars::sky),
    ("fireflies", fireflies::meadow),
    ("rain", rain::window),
    ("garden", garden::sand),
    ("abyss", abyss::deep),
    ("snow", snow::pines),
    ("campfire", campfire::embers),
    ("life", life::cells),
];

const OFF: &str = "off";

fn theme(settings: &serde_json::Value) -> Result<usize, String> {
    let Some(name) = settings.get("theme") else {
        return Ok(0);
    };
    name.as_str()
        .and_then(|name| THEMES.iter().position(|(known, _)| *known == name))
        .ok_or_else(|| {
            let known = THEMES.iter().map(|(name, _)| *name).collect::<Vec<_>>();
            format!(
                "settings.theme must be one of: {}, got {name}",
                known.join(", ")
            )
        })
}

/// The theme to start with; `None` is off. A remembered name that is still a theme wins, then
/// `settings.theme`.
fn start(settings: &serde_json::Value, remembered: Option<&str>) -> Result<Option<usize>, String> {
    if remembered == Some(OFF) {
        return Ok(None);
    }
    if let Some(index) = THEMES
        .iter()
        .position(|(name, _)| Some(*name) == remembered)
    {
        return Ok(Some(index));
    }
    theme(settings).map(Some)
}

fn save(file: &Path, choice: Option<&str>) {
    let write = || {
        fs::create_dir_all(file.parent().expect("the file sits in a directory"))?;
        fs::write(file, format!("{}\n", choice.unwrap_or(OFF)))
    };
    // Cosmetic: a choice that fails to save is just forgotten.
    let _ = write();
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HomeThemesPlugin;

impl Plugin for HomeThemesPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        let file = r.config().plugin_data_dir(PLUGIN_ID).join("theme");
        let remembered = fs::read_to_string(&file).ok();
        let start =
            start(r.settings(), remembered.as_deref().map(str::trim)).map_err(|reason| {
                PluginError::Register {
                    plugin: PLUGIN_ID.to_owned(),
                    reason,
                }
            })?;
        install(Catalogue {
            themes: THEMES,
            start,
            remember: Box::new(move |choice| save(&file, choice)),
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn no_setting_picks_the_first_theme() {
        assert_eq!(theme(&Value::Null), Ok(0));
        assert_eq!(theme(&json!({})), Ok(0));
    }

    #[test]
    fn a_known_name_picks_that_theme() {
        assert_eq!(theme(&json!({"theme": "koi"})), Ok(0));
    }

    #[test]
    fn an_unknown_name_lists_the_known_ones() {
        let known = THEMES
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(
            theme(&json!({"theme": "nope"})).unwrap_err(),
            format!(r#"settings.theme must be one of: {known}, got "nope""#)
        );
    }

    #[test]
    fn koi_stays_the_default() {
        assert_eq!(THEMES[0].0, "koi");
    }

    #[test]
    fn a_non_string_is_refused() {
        assert!(theme(&json!({"theme": 3})).unwrap_err().contains("got 3"));
    }

    #[test]
    fn a_remembered_theme_wins_over_the_setting() {
        assert_eq!(start(&json!({"theme": "nope"}), Some("koi")), Ok(Some(0)));
    }

    #[test]
    fn a_remembered_off_starts_plain() {
        assert_eq!(start(&json!({"theme": "koi"}), Some("off")), Ok(None));
    }

    #[test]
    fn a_stale_remembered_name_falls_back() {
        assert_eq!(start(&Value::Null, Some("removed")), Ok(Some(0)));
        assert_eq!(start(&json!({"theme": "koi"}), Some("")), Ok(Some(0)));
    }

    #[test]
    fn an_unknown_setting_still_errors_without_a_remembered_choice() {
        assert!(start(&json!({"theme": "nope"}), None).is_err());
        assert!(start(&json!({"theme": "nope"}), Some("removed")).is_err());
    }

    #[test]
    fn a_choice_is_saved_with_a_newline_and_off_too() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("data/theme");
        save(&file, Some("koi"));
        assert_eq!(fs::read_to_string(&file).unwrap(), "koi\n");
        save(&file, None);
        assert_eq!(fs::read_to_string(&file).unwrap(), "off\n");
    }
}
