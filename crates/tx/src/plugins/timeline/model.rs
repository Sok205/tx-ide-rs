//! The timeline payload — swimlanes of session activity over a window; a port of `_build` in
//! the artifact-browser's `timeline.py`.
//!
//! - lanes: session records are MERGED when they share a transcript path (resume chains
//!   re-record the same conversation under a new record id).
//! - segments: activity runs split on > `SEG_GAP_MIN` minutes of silence (`FSEG_GAP_MIN` for the
//!   reference's fine tier; the view draws exact turn spans — [`super::turns`] — when zoomed in).
//! - events: `u` rows (burst-capped per minute) + milestone tool calls.
//! - edges: peer-message envelopes and spawn / resume parentage, lane to lane.
//!
//! Times inside the payload are MINUTES from `epoch` (the local midnight `days` days back), as
//! in the reference, so `tx _timeline-json` diffs byte-for-byte against the Python prototype.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use super::cache::TranscriptCache;
use super::clock;
use super::metadata::Metadata;
use super::rows::{BLURB_CAP, EV_TEXT, Kind, Row, cap, milestone_label, peer_parts};
use super::turns::Turns;
use crate::artifact::Artifact;
use crate::artifact_store::ArtifactStore;
use crate::engines::claude::{BUNDLE_TRANSCRIPT_NAME, bundle_transcript_path};
use crate::grouping::GroupResolver;
use crate::palette::tag_cube;
use crate::session::{Role, Session};
use crate::storage::Home;
use crate::store::SessionStore;

pub const ASSISTANT_NAME: &str = "tx-assistant";
pub const SEG_GAP_MIN: f64 = 180.0;
pub const FSEG_GAP_MIN: f64 = 10.0;
pub const BURST_PER_MIN: usize = 4;
/// An unclosed turn (a crash, a thinking-only final message) counts as working for at most this
/// long after the transcript's last entry.
pub const HOLD_CAP_S: f64 = 900.0;
pub const CREATE_MATCH_S: f64 = 180.0;
pub const DEFAULT_DAYS: f64 = 14.0;
pub const ORDER_FILE: &str = "timeline.group-order.json";

static UUID_RX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
        .unwrap()
});
static SHORT_ID_RX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bartifact modify\s+([0-9a-fA-F]{4,31})\b").unwrap());
static SUFFIX_RX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"@\w+$").unwrap());

