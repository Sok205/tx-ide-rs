//! The layout file: `panes:`, one record per pane in build order. A record is read into a flat
//! struct with every key optional (serde names an unknown key or a bad value with its line),
//! then checked by hand into a [`Pane`]: its content, and for every pane after the first where
//! it goes.
//!
//! ```yaml
//! panes:
//!   - agent: developer          # pane 0: the window itself
//!   - agent: qa
//!     model: claude-sonnet-5-5
//!     effort: 2
//!     split: right
//!     of: 0
//!     size: 50%
//!   - command: nvim .
//!     split: below
//!     of: 1
//!     size: 40%
//!   - split: below              # nothing chosen: a shell
//!     of: 0
//!     size: 30%
//! ```

use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use tx_core::EngineKind;

/// One pane of a layout, in file order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pane {
    pub content: Content,
    /// `None` for pane 0, which is the window itself.
    pub placement: Option<Placement>,
}

/// What a pane shows: exactly one of `agent:`, `attach:` and `command:`, or a shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Shell,
    /// A new agent running a definition.
    Agent(AgentPane),
    /// An existing agent, by name or id; a shell when it is not running.
    Attach(String),
    /// A program and its arguments, split on whitespace.
    Command(Vec<String>),
}

/// `agent:` and the keys valid only next to it. Every unset key takes its default when the
/// layout opens.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentPane {
    pub definition: String,
    pub prompt: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub engine: Option<EngineKind>,
    pub tags: Option<Vec<String>>,
    pub name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Split {
    Right,
    Below,
}

/// Where a pane after the first goes: `split` of an earlier pane `of`, taking `size` of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    pub split: Split,
    pub of: usize,
    /// tmux's `-l`: `N%` of the pane split, or a whole number of cells.
    pub size: String,
}

/// A layout file that cannot be used, naming the file.
#[derive(Debug, thiserror::Error)]
#[error("{}: {message}", path.display())]
pub struct FormatError {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    panes: Vec<Record>,
}

/// One pane as written. `agent`, `attach` and `command` refuse a bare or blank value: that is
/// a mistake, not a shell.
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Record {
    #[serde(deserialize_with = "present")]
    agent: Option<String>,
    #[serde(deserialize_with = "present")]
    attach: Option<String>,
    #[serde(deserialize_with = "present")]
    command: Option<String>,
    prompt: Option<String>,
    model: Option<String>,
    #[serde(deserialize_with = "tx_core::deserialize_optional_effort")]
    effort: Option<String>,
    engine: Option<EngineKind>,
    tags: Option<Vec<String>>,
    name: Option<String>,
    split: Option<Split>,
    of: Option<usize>,
    #[serde(deserialize_with = "size_text")]
    size: Option<String>,
}

fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    deserializer.deserialize_any(Present).map(Some)
}

struct Present;

impl Visitor<'_> for Present {
    type Value = String;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a non-blank string")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<String, E> {
        if value.trim().is_empty() {
            return Err(E::invalid_value(de::Unexpected::Str(value), &self));
        }
        Ok(value.to_owned())
    }
}

/// `size: 50%` or `size: 10`: a string or a whole number, kept as text.
fn size_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    deserializer.deserialize_any(SizeText).map(Some)
}

struct SizeText;

impl Visitor<'_> for SizeText {
    type Value = String;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a size: N% or a whole number of cells")
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<String, E> {
        Ok(value.to_string())
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<String, E> {
        Ok(value.to_owned())
    }
}

/// Layout names are `[A-Za-z0-9_.-]+`: a file stem, and a word the home's choice line can split.
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

/// The panes of the layout file at `path`, which holds `text`.
pub fn parse(path: &Path, text: &str) -> Result<Vec<Pane>, FormatError> {
    let failed = |message: String| FormatError {
        path: path.to_owned(),
        message,
    };
    let file: File = serde_norway::from_str(text).map_err(|error| failed(error.to_string()))?;
    if file.panes.is_empty() {
        return Err(failed(
            "`panes` is empty; a layout has at least one pane".to_owned(),
        ));
    }
    file.panes
        .into_iter()
        .enumerate()
        .map(|(index, record)| {
            pane(index, record).map_err(|message| failed(format!("pane {index}: {message}")))
        })
        .collect()
}

