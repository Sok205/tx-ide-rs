//! Gathering the home's [`Context`]: the agents, projects and views, the assistant's state,
//! plugin status and the cheat sheet from the core's registry. Reads only; another plugin is seen through its status, its resources and
//! `Agent.ext` (D3).

use std::path::PathBuf;

use tx_client::tmux::Ui;
use tx_client::views::ViewKind;
use tx_core::{Agent, CoreError, ListFilter, LlmEngineState, PluginStatus, TxCore};

use crate::model::{AgentRow, AssistantState, Context, KeyHint, Project, ViewRow};
use crate::project::ProjectCache;

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

const ASSISTANT_TAG: &str = "tx-system";
const ASSISTANT_ROLE: &str = "TX-ASSISTANT";
const ASSISTANT_PLUGIN: &str = "assistant";
const ASSISTANT_FAILURE_FILE: &str = "warm.failed";

/// The home's whole context. Cheap by design: no transcript, artifact or detail is read here
/// (details are asked for through `Home::needs_detail`, `previous.details` carries over); the
/// only slow step is the first git lookup per worktree, which the TUI makes on a worker thread.
pub fn context(
    core: &TxCore,
    ui: &Ui,
    cache: &mut ProjectCache,
    previous: Option<&Context>,
) -> Result<Context, CoreError> {
    let mut agents = records(core, &ListFilter::new().running(), true)?;
    agents.extend(records(core, &ListFilter::new().ended(), false)?);
    let mut projects: Vec<Project> = Vec::new();
    let mut rows = Vec::with_capacity(agents.len());
    for agent in &agents {
        let path = cache.main_checkout(&agent.working_directory);
        let project = match projects.iter().position(|project| project.path == path) {
            Some(index) => index,
            None => {
                projects.push(Project {
                    name: path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    path,
                });
                projects.len() - 1
            }
        };
        rows.push(AgentRow {
            id: agent.id.to_string(),
            name: agent.name.clone(),
            tag: agent.tags.first().cloned().unwrap_or_default(),
            state: agent.state,
            age_at: agent.last_activity_at.unwrap_or(agent.created_at),
            branch: cache.branch(&agent.working_directory),
            chats: cache.chats(agent.id.as_str()),
            project,
            is_assistant: is_assistant(agent),
        });
    }
    let views = tx_client::views::list(ui)
        .unwrap_or_default()
        .into_iter()
        .filter(|view| view.kind != ViewKind::Home)
        .map(|view| ViewRow {
            project: projects
                .iter()
                .enumerate()
                .filter(|(_, project)| view.working_directory.starts_with(&project.path))
                .max_by_key(|(_, project)| project.path.components().count())
                .map(|(index, _)| index),
            parent: view.parent_id.map(|parent| parent.to_string()),
            name: view.name,
        })
        .collect();
    let details = previous
        .map(|previous| {
            previous
                .details
                .iter()
                .filter(|(id, _)| rows.iter().any(|row| &row.id == *id))
                .map(|(id, detail)| (id.clone(), detail.clone()))
                .collect()
        })
        .unwrap_or_default();
    Ok(Context {
        assistant: assistant_from(core, &agents),
        projects,
        agents: rows,
        views,
        details,
        plugins: core
            .plugin_status()
            .iter()
            .map(|(id, status)| (id.clone(), *status == PluginStatus::Active))
            .collect(),
        keys: core
            .resources("keys/")
            .iter()
            .filter_map(|resource| {
                key_hint(&resource.plugin, &String::from_utf8_lossy(resource.bytes))
            })
            .collect(),
        now: tx_core::events::now(),
        home_directory: std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default(),
    })
}

/// The records `filter` lists; `live` ones are refreshed against their engine first. A record
/// that vanished between the listing and the read is skipped.
fn records(core: &TxCore, filter: &ListFilter, live: bool) -> Result<Vec<Agent>, CoreError> {
    Ok(core
        .list_agents(filter)?
        .iter()
        .filter_map(|id| {
            if live {
                core.refresh_agent(id)
                    .ok()
                    .map(|result| result.agent().clone())
            } else {
                core.get_agent(id).ok()
            }
        })
        .collect())
}

fn is_assistant(agent: &Agent) -> bool {
    agent.tags.iter().any(|tag| tag == ASSISTANT_TAG)
        && agent.roles.iter().any(|role| role == ASSISTANT_ROLE)
}

/// The assistant's state from the records of tag `tx-system` (or more) and the failure file
/// its last failed `warm` left.
pub fn assistant_state(core: &TxCore) -> AssistantState {
    let tagged = |filter: ListFilter, live| {
        records(core, &filter.tag(ASSISTANT_TAG), live).unwrap_or_default()
    };
    let mut agents = tagged(ListFilter::new().running(), true);
    agents.extend(tagged(ListFilter::new().ended(), false));
    assistant_from(core, &agents)
}

fn assistant_from(core: &TxCore, agents: &[Agent]) -> AssistantState {
    let assistants = || agents.iter().filter(|agent| is_assistant(agent));
    if let Some(running) = assistants().find(|agent| agent.state != LlmEngineState::Ended) {
        return match running.state {
            LlmEngineState::Starting => AssistantState::Starting,
            _ => AssistantState::Ready,
        };
    }
    let failure = core
        .config()
        .plugin_data_dir(ASSISTANT_PLUGIN)
        .join(ASSISTANT_FAILURE_FILE);
    if let Ok(reason) = std::fs::read_to_string(failure) {
        return AssistantState::Failed(reason.trim().to_owned());
    }
    match assistants().max_by(|a, b| a.last_modified_at.total_cmp(&b.last_modified_at)) {
        Some(newest) => AssistantState::Stopped {
            id: newest.id.to_string(),
        },
        None => AssistantState::Absent,
    }
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
