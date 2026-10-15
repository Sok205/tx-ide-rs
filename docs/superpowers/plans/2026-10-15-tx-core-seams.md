# tx-core seams Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `poc/tx-core` truly extendable by adding exactly four seams and nothing else: API hygiene, an in-process event sink, a `resume` slot for engines, and a per-engine envelope.

**Architecture:** The core keeps its three invariants (disk is the record, tmux holds only `@tx-id`, every write under its lock) and exposes seams only. Events are emitted after the record is saved and the lock is released; sinks cannot veto and never receive a handle to the core. Everything else (chat ops, worktrees, discovery, notifications) stays in plugin crates.

**Tech Stack:** unchanged (Rust 2024, serde, thiserror). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-10-15-tx-core-design.md` plus the agreed seam list in this plan.

## Global Constraints

- Nothing but the four seams. No chat ops, no discovery, no worktrees in core.
- Every seam change keeps the existing 434 tests green (edited only where the API shape forces it).
- `cargo clippy --workspace --all-targets -- -D warnings` clean from `poc/`.
- No sink runs while `create.lock` or `<id>.lock` is held. A panicking sink never breaks an operation.
- Record JSON stays version 1; new fields deserialize with defaults.

## Decisions

| question | decision |
|---|---|
| Which types get `#[non_exhaustive]` | Inputs and enums plugins match on or build: `CoreError`, `EventType`, `WorkspaceAction`, `LaunchRequest`, `LaunchPlan`, `CreateOptions`, `LaunchOptions`, `CoreConfig`, `ListFilter`, `SendOptions`, `Event`. Outputs stay exhaustive so callers can destructure them: `Session`, `Launch`, `DeliveryReceipt`, `ExecutionLocation`, `RefreshResult`. |
| Builders | `LaunchRequest::new(cwd, profile)` + `.model() .effort() .initial_prompt() .resume()`; `LaunchPlan::new(program)` + `.arg() .args() .env() .workspace()`; `CreateOptions::new(name, cwd, launch)` + `.parent() .tags() .env()`; `LaunchOptions::agent(engine)` + `.profile() .model() .effort() .initial_prompt() .resume()`, `LaunchOptions::process(program, args)`; `CoreConfig::new(state_dir)` + `.tmux_socket() .env() .profile_dirs() .skill_dirs()`; `ListFilter::new()` + `.parent() .tag() .lifecycle()`; `SendOptions::from_agent(id)`. Fields stay `pub` for reading. |
| Module visibility | API modules stay `pub` (`config`, `core`, `engine`, `error`, `events`, `profile`, `session`). Internals become `#[doc(hidden)] pub` (`tmux`, `locks`, `store`, `messaging`) with a doc line "no stability promise": the fuzz suites use them, plugin authors must not. |
| `EventType::ALL` | becomes `&'static [EventType]`. |
| `Event` | `{ ts: f64, type_: EventType, id: SessionId, name: String, actor: Option<SessionId>, detail: String }`. The log line gains `"id"`. |
| Sink contract | `trait EventSink: Send + Sync { fn on_event(&self, event: &Event); }`; blanket impl for `F: Fn(&Event) + Send + Sync`. `TxCore::subscribe(&mut self, Box<dyn EventSink>)`. Sinks run in registration order, after the operation's lock guards drop, wrapped in `catch_unwind`; a panic is swallowed (the log line was already written). |
| Where emit happens | Each mutating operation returns its pending events from the locked inner function; the public method drops guards, writes the log line(s), then calls sinks. Log before sinks, so the file never lags the sinks. |
| `resume` | `LaunchOptions::Agent { resume: Option<String> }` → `LaunchRequest::resume: Option<&str>` → stored in `Launch::Agent { resume }` (serde default). Engines: claude emits `--resume <id>` and drops the initial prompt flagging rules unchanged; codex emits `resume <id>` as the subcommand. Core does not interpret it. |
| Envelope | `EngineAdapter::envelope(&self, sender: Option<&str>, text: &str) -> String { messaging::build_envelope(sender, text) }`. `send_message` uses the recipient's engine's envelope when the recipient is an agent whose engine is registered; otherwise the default. |
| `Send + Sync` | required on `EngineAdapter` and `EventSink`; `TxCore` becomes `Send + Sync` (assert with a compile-time test). |

## Tasks

### Task 1: API hygiene
Files: `src/{config,core,engine,error,events,session,lib}.rs`, every test and engine crate that used struct literals, `README.md` examples.
- [ ] Tests first: a compile-time `fn assert_send_sync<T: Send + Sync>()` for `TxCore` and `Box<dyn EngineAdapter>`; builder round-trip tests for each builder; `EventType::ALL` is a slice.
- [ ] Add `#[non_exhaustive]` and builders; mark internals `#[doc(hidden)]`; rewrite literals in `tests/`, `tx-engine-*`, `tx-core-cli` to builders.
- [ ] `cargo test --workspace`, clippy clean. Fuzz agent: builders vs. fields, exhaustiveness.

### Task 2: Event + EventSink + subscribe
Files: `src/events.rs`, `src/core.rs`, `tests/events_sink.rs`.
- [ ] Tests first: sink receives one event per mutation in order with the right type/id/name/actor; log line has `"id"`; a sink holding a second `TxCore` on the same dir can call `get_session` and `set_session_tags` on the same id from inside `on_event` without deadlock (proves locks are released); a panicking sink does not fail the operation and later sinks still run; sinks are called after the record is saved (sink reads the new lifecycle from disk).
- [ ] Implement: inner `*_locked` functions return `(result, Vec<Event>)`; `emit` after guards drop.
- [ ] Fuzz agent: ordering, panics, re-entrancy, concurrent operations with sinks.

### Task 3: `resume`
Files: `src/session.rs`, `src/core.rs`, `src/engine.rs`, both engine crates, CLI (`--resume ID`).
- [ ] Tests first: round trip in the record (old JSON without `resume` still loads); `LaunchRequest.resume` reaches the adapter; claude/codex argv with resume; CLI flag.
- [ ] Implement. Fuzz agent on the engines' argv.

### Task 4: per-engine envelope
Files: `src/engine.rs`, `src/core.rs`, `tests/lifecycle.rs`.
- [ ] Tests first: a test engine overriding `envelope` sees its format typed into the pane; a `Process` recipient and an agent with an unregistered engine get the default.
- [ ] Implement. Final: full suite, clippy, README update, fresh review.
