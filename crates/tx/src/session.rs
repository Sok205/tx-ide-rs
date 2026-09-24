//! Domain entities (session.py): the session record model and its on-disk (schema v6) shape.
//!
//! `Session` is one struct holding the fields every role shares, plus [`SessionKind`] for the
//! llm-only axis (`LlmSession`) versus the non-llm one (`OtherSession`). `to_value` / `from_value`
//! reproduce `to_dict` / `from_dict`: exact key order, the schema-version guard, and the error
//! texts the store scan prints (`'name'` for a missing key, `'bogus' is not a valid State`).
//!
//! Deviation (Q26 territory): a value of the wrong JSON type (e.g. `"tags": 5`) makes the Python
//! crash with an uncaught `TypeError`; here it is a [`RecordError::WrongType`], which the store
//! reports like any other unreadable record.

use std::fmt;

use serde_json::{Map, Number, Value, json};

use crate::pyjson;

/// On-disk record shape version; any other version is refused at load (OPEN-0b).
pub const SCHEMA_VERSION: u64 = 6;
pub const READ_ONLY_ENV: &str = "TX_READ_ONLY";
pub const REQUIRE_WORKTREE_ENV: &str = "TX_REQUIRE_WORKTREE";

/// Why a persisted record could not be turned into a model value. `Display` is the text Python
/// shows for the matching exception (`str(error)`).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    /// `UnsupportedRecordError`; carries `repr(schema_version)`.
    #[error(
        "record schema_version={0} is unsupported (expected {SCHEMA_VERSION}); tx-ide does not \
         back-migrate older records on load (§9) — run `tx migrate` to upgrade older records"
    )]
    Unsupported(String),
    /// `KeyError(key)`: `str()` of a `KeyError` is the key's repr.
    #[error("{}", py_repr_str(.0))]
    MissingKey(String),
    /// `ValueError` from an enum lookup; carries `repr(value)` and the enum's class name.
    #[error("{value} is not a valid {enum_name}")]
    InvalidEnum {
        value: String,
        enum_name: &'static str,
    },
    #[error("{} must be {expected}", py_repr_str(.key))]
    WrongType { key: String, expected: &'static str },
}

macro_rules! str_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const ALL: &[$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }

            /// `Enum(value)`: lookup by value, `ValueError` text on a miss.
            pub fn from_value(value: &Value) -> Result<Self, RecordError> {
                match value.as_str() {
                    $(Some($text) => Ok($name::$variant),)+
                    _ => Err(RecordError::InvalidEnum {
                        value: py_repr(value),
                        enum_name: stringify!($name),
                    }),
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl std::str::FromStr for $name {
            type Err = RecordError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::from_value(&Value::String(s.to_owned()))
            }
        }
    };
}

str_enum!(
    /// What runs in the session.
    Role { Llm = "llm", Nvim = "nvim", Shell = "shell", Other = "other" }
);
str_enum!(
    /// Combined liveness + activity state, role-dependent (D3).
    State {
        Alive = "alive",
        Working = "working",
        Waiting = "waiting",
        Idle = "idle",
        Exited = "exited",
        Archived = "archived",
    }
);
str_enum!(
    /// The coding-agent CLI driving an llm session.
    Engine { Claude = "claude", Codex = "codex", Antigravity = "antigravity" }
);

impl State {
    /// EXITED / ARCHIVED are absorbing (C3).
    pub fn is_terminal(self) -> bool {
        matches!(self, State::Exited | State::Archived)
    }

    pub fn initial_for(role: Role) -> State {
        if role == Role::Llm {
            State::Idle
        } else {
            State::Alive
        }
    }

    pub fn valid_for(role: Role) -> &'static [State] {
        if role == Role::Llm {
            &[
                State::Working,
                State::Waiting,
                State::Idle,
                State::Exited,
                State::Archived,
            ]
        } else {
            &[State::Alive, State::Exited]
        }
    }
}

