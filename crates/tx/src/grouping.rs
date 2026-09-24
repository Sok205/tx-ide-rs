//! Port of lib/tx/grouping.py — read-time effort-group resolution.
//!
//! ```text
//! resolved(session)  = session.group
//!                      ?? first override up the parent chain (stops at hubs and cycles)
//!                      ?? the bound artifact's group (nvim views from `tx artifact open`)
//!                      ?? tags[0]
//!                      ?? session.name
//! resolved(artifact) = artifact.group
//!                      ?? resolved(creator = history[0].session_id)
//!                      ?? newest surviving toucher's resolved group
//!                      ?? "ungrouped"
//! ```
//!
//! The two cascades recurse into each other; the cycle through artifacts is guarded by the set of
//! artifact ids under resolution. Build one resolver per read over full store snapshots.

use std::collections::{HashMap, HashSet};

use crate::artifact::{Artifact, USER_ACTOR};
use crate::session::{Session, SessionKind};

/// Display fallback when no touch author survives.
pub const UNGROUPED: &str = "ungrouped";

/// The parent walk stops AT a hub, so one effort's group never leaks through it.
pub const HUB_SESSION_NAMES: [&str; 1] = ["tx-assistant"];

fn is_hub(session: &Session) -> bool {
    HUB_SESSION_NAMES.contains(&session.name.as_str())
}

/// Python truthiness of an optional override: `None` and `""` are both "no override".
fn set(group: &Option<String>) -> Option<&str> {
    group.as_deref().filter(|group| !group.is_empty())
}

pub struct GroupResolver<'a> {
    sessions_by_id: HashMap<&'a str, &'a Session>,
    sessions_by_name: HashMap<&'a str, Vec<&'a Session>>,
    artifacts_by_id: HashMap<&'a str, &'a Artifact>,
}

impl<'a> GroupResolver<'a> {
    pub fn new(sessions: &'a [Session], artifacts: &'a [Artifact]) -> Self {
        let mut sessions_by_name: HashMap<&str, Vec<&Session>> = HashMap::new();
        for session in sessions {
            sessions_by_name
                .entry(session.name.as_str())
                .or_default()
                .push(session);
        }
        Self {
            sessions_by_id: sessions.iter().map(|s| (s.id.as_str(), s)).collect(),
            sessions_by_name,
            artifacts_by_id: artifacts.iter().map(|a| (a.id.as_str(), a)).collect(),
        }
    }

    /// The session's effective group — never empty unless the name is (name is the floor).
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

