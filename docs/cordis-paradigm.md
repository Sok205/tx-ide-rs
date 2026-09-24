# "A Programming Paradigm for Spatiotemporal Composability" — explained in OOP terms

Paper: Shi, Zhang, Cui — arXiv 2608.25512 (Aug 2026). Implementation: **Cordis** (TypeScript). Case study: **Koishi** (chatbot framework, 4000+ plugins).

The paper is written in the language of type theory and category theory, but the *ideas* are ones an OOP programmer already has intuitions for. Part 1 walks through the paper section by section, replacing each formal idea with the design pattern or OOP concept it most resembles. Part 2 is a dictionary of the important terms.

---

## Part 1 — The walkthrough

### 1. The problem: plugins that can leave

Think of an application as a big shared object — call it `World`. Plugins (the paper says *components*) are objects that get constructed, poke at `World` (register routes, open connections, add event listeners), and expose services to other plugins.

Two things go wrong when you try to **unload** a plugin at runtime:

- **Temporal problem** — the plugin scribbled all over `World`. Who undoes that? In VSCode, nobody: 87 of the top 100 extensions contain code, and removing any of them requires restarting the whole extension host. The `deactivate()` hook is a separate method from `activate()`, so it's easy to forget to undo something.
- **Spatial problem** — other plugins depended on this one. Who tells them? Who deactivates them, and who reactivates them when a replacement shows up? In VSCode, `getExtension(...).exports` is typed `any` and only 7 of the top 100 extensions declare dependencies at all.

The usual workaround is coarse: restart the whole process (the OS gives you "temporal composability" per process) and let Kubernetes manage dependencies (per service). The paper argues that's the wrong granularity — you lose all in-memory state and pay network overhead for what could be a method call.

**OOP framing:** you want *objects* that can be destroyed cleanly (temporal) and a *DI container* that reacts to change (spatial), and you want proofs that this works even when many objects come and go in an interleaved way.

### 2. Background: effects and coeffects

Two words from type theory:

- An **effect** is what a computation *does to* the world (mutation, I/O). Effect systems annotate the *return type*: "this method mutates state."
- A **coeffect** is what a computation *needs from* the world (a resource, a permission, a service). Coeffect systems annotate the *context*: "this method requires a DB connection."

OOP analogy: an effect is a method with side effects; a coeffect is a constructor parameter / `@Inject` field. Monads and comonads are the categorical machinery behind these — you don't need them to follow the paper.

The paper's move: classical effect/coeffect systems are **compile-time, lexically-scoped** analyses. Plugins are loaded after compilation, so the paper takes the same two ideas and turns them into **runtime objects** the framework can manipulate.

### 3.1 Revertible effects — the Command pattern with mandatory `undo()`

Start with `World` as a plain value type `Γ` (gamma). Any side effect is a function `f: Γ → Γ`. Chaining side effects is function composition — a monoid, if you like the word.

**Definition 2, the effect context `∂Γ`:** pair the world with an undo stack.

```ts
class EffectContext {
  state: World;            // γ — current state
  accumulator: () => void; // φ — composed undo of everything done so far
}
```

`track(f, g)` = run `f`, and push its undo `g` onto the accumulator. `recover()` = run the whole accumulator and reset it. Theorems 4–7 say the obvious-but-necessary things: tracking doesn't change what `f` does, tracking two steps is the same as tracking their composite, and if each `g` really undoes its `f`, recovery lands you back at the start (the **soundness invariant**: `φ(γ) = γ₀`).

**Two upgrades (Section 3.1.2).** The simple model fixes the undo function before seeing the state, and can only undo *everything at once*. So the paper upgrades an effect to:

```ts
type Effect = (state: World) => [World, () => World];  // returns new state AND its own undo
```

This is `𝔈_Γ`. It's exactly the Command pattern where `execute()` returns the `undo` closure. The **witness** (`𝔈*_Γ`) is the contract: `undo(newState) === state` — the undo must actually revert, at least at the state it was applied to. Composition `f ⋄ g` chains commands and composes undos in reverse (LIFO), like an undo stack.

