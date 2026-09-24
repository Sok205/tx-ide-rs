# Architecture

tx is a per-invocation CLI (no daemon). The Rust port keeps that, and builds every invocation
out of **components** loaded into a `cordis::Runtime` — the paradigm in `cordis-paradigm.md`.

## Crates

| crate | role |
|---|---|
| `crates/cordis` | the runtime: `Component` (inject / provide / apply), `Ctx` (get, provide, effect, plug), fibers with the paper's lifecycle (`Inactive → Active → Unloading`), the unload guard, LIFO undo stacks, failure unwinding. Synchronous; `apply` runs to completion, so `Reloading` is transient. |
| `crates/tx` | the `tx` binary: domain model, adapters (tmux, git, fs), and the components that implement the verbs. |

## How an invocation runs

1. `main` builds a `Runtime` and plugs the **core** components (home paths, config, event log,
   session store, tmux adapter, command table).
2. It plugs the **feature** components listed in the plugin manifest (defaults = everything the
   reference ships; `config.json` `plugins.<name>.disabled` turns one off). Each feature injects
   the core keys it needs and registers its verbs, engines, hook handlers or renderers into the
   shared tables — every registration is an effect whose undo removes exactly that row.
3. The CLI resolves argv against the command table and runs the verb.
4. `Runtime::shutdown` unwinds everything, consumers before providers.

## Keys

Core keys are provided once (single provider, per the calculus). Extensible sets — CLI verbs,
engines (claude / codex / antigravity / future), hook events, `tx ls` sections — are **commutative
tables with unique row ids** (paper §3.4.2): any number of components register into them in any
order, and removing one component removes only its rows. Order-sensitive concerns stay behind a
single provider.

## Customizability

A third-party feature is a `Component` that injects the keys it needs and registers rows. Because
dependencies are declared, a feature whose engine or service is disabled simply stays inactive
instead of failing, and disabling a feature cannot leave debris (recovery exactness).
The long-lived half of the paper (hot reload, reactive re-resolution while running) applies once a
resident process exists (a server for the remote-control / graph prototypes, or an nvim bridge);
the runtime already supports it because `plug` / `unplug` settle at any time.

## Not observable

None of this leaks into the parity surface (`PARITY-CONTRACT.md`): the output of every verb is
identical to the reference's.