#[derive(Clone, Debug, PartialEq)]
pub struct Day {
    /// Minutes from the epoch.
    pub offset: f64,
    pub label: String,
    pub weekend: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub key: String,
    /// xterm-256 cube index (`palette::tag_cube`).
    pub cube: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// The human typed (`u`): a ring.
    User,
    /// A milestone tool call (`m`): a diamond.
    Milestone,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub at: f64,
    pub kind: EventKind,
    pub text: String,
    pub artifact: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    Message,
    Spawn,
    Resume,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    pub at: f64,
    /// Sender lane; `None` when the sender has no lane (out of window).
    pub from: Option<usize>,
    pub to: usize,
    pub token: String,
    pub snippet: String,
    pub kind: EdgeKind,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lane {
    pub sid: String,
    pub name: String,
    pub group: String,
    pub meta: String,
    pub metadata: Metadata,
    pub blurb: String,
    pub state: String,
    pub sys: bool,
    pub live: bool,
    pub kill: Option<f64>,
    pub a: f64,
    pub b: f64,
    pub segs: Vec<(f64, f64)>,
    pub fsegs: Vec<(f64, f64)>,
    /// Exact turn spans (the agent held the floor), minutes, merged across the lane's
    /// transcripts; a live turn runs to `now`. Not in the reference payload.
    pub work: Vec<(f64, f64)>,
    pub events: Vec<Event>,
    /// Every parsed row, time-sorted; epoch seconds.
    pub rows: Vec<Row>,
}

impl Lane {
    /// The operator's hub conversation (`tx-assistant`, any `@MonDD` suffix).
    pub fn hub(&self) -> bool {
        SUFFIX_RX.replace(&self.name, "") == ASSISTANT_NAME
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Payload {
    /// Epoch seconds of the window start (a local midnight).
    pub epoch: f64,
    /// Data-max, minutes — the view extends the viewport to its own clock.
    pub t1: f64,
    pub days: Vec<Day>,
    pub groups: Vec<Group>,
    pub lanes: Vec<Lane>,
    pub edges: Vec<Edge>,
}

impl Payload {
    pub fn minutes(&self, timestamp: f64) -> f64 {
        (timestamp - self.epoch) / 60.0
    }

    pub fn timestamp(&self, minutes: f64) -> f64 {
        self.epoch + minutes * 60.0
    }

    pub fn group_cube(&self, key: &str) -> u8 {
        self.groups
            .iter()
            .find(|group| group.key == key)
            .map_or(250, |group| group.cube)
    }

    /// The reference's JSON shape (`/api/timeline`), for parity diffs.
    pub fn to_json(&self) -> Value {
        let sessions: Vec<Value> = self
            .lanes
            .iter()
            .map(|lane| {
                let events: Vec<Value> = lane
                    .events
                    .iter()
                    .map(|event| {
                        let kind = match event.kind {
                            EventKind::User => "u",
                            EventKind::Milestone => "m",
                        };
                        match &event.artifact {
                            Some(artifact) => json!([event.at, kind, event.text, artifact]),
                            None => json!([event.at, kind, event.text]),
                        }
                    })
                    .collect();
                json!({
                    "sid": lane.sid,
                    "id": lane.name,
                    "g": lane.group,
                    "meta": lane.meta,
                    "blurb": lane.blurb,
                    "state": lane.state,
                    "sys": u8::from(lane.sys),
                    "live": u8::from(lane.live),
                    "kill": lane.kill,
                    "a": lane.a,
                    "b": lane.b,
                    "segs": lane.segs.iter().map(|(a, b)| json!([a, b])).collect::<Vec<_>>(),
                    "fsegs": lane.fsegs.iter().map(|(a, b)| json!([a, b])).collect::<Vec<_>>(),
                    "ev": events,
                })
            })
            .collect();
        let edges: Vec<Value> = self
            .edges
            .iter()
            .map(|edge| {
                let from = edge.from.map_or(-1, |index| index as i64);
                let kind = match edge.kind {
                    EdgeKind::Message => "msg",
                    EdgeKind::Spawn => "spawn",
                    EdgeKind::Resume => "resume",
                };
                json!([edge.at, from, edge.to, edge.token, edge.snippet, kind])
            })
            .collect();
        json!({
            "epoch_ms": (self.epoch * 1000.0) as i64,
            "t1": self.t1,
            "days": self.days.iter().map(|day| json!({
                "o": day.offset, "label": day.label, "wk": u8::from(day.weekend),
            })).collect::<Vec<_>>(),
            "groups": self.groups.iter().map(|group| json!({
                "k": group.key, "color": cube_to_hex(group.cube),
            })).collect::<Vec<_>>(),
            "sessions": sessions,
            "edges": edges,
        })
    }
}

/// The lane's turn spans in window minutes: open turns run to `end` (now, or the kill) but no
/// further than `HOLD_CAP_S` past the transcript's last entry; overlapping spans from a resume
/// chain are merged.
fn work_spans(
    turns: &[Turns],
    end: f64,
    now: f64,
    minutes: &impl Fn(f64) -> f64,
) -> Vec<(f64, f64)> {
    let mut spans: Vec<(f64, f64)> = Vec::new();
    for turn in turns {
        spans.extend_from_slice(&turn.spans);
        if let Some(start) = turn.open {
            let close = end.min(turn.last_at + HOLD_CAP_S).min(now);
            if close > start {
                spans.push((start, close));
            }
        }
    }
    spans.sort_by(|left, right| left.0.total_cmp(&right.0));
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (start, close) in spans {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(close),
            _ => merged.push((start, close)),
        }
    }
    merged
        .into_iter()
        .filter(|(_, close)| minutes(*close) >= 0.0)
        .map(|(start, close)| (round2(minutes(start).max(0.0)), round2(minutes(close))))
        .collect()
}

/// xterm-256 index → `#rrggbb` (the reference's `_cube_to_hex`).
pub fn cube_to_hex(cube: u8) -> String {
    if (16..=231).contains(&cube) {
        let offset = u32::from(cube - 16);
        let steps = [offset / 36, (offset / 6) % 6, offset % 6];
        let channel = |step: u32| if step == 0 { 0 } else { 55 + 40 * step };
        format!(
            "#{:02x}{:02x}{:02x}",
            channel(steps[0]),
            channel(steps[1]),
            channel(steps[2])
        )
    } else {
        let grey = 8 + 10 * u32::from(cube.saturating_sub(232));
        format!("#{grey:02x}{grey:02x}{grey:02x}")
    }
}

/// Python `round(x, 2)` (half away from zero is close enough for a parity diff).
fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn number(value: &Option<serde_json::Number>) -> Option<f64> {
    value.as_ref().and_then(serde_json::Number::as_f64)
}

/// The user-dragged group order (mirrors the HTML view's drag & drop; shared file).
pub fn load_group_order(home: &Home) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(home.root().join(ORDER_FILE)) else {
        return Vec::new();
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Array(items)) => items
            .into_iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

pub fn save_group_order(home: &Home, order: &[String]) -> std::io::Result<()> {
    let path = home.root().join(ORDER_FILE);
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_string(order)?)?;
    std::fs::rename(temporary, path)
}

struct LaneSeed {
    name: String,
    records: Vec<Session>,
    paths: HashSet<String>,
}

/// Builds payloads over a home + store, keeping the transcript cache warm between ticks.
pub struct Builder {
    home: Rc<Home>,
    store: Rc<SessionStore>,
    cache: TranscriptCache,
}

impl Builder {
    pub fn new(home: Rc<Home>, store: Rc<SessionStore>) -> Self {
        Self {
            home,
            store,
            cache: TranscriptCache::default(),
        }
    }