`effect_Γ` then lifts an effect one level up so that it operates on the `EffectContext` and *also* returns an undo — so undoing is itself a tracked effect (whose undo is "redo"). The `∂²Γ` tower is just "undo stacks can be nested."

**Effect iterators (3.1.3)** — a component doesn't perform one effect, it performs a sequence. The paper models this as a **generator**: each `yield` produces `(newState, undo, continuation?)`. Undoing runs the yielded undos in reverse. This maps directly onto `function*` / `yield` in JS or Python. The `Maybe(ℑ)` continuation is the generator's `done` flag.

**What "local temporal composability" buys you:** for *one* component in isolation, loading = run the generator and collect undos; unloading = run the undos in reverse. Guaranteed to restore the world.

### 3.2 Reactive coeffects — a DI container with an Observer attached

Now the dependency side. The **coeffect context `Σ`** is a typed dictionary:

```ts
class DIContainer {
  get<K>(key: K): Value<K>;                 // precondition: key is present
  set<K>(key: K, v: Value<K>): () => void;  // precondition: key absent; returns the undo (delete)
}
```

The crucial observation: `set` **is** a revertible effect — its undo is `delete key`. So dependency registration is automatically tracked and reverted by the machinery from 3.1. That's the "synergy" the paper keeps mentioning: coeffect operations are effects, effects are revertible.

**Specification and notification (3.2.2).** A component declares what it needs: `d = Set<Key>` — think `@Inject` annotations or a constructor's parameter list. The satisfaction predicate `σ ⊧ d` is "every required key is present." Every time the container changes, each component's requirements are re-checked and the change is classified:

- unsatisfied → satisfied: **activating** — run the component's effects
- satisfied → unsatisfied: **deactivating** — run its undo stack
- no change: **neutral** — nothing

This is the Observer pattern: the container is the subject, components are observers, and `notify` is the callback. Because all mutation flows through `set` (an effect), no change can slip past the observer.

**Isolation and interception (3.2.3).** Two extras that real DI containers have:

- **Isolation** = *realms/namespaces*. The same key can resolve to different values for different components (multi-tenancy, sandboxes, tests). Implemented as a two-layer lookup `key → realm → value`. Like child injectors in Angular/Guice.
- **Interception** = *middleware on dependency access*. Attach metadata to a key; the provider gets `component's metadata ⊕ context's metadata` (context wins) and can adjust behavior. Like an AOP advice or a decorator on the getter — e.g. "this component gets read-only DB access."

Neither one writes the shared table; they produce a *derived* child context. This leads to **Definition 23, two realizations**: an effect can be *in-place* (mutate and return a real undo) or *derived* (return a fresh child object with identity as its undo — undo is "throw the child away"). Prototype-chain inheritance in JS is the derived flavor.

### 3.3 The context paradigm — one `ctx` object to rule them all

**Unified context (3.3.1):** `Γ∞ = μΓ. Γ × (Γ → Γ) × Σ` — a recursive type, meaning:

```ts
class Context {
  parent: Context | null;   // the enclosing level
  accumulator: () => void;  // this level's undo stack
  store: DIContainer;       // this level's dependencies
}
```

Contexts form a **tree** (parent/child), and a parent's undo stack includes "dispose all children." That's the literal "plug-in" metaphor: load = run effects, unload = undo them, nested arbitrarily.

**Coeffect = value + allowed operations (Definition 29).** A key doesn't just have a value type; it comes with an **interface** — the set of operations `𝒜_k` you may call on it, each of which is itself a revertible effect on that value (and may return an *outcome*). This is encapsulation: you interact with a service only through its published methods.

**The discipline (Definition 30):** a component's effect generator may only consist of *stages* that are (a) an operation on a key it declared, or (b) providing a key it said it provides. Nothing else. No touching globals. If you have a global counter, bind it at a key so the context knows about it. That's the "context paradigm": every interaction with the environment is mediated by `ctx`.

