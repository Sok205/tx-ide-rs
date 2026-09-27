# tx tutor — a vimtutor-style walkthrough of tx-ide

## Goal

Teach tx-ide by having the user build a small HTTP server, one lesson at a time, the way
`vimtutor` teaches nvim. Every lesson pairs one **HTTP concept** (no prior networking knowledge
assumed) with one **tx-ide skill**, and ends with a task that `tx tutor check` verifies.

v1 ships one language pack: **Python, stdlib `http.server`** (no dependencies). The format is
language-agnostic so Go / Rust / … packs can be added later without Rust changes.

## Decisions

| Question | Decision |
|---|---|
| Delivery | Lesson files + deterministic checker (no tutor agent in v1) |
| Code authorship | Mixed: some steps typed by the user in nvim (✍), some delegated to agent workers (🤖) |
| Python stack | stdlib `http.server`; checker talks raw HTTP/1.1 |
| Coverage | Navigation, spawning, chat operations, tx-assistant, lifecycle |
| Architecture | New `verbs.tutor` component + declarative lesson packs under `tutor/<lang>/` |

## User flow

```
tx tutor start python     # scaffold ~/tx-tutor/python (git repo), open the tutor view, attach
tx tutor check            # verify the current lesson; advance on pass
tx tutor next | prev | goto N | status | hint | reset [--hard]
```

The tutor view `tutor-<lang>`: left pane nvim on the current lesson file, right pane a shell in
the project directory. Re-running `start` resumes at the saved lesson.

## Curriculum (Python pack)

✍ = user types code in nvim; 🤖 = an agent worker writes it.

| # | HTTP concept | tx skill | Task | Checks |
|---|---|---|---|---|
| 01 | What a server is | `tx tutor start`, tutor view, `C-h/j/k/l` | Look around | `confirm` |
| 02 | Request → response, `curl` | shell pane | ✍ run `python3 server.py`, `curl localhost:8000` | `http GET / → 200` |
| 03 | — | `tx spawn --cmd`, `prefix+t`, `tx ls`, `prefix+e`, `prefix+s` | Run the server in its own tx session `server` (views are not records, so they cannot be tagged), then tag it `http-tutor` with `prefix+e` | `tx`: session `server` has tag `http-tutor` |
| 04 | Paths, status codes | nvim editing | ✍ `GET /hello` → `hello` | `http` |
| 05 | JSON, `Content-Type` | `tx spawn --prompt --tag` | 🤖 worker adds `GET /time` (JSON) | `tx`: claude worker tagged `tutor` |
| 06 | — | worktrees, `spawn-nvim --diff`, merge | Review the worker's diff, merge its branch into the main checkout | `http GET /time` 200, `application/json`, valid JSON |
| 07 | 404, error handling | attach via picker, `send-message` | Ask the worker to explain status codes; ✍ 404 for unknown paths | `http GET /nope → 404` |
| 08 | POST, request body, 201 | `tx fork` | 🤖 fork the worker's chat, try two `POST /notes` designs, merge one | `tx`: a chat with origin `fork`; `http POST /notes → 201`; `GET /notes` body contains the posted note |
| 09 | — | `tx history`, `chat ls`, `tx resume` | Find and resume the discarded fork | `tx`: a chat with origin `resume` |
| 10 | Testing a server | `prefix+/` tx-assistant | 🤖 ask the assistant to spawn a test-writing worker; merge | `command`: `python3 -m unittest` exits 0 |
| 11 | — | `handover` / `rollover` | Hand the test worker's context to a fresh chat | `tx`: a chat with origin `handover` |
| 12 | Recap | `kill` / `archive` / `revive`, `prefix+X` | Archive the tutorial workers | `tx`: `absent` llm records tagged `tutor` in a live state; ≥1 archived |

Rules:
- Checks on agent-written work assert **behaviour** (status, headers, JSON shape, substrings),
  never exact code.
- Agent workers run in tx worktrees (`$TX_IDE_HOME/worktrees/`); their code reaches the project
  only after the user merges. All `http` / `file` / `command` checks run against the **main
  checkout**. Lesson 06 teaches this explicitly.
- Lessons 01–04 and all ✍ parts work without an agent engine.
- Every ✍ lesson has a hint block in its markdown; `tx tutor hint` prints the solution snippet
  from `solutions/`.

## Pack format

```
tutor/<lang>/
  pack.toml        # name, run, port, requires
  skeleton/        # copied into the project dir, then git init + initial commit
  lessons/NN-slug.md
  lessons.toml     # ordered lessons + their checks
  solutions/NN-slug.<ext>
```

`pack.toml` (Python):
```toml
name = "Python (stdlib http.server)"
run = ["python3", "server.py"]
requires = ["python3"]
```

The skeleton server reads its port from the `PORT` env var (default 8000) so the checker can run
it on a free port without colliding with a server the user left running.