    /// First existing candidate per chat: the recorded live path, else the ingested bundle copy.
    fn transcript_paths(&self, session: &Session) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let Some(llm) = session.llm() else {
            return found;
        };
        for chat in &llm.chats {
            let mut candidates = Vec::new();
            if !chat.transcript_path.is_empty() {
                candidates.push(PathBuf::from(&chat.transcript_path));
            }
            if let Some(chat_id) = &chat.id {
                match &chat.bundle_path {
                    Some(bundle) => candidates.push(Path::new(bundle).join(BUNDLE_TRANSCRIPT_NAME)),
                    None => {
                        candidates.push(bundle_transcript_path(&self.home, &session.id, chat_id))
                    }
                }
            }
            if let Some(path) = candidates.into_iter().find(|path| path.is_file()) {
                found.push(path);
            }
        }
        found
    }

    /// Window-filtered llm records merged into lanes, same-name dead lanes disambiguated with a
    /// `@MonDD` suffix.
    fn lane_seeds(&self, everyone: &[Session], cut: f64) -> Vec<LaneSeed> {
        let mut records: Vec<&Session> = everyone
            .iter()
            .filter(|session| session.role() == Role::Llm)
            .filter(|session| !session.tags.iter().any(|tag| tag == "temporary"))
            .filter(|session| {
                session.activity_at() >= cut || number(&session.created_at).unwrap_or(0.0) >= cut
            })
            .collect();
        records.sort_by(|left, right| {
            number(&left.created_at)
                .unwrap_or(0.0)
                .total_cmp(&number(&right.created_at).unwrap_or(0.0))
        });
        let mut seeds: Vec<LaneSeed> = Vec::new();
        for record in records {
            let paths: HashSet<String> = record
                .llm()
                .map(|llm| {
                    llm.chats
                        .iter()
                        .filter(|chat| !chat.transcript_path.is_empty())
                        .map(|chat| chat.transcript_path.clone())
                        .collect()
                })
                .unwrap_or_default();
            let merged = seeds.iter_mut().find(|seed| {
                seed.name == record.name
                    && (!paths.is_disjoint(&seed.paths)
                        || (paths.is_empty() && seed.paths.is_empty()))
            });
            match merged {
                Some(seed) => {
                    seed.records.push(record.clone());
                    seed.paths.extend(paths);
                }
                None => seeds.push(LaneSeed {
                    name: record.name.clone(),
                    records: vec![record.clone()],
                    paths,
                }),
            }
        }
        let mut by_name: HashMap<String, usize> = HashMap::new();
        for seed in &seeds {
            *by_name.entry(seed.name.clone()).or_default() += 1;
        }
        for seed in &mut seeds {
            if by_name[&seed.name] > 1 && !seed.records.iter().any(Session::is_alive) {
                let created = number(&seed.records[0].created_at).unwrap_or(0.0);
                seed.name = format!("{}@{}", seed.name, clock::month_day(created));
            }
        }
        seeds
    }