**Observational equivalence (3.3.2)** — behavioral `equals()`. "Recovery restores the state" can't mean bitwise equality: `free()` doesn't restore the heap layout, and a freshly generated ID is a different ID. So the paper defines `≃`: two values at a key are **indistinguishable** (`≈`) if no *test* — any sequence of the key's public operations — can tell them apart by their outcomes. Two contexts are `≃` if they bind the same keys to indistinguishable values. Everything hidden behind the interface is ignored.

In OOP: `equals()` defined by observable behavior, not memory identity. And a design lever falls out (Definition 31): the *fewer* outcomes an interface publishes, the coarser the equivalence, the more orders of operations count as "the same." (POSIX `open` must return the *lowest* free descriptor, so two `open`s don't commute; `mmap` can return any address, so two `mmap`s do.)

### 3.4 Independence — when do two components not step on each other?

Local guarantees hold for one component alone. With many components interleaved, an undo runs at a state that *other* components have moved since. When is that still correct?

**Definition 42, independence:** two components are independent when every transformation of one **commutes** with every transformation of the other (forward maps and undos alike), and neither changes what the other yields. Theorem 43: if a family is pairwise independent, you can apply their undos in *any order* and still get back to the start.

**Coeffect commutativity (3.4.2):** operations at *different* keys are always independent (each only touches its own binding — Theorem 45). So the question reduces to: do the operations at *one* key commute? A key is **commutative** when they do, and a coeffect is **witnessed** when its provider ships a proof of that. Examples:

- A table of listeners where each registration gets its own unique ID → commutative (either order, either undo order works). CRDTs attach unique tags for exactly this reason.
- An ordered middleware chain → *not* commutative (inserting A before B changes what B sees).

**Theorem 47:** two components whose effect generators follow the paradigm are independent as long as neither *provides* a key the other *uses*, and every shared key is commutative. And that disjointness is readable straight off their declarations.

Design takeaway the paper draws: the commuting part of your system lives in effects (revert in any order); the order-sensitive part lives in coeffects, where order is imposed either by a single component's LIFO undo stack or by the provider-before-consumer dependency relation.

### 4. A calculus of dynamic composition — the lifecycle state machine

Now we get an *operational semantics*: precise rules for how the runtime moves things.

**Component (Definition 48)** = the class:

```ts
interface Component {
  inject: Set<Key>;     // d — what I need
  provide: Set<Key>;    // p — what I may install
  apply: Generator;     // e — my effect iterator, only touching keys in d ∪ p
}
```

**Fiber (Definition 49)** = an instance of the class, with runtime state:

```ts
class Fiber {
  component: Component;
  parent: Fiber | 'root';                  // π — who instantiated me
  table: Map<Key, Value>;                  // σ — bindings I've installed
  retired: boolean;                        // τ — orchestrator asked me to leave
  state: Inactive | Reloading | Active | Unloading;   // θ
  // inside the non-Inactive states:
  accumulator: () => void;                 // g — my undo stack
  committed: Map<Key, FiberName>;          // ω — which fiber I resolved each dependency to
}
```

The **registry** is the tree of all fibers (like a DOM). The global DI container isn't stored; it's *derived*: the union of the tables of all `Active` fibers. Each key has at most one provider (the orchestrator rejects overlapping `provide` sets).

**Nine rules.** Three **orchestration** rules are the external API:

- `O-Insert` — `new Fiber()` (as `Inactive`).
- `O-Retire` — mark `retired = true`. Unconditional; a request, not an action.
- `O-Remove` — delete a fiber that is `Inactive`, retired, has an empty table, and no children. (Garbage collection.)

Six **lifecycle** rules run automatically whenever their preconditions hold — the state machine:

- **Target view** `target(n)`: for each key the fiber needs, *which fiber should provide it right now* — or `⊥` if retired or unsatisfiable. Recomputed from the current state.
- **Committed view** `ω`: which fiber it *actually* resolved to when it last activated. Recording the *provider's name* rather than the value matters: a replacement provider with an identical value still counts as a change.
- The whole machine runs on `target == committed ?`:
  - `L-Begin`: `Inactive` and target ≠ ⊥ → enter `Reloading` with an empty accumulator and `committed = target`.
  - `L-Iter` / `L-Finish`: while target still == committed, pull the next `yield` from the generator, push its undo. Last one → `Active`.
  - `L-Divert`: mid-transition and target changed → go to `Unloading` with the undos collected so far (optionally abort the in-flight step).
  - `L-Leave`: `Active` and target changed → `Unloading`. Records the decision *without acting on it* — the fiber stops providing, but keeps its committed view.
  - `L-Unload`: `Unloading` and **nobody still relies on me** → run the accumulator, drop the view, go `Inactive`. This is the only rule that ever runs an undo stack.

**The guard** (`¬relied(n)`) is the key trick. A provider must not withdraw its bindings until every consumer that resolved a key to it has finished its *own* teardown — because teardown code often needs the dependency (closing a pool means handing connections back). Consumers keep reading through their committed view during teardown; only after they're gone does the provider's undo run. This is reference counting. Why it doesn't deadlock: once a provider enters `Unloading` it drops out of the derived container, every consumer's target flips, and they all start leaving.

**Instantiation (Definition 52):** a component's generator may `ctx.use(child)` — an `O-Insert` whose undo is `O-Retire` of the child. So unloading a parent cascades to children. This is how a plugin host loads plugins.

**Confinement (4.2.3):** a fiber's effects may write only its own table (plus values at keys it declared, which live in the provider's table), and read only those. It may not branch on the lifecycle state of a fiber it didn't declare. Lemma 57 says following the paradigm gives you confinement for free.