`lessons.toml`:
```toml
[[lesson]]
id = "05-spawn-worker"
file = "lessons/05-spawn-worker.md"
requires = ["claude"]            # optional; warns, does not block
checks = [
  { kind = "tx", label = "a Claude worker tagged tutor exists",
    session = { tag = "tutor", engine = "claude" } },
]

[[lesson]]
id = "06-review-merge"
file = "lessons/06-review-merge.md"
checks = [
  { kind = "http", label = "GET /time returns JSON", method = "GET", path = "/time",
    status = 200, headers = { "Content-Type" = "application/json*" }, json = true },
]
```

### Check kinds

| kind | fields | passes when |
|---|---|---|
| `http` | `method`, `path`, `body?`, `status`, `headers?` (glob values), `contains?`, `json?` | the pack's server, started in the main checkout on a free port, answers as specified |
| `tx` | `session = { tag?, name?, engine?, role?, state? (list, any-of), has_parent?, chat_origin? }`, `count?` (default ≥1), `absent?` | enough records from `SessionService::reconcile()` match every given field (with `absent = true`: none match); `chat_origin` matches any chat's `origin.how` (`fork`, `handover`, `resume`, …) |
| `file` | `path`, `contains?` (substring; no regex crate) | file exists in the main checkout (and contains the text) |
| `command` | `argv` | exits 0 in the main checkout; output shown on failure |
| `confirm` | `prompt` | the user answers `y` |

Every check has a `label` printed next to ✓/✗.

`http` mechanics: bind port 0 to pick a free port, spawn `run` with `PORT=<port>`, poll with
exponential backoff until the port accepts (5 s cap), send one request over `std::net::TcpStream`
(HTTP/1.1, `Connection: close`), parse the status line, headers and body, then kill the server.
Checks in one lesson share one server start. No new crate dependencies.

## Component

`crates/tx/src/verbs/tutor.rs` (+ `crates/tx/src/tutor/` for pack loading, checks, progress),
registered in `plugins::MANIFEST` as `verbs.tutor`, and so can be switched off in `config.json`.
It injects `home`, `commands`, `service`, `tmux`, like the other verb groups, and registers one
visible verb, `tutor`, with subcommands. (`tx --help` lists it after the reference's verbs,
like `revive`.)

- **Pack loading**: `tutor/<lang>/` resolved from the repo root (`deps.repo_root()`); parsed with
  the existing `toml` + `serde`.
- **View**: `start` creates `tutor-<lang>` through `SpawnSpec::for_view` (a view is a live tmux
  session, not a record). Pane 0 runs `nvim --listen $TX_IDE_HOME/tutor/<lang>.sock <lesson>`;
  a new `Tmux::split_window` adds the shell pane. `check` / `next` / … reopen the current lesson
  with `nvim --server <sock> --remote <file>` (silently skipped when nvim is not listening).
- **Progress**: `$TX_IDE_HOME/tutor/<lang>.json` = `{ "dir": ..., "current": "<id>",
  "passed": ["<id>", ...] }`, written atomically through the storage helpers.

Subcommands:
- `start [lang] [--dir PATH]` — default lang `python`, default dir `~/tx-tutor/<lang>`. Checks
  `requires`, scaffolds (skeleton copy, `git init`, initial commit) unless already a tutor project,
  creates or reattaches the view.
- `check` — runs the current lesson's checks; on all-pass records it, advances, reopens the lesson
  in nvim. Exit 0 on pass, 1 on any ✗.
- `next`, `prev`, `goto N|id` — move without checking.
- `status` — list lessons with ✓ / current marker.
- `hint` — print the current lesson's solution file (or "no hint for this lesson").
- `reset` — progress back to lesson 01; `--hard` also re-scaffolds the project dir (asks first).

## Error handling

- Missing `requires` binary: clear message at `start`; lessons requiring `claude` state it at the
  top and suggest `tx tutor goto` to skip.
- Malformed pack (TOML error, missing lesson/solution file, unknown check kind): reported at load
  with file + lesson id; nothing runs.
- `http` server fails to start or never binds: ✗ with the last 20 lines of its stderr.
- A failed check never advances progress.
- `start --dir` on a non-empty directory that is not a tutor project is refused.

## Testing

- Unit: `lessons.toml` / `pack.toml` parsing (every shipped pack loads, every referenced file
  exists); `tx` predicate against fixture records; HTTP client + matcher against a throwaway
  `TcpListener`; progress transitions.
- Integration: copy the Python skeleton to a temp dir; assert lesson 02 passes and lessons 04 / 07
  fail against the bare skeleton (checks are not vacuous); then drop in a reference solution
  (`crates/tx/tests/fixtures/tutor-python/`) and assert every `http` / `file` / `command` check of
  every lesson passes — proves the curriculum is completable. `tx` checks are covered by fixtures, not live tmux.
- `cargo check`, `cargo clippy --workspace --all-targets`, `cargo test --workspace`.

## Out of scope (v1)

- Detecting keypresses via `keylog.lua` (such steps use `confirm`).
- A hint / tutor agent.
- Packs other than Python.