/// The role of a non-llm session: an `OtherSession` can never be `llm`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OtherRole {
    Nvim,
    Shell,
    Other,
}

impl From<OtherRole> for Role {
    fn from(role: OtherRole) -> Role {
        match role {
            OtherRole::Nvim => Role::Nvim,
            OtherRole::Shell => Role::Shell,
            OtherRole::Other => Role::Other,
        }
    }
}

impl OtherRole {
    /// `None` for `Role::Llm`.
    pub fn from_role(role: Role) -> Option<OtherRole> {
        match role {
            Role::Llm => None,
            Role::Nvim => Some(OtherRole::Nvim),
            Role::Shell => Some(OtherRole::Shell),
            Role::Other => Some(OtherRole::Other),
        }
    }
}

/// One pane currently surfacing a session (FROZEN shape, no `remote` field).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub host: String,
    pub window_index: String,
    pub window_name: String,
    pub pane_id: String,
    pub pane_index: String,
}

impl Location {
    pub fn to_value(&self) -> Value {
        json!({
            "host": self.host,
            "window_index": self.window_index,
            "window_name": self.window_name,
            "pane_id": self.pane_id,
            "pane_index": self.pane_index,
        })
    }

    pub fn from_value(value: &Value) -> Result<Self, RecordError> {
        let data = as_object(value, "attached_to")?;
        Ok(Self {
            host: string(data, "host")?,
            window_index: string(data, "window_index")?,
            window_name: string(data, "window_name")?,
            pane_id: string(data, "pane_id")?,
            pane_index: string(data, "pane_index")?,
        })
    }
}

/// Provenance edge for a `ChatRef`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    pub how: String,
    pub session_id: String,
    pub chat_id: Option<String>,
}

impl Origin {
    pub fn to_value(&self) -> Value {
        json!({"how": self.how, "session_id": self.session_id, "chat_id": self.chat_id})
    }

    pub fn from_value(value: &Value) -> Result<Self, RecordError> {
        let data = as_object(value, "origin")?;
        Ok(Self {
            how: string(data, "how")?,
            session_id: string(data, "session_id")?,
            chat_id: opt_string(data, "chat_id")?,
        })
    }
}

/// One conversation a tx session has hosted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatRef {
    pub id: Option<String>,
    pub role: String,
    pub cwd: String,
    pub transcript_path: String,
    pub origin: Origin,
    pub bundle_path: Option<String>,
    pub started_at: Option<Number>,
    pub ended_at: Option<Number>,
    pub summary: String,
    pub engine: Option<Engine>,
}

impl ChatRef {
    pub fn to_value(&self) -> Value {
        json!({
            "id": self.id,
            "role": self.role,
            "cwd": self.cwd,
            "transcript_path": self.transcript_path,
            "origin": self.origin.to_value(),
            "bundle_path": self.bundle_path,
            "started_at": self.started_at,
            "ended_at": self.ended_at,
            "summary": self.summary,
            "engine": self.engine.map(Engine::as_str),
        })
    }

    pub fn from_value(value: &Value) -> Result<Self, RecordError> {
        let data = as_object(value, "chats")?;
        Ok(Self {
            id: opt_string(data, "id")?,
            role: string(data, "role")?,
            cwd: string(data, "cwd")?,
            transcript_path: string(data, "transcript_path")?,
            origin: Origin::from_value(key(data, "origin")?)?,
            bundle_path: opt_string(data, "bundle_path")?,
            started_at: opt_number(data, "started_at")?,
            ended_at: opt_number(data, "ended_at")?,
            summary: string(data, "summary")?,
            engine: match key(data, "engine")? {
                Value::Null => None,
                engine => Some(Engine::from_value(engine)?),
            },
        })
    }
}

/// The llm-only axis (`LlmSession`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LlmSession {
    pub engine: Engine,
    pub chats: Vec<ChatRef>,
    pub last_activity: Option<Number>,
    /// C5 stuck-`WORKING` clock; `None` before the first turn.
    pub turn_started_at: Option<Number>,
}