fn pane(index: usize, record: Record) -> Result<Pane, String> {
    let placement = placement(index, record.split, record.of, record.size)?;
    let chosen: Vec<&str> = [
        ("agent", record.agent.is_some()),
        ("attach", record.attach.is_some()),
        ("command", record.command.is_some()),
    ]
    .into_iter()
    .filter_map(|(key, set)| set.then_some(key))
    .collect();
    if chosen.len() > 1 {
        return Err(format!(
            "`{}` given together; a pane holds at most one of `agent`, `attach` and `command`",
            chosen.join("` and `")
        ));
    }
    if let Some(definition) = record.agent {
        let content = Content::Agent(AgentPane {
            definition,
            prompt: record.prompt,
            model: record.model,
            effort: record.effort,
            engine: record.engine,
            tags: record.tags,
            name: record.name,
        });
        return Ok(Pane { content, placement });
    }
    let agent_only = [
        ("prompt", record.prompt.is_some()),
        ("model", record.model.is_some()),
        ("effort", record.effort.is_some()),
        ("engine", record.engine.is_some()),
        ("tags", record.tags.is_some()),
        ("name", record.name.is_some()),
    ];
    if let Some((key, _)) = agent_only.iter().find(|(_, set)| *set) {
        return Err(format!("`{key}` is valid only next to `agent:`"));
    }
    let content = match (record.attach, record.command) {
        (Some(agent), _) => Content::Attach(agent),
        (_, Some(command)) => {
            Content::Command(command.split_whitespace().map(str::to_owned).collect())
        }
        (None, None) => Content::Shell,
    };
    Ok(Pane { content, placement })
}

fn placement(
    index: usize,
    split: Option<Split>,
    of: Option<usize>,
    size: Option<String>,
) -> Result<Option<Placement>, String> {
    if index == 0 {
        if split.is_some() || of.is_some() || size.is_some() {
            return Err(
                "the first pane is the window itself; it takes no `split`, `of` or `size`"
                    .to_owned(),
            );
        }
        return Ok(None);
    }
    let (Some(split), Some(of), Some(size)) = (split, of, size) else {
        return Err("every pane after the first needs `split`, `of` and `size`".to_owned());
    };
    if of >= index {
        return Err(format!(
            "`of: {of}` must name an earlier pane, 0 to {}",
            index - 1
        ));
    }
    if !is_valid_size(&size) {
        return Err(format!(
            "`size: {size}` is not N% (1 to 99) or a whole number of cells"
        ));
    }
    Ok(Some(Placement { split, of, size }))
}

