//! Read-time effort-group resolution, ported from main's `grouping.rs`.
//!
//! ```text
//! resolved(session)  = own override (ext.edit.group, else ext.artifacts.group)
//!                      ?? first override up the parent chain (stops at hubs and cycles)
//!                      ?? the bound artifact's group (legacy nvim records' ext.artifacts.bound)
//!                      ?? tags[0]
//!                      ?? session.name
//! resolved(view)     = first override up the parent chain (from the view's parent agent)
//!                      ?? the bound artifact's group (@tx-view-bound)
//!                      ?? tags[0]
//!                      ?? view.name
//! resolved(artifact) = artifact.group
//!                      ?? resolved(creator = history[0].session_id)
//!                      ?? newest surviving toucher's resolved group
//!                      ?? "ungrouped"
//! ```
//!
//! The two cascades recurse into each other; the cycle through artifacts is guarded by the set of
//! artifact ids under resolution. Parents are ids in tx-core, so main's by-name parent lookup is
//! gone. A malformed `ext` entry reads as absent.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tx_client::views::View;
use tx_core::Session;

use crate::PLUGIN_ID;
use crate::artifact::{Artifact, USER_ACTOR};

/// Display fallback when no touch author survives.
pub const UNGROUPED: &str = "ungrouped";

/// The parent walk stops AT a hub, so one effort's group never leaks through it.
pub const HUB_SESSION_NAMES: [&str; 1] = ["tx-assistant"];

/// The prefix+e editor's key; its `group` is the operator's override and wins.
const EDIT_PLUGIN: &str = "edit";

/// `ext["artifacts"]` on a session record.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ext {
    /// Legacy, PR 11 deletes: the artifact an nvim view shows, on records written before
    /// `artifact open` made tx-client views (which carry it as `@tx-view-bound`). Read, never
    /// written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound: Option<String>,
    /// The group override `txc group` sets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// The session's `ext["artifacts"]`; `Err` names what is wrong with a malformed one.
pub fn ext_of(session: &Session) -> Result<Ext, String> {
    session
        .ext::<Ext>(PLUGIN_ID)
        .map(Option::unwrap_or_default)
        .map_err(|error| {
            format!(
                "session {} has a malformed ext.{PLUGIN_ID}: {error}",
                session.name
            )
        })
}

/// Legacy, PR 11 deletes: the artifact a core record is bound to; `None` for a malformed record.
pub fn bound_artifact(session: &Session) -> Option<String> {
    ext_of(session).ok()?.bound
}

fn non_empty(value: Option<&Value>) -> Option<&str> {
    value?.as_str().filter(|group| !group.is_empty())
}

/// The session's own override: the prefix+e editor's, else `txc group`'s.
pub fn own_group(session: &Session) -> Option<String> {
    let edit = session
        .ext
        .get(EDIT_PLUGIN)
        .and_then(|edit| edit.get("group"));
    let ours = session
        .ext
        .get(PLUGIN_ID)
        .and_then(|ours| ours.as_object())
        .and_then(|ours| ours.get("group"));
    non_empty(edit).or(non_empty(ours)).map(str::to_owned)
}

fn is_hub(session: &Session) -> bool {
    HUB_SESSION_NAMES.contains(&session.name.as_str())
}

fn set(group: &Option<String>) -> Option<&str> {
    group.as_deref().filter(|group| !group.is_empty())
}

pub struct GroupResolver<'a> {
    sessions_by_id: HashMap<&'a str, &'a Session>,
    artifacts_by_id: HashMap<&'a str, &'a Artifact>,
}

impl<'a> GroupResolver<'a> {
    pub fn new(sessions: &'a [Session], artifacts: &'a [Artifact]) -> Self {
        Self {
            sessions_by_id: sessions.iter().map(|s| (s.id.as_str(), s)).collect(),
            artifacts_by_id: artifacts.iter().map(|a| (a.id.as_str(), a)).collect(),
        }
    }

    /// The session's effective group; the name is the floor.
    pub fn session_group(&self, session: &Session) -> String {
        self.session_group_guarded(session, &HashSet::new())
    }

    /// The artifact's effective group, [`UNGROUPED`] when nothing survives.
    pub fn artifact_group(&self, artifact: &Artifact) -> String {
        self.artifact_override(artifact, &HashSet::new())
            .unwrap_or_else(|| UNGROUPED.into())
    }

    fn session_group_guarded(&self, session: &Session, resolving: &HashSet<&'a str>) -> String {
        if let Some(derived) = self.session_override(session, resolving) {
            return derived;
        }
        session.tags.first().unwrap_or(&session.name).to_owned()
    }