    /// Own override, first override up the parent chain, or the bound artifact's. Only overrides
    /// inherit — an ancestor's tags never leak down.
    fn session_override(&self, session: &Session, resolving: &HashSet<&'a str>) -> Option<String> {
        if let Some(group) = set(&session.group) {
            return Some(group.to_owned());
        }
        let mut visited: HashSet<&str> = HashSet::from([session.id.as_str()]);
        if !is_hub(session) {
            let mut ancestor = self.resolve_reference(session.parent.as_deref(), session);
            while let Some(current) = ancestor {
                if visited.contains(current.id.as_str()) {
                    break;
                }
                if let Some(group) = set(&current.group) {
                    return Some(group.to_owned());
                }
                if is_hub(current) {
                    break;
                }
                visited.insert(&current.id);
                ancestor = self.resolve_reference(current.parent.as_deref(), current);
            }
        }
        let artifact = match &session.kind {
            SessionKind::Other(other) => other
                .artifact_id
                .as_deref()
                .and_then(|id| self.artifacts_by_id.get(id)),
            SessionKind::Llm(_) => None,
        };
        match artifact {
            Some(artifact) if !resolving.contains(artifact.id.as_str()) => {
                self.artifact_override(artifact, resolving)
            }
            _ => None,
        }
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

    /// A `parent` value as a record: by id, else by display name (legacy shape). Name matches are
    /// era-scoped (no candidate born after the referrer) and rank live-then-newest.
    fn resolve_reference(
        &self,
        reference: Option<&str>,
        referrer: &Session,
    ) -> Option<&'a Session> {
        let reference = reference?;
        if let Some(by_id) = self.sessions_by_id.get(reference) {
            return Some(by_id);
        }
        let born = referrer.created_at.as_ref().and_then(|n| n.as_f64());
        let created = |session: &Session| {
            session
                .created_at
                .as_ref()
                .and_then(|n| n.as_f64())
                .filter(|value| *value != 0.0)
                .unwrap_or(0.0)
        };
        let mut best: Option<(&'a Session, (bool, f64))> = None;
        for &candidate in self.sessions_by_name.get(reference)? {
            if born.is_some_and(|born| created(candidate) > born) {
                continue;
            }
            let key = (candidate.is_alive(), created(candidate));
            // `max` keeps the first of equal keys.
            let better = match &best {
                None => true,
                Some((_, best_key)) => key
                    .0
                    .cmp(&best_key.0)
                    .then(key.1.total_cmp(&best_key.1))
                    .is_gt(),
            };
            if better {
                best = Some((candidate, key));
            }
        }
        best.map(|(session, _)| session)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn llm(id: &str, name: &str, extra: Value) -> Session {
        let mut record = json!({
            "schema_version": 6, "id": id, "name": name, "role": "llm", "state": "idle",
            "cwd": "/r", "cmd": "", "tags": [], "group": null, "env": {}, "parent": null,
            "pid": null, "attached_to": [], "created_at": null, "ended_at": null,
            "engine": "claude", "last_activity": null, "chats": [], "turn_started_at": null,
        });
        for (key, value) in extra.as_object().unwrap() {
            record[key] = value.clone();
        }
        Session::from_value(&record).unwrap()
    }

    fn view(id: &str, name: &str, artifact_id: &str) -> Session {
        let record = json!({
            "schema_version": 6, "id": id, "name": name, "role": "nvim", "state": "alive",
            "cwd": "/r", "cmd": "", "tags": [], "group": null, "env": {}, "parent": null,
            "pid": null, "attached_to": [], "created_at": null, "ended_at": null,
            "artifact_id": artifact_id,
        });
        Session::from_value(&record).unwrap()
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

    /// Fixture G: assist(a1) ← worker(b1, tags feat-x,extra) ← child(c1); hub tx-assistant(h1).
    fn fixture_g(a1_parent: Option<&str>, hub_group: Option<&str>) -> Vec<Session> {
        vec![
            llm(
                "a1",
                "assist",
                json!({"parent": a1_parent, "created_at": 100.0}),
            ),
            llm(
                "b1",
                "worker",
                json!({"tags": ["feat-x", "extra"], "parent": "a1", "created_at": 200.0}),
            ),
            llm("c1", "child", json!({"parent": "b1", "created_at": 300.0})),
            llm(
                "h1",
                "tx-assistant",
                json!({"group": hub_group, "parent": "x1", "created_at": 50.0}),
            ),
            llm("x1", "up", json!({"group": "upgrp", "created_at": 10.0})),
        ]
    }

    fn group_of(sessions: &[Session], artifacts: &[Artifact], id: &str) -> String {
        let resolver = GroupResolver::new(sessions, artifacts);
        let session = sessions.iter().find(|s| s.id == id).unwrap();
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
            llm("p1", "P", json!({"parent": "q1"})),
            llm("q1", "Q", json!({"parent": "p1"})),
        ];
        assert_eq!(group_of(&sessions, &[], "p1"), "P");
    }

    #[test]
    fn era_scoped_parent_by_name() {
        let mut sessions = vec![
            llm(
                "n1",
                "dup",
                json!({"group": "old", "created_at": 100.0, "state": "exited", "ended_at": 150.0}),
            ),
            llm("n2", "dup", json!({"group": "new", "created_at": 500.0})),
            llm("k1", "K", json!({"parent": "dup", "created_at": 300.0})),
            llm("k2", "K2", json!({"parent": "dup", "created_at": 600.0})),
        ];
        assert_eq!(group_of(&sessions, &[], "k1"), "old");
        assert_eq!(group_of(&sessions, &[], "k2"), "new");
        sessions.push(llm("k3", "K3", json!({"parent": "nothing"})));
        assert_eq!(group_of(&sessions, &[], "k3"), "K3");
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

        let sessions = [view("v1", "view", "art3")];
        let artifacts = [artifact("art3", None, &["v1"])];
        let resolver = GroupResolver::new(&sessions, &artifacts);
        assert_eq!(resolver.session_group(&sessions[0]), "view");
        assert_eq!(resolver.artifact_group(&artifacts[0]), "view");
    }
}