fn is_valid_size(size: &str) -> bool {
    match size.strip_suffix('%') {
        Some(percent) => percent
            .parse::<u8>()
            .is_ok_and(|percent| (1..100).contains(&percent)),
        None => size.parse::<u16>().is_ok_and(|cells| cells > 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAIR: &str = "\
panes:
  - agent: developer
  - agent: qa
    model: claude-sonnet-5-5
    effort: 2
    prompt: Review every commit the developer makes.
    split: right
    of: 0
    size: 50%
  - command: nvim .
    split: below
    of: 1
    size: 40%
  - split: below
    of: 0
    size: 30%
";

    fn parsed(text: &str) -> Result<Vec<Pane>, String> {
        parse(Path::new("/c/layouts/x.yaml"), text).map_err(|error| error.to_string())
    }

    /// The error for a single later pane holding `keys`.
    fn later_pane_error(keys: &str) -> String {
        parsed(&format!(
            "panes:\n  - {{}}\n  - {keys}\n    split: below\n    of: 0\n    size: 10\n"
        ))
        .unwrap_err()
    }

    fn placed(split: Split, of: usize, size: &str) -> Option<Placement> {
        Some(Placement {
            split,
            of,
            size: size.to_owned(),
        })
    }

    #[test]
    fn the_four_pane_example_parses_in_file_order() {
        assert_eq!(
            parsed(PAIR).unwrap(),
            [
                Pane {
                    content: Content::Agent(AgentPane {
                        definition: "developer".to_owned(),
                        ..AgentPane::default()
                    }),
                    placement: None,
                },
                Pane {
                    content: Content::Agent(AgentPane {
                        definition: "qa".to_owned(),
                        prompt: Some("Review every commit the developer makes.".to_owned()),
                        model: Some("claude-sonnet-5-5".to_owned()),
                        effort: Some("2".to_owned()),
                        ..AgentPane::default()
                    }),
                    placement: placed(Split::Right, 0, "50%"),
                },
                Pane {
                    content: Content::Command(vec!["nvim".to_owned(), ".".to_owned()]),
                    placement: placed(Split::Below, 1, "40%"),
                },
                Pane {
                    content: Content::Shell,
                    placement: placed(Split::Below, 0, "30%"),
                },
            ]
        );
    }

    #[test]
    fn every_agent_key_is_read() {
        let panes = parsed(
            "panes:\n  - agent: qa\n    engine: codex\n    effort: high\n    tags: [a, b]\n    name: q1\n",
        )
        .unwrap();
        assert_eq!(
            panes[0].content,
            Content::Agent(AgentPane {
                definition: "qa".to_owned(),
                effort: Some("high".to_owned()),
                engine: Some(EngineKind::Codex),
                tags: Some(vec!["a".to_owned(), "b".to_owned()]),
                name: Some("q1".to_owned()),
                ..AgentPane::default()
            })
        );
    }

    #[test]
    fn nothing_chosen_is_a_shell_and_attach_names_an_agent() {
        let panes = parsed(
            "panes:\n  - {}\n  - attach: dev-2\n    split: right\n    of: 0\n    size: 20\n",
        )
        .unwrap();
        assert_eq!(panes[0].content, Content::Shell);
        assert_eq!(panes[1].content, Content::Attach("dev-2".to_owned()));
    }

    #[test]
    fn a_pane_holds_at_most_one_content() {
        assert_eq!(
            parsed("panes:\n  - agent: qa\n    command: nvim\n").unwrap_err(),
            "/c/layouts/x.yaml: pane 0: `agent` and `command` given together; a pane holds at \
             most one of `agent`, `attach` and `command`"
        );
        assert!(
            later_pane_error("attach: a\n    command: b")
                .contains("pane 1: `attach` and `command`")
        );
    }

    #[test]
    fn a_bare_or_blank_content_key_is_refused() {
        for text in [
            "panes:\n  - agent:\n",
            "panes:\n  - agent: ''\n",
            "panes:\n  - attach: ~\n",
            "panes:\n  - command: '  '\n",
        ] {
            let error = parsed(text).unwrap_err();
            assert!(error.starts_with("/c/layouts/x.yaml: "), "{error}");
            assert!(error.contains("line 2"), "{text}: {error}");
        }
    }

    #[test]
    fn agent_only_keys_are_refused_on_every_other_pane() {
        for key in [
            "prompt: hi",
            "model: m",
            "effort: 2",
            "engine: codex",
            "tags: [t]",
            "name: n",
        ] {
            let field = key.split(':').next().unwrap();
            for content in ["command: nvim\n    ", "attach: dev\n    ", ""] {
                assert_eq!(
                    later_pane_error(&format!("{content}{key}")),
                    format!("/c/layouts/x.yaml: pane 1: `{field}` is valid only next to `agent:`"),
                    "{content}{key}"
                );
            }
        }
    }

    #[test]
    fn placement_is_required_after_the_first_pane_and_refused_on_it() {
        assert_eq!(
            parsed("panes:\n  - split: right\n").unwrap_err(),
            "/c/layouts/x.yaml: pane 0: the first pane is the window itself; it takes no \
             `split`, `of` or `size`"
        );
        assert_eq!(
            parsed("panes:\n  - {}\n  - split: right\n    of: 0\n").unwrap_err(),
            "/c/layouts/x.yaml: pane 1: every pane after the first needs `split`, `of` and `size`"
        );
        assert_eq!(
            parsed("panes: []\n").unwrap_err(),
            "/c/layouts/x.yaml: `panes` is empty; a layout has at least one pane"
        );
    }

    #[test]
    fn of_names_an_earlier_pane() {
        // The pane itself, then a later one: `of` must be smaller than the pane's index.
        let refused_text = |of: usize| {
            format!(
                "panes:\n  - {{}}\n  - split: right\n    of: 0\n    size: 50%\n  - split: below\n    of: {of}\n    size: 10\n  - split: below\n    of: 0\n    size: 10\n"
            )
        };
        let refused = |of: usize| parsed(&refused_text(of)).unwrap_err();
        assert_eq!(
            parse(Path::new("/c/layouts/x.yaml"), &refused_text(1)).unwrap()[2].placement,
            placed(Split::Below, 1, "10")
        );
        assert_eq!(
            refused(2),
            "/c/layouts/x.yaml: pane 2: `of: 2` must name an earlier pane, 0 to 1"
        );
        assert_eq!(
            refused(3),
            "/c/layouts/x.yaml: pane 2: `of: 3` must name an earlier pane, 0 to 1"
        );
    }

    #[test]
    fn size_is_a_percentage_or_cells() {
        let size = |value: &str| {
            parsed(&format!(
                "panes:\n  - {{}}\n  - split: right\n    of: 0\n    size: {value}\n"
            ))
            .map(|panes| panes[1].placement.clone().unwrap().size)
        };
        assert_eq!(size("50%").unwrap(), "50%");
        assert_eq!(size("10").unwrap(), "10");
        assert_eq!(size("'10'").unwrap(), "10");
        for bad in ["0%", "100%", "0", "half", "-3", "5.5", "50 %"] {
            assert!(size(bad).is_err(), "{bad}");
        }
        assert_eq!(
            size("half").unwrap_err(),
            "/c/layouts/x.yaml: pane 1: `size: half` is not N% (1 to 99) or a whole number of cells"
        );
    }

    #[test]
    fn unknown_keys_name_the_file_and_a_pane_key_its_line() {
        for (text, key) in [
            ("panes:\n  - agent: qa\n    geometry: 5ae0\n", "geometry"),
            ("panes:\n  - agent: qa\n    rol: QA\n", "rol"),
        ] {
            let error = parsed(text).unwrap_err();
            assert!(error.starts_with("/c/layouts/x.yaml: "), "{error}");
            assert!(error.contains(&format!("unknown field `{key}`")), "{error}");
            assert!(error.contains("line 3"), "{error}");
        }
        // serde_norway gives no line for a top-level key; the file is still named.
        assert_eq!(
            parsed("geometry: 5ae0,200x50\npanes:\n  - {}\n").unwrap_err(),
            "/c/layouts/x.yaml: unknown field `geometry`, expected `panes`"
        );
    }

    #[test]
    fn a_bad_value_names_the_file_and_line() {
        for text in [
            "panes:\n  - split: left\n",
            "panes:\n  - agent: qa\n    engine: gpt\n",
            "panes:\n  - agent: qa\n    effort: [1]\n",
            "panes:\n  - {}\n  - split: right\n    of: -1\n    size: 10\n",
        ] {
            let error = parsed(text).unwrap_err();
            assert!(error.starts_with("/c/layouts/x.yaml: "), "{error}");
            assert!(error.contains("line "), "{text}: {error}");
        }
    }

    #[test]
    fn layout_names_are_a_file_stem_without_spaces() {
        for good in ["pair", "my_layout-2", "v1.2"] {
            assert!(is_valid_name(good), "{good}");
        }
        for bad in ["", "a b", "a/b", "é", "x:y"] {
            assert!(!is_valid_name(bad), "{bad}");
        }
    }
}