    fn parent(&self, session: &Session) -> Option<&'a Session> {
        let parent = session.parent.as_ref()?;
        self.sessions_by_id.get(parent.as_str()).copied()
    }

    /// A view's effective group; a view has no override of its own, and the name is the floor.
    pub fn view_group(&self, view: &View) -> String {
        let parent = view
            .parent_id
            .as_ref()
            .and_then(|parent| self.sessions_by_id.get(parent.as_str()).copied());
        self.ancestor_override(parent, HashSet::new())
            .or_else(|| self.bound_override(view.bound.as_deref(), &HashSet::new()))
            .unwrap_or_else(|| view.tags.first().unwrap_or(&view.name).to_owned())
    }

    /// Own override, first override up the parent chain, or the bound artifact's. Only overrides
    /// inherit; an ancestor's tags never leak down.
    fn session_override(&self, session: &Session, resolving: &HashSet<&'a str>) -> Option<String> {
        if let Some(group) = own_group(session) {
            return Some(group);
        }
        if !is_hub(session) {
            let visited = HashSet::from([session.id.as_str()]);
            if let Some(group) = self.ancestor_override(self.parent(session), visited) {
                return Some(group);
            }
        }
        self.bound_override(bound_artifact(session).as_deref(), resolving)
    }

    /// The first override from `ancestor` up the parent chain; stops after a hub and at a cycle.
    fn ancestor_override<'s>(
        &self,
        mut ancestor: Option<&'a Session>,
        mut visited: HashSet<&'s str>,
    ) -> Option<String>
    where
        'a: 's,
    {
        while let Some(current) = ancestor {
            if visited.contains(current.id.as_str()) {
                break;
            }
            if let Some(group) = own_group(current) {
                return Some(group);
            }
            if is_hub(current) {
                break;
            }
            visited.insert(current.id.as_str());
            ancestor = self.parent(current);
        }
        None
    }

    /// The bound artifact's override, unless that artifact is already being resolved.
    fn bound_override(&self, bound: Option<&str>, resolving: &HashSet<&'a str>) -> Option<String> {
        let artifact = self.artifacts_by_id.get(bound?)?;
        if resolving.contains(artifact.id.as_str()) {
            return None;
        }
        self.artifact_override(artifact, resolving)
    }

    /// Own override, else the creator's full resolution, else the newest surviving toucher's.
    fn artifact_override(
        &self,
        artifact: &'a Artifact,
        resolving: &HashSet<&'a str>,
    ) -> Option<String> {
        if let Some(group) = set(&artifact.group) {
            return Some(group.to_owned());
        }
        let mut resolving = resolving.clone();
        resolving.insert(&artifact.id);
        let history = artifact.history();
        let creator_first = history.iter().take(1).chain(history.iter().skip(1).rev());
        for touch in creator_first {
            if touch.session_id == USER_ACTOR {
                continue;
            }
            if let Some(author) = self.sessions_by_id.get(touch.session_id.as_str()) {
                return Some(self.session_group_guarded(author, &resolving));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn session(id: &str, name: &str, extra: Value) -> Session {
        let mut record = json!({
            "version": 1, "id": id, "name": name, "parent": null, "tags": [], "cwd": "/r",
            "created_at": 1.0, "ended_at": null, "last_modified_at": 1.0, "lifecycle": "running",
            "launch": {"kind": "process", "program": "sleep", "args": []},
        });
        for (key, value) in extra.as_object().unwrap() {
            record[key] = value.clone();
        }
        Session::from_json(&record.to_string()).unwrap()
    }

    fn group(name: &str) -> Value {
        json!({"artifacts": {"group": name}})
    }

    fn artifact(id: &str, group: Option<&str>, authors: &[&str]) -> Artifact {
        let history: Vec<Value> = authors
            .iter()
            .enumerate()
            .map(|(rev, author)| json!({"session_id": author, "at": 1000.0, "rev": rev, "changes": null}))
            .collect();
        Artifact::from_value(&json!({
            "artifact_schema_version": 2, "id": id, "title": null, "filename": "f.md",
            "created_at": 1000.0, "group": group, "history": history,
        }))
        .unwrap()
    }

    /// Main's fixture G: assist(a1) ← worker(b1, tags feat-x,extra) ← child(c1); hub
    /// tx-assistant(h1) ← up(x1, group upgrp).
    fn fixture_g(a1_parent: Option<&str>, hub_group: Option<&str>) -> Vec<Session> {
        let hub_ext = hub_group.map_or(json!({}), group);
        vec![
            session("a1", "assist", json!({"parent": a1_parent})),
            session(
                "b1",
                "worker",
                json!({"tags": ["feat-x", "extra"], "parent": "a1"}),
            ),
            session("c1", "child", json!({"parent": "b1"})),
            session(
                "h1",
                "tx-assistant",
                json!({"ext": hub_ext, "parent": "x1"}),
            ),
            session("x1", "up", json!({"ext": group("upgrp")})),
        ]
    }

    fn group_of(sessions: &[Session], artifacts: &[Artifact], id: &str) -> String {
        let resolver = GroupResolver::new(sessions, artifacts);
        let session = sessions.iter().find(|s| s.id.as_str() == id).unwrap();
        resolver.session_group(session)
    }

    #[test]
    fn tags_then_name_floor_and_no_tag_inheritance() {
        let sessions = fixture_g(None, Some("hubgrp"));
        assert_eq!(group_of(&sessions, &[], "b1"), "feat-x");
        assert_eq!(group_of(&sessions, &[], "a1"), "assist");
        assert_eq!(group_of(&sessions, &[], "c1"), "child");
    }

    #[test]
    fn hub_stops_the_walk() {
        let sessions = fixture_g(Some("h1"), Some("hubgrp"));
        assert_eq!(group_of(&sessions, &[], "c1"), "hubgrp");
        let sessions = fixture_g(Some("h1"), None);
        assert_eq!(group_of(&sessions, &[], "c1"), "child");
        assert_eq!(group_of(&sessions, &[], "h1"), "tx-assistant");
    }

    #[test]
    fn parent_cycle_terminates() {
        let sessions = vec![
            session("p1", "P", json!({"parent": "q1"})),
            session("q1", "Q", json!({"parent": "p1"})),
        ];
        assert_eq!(group_of(&sessions, &[], "p1"), "P");
    }

    #[test]
    fn the_editor_override_wins_over_ours() {
        let sessions = vec![session(
            "e1",
            "E",
            json!({"ext": {"edit": {"group": "edited"}, "artifacts": {"group": "ours"}}}),
        )];
        assert_eq!(group_of(&sessions, &[], "e1"), "edited");
        let sessions = vec![session(
            "e1",
            "E",
            json!({"ext": {"edit": {"group": null}, "artifacts": {"group": "ours"}}}),
        )];
        assert_eq!(group_of(&sessions, &[], "e1"), "ours");
    }

    #[test]
    fn a_malformed_ext_reads_as_absent() {
        let sessions = vec![
            session("m1", "M", json!({"ext": {"artifacts": "junk", "edit": 5}})),
            session("m2", "M2", json!({"ext": {"artifacts": {"bound": 7}}})),
        ];
        assert_eq!(group_of(&sessions, &[], "m1"), "M");
        assert_eq!(group_of(&sessions, &[], "m2"), "M2");
        assert!(ext_of(&sessions[0]).unwrap_err().contains("malformed"));
    }

    #[test]
    fn artifact_cascade_and_recursion_guard() {
        let sessions = fixture_g(None, None);
        let resolve = |authors: &[&str], group: Option<&str>| {
            let artifacts = [artifact("art", group, authors)];
            GroupResolver::new(&sessions, &artifacts).artifact_group(&artifacts[0])
        };
        assert_eq!(resolve(&["user", "c1", "b1"], None), "feat-x");
        assert_eq!(resolve(&["user", "gone", "user"], None), UNGROUPED);
        assert_eq!(resolve(&["c1", "b1"], None), "child");
        assert_eq!(resolve(&["user", "b1"], Some("g")), "g");

        let sessions = [session(
            "v1",
            "view",
            json!({"ext": {"artifacts": {"bound": "art3"}}}),
        )];
        let artifacts = [artifact("art3", None, &["v1"])];
        let resolver = GroupResolver::new(&sessions, &artifacts);
        assert_eq!(resolver.session_group(&sessions[0]), "view");
        assert_eq!(resolver.artifact_group(&artifacts[0]), "view");
    }

    fn view(parent: Option<&str>, bound: Option<&str>, tags: &[&str]) -> View {
        View {
            name: "plan-md".to_owned(),
            kind: tx_client::views::ViewKind::Editor,
            working_directory: "/r".into(),
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            parent_id: parent.map(|parent| parent.parse().unwrap()),
            bound: bound.map(str::to_owned),
            created_at: 1.0,
        }
    }

    #[test]
    fn a_tx_client_view_resolves_through_its_parent_then_its_artifact() {
        let mut sessions = fixture_g(Some("h1"), Some("hubgrp"));
        sessions.push(session("w1", "writer", json!({"ext": group("effort")})));
        sessions.push(session("l1", "lone", json!({"tags": ["lone-tag"]})));
        let artifacts = [artifact("art", None, &["w1"])];
        let resolver = GroupResolver::new(&sessions, &artifacts);
        // The parent chain's override first (here up through assist to the hub), …
        assert_eq!(
            resolver.view_group(&view(Some("c1"), Some("art"), &[])),
            "hubgrp"
        );
        // … then the bound artifact's group, …
        assert_eq!(
            resolver.view_group(&view(Some("l1"), Some("art"), &["t"])),
            "effort"
        );
        assert_eq!(resolver.view_group(&view(None, Some("art"), &[])), "effort");
        // … then its own tags, never the parent's, then its name.
        assert_eq!(resolver.view_group(&view(Some("l1"), None, &["t"])), "t");
        assert_eq!(
            resolver.view_group(&view(Some("l1"), Some("gone"), &[])),
            "plan-md"
        );
    }

    /// Legacy, PR 11 deletes: a core record bound through `ext.artifacts.bound`.
    #[test]
    fn a_view_takes_its_artifacts_group() {
        let sessions = [
            session("w1", "worker", json!({"ext": group("effort")})),
            session(
                "v1",
                "view",
                json!({"ext": {"artifacts": {"bound": "art"}}}),
            ),
        ];
        let artifacts = [artifact("art", None, &["w1"])];
        assert_eq!(group_of(&sessions, &artifacts, "v1"), "effort");
    }
}
