# Architecture

tx is a per-invocation CLI (no daemon). The Rust port keeps that, and builds every invocation
out of **components** loaded into a `cordis::Runtime` — the paradigm in `cordis-paradigm.md`.

## Crates

| crate | role |
|---|---|
| `crates/cordis` | the runtime: `Component` (inject / provide / apply), `Ctx` (get, provide, effect, plug), fibers with the paper's lifecycle (`Inactive → Active → Unloading`), the unload guard, LIFO undo stacks, failure unwinding; `loader` reconciles a list of entries into fibers (§5.2). Synchronous; `apply` runs to completion, so `Reloading` is transient. |
| `crates/tx` | the `tx` binary: domain model, adapters (tmux, git, fs), and the components that implement the verbs. |

## How an invocation runs

1. `main` snapshots the environment and plugs the **core** services (`app::plug_core`): env,
   home, events, store, tmux, the engine table, the session service, the command table. They are
   not configurable — every verb needs them.
2. `plugins::plug_features` builds the entry list from the manifest (`plugins::MANIFEST`) and the
   `plugins` section of `config.json`, and the loader plugs the enabled ones. Each registers rows
   into shared tables (verbs into the command table, adapters into the engine table); every
   registration is an effect whose undo removes exactly that row.
3. The CLI resolves argv against the command table and runs the verb.
4. `Runtime::shutdown` unwinds everything, consumers before providers.

## Configuring components

```json
{ "plugins": { "engine.codex": { "disabled": true }, "verbs.chat": { "disabled": true } } }
```

Entries: `engine.claude`, `engine.codex`, `engine.antigravity`, `verbs.home`, `verbs.sessions`,
`verbs.listing`, `verbs.hooks`, `verbs.chat`, `verbs.artifacts`, `verbs.install`. A disabled verb
group's verbs disappear from the command table and `--help`; a disabled engine makes spawns that
name it fail with `no engine adapter is registered for <engine>`. `tx _plugins` lists every entry
and its status (active / waiting / disabled / failed). Keys other than `disabled` are the entry's
configuration: when they change, the loader rebuilds only that component (none of the built-in
components read configuration yet). A malformed `config.json` loads the defaults — only the
reference's own readers (reconcile, sync) fail on it — and a bad `plugins` section prints one
warning per problem and keeps going.

## Keys

Core keys are provided once (single provider, per the calculus). Extensible sets — CLI verbs and
engines today — are **commutative tables with unique row ids** (paper §3.4.2): any number of
components register into them in any order, and removing one component removes only its rows.
Order-sensitive concerns stay behind a single provider.

## What of the paper is built, and what is not

Built: revertible effects with LIFO undo, typed keys with declared inject / provide (undeclared
and inactive access are errors), the fiber lifecycle with the unload guard, instantiation with
cascading unload, failure unwinding, commutative tables, the loader. Tested: ordering, the guard,
LIFO, cascade, failure, and confluence + recovery by property tests (runtime and loader).

Not built: hot module replacement (§5.2.2 — needs a dynamic-library or WASM boundary in Rust),
asynchronous transitions and mid-transition divert (§4.4 — `apply` is synchronous), isolation
realms and interception (§3.2.3), cycle detection (§6.5 — cyclic components stay inactive, as the
paper describes), third-party components (only the built-in manifest can be enabled).

Because tx lives for one invocation, unload happens only at exit, and the side effects verbs cause
(tmux sessions, records) are emissions past the system boundary (§6.1), not reverted. The paper's
reactive half — replacing a provider while running, reloading — pays off once a resident process
(an nvim bridge, the sessions-graph server) runs on the same runtime; `plug` / `unplug` /
`Loader::reconcile` already settle at any time.

## Not observable

None of this leaks into the parity surface (`PARITY-CONTRACT.md`): with no `plugins` section the
output of every verb is identical to the reference's.