### 4.3 Metatheory — the five guarantees

These are proved over *all* interleavings, so they hold for any scheduler.

1. **Preservation (Theorem 64)** — the registry stays well-formed: parent pointers are valid, provisions are disjoint, a committed view always names installed fibers. *Invariants hold.*
2. **Temporal composability (Theorem 68, Corollary 69 — "recovery exactness")** — running a fiber's undo stack at *any* later point removes exactly its contribution and nothing else, up to `≃`. Its table ends empty. Requires pairwise independence, which Lemma 66 says the paradigm supplies automatically (commutative keys + declared disjointness), and Lemma 67 handles the "entangled" provider/consumer pairs by noting the rules never interleave them the bad way.
3. **Spatial composability (Theorems 70–71)** — *Ordering*: a fiber only activates when its dependencies are provided, and a provider outlives every consumer that resolved to it. *Resolution coherence*: a single transition never runs against two different resolutions; if the resolution changes mid-way, the fiber is diverted and cleanly unwound.
4. **Progress (Theorem 73)** — no deadlock (if the system isn't quiescent, some rule applies) and termination (bounded steps), assuming the dependency graph is acyclic.
5. **Confluence (Theorem 80)** — whatever sequence of loads/unloads/replacements happened, the quiescent state equals the one you'd get by loading the *final* configuration from scratch in dependency order. *History leaves no trace.* Same idea as "React's UI is a pure function of state." It licenses reasoning about the system as if it were statically assembled.

**Extensions (4.4):** *asynchrony* (a transition in flight runs to completion — "inertia"; the guarantees survive because they never relied on aborting); *failure* (a step can throw; the fiber unwinds via the `Unloading` route and is marked FAILED, not retried); *isolation* (realms = extending the key set to `Key × Realm`); *configuration* (config is baked into the effect function; revising a fiber = retire → unload → remove → reinsert at the same name).

### 5. Cordis — the implementation

Theory-to-code map (abbreviated):

- `Γ∞`, the unified context → `ctx`
- `effect_Γ(e)` → `ctx.effect(callback)` — callback returns/yields disposers
- `get`, `set` → `ctx.get(key)`, `ctx.set(key, value)`
- `isolate`, `intercept` → `ctx.isolate(key, realm)`, `ctx.intercept(key, meta)`
- Fiber ⟨d, p, e, π, σ, τ, θ⟩ → `fiber` with `.inject`, `.apply`, `.parent`, `.state`, `.committed`, `.target`, `.dispose`
- `O-Insert` + `O-Retire` → `ctx.use(component, config)` and the disposer it registers
- Lifecycle rules → `refresh` / `reload` / `unload` (Algorithm 5)
- The guard → `unload` awaits all notified dependents before running `dispose`

Everything funnels through `ctx.effect`. `execute` drives the generator, folds each yielded disposer in LIFO order, and checks a guard before each step (the iteration-boundary divert). `dispose` is armed once (a double undo would be an undo at a state no effect produced).

`ctx.set` is `ctx.effect` with "insert into store; return delete" and calls `notify`, which walks all fibers and `refresh`es those that inject the changed key in the same realm. `refresh` recomputes `fiber.target` (a hash of provider UIDs), and if it differs from before and no transition is in flight, spawns `reload` or `unload`. `reload` commits the view, runs `apply`, then checks the target again — if it moved, chain straight into `unload`; symmetrically `unload` chains back into `reload`. That mutual recursion is the inertial state machine.

**Proxy access (5.1.4):** `ctx.database` instead of `ctx.get('database')`. A JS `Proxy` walks up the fiber chain; if the key is in a committed view, return it; if a fiber declared it but isn't loaded, throw `INACTIVE_ACCESS`; if nobody declared it, throw `UNDECLARED_ACCESS`. This turns the `inject` declaration into a *capability list* enforced at the point of use.

**Component loader (5.2):** a declarative configuration tree of *entries* (`id`, `url`, `isolate`, `intercept`, `config`, `disabled`) that the loader reconciles into fibers. Each field change gets the least disruptive operation (config → hand to component; intercept → in place; url → rebuild; disabled → unload/reload). Sound *because* of confluence (final state depends only on final config), progress (reconciliation completes), recovery exactness (rebuilding one entry doesn't disturb neighbors), and ordering (no manual load order — modules load concurrently and activate when dependencies appear).

**Hot module replacement (5.2.2):** since a fiber already bounds all of a module's effects, HMR is just: classify changed modules (accept/decline fixpoint), find stale entries, then `dispose` old fiber + `use` fresh one — transactionally, with cache rollback on import failure. No `module.hot.accept` boundaries needed.

**Koishi case study (5.3):** 4000+ community plugins; plugins are disabled from a console and reverted in place; HMR re-applies edited plugins while preserving live connections elsewhere; switching a database backend reactivates only its dependents. Authors coordinate on nothing but the coeffect key. Threat to validity: single ecosystem, single language, no controlled comparison.

### 6. Discussion — the honest limits

- **System boundary (6.1):** an effect is revertible only for locations the system *exclusively* controls. A `write()` to a socket is an *emission* — it crosses the boundary and acts as identity on `Γ`. The *acquisition* (the descriptor record) is inside. For emissions you either withhold (output-commit) or compensate (refund the charge) — compensations compose LIFO, but the commutation proofs would need redoing against the coarser equivalence.
- **Service multiplexing (6.2):** one key = one provider in the calculus. For many providers, use a *broker* component that dispatches — that gives load balancing, rolling updates (add new provider, shift traffic, retire old), and cross-process RPC.
- **Access control (6.3):** `inject` = capability request; interception = fine-grained policy without touching provider or consumer. True sandboxing of untrusted code still needs a process/WASM boundary.
- **Language requirements (6.4):** closures (for undos), runtime module load/unload, a way to type keys (typeclasses/traits/module augmentation), and a way to interpose on access (Proxy, descriptors, macros).
- **Cycles (6.5):** A needs B and B needs A → both stay inactive forever, detectable from declarations alone. Fix by splitting into cores + integration components (may grow quadratically).
- **Versioning (6.6):** linking is by key *name*, so interface drift and key collision are unsolved. Options: namespaced keys, peer dependencies (what Cordis does), structural compatibility (hard in general).
- **Co-design (6.7):** a language could make `ctx` implicit and compile generators to state machines; an OS could hand out memory/FDs as coeffects and make storage writes transactional.

### 7. How it differs from things you may know

- **Command pattern / sagas / `deactivate()` hooks** — undo is a *separate, optional* duty there; here every atomic effect *returns* its undo and composites are derived automatically.
- **React `useEffect` cleanup** — closest in spirit, but hooks can't be conditional, async, or nested, so there's no composite inverse.
- **STM / reversible computing / RAII / Rust ownership** — scope of reversal is fixed statically; here it spans a component's whole runtime lifecycle.
- **Erlang hot code loading, DSU, webpack HMR** — they *migrate state forward* with hand-written functions; Cordis reverts and reapplies from a clean slate (loses in-memory state unless placed in a longer-lived dependency, but needs no migration code and can fully unload).
- **Spring / Guice / Angular DI** — wire once at init; no reactive re-resolution when a provider is replaced.
- **OSGi Declarative Services / iPOJO** — closest for reactivity, but recovery is a hand-written synchronous callback.
- **Signals / FRP** — value-level reactivity with glitch freedom; Cordis is component-level with async lifecycle. Complementary.
- **Effekt (effects as capabilities)** — static, type-level; Cordis is runtime and aims at reversion, not modular interpretation.

---

## Part 2 — Dictionary

OOP analogy in *italics*.

### Core vocabulary

- **Composition** — assembling a system from parts. *Static* = resolved at compile time. *Dynamic* = parts arrive and leave at runtime (plugins).
- **Temporal composability** — a component's side effects can be *completely reverted* when it's removed. *Destructor that really cleans up everything.*
- **Spatial composability** — components declare dependencies on each other and the runtime resolves, provides, and withdraws them reactively. *A DI container that fires observers.*
- **Effect** — what a computation does *to* its environment. *A method with side effects.*
- **Coeffect** — what a computation needs *from* its environment. *A constructor dependency / `@Inject` field.* In Section 3.3 it becomes (value type, allowed operations) — *an interface*.
- **Context (Γ)** — the shared state the system runs against. `Γ∞` is the unified version: a recursive tree of (state, undo stack, DI store). *The `ctx` object.*
- **Context paradigm** — the discipline that every effect and coeffect must go through the context; nothing touches globals directly.

### Revertible effects (3.1)

- **Inverse / left inverse (g)** — a function that undoes another: `g ∘ f = id`. One-sided. *`undo()`.*
- **Twisted composition** — composing (do, undo) pairs: dos chain forward, undos chain in reverse. *Pushing commands onto an undo stack.*
- **Effect context (∂Γ)** — `(state, accumulator)`. *World + undo stack.*
- **Accumulator (φ; `g` in Section 4; `fiber.dispose` in Cordis)** — the composed undo of everything done so far.
- **track** — apply an effect and push its undo. **recover** — run the accumulator and reset it.
- **Soundness invariant** — `φ(γ) = γ₀`: running the accumulator gets you back to the start.
- **Effect function (𝔈_Γ)** — `state ↦ (newState, undo)`. *`execute()` that returns its own `undo`.*
- **Witness / witnessed (𝔈*_Γ)** — a proof that the undo actually reverts at the state it was applied. *A contract the author is responsible for; the runtime does not check it.*
- **Effect composition (⋄)** — chain two effect functions; undos compose in reverse.
- **Realization** — *in-place* (mutate, return a real undo) vs *derived* (return a fresh child; undo = discard it). *Mutation vs copy-on-write.*
- **Effect iterator (ℑ_Γ)** — an effect function that yields a sequence of `(newState, undo, next?)` steps. *A generator function.*
- **Delimited continuation** — the "rest of the computation" captured at a `yield` boundary.
- **Local temporal composability** — the guarantee for one component alone: load = collect undos, unload = run them LIFO.

### Reactive coeffects (3.2)

- **Coeffect context (Σ)** — a typed partial map `Key ⇀ Value_key`. *DI container.*
- **Dependent type family (𝒱_k)** — each key has its own value type. *Generic `get<K>(k): Value<K>`.*
- **get / set** — read a key; install a key and return the undo that deletes it. `set` is itself a revertible effect.
- **Coeffect specification (d)** — the set of keys a component requires. *Injected dependencies.*
- **Satisfaction predicate (σ ⊧ d)** — every key in `d` is present.
- **notify / classification** — each change is *activating*, *deactivating*, or *neutral* relative to a specification. *Observer callback.*
- **Isolation / realm** — a second lookup layer `key → realm → value`. *Scoped/child injectors; namespaces.*
- **Interception** — metadata attached to a key, merged (`⊕`, context wins) and handed to the provider on access. *Middleware / AOP advice on a getter.*
- **Provider / consumer** — installs a key vs declares it.

### Unification and equivalence (3.3)

- **Coeffect operations (𝒜_k)** — the operations a value at key `k` offers; each a revertible effect on the value, possibly returning an *outcome*. *Public methods of an interface.*
- **Lift (a^Σ)** — an operation on a value, extended to the whole container by touching only that key's binding.
- **Stage** — one step of a component's iterator: *operation stage*, *provision stage*, or *instantiation*.
- **Context-mediated iterator** — an iterator built only from stages. "Everything goes through `ctx`."
- **Observational equivalence (≃)** — two states are equal when no observer can distinguish them. *Behavioral `equals()`.*
- **Test** — a finite sequence of operations (and undos) applied to a value, judged by outcomes.
- **Indistinguishable (≈)** — no test tells two values apart. `≃_k` = indistinguishability under key `k`'s operations.
- **≃_S** — equivalence restricted to keys in `S`. Components are judged at `S = d ∪ p`.
- **Respect** — a function maps related inputs to related outputs.
- **Hierarchical composition** — contexts form a tree; a parent's undo disposes its children. *Composite pattern.*

### Independence (3.4)

- **Transformation monoid (𝔐(i))** — everything an iterator can do to the state, closed under composition.
- **Independent** — every transformation of one iterator commutes with every transformation of the other, and neither changes what the other yields. *Objects whose methods never interfere.*
- **Commutative key** — all operations at that key are pairwise independent. *Order of method calls doesn't matter observably.*
- **Witnessed coeffect** — a coeffect that ships a proof its key is commutative. Obligation on the provider.
- **Theorem 43** — under pairwise independence, undos can be applied in any order.
- **Theorem 47** — context-mediated iterators are independent iff neither provides a key the other uses and every shared key is commutative.

### The calculus (4)

- **Component** — `(inject d, provide p, effect iterator e)`. *A class.*
- **Provision (p)** — the keys a component may install. *Exports.*
- **Fiber** — an instance of a component with lifecycle state. *An object instance.*
- **Registry (F_γ)** — the tree of fibers. *Component tree / DOM.*
- **Name (𝔑)** — an atomic identity for a fiber; only ever compared. *UID.*
- **Parent (π)** — the fiber that instantiated this one, or `root`.
- **Table (σ_n)** — the bindings this fiber has installed. The global container is the union over `Active` fibers.
- **Retirement flag (τ)** — set once by `O-Retire`; monotone.
- **Lifecycle state (θ)** — `Inactive | Reloading | Active | Unloading`. *State machine.*
- **Installed** — any state other than `Inactive`.
- **Committed view (ω; `fiber.committed`)** — map from each declared key to the fiber that provided it at activation.
- **Target view (`fiber.target`)** — the same map computed from the *current* state, or `⊥` if retired/unsatisfiable.
- **Provider_k(γ)** — the unique `Active` fiber whose table holds `k`.
- **Quiescent** — every fiber is settled. *Steady state.*
- **Orchestration rules (O-Insert, O-Retire, O-Remove)** — external inputs. *`new`, request deletion, garbage-collect.*
- **Lifecycle rules (L-Begin, L-Iter, L-Finish, L-Divert, L-Leave, L-Unload)** — automatic steps.
- **Instantiation** — an iterator step that performs `O-Insert` of a child, with `O-Retire` as its undo. *`ctx.use()`.*
- **Relied upon** — some other installed fiber's committed view names this fiber.
- **Guard** — the premise `¬relied_n` on `L-Unload`: a provider's undo waits until its consumers are gone. *Refcount must hit zero.*
- **Entangled** — two fibers where one provides a key the other declares or provides.
- **Confined** — an effect writes only its own table (and declared keys' values) and reads only those. *Encapsulation enforced by construction.*
- **Vestigial entry** — a retired, `Inactive`, empty, childless fiber; invisible to every rule.
- **Episode** — a maximal interval during which a fiber is installed.
- **State map (Ψ) / edit** — each rule factors into a state transformation and a control-field write.
- **Precedence (n ≺ m)** — `n` may provide a key `m` declares. Assumed acyclic.
- **Support set (A)** — fibers that are not retired, whose parent is supported, and whose declared keys are provided by supported fibers. Equals the `Active` set at quiescence.
- **Total on its provision** — a component that installs *every* key in `p` when it finishes loading.
- **Inertia** — an in-flight async transition runs to completion before the system reacts to a new target.

### Metatheory results (4.3)

- **Preservation** — well-formedness of the registry is invariant under every rule.
- **≃-invariance** — the rules can't see anything `≃` hides.
- **Equivariance** — renaming fibers changes nothing.
- **Recovery exactness / Terminal recovery** — a fiber's undo removes exactly its contribution, regardless of what others did in between; its table ends empty.
- **Ordering** — activate only when dependencies are present; providers outlive consumers.
- **Resolution coherence** — a transition runs against one resolution or is cleanly diverted.
- **Progress** — no deadlock; terminates.
- **Transposition** — adjacent independent steps can be swapped. *Trace theory.*
- **Confluence** — quiescent state = from-scratch load of the final configuration.

### Implementation (5)

- **Cordis** — the TypeScript meta-framework; prescribes no domain, only composition semantics.
- **`ctx.effect(cb)`** — the single mutation primitive; runs `cb` as a generator, folds disposers LIFO, returns `dispose`.
- **`ctx.use(component, config)`** — instantiate a fiber under this context.
- **`fiber.dispose`** — the accumulator. **`fiber.inertia`** — handle to the in-flight transition.
- **`refresh` / `reload` / `unload`** — the inertial state machine.
- **Proxy-mediated access** — `ctx[key]` resolved through committed views; throws `INACTIVE_ACCESS` or `UNDECLARED_ACCESS`.
- **Entry** — one node of the declarative configuration tree.
- **Reconciliation** — the loader diffing entry changes into the least disruptive fiber operations. *Desired-state / React diffing.*
- **Delimiter (δ_k)** — a per-key tag the loader uses to tell whether a binding is an entry's own when moving it between realms.
- **HMR** — dispose old fiber, import new module, `use` new fiber; transactional with cache backup.
- **Koishi** — the chatbot framework built on Cordis; the paper's adoption evidence.

### Discussion (6)

- **System boundary** — the line between locations the system can exclusively modify and restore (inside; tracked) and those it can't (outside; act as identity).
- **Acquisition vs emission** — obtaining a channel (inside, revertible) vs pushing data through it (outside, not).
- **Output commit / compensation** — the two ways to "undo" an emission: don't send until safe, or send a compensating action.
- **Service broker** — a single provider that dispatches among many backing implementations.
- **Capability** — authority conferred by holding a reference. `inject` acts as a capability request.
- **Interface drift / key collision** — the two versioning failures of name-based linking.
- **Peer dependency** — Cordis's current answer to versioning: let the package manager enforce version ranges.