/// A non-llm work session (`OtherSession`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtherSession {
    pub role: OtherRole,
    /// v5: the artifact an nvim view renders.
    pub artifact_id: Option<String>,
    /// D11: the `--listen` socket of an nvim companion.
    pub nvim_socket: NvimSocket,
}

/// D11 `nvim_socket`, three states so a record written before D11 round-trips unchanged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum NvimSocket {
    /// No key in the record (a pre-D11 record); none is written back.
    #[default]
    Absent,
    /// `"nvim_socket": null` — a non-nvim session written by the port.
    Unset,
    /// The socket nvim was told to `--listen` on.
    Path(String),
}

impl NvimSocket {
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Path(path) => Some(path),
            Self::Absent | Self::Unset => None,
        }
    }

    fn from_record(data: &Map<String, Value>) -> Result<Self, RecordError> {
        if !data.contains_key("nvim_socket") {
            return Ok(Self::Absent);
        }
        Ok(opt_string(data, "nvim_socket")?.map_or(Self::Unset, Self::Path))
    }

    fn to_value(&self) -> Option<Value> {
        match self {
            Self::Absent => None,
            Self::Unset => Some(Value::Null),
            Self::Path(path) => Some(path.as_str().into()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionKind {
    Llm(LlmSession),
    Other(OtherSession),
}

/// A tx-managed tmux session record (`<id>.json`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub state: State,
    pub cwd: String,
    /// JSON key `"cmd"`.
    pub initial_cmd: String,
    pub tags: Vec<String>,
    pub group: Option<String>,
    /// JSON key `"env"`, in record order.
    pub spawn_env: Vec<(String, String)>,
    pub parent: Option<String>,
    /// Spawn provenance only (C1).
    pub pid: Option<Number>,
    pub attached_to: Vec<Location>,
    pub created_at: Option<Number>,
    pub ended_at: Option<Number>,
    /// Kept as read (a `6.0` stays `6.0` on resave).
    pub schema_version: Number,
    pub kind: SessionKind,
}

impl Session {
    pub fn role(&self) -> Role {
        match &self.kind {
            SessionKind::Llm(_) => Role::Llm,
            SessionKind::Other(other) => other.role.into(),
        }
    }

    pub fn llm(&self) -> Option<&LlmSession> {
        match &self.kind {
            SessionKind::Llm(llm) => Some(llm),
            SessionKind::Other(_) => None,
        }
    }

    pub fn llm_mut(&mut self) -> Option<&mut LlmSession> {
        match &mut self.kind {
            SessionKind::Llm(llm) => Some(llm),
            SessionKind::Other(_) => None,
        }
    }

    /// Non-llm sessions host no chats.
    pub fn chats(&self) -> &[ChatRef] {
        self.llm().map_or(&[], |llm| &llm.chats)
    }

    pub fn env_var(&self, name: &str) -> Option<&str> {
        self.spawn_env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Every record is a process, tmux-named by its id (D7).
    pub fn tmux_name(&self) -> &str {
        &self.id
    }

    /// `last_activity or created_at or 0.0` (llm), `created_at or 0.0` (other) — Python
    /// truthiness, so a `0.0` falls through.
    pub fn activity_at(&self) -> f64 {
        let last_activity = self.llm().and_then(|llm| truthy(&llm.last_activity));
        last_activity
            .or_else(|| truthy(&self.created_at))
            .unwrap_or(0.0)
    }

    /// The record's state is non-terminal (not tmux liveness, C1).
    pub fn is_alive(&self) -> bool {
        !self.state.is_terminal()
    }

    pub fn needs_attention(&self) -> bool {
        self.role() == Role::Llm && self.state == State::Waiting
    }

    pub fn read_only(&self) -> bool {
        self.role() == Role::Llm && self.env_var(READ_ONLY_ENV) == Some("1")
    }

    /// C3: terminal states are absorbing. True iff the state actually changed.
    pub fn transition_to(&mut self, new_state: State) -> bool {
        if new_state == self.state || self.state.is_terminal() {
            return false;
        }
        self.state = new_state;
        true
    }

    /// Case-insensitive substring match over name, role, state, access mode, cwd and tags.
    pub fn matches(&self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let access = if self.read_only() {
            "read-only"
        } else {
            "writable"
        };
        let mut fields = vec![
            self.name.as_str(),
            self.role().as_str(),
            self.state.as_str(),
            access,
            self.cwd.as_str(),
        ];
        fields.extend(self.tags.iter().map(String::as_str));
        fields
            .join(" ")
            .to_lowercase()
            .contains(&query.to_lowercase())
    }

    pub fn to_value(&self) -> Value {
        let env: Map<String, Value> = self
            .spawn_env
            .iter()
            .map(|(key, value)| (key.clone(), Value::String(value.clone())))
            .collect();
        let mut map = Map::new();
        map.insert("schema_version".into(), self.schema_version.clone().into());
        map.insert("id".into(), self.id.clone().into());
        map.insert("name".into(), self.name.clone().into());
        map.insert("role".into(), self.role().as_str().into());
        map.insert("state".into(), self.state.as_str().into());
        map.insert("cwd".into(), self.cwd.clone().into());
        map.insert("cmd".into(), self.initial_cmd.clone().into());
        map.insert("tags".into(), json!(self.tags));
        map.insert("group".into(), json!(self.group));
        map.insert("env".into(), Value::Object(env));
        map.insert("parent".into(), json!(self.parent));
        map.insert("pid".into(), json!(self.pid));
        map.insert(
            "attached_to".into(),
            self.attached_to.iter().map(Location::to_value).collect(),
        );
        map.insert("created_at".into(), json!(self.created_at));
        map.insert("ended_at".into(), json!(self.ended_at));
        match &self.kind {
            SessionKind::Llm(llm) => {
                map.insert("engine".into(), llm.engine.as_str().into());
                map.insert("last_activity".into(), json!(llm.last_activity));
                map.insert(
                    "chats".into(),
                    llm.chats.iter().map(ChatRef::to_value).collect(),
                );
                map.insert("turn_started_at".into(), json!(llm.turn_started_at));
            }
            SessionKind::Other(other) => {
                map.insert("artifact_id".into(), json!(other.artifact_id));
                if let Some(socket) = other.nvim_socket.to_value() {
                    map.insert("nvim_socket".into(), socket);
                }
            }
        }
        Value::Object(map)
    }

    /// `Session.from_dict`: schema guard first, then the fields in the order Python reads them
    /// (so a record with several defects reports the same one).
    pub fn from_value(value: &Value) -> Result<Self, RecordError> {
        let Value::Object(data) = value else {
            return Err(wrong_type("record", "a JSON object"));
        };
        let version = data.get("schema_version").unwrap_or(&Value::Null);
        // `version != SCHEMA_VERSION` in Python: `6.0` is accepted, `True`/`"6"` are not.
        let schema_version = match version {
            Value::Number(n) if n.as_f64() == Some(SCHEMA_VERSION as f64) => n.clone(),
            other => return Err(RecordError::Unsupported(py_repr(other))),
        };
        let id = string(data, "id")?;
        let name = string(data, "name")?;
        let state = State::from_value(key(data, "state")?)?;
        let cwd = string(data, "cwd")?;
        let initial_cmd = string(data, "cmd")?;
        let tags = string_list(data, "tags")?;
        // `.get()`: the v6 addition, absent mid-migration.
        let group = match data.get("group") {
            None => None,
            Some(_) => opt_string(data, "group")?,
        };
        let spawn_env = string_map(data, "env")?;
        let parent = opt_string(data, "parent")?;
        let pid = opt_number(data, "pid")?;
        let attached_to = match key(data, "attached_to")? {
            Value::Array(items) => items
                .iter()
                .map(Location::from_value)
                .collect::<Result<_, _>>()?,
            _ => return Err(wrong_type("attached_to", "a list")),
        };
        let created_at = opt_number(data, "created_at")?;
        let ended_at = opt_number(data, "ended_at")?;
        let role = key(data, "role")?;
        let kind = if role.as_str() == Some(Role::Llm.as_str()) {
            let engine = Engine::from_value(key(data, "engine")?)?;
            let chats = match key(data, "chats")? {
                Value::Array(items) => items
                    .iter()
                    .map(ChatRef::from_value)
                    .collect::<Result<_, _>>()?,
                _ => return Err(wrong_type("chats", "a list")),
            };
            let last_activity = opt_number(data, "last_activity")?;
            let turn_started_at = match data.get("turn_started_at") {
                None => None,
                Some(_) => opt_number(data, "turn_started_at")?,
            };
            SessionKind::Llm(LlmSession {
                engine,
                chats,
                last_activity,
                turn_started_at,
            })
        } else {
            let role = Role::from_value(role)?;
            // Invariant: the `llm` string took the branch above.
            let role = OtherRole::from_role(role).expect("llm handled above");
            let artifact_id = match data.get("artifact_id") {
                None => None,
                Some(_) => opt_string(data, "artifact_id")?,
            };
            let nvim_socket = NvimSocket::from_record(data)?;
            SessionKind::Other(OtherSession {
                role,
                artifact_id,
                nvim_socket,
            })
        };
        Ok(Self {
            id,
            name,
            state,
            cwd,
            initial_cmd,
            tags,
            group,
            spawn_env,
            parent,
            pid,
            attached_to,
            created_at,
            ended_at,
            schema_version,
            kind,
        })
    }
}

fn truthy(n: &Option<Number>) -> Option<f64> {
    n.as_ref()
        .and_then(Number::as_f64)
        .filter(|value| *value != 0.0)
}

fn wrong_type(key: &str, expected: &'static str) -> RecordError {
    RecordError::WrongType {
        key: key.into(),
        expected,
    }
}

fn as_object<'a>(value: &'a Value, what: &str) -> Result<&'a Map<String, Value>, RecordError> {
    value
        .as_object()
        .ok_or_else(|| wrong_type(what, "a JSON object"))
}

fn key<'a>(data: &'a Map<String, Value>, name: &str) -> Result<&'a Value, RecordError> {
    data.get(name)
        .ok_or_else(|| RecordError::MissingKey(name.into()))
}