    /// All parsed rows for a lane's merged records, time-sorted, paths deduplicated; and the
    /// turn spans of the same transcripts.
    fn lane_rows(&mut self, seed: &LaneSeed) -> (Vec<Row>, Vec<Turns>) {
        let mut rows = Vec::new();
        let mut turns = Vec::new();
        let mut seen = HashSet::new();
        for record in &seed.records {
            for path in self.transcript_paths(record) {
                if seen.insert(path.clone()) {
                    rows.extend_from_slice(self.cache.rows(&path));
                    turns.extend(self.cache.turns(&path).cloned());
                }
            }
        }
        rows.sort_by(|left, right| left.at.total_cmp(&right.at));
        (rows, turns)
    }

    pub fn build(&mut self, days: f64, now: f64) -> Payload {
        let epoch = clock::local_midnight(now - days * 86_400.0, 0);
        let minutes = |timestamp: f64| (timestamp - epoch) / 60.0;

        let everyone = self.store.all();
        let names: HashMap<&str, &str> = everyone
            .iter()
            .map(|session| (session.id.as_str(), session.name.as_str()))
            .collect();
        let artifacts: Vec<Artifact> = ArtifactStore::new(&self.home).all().artifacts;
        let resolver = GroupResolver::new(&everyone, &artifacts);
        let seeds = self.lane_seeds(&everyone, epoch);

        let mut created: HashMap<&str, Vec<(f64, &str)>> = HashMap::new();
        let mut titles: HashMap<&str, &str> = HashMap::new();
        for artifact in &artifacts {
            titles.insert(
                artifact.id.as_str(),
                artifact.title.as_deref().unwrap_or(&artifact.filename),
            );
            if let Some(first) = artifact.history().first() {
                created
                    .entry(first.session_id.as_str())
                    .or_default()
                    .push((first.at.as_f64().unwrap_or(0.0), artifact.id.as_str()));
            }
        }

        let mut lanes: Vec<Lane> = Vec::new();
        let mut lane_peers: Vec<Vec<(f64, String, String)>> = Vec::new();
        let mut spawns: Vec<(f64, String, usize, EdgeKind)> = Vec::new();
        let mut seed_rows: Vec<(Vec<Row>, Vec<Turns>)> = Vec::new();
        for seed in &seeds {
            seed_rows.push(self.lane_rows(seed));
        }
        for (seed, (rows, turns)) in seeds.iter().zip(seed_rows) {
            let records = &seed.records;
            let tags: HashSet<&str> = records
                .iter()
                .flat_map(|record| record.tags.iter().map(String::as_str))
                .collect();
            let live = records.iter().any(Session::is_alive);
            let primary = records
                .iter()
                .max_by(|left, right| {
                    (left.is_alive(), left.activity_at())
                        .partial_cmp(&(right.is_alive(), right.activity_at()))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .expect("a lane has at least one record");
            let kill = (!live).then(|| {
                records
                    .iter()
                    .map(|record| number(&record.ended_at).unwrap_or_else(|| record.activity_at()))
                    .fold(f64::MIN, f64::max)
            });
            let mut engines: Vec<&str> = records
                .iter()
                .flat_map(|record| record.llm().into_iter().flat_map(|llm| llm.chats.iter()))
                .map(|chat| chat.engine.map_or("?", |engine| engine.as_str()))
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            engines.sort_unstable();
            if engines.is_empty() {
                engines.push("?");
            }

            let mut times: Vec<f64> = rows.iter().map(|row| row.at).collect();
            if times.is_empty() {
                let first = records
                    .iter()
                    .map(|record| number(&record.created_at).unwrap_or(0.0))
                    .fold(f64::MAX, f64::min);
                let last = records
                    .iter()
                    .map(Session::activity_at)
                    .fold(f64::MIN, f64::max);
                times = vec![first, last.max(first + 120.0)];
            }
            let segments = |gap_seconds: f64| -> Vec<(f64, f64)> {
                let mut segments = Vec::new();
                let (mut start, mut previous) = (times[0], times[0]);
                for &time in &times[1..] {
                    if time - previous > gap_seconds {
                        segments.push((start, previous.max(start + 60.0)));
                        start = time;
                    }
                    previous = time;
                }
                segments.push((start, previous.max(start + 60.0)));
                segments
                    .into_iter()
                    .filter(|(_, end)| minutes(*end) >= 0.0)
                    .map(|(begin, end)| (round2(minutes(begin).max(0.0)), round2(minutes(end))))
                    .collect()
            };
            let mut segs = segments(SEG_GAP_MIN * 60.0);
            let mut fsegs = segments(FSEG_GAP_MIN * 60.0);
            if segs.is_empty() {
                segs.push((0.0, 1.0));
            }
            if fsegs.is_empty() {
                fsegs.push((0.0, 1.0));
            }
            let work = work_spans(&turns, kill.unwrap_or(now), now, &minutes);

            let mut events = Vec::new();
            let mut per_minute: HashMap<i64, usize> = HashMap::new();
            for row in &rows {
                let at = minutes(row.at);
                if at < 0.0 {
                    continue;
                }
                match row.kind {
                    Kind::User => {
                        let count = per_minute.entry(at as i64).or_default();
                        *count += 1;
                        if *count > BURST_PER_MIN {
                            if *count == BURST_PER_MIN + 1 {
                                events.push(Event {
                                    at: round2(at),
                                    kind: EventKind::User,
                                    text: "(burst: more messages this minute — open chat)".into(),
                                    artifact: None,
                                });
                            }
                            continue;
                        }
                        events.push(Event {
                            at: round2(at),
                            kind: EventKind::User,
                            text: cap(&row.text, EV_TEXT),
                            artifact: None,
                        });
                    }
                    Kind::Tool => {
                        let Some(probe) = &row.probe else { continue };
                        let Some(label) = milestone_label(probe) else {
                            continue;
                        };
                        let mut event = Event {
                            at: round2(at),
                            kind: EventKind::Milestone,
                            text: cap(&format!("{label}: {probe}"), EV_TEXT),
                            artifact: None,
                        };
                        if label == "artifact" {
                            let mut artifact_id = UUID_RX
                                .find(probe)
                                .map(|found| found.as_str().to_ascii_lowercase())
                                .filter(|id| titles.contains_key(id.as_str()));
                            if artifact_id.is_none()
                                && let Some(short) = SHORT_ID_RX.captures(probe)
                            {
                                let prefix = short[1].to_ascii_lowercase();
                                let hits: Vec<&str> = titles
                                    .keys()
                                    .copied()
                                    .filter(|id| id.starts_with(&prefix))
                                    .collect();
                                if hits.len() == 1 {
                                    artifact_id = Some(hits[0].to_owned());
                                }
                            }
                            if artifact_id.is_none() {
                                let mut best: Option<(f64, &str)> = None;
                                for record in records {
                                    for &(touched_at, id) in
                                        created.get(record.id.as_str()).into_iter().flatten()
                                    {
                                        let distance = (touched_at - row.at).abs();
                                        if distance <= CREATE_MATCH_S
                                            && best.is_none_or(|(closest, _)| distance < closest)
                                        {
                                            best = Some((distance, id));
                                        }
                                    }
                                }
                                artifact_id = best.map(|(_, id)| id.to_owned());
                            }
                            let verb = if probe.contains("artifact modify") {
                                "modified"
                            } else {
                                "created"
                            };
                            match artifact_id {
                                Some(id) => {
                                    let title =
                                        titles.get(id.as_str()).copied().unwrap_or(&id[..8]);
                                    event.text = cap(&format!("artifact {verb}: {title}"), EV_TEXT);
                                    event.artifact = Some(id);
                                }
                                None => {
                                    event.text = cap(&format!("artifact {verb}: {probe}"), EV_TEXT);
                                }
                            }
                        }
                        events.push(event);
                    }
                    _ => {}
                }
            }

            for (index, record) in records.iter().enumerate() {
                let spawned = minutes(number(&record.created_at).unwrap_or(0.0));
                let Some(parent) = &record.parent else {
                    continue;
                };
                if spawned < 0.0 {
                    continue;
                }
                let kind = if index == 0 {
                    EdgeKind::Spawn
                } else {
                    EdgeKind::Resume
                };
                spawns.push((round2(spawned), parent.clone(), lanes.len(), kind));
            }

            let peers = rows
                .iter()
                .filter(|row| row.kind == Kind::Peer && minutes(row.at) >= 0.0)
                .filter_map(|row| {
                    let (token, snippet) = peer_parts(&row.text)?;
                    Some((round2(minutes(row.at)), token, cap(&snippet, 120)))
                })
                .collect();
            lane_peers.push(peers);

            let first_user = rows
                .iter()
                .find(|row| row.kind == Kind::User)
                .map(|row| row.text.as_str())
                .unwrap_or("");
            let message_count = rows.iter().filter(|row| row.kind.is_message()).count();
            let state = if live {
                primary.state.as_str().to_owned()
            } else {
                "killed".to_owned()
            };
            let blurb = cap(first_user, BLURB_CAP);
            let metadata = self
                .transcript_paths(primary)
                .last()
                .and_then(|path| self.cache.metadata(path))
                .cloned()
                .unwrap_or_default()
                .with_launch_fallback(&primary.initial_cmd);
            lanes.push(Lane {
                sid: primary.id.clone(),
                name: seed.name.clone(),
                group: resolver.session_group(primary),
                meta: format!("{} · {state} · {message_count} msgs", engines.join("/")),
                metadata,
                blurb: if blurb.is_empty() {
                    "(no user messages recorded)".to_owned()
                } else {
                    blurb
                },
                state,
                sys: tags.contains("tx-system"),
                live,
                kill: kill.map(|at| round2(minutes(at))),
                a: round2(minutes(times[0]).max(0.0)),
                b: round2(minutes(times[times.len() - 1])),
                segs,
                fsegs,
                work,
                events,
                rows,
            });
        }

        // Peer-message edges: resolve the envelope's sender token (display name, base name before
        // a @MonDD suffix, or record uuid) to a lane.
        let mut resolve: HashMap<String, usize> = HashMap::new();
        for (index, seed) in seeds.iter().enumerate() {
            for record in &seed.records {
                resolve.insert(record.id.clone(), index);
            }
        }
        for (index, seed) in seeds.iter().enumerate() {
            resolve.entry(seed.name.clone()).or_insert(index);
        }
        for (index, seed) in seeds.iter().enumerate() {
            resolve
                .entry(SUFFIX_RX.replace(&seed.name, "").into_owned())
                .or_insert(index);
        }
        let resolve = &resolve;
        let mut edges: Vec<Edge> = lane_peers
            .iter()
            .enumerate()
            .flat_map(|(to, peers)| {
                peers.iter().map(move |(at, token, snippet)| Edge {
                    at: *at,
                    from: resolve.get(token).copied(),
                    to,
                    token: token.clone(),
                    snippet: snippet.clone(),
                    kind: EdgeKind::Message,
                })
            })
            .collect();
        for (at, parent_id, to, kind) in spawns {
            let token = names.get(parent_id.as_str()).map_or_else(
                || parent_id.chars().take(8).collect(),
                |name| (*name).to_owned(),
            );
            edges.push(Edge {
                at,
                from: resolve.get(&parent_id).copied(),
                to,
                token,
                snippet: String::new(),
                kind,
            });
        }

        // Lanes by first activity; edges follow the permutation.
        let mut order: Vec<usize> = (0..lanes.len()).collect();
        order.sort_by(|left, right| lanes[*left].a.total_cmp(&lanes[*right].a));
        let mut permutation = vec![0; lanes.len()];
        for (new_index, old_index) in order.iter().enumerate() {
            permutation[*old_index] = new_index;
        }
        let mut sorted_lanes: Vec<Option<Lane>> = lanes.into_iter().map(Some).collect();
        let lanes: Vec<Lane> = order
            .iter()
            .map(|index| sorted_lanes[*index].take().expect("each lane moves once"))
            .collect();
        for edge in &mut edges {
            edge.from = edge.from.map(|from| permutation[from]);
            edge.to = permutation[edge.to];
        }
        edges.sort_by(|left, right| left.at.total_cmp(&right.at));

        let mut group_first: Vec<(String, f64)> = Vec::new();
        for lane in &lanes {
            if !group_first.iter().any(|(key, _)| *key == lane.group) {
                group_first.push((lane.group.clone(), lane.a));
            }
        }
        group_first.sort_by(|left, right| left.1.total_cmp(&right.1));
        let mut groups: Vec<Group> = group_first
            .into_iter()
            .map(|(key, _)| Group {
                cube: tag_cube(&key),
                key,
            })
            .collect();
        let saved = load_group_order(&self.home);
        if !saved.is_empty() {
            let position = |key: &str| {
                saved
                    .iter()
                    .position(|known| known == key)
                    .unwrap_or(saved.len())
            };
            groups.sort_by_key(|group| position(&group.key));
        }

        let mut days = Vec::new();
        for offset in 0.. {
            let midnight = clock::local_midnight(epoch, offset);
            if midnight > now {
                break;
            }
            days.push(Day {
                offset: round1(minutes(midnight)),
                label: clock::day_label(midnight),
                weekend: clock::is_weekend(midnight),
            });
        }

        let t1 = lanes
            .iter()
            .map(|lane| lane.b)
            .chain(days.iter().map(|day| day.offset))
            .fold(0.0, f64::max);
        Payload {
            epoch,
            t1: round1(t1),
            days,
            groups,
            lanes,
            edges,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_hex_matches_reference() {
        assert_eq!(cube_to_hex(16), "#000000");
        assert_eq!(cube_to_hex(231), "#ffffff");
        assert_eq!(cube_to_hex(196), "#ff0000");
        assert_eq!(cube_to_hex(232), "#080808");
        assert_eq!(cube_to_hex(255), "#eeeeee");
    }
}