fn string(data: &Map<String, Value>, name: &str) -> Result<String, RecordError> {
    match key(data, name)? {
        Value::String(s) => Ok(s.clone()),
        _ => Err(wrong_type(name, "a string")),
    }
}

fn opt_string(data: &Map<String, Value>, name: &str) -> Result<Option<String>, RecordError> {
    match key(data, name)? {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        _ => Err(wrong_type(name, "a string or null")),
    }
}

fn opt_number(data: &Map<String, Value>, name: &str) -> Result<Option<Number>, RecordError> {
    match key(data, name)? {
        Value::Null => Ok(None),
        Value::Number(n) => Ok(Some(n.clone())),
        _ => Err(wrong_type(name, "a number or null")),
    }
}

fn string_list(data: &Map<String, Value>, name: &str) -> Result<Vec<String>, RecordError> {
    match key(data, name)? {
        Value::Array(items) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect::<Option<_>>()
            .ok_or_else(|| wrong_type(name, "a list of strings")),
        _ => Err(wrong_type(name, "a list of strings")),
    }
}

fn string_map(data: &Map<String, Value>, name: &str) -> Result<Vec<(String, String)>, RecordError> {
    match key(data, name)? {
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
            .collect::<Option<_>>()
            .ok_or_else(|| wrong_type(name, "an object of strings")),
        _ => Err(wrong_type(name, "an object of strings")),
    }
}

/// Python `repr()` of a JSON-decoded value (`None`, `True`, `5`, `6.0`, `'x'`, `[1, 'a']`,
/// `{'k': None}`).
pub fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) if n.is_f64() => match n.as_f64() {
            Some(f) if f.is_nan() => "nan".into(),
            Some(f) if f.is_infinite() => if f > 0.0 { "inf" } else { "-inf" }.into(),
            Some(f) => pyjson::float_repr(f),
            None => n.to_string(),
        },
        Value::Number(n) => n.to_string(),
        Value::String(s) => py_repr_str(s),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(py_repr).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

/// Python `repr(str)`: single quotes unless the text has `'` and no `"`; non-printable characters
/// escaped (`\n`, `\x07`, `​`), printable non-ASCII kept.
pub fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if is_printable(c) => out.push(c),
            c if (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

/// Approximates `str.isprintable` (categories Cc, Cf, Zl, Zp and Zs other than space are not).
fn is_printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    if c.is_control() || c.is_whitespace() {
        return false;
    }
    !matches!(
        c,
        '\u{ad}'
            | '\u{600}'..='\u{605}'
            | '\u{61c}'
            | '\u{6dd}'
            | '\u{70f}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    fn load_err(value: &Value) -> String {
        Session::from_value(value).unwrap_err().to_string()
    }

    // Inputs, and the bytes of `json.dumps(Session.from_dict(d).to_dict(), indent=2)` from
    // CPython 3.14 against lib/tx.
    const LLM: &str = r#"{"turn_started_at": 950.0, "zzz": 1, "schema_version": 6, "id": "ident-llm", "name": "worker", "role": "llm", "state": "exited", "cwd": "/repo", "cmd": "claude --x", "tags": ["a", "b"], "group": "g", "env": {"TX_READ_ONLY": "1", "B": "2"}, "parent": "p", "pid": 123, "attached_to": [{"pane_index": "1", "host": "Views", "window_index": "1", "window_name": "work", "pane_id": "%41"}], "created_at": 900, "ended_at": 1000.5, "engine": "claude", "last_activity": 970.0, "chats": [{"id": null, "role": "fork", "cwd": "/r", "transcript_path": "/t", "origin": {"how": "fork", "session_id": "s1", "chat_id": "c0"}, "bundle_path": null, "started_at": 1.0, "ended_at": null, "summary": "", "engine": "codex"}]}"#;
    const LLM_OUT: &str = "{\n  \"schema_version\": 6,\n  \"id\": \"ident-llm\",\n  \"name\": \"worker\",\n  \"role\": \"llm\",\n  \"state\": \"exited\",\n  \"cwd\": \"/repo\",\n  \"cmd\": \"claude --x\",\n  \"tags\": [\n    \"a\",\n    \"b\"\n  ],\n  \"group\": \"g\",\n  \"env\": {\n    \"TX_READ_ONLY\": \"1\",\n    \"B\": \"2\"\n  },\n  \"parent\": \"p\",\n  \"pid\": 123,\n  \"attached_to\": [\n    {\n      \"host\": \"Views\",\n      \"window_index\": \"1\",\n      \"window_name\": \"work\",\n      \"pane_id\": \"%41\",\n      \"pane_index\": \"1\"\n    }\n  ],\n  \"created_at\": 900,\n  \"ended_at\": 1000.5,\n  \"engine\": \"claude\",\n  \"last_activity\": 970.0,\n  \"chats\": [\n    {\n      \"id\": null,\n      \"role\": \"fork\",\n      \"cwd\": \"/r\",\n      \"transcript_path\": \"/t\",\n      \"origin\": {\n        \"how\": \"fork\",\n        \"session_id\": \"s1\",\n        \"chat_id\": \"c0\"\n      },\n      \"bundle_path\": null,\n      \"started_at\": 1.0,\n      \"ended_at\": null,\n      \"summary\": \"\",\n      \"engine\": \"codex\"\n    }\n  ],\n  \"turn_started_at\": 950.0\n}";
    const OTHER: &str = r#"{"schema_version": 6.0, "id": "sh", "name": "sh", "role": "shell", "state": "alive", "cwd": "/repo", "cmd": "", "tags": [], "env": {}, "parent": null, "pid": null, "attached_to": [], "created_at": 950.0, "ended_at": null, "engine": "claude", "chats": [], "last_activity": 970.0, "kind": "process"}"#;
    const OTHER_OUT: &str = "{\n  \"schema_version\": 6.0,\n  \"id\": \"sh\",\n  \"name\": \"sh\",\n  \"role\": \"shell\",\n  \"state\": \"alive\",\n  \"cwd\": \"/repo\",\n  \"cmd\": \"\",\n  \"tags\": [],\n  \"group\": null,\n  \"env\": {},\n  \"parent\": null,\n  \"pid\": null,\n  \"attached_to\": [],\n  \"created_at\": 950.0,\n  \"ended_at\": null,\n  \"artifact_id\": null\n}";

    #[test]
    fn round_trip_matches_python_bytes() {
        let llm = Session::from_value(&parse(LLM)).unwrap();
        assert_eq!(pyjson::dumps_pretty(&llm.to_value()), LLM_OUT);
        let other = Session::from_value(&parse(OTHER)).unwrap();
        assert_eq!(pyjson::dumps_pretty(&other.to_value()), OTHER_OUT);
        assert_eq!(other.role(), Role::Shell);
        assert!(other.chats().is_empty());
    }

    #[test]
    fn error_texts_match_python() {
        let base = parse(LLM);
        let with = |k: &str, v: Value| {
            let mut d = base.clone();
            d[k] = v;
            load_err(&d)
        };
        let without = |k: &str| {
            let mut d = base.clone();
            d.as_object_mut().unwrap().remove(k);
            load_err(&d)
        };
        let unsupported = |v: &str| {
            format!(
                "record schema_version={v} is unsupported (expected 6); tx-ide does not \
                 back-migrate older records on load (§9) — run `tx migrate` to upgrade older records"
            )
        };
        assert_eq!(load_err(&json!({})), unsupported("None"));
        assert_eq!(with("schema_version", json!(5)), unsupported("5"));
        assert_eq!(with("schema_version", json!("6")), unsupported("'6'"));
        assert_eq!(with("schema_version", json!(5.5)), unsupported("5.5"));
        assert_eq!(without("name"), "'name'");
        assert_eq!(without("role"), "'role'");
        assert_eq!(without("chats"), "'chats'");
        assert_eq!(
            with("state", json!("running")),
            "'running' is not a valid State"
        );
        assert_eq!(with("state", json!(5)), "5 is not a valid State");
        assert_eq!(with("role", json!("view")), "'view' is not a valid Role");
        assert_eq!(with("role", Value::Null), "None is not a valid Role");
        assert_eq!(
            with("engine", json!("gemini")),
            "'gemini' is not a valid Engine"
        );
        assert_eq!(
            with("attached_to", json!([{"host": "x"}])),
            "'window_index'"
        );
        // Python evaluates `state` before `cmd`, so the enum error wins.
        let mut d = base.clone();
        d["state"] = json!("bogus");
        d.as_object_mut().unwrap().remove("cmd");
        assert_eq!(load_err(&d), "'bogus' is not a valid State");
        let mut chat = base.clone();
        chat["chats"][0].as_object_mut().unwrap().remove("summary");
        assert_eq!(load_err(&chat), "'summary'");
        chat["chats"][0]["origin"]
            .as_object_mut()
            .unwrap()
            .remove("chat_id");
        assert_eq!(load_err(&chat), "'chat_id'");
        assert_eq!(with("tags", json!(5)), "'tags' must be a list of strings");
    }

    #[test]
    fn optional_v5_v6_keys_default_to_null() {
        let mut d = parse(LLM);
        let map = d.as_object_mut().unwrap();
        map.remove("turn_started_at");
        map.remove("group");
        let s = Session::from_value(&d).unwrap();
        assert_eq!(s.group, None);
        assert_eq!(s.llm().unwrap().turn_started_at, None);
    }

    fn keys_of(session: &Session) -> Vec<String> {
        session
            .to_value()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    #[test]
    fn nvim_socket_three_states_round_trip() {
        // Absent (a pre-D11 record): stays absent on resave.
        let absent = Session::from_value(&parse(OTHER)).unwrap();
        assert_eq!(pyjson::dumps_pretty(&absent.to_value()), OTHER_OUT);

        let mut d = parse(OTHER);
        d["nvim_socket"] = Value::Null;
        let unset = Session::from_value(&d).unwrap();
        assert_eq!(unset.to_value()["nvim_socket"], Value::Null);
        assert_eq!(keys_of(&unset).last().unwrap(), "nvim_socket");

        d["nvim_socket"] = json!("/h/nvim/sh.sock");
        let path = Session::from_value(&d).unwrap();
        assert_eq!(path.to_value()["nvim_socket"], json!("/h/nvim/sh.sock"));
        assert_eq!(keys_of(&path).last().unwrap(), "nvim_socket");

        d["nvim_socket"] = json!(5);
        assert_eq!(
            load_err(&d),
            wrong_type("nvim_socket", "a string or null").to_string()
        );

        // llm records never carry the key, even if one is on disk.
        let mut llm = parse(LLM);
        llm["nvim_socket"] = json!("/x.sock");
        let llm = Session::from_value(&llm).unwrap();
        assert!(!keys_of(&llm).contains(&"nvim_socket".to_owned()));
    }

    fn llm_session(state: State) -> Session {
        let mut s = Session::from_value(&parse(LLM)).unwrap();
        s.state = state;
        s
    }

    #[test]
    fn behaviour() {
        let mut s = llm_session(State::Idle);
        assert_eq!(s.tmux_name(), "ident-llm");
        assert!(s.read_only());
        assert!(s.matches("READ-only"));
        assert!(s.matches("LLM idle"));
        assert!(s.matches(""));
        assert!(!s.matches("writable"));
        assert!(!s.needs_attention());
        assert!(s.transition_to(State::Waiting));
        assert!(s.needs_attention());
        assert!(!s.transition_to(State::Waiting));
        assert!(s.transition_to(State::Exited));
        assert!(!s.transition_to(State::Idle));
        assert_eq!(s.state, State::Exited);
        assert!(!s.is_alive());

        assert_eq!(State::initial_for(Role::Llm), State::Idle);
        assert_eq!(State::initial_for(Role::Nvim), State::Alive);
        assert_eq!(
            State::valid_for(Role::Shell),
            &[State::Alive, State::Exited]
        );
        assert!(State::Archived.is_terminal() && !State::Alive.is_terminal());
    }

    #[test]
    fn activity_at_uses_python_truthiness() {
        let mut s = llm_session(State::Exited);
        assert_eq!(s.activity_at(), 970.0);
        s.llm_mut().unwrap().last_activity = Number::from_f64(0.0);
        assert_eq!(s.activity_at(), 900.0);
        s.created_at = None;
        assert_eq!(s.activity_at(), 0.0);
        let other = Session::from_value(&parse(OTHER)).unwrap();
        assert_eq!(other.activity_at(), 950.0);
        assert!(!other.read_only());
    }

    #[test]
    fn repr_matches_python() {
        // Expected values from CPython 3.14 `repr()`.
        assert_eq!(py_repr_str("it's"), "\"it's\"");
        assert_eq!(py_repr_str("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(
            py_repr_str("é\u{7}\n\u{a0}\u{200b}→"),
            "'é\\x07\\n\\xa0\\u200b→'"
        );
        assert_eq!(
            py_repr(&json!([1, "a", null, true, {"k": 2.5}])),
            "[1, 'a', None, True, {'k': 2.5}]"
        );
        assert_eq!(py_repr(&json!(1e16)), "1e+16");
    }
}
