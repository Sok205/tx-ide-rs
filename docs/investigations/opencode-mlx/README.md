# Open-weight models on MLX: OpenCode engine and observability

Investigation for issue #120. It records what was tried, what was measured, and the design that
follows, so the engine can be built on a larger machine without repeating the spikes. Every
finding below comes from a real run unless it is marked as an estimate or as read from source.

The test machine was a base Apple M2 (10 GPU cores, 16 GB unified memory) with OpenCode 2.3.2
(Homebrew), mlx-vlm 0.8.0 (PyPI, the same version as https://github.com/Neywa-Labs/mlx-vlm) on
mlx 0.32.4, and LM Studio for comparison.

## Summary

- A new engine plugin, `engine-opencode`, adds the engine kind `opencode`. Its agents run the
  OpenCode TUI; the model is any OpenAI-compatible server, here `mlx_vlm.server`.
- OpenCode works against `mlx_vlm.server`: tool calls come back in OpenAI form (streaming and
  not), and OpenCode completed a file-writing task through it with a local Qwen3 4B MLX model.
- OpenCode's prompt is about 10k tokens per turn. On the M2 the 4B model's first token takes
  about 60 s; with prefix caching on, the next turn's wait drops to about 19 s. A machine with more
  GPU cores and memory is where this becomes practical (see "Hardware").
- Observability is available at three layers: OpenCode's events (tokens, steps, tool calls),
  mlx-vlm's `/metrics`, `timings` and log-probabilities (prefill and decode speed, time to first
  token, cache hits), and the machine (`ioreg` for GPU, `footprint` for memory). mlx-vlm runs
  the model in Python, so per-layer hooks are possible later.

## Why OpenCode, why mlx-vlm

OpenCode treats the provider as configuration: any OpenAI-compatible server is declared once and
models are addressed as `provider/model`, so switching models is a config change, with no proxy
between harness and server. (Codex CLI with an OSS provider and Claude Code behind an
Anthropic-compatible proxy were considered and set aside.)

mlx-vlm's server runs MLX models in-process and exposes, besides `/v1/chat/completions`,
`/v1/responses` and `/v1/messages`, a `/metrics` endpoint, per-response `timings`, per-token
log-probabilities, prefix caching, KV-cache quantization and speculative decoding with
acceptance counters. LM Studio exposes speed stats too (`lms log stream --stats --json` works for
every client), but nothing inside the model.

## OpenCode 2.3.2 facts the engine depends on

OpenCode is v2; its CLI, config schema, plugin API and event names differ from the v1
documentation.

**Launch.** `opencode --standalone [--prompt TEXT] [-s SESSION_ID] <directory>`.

- `--standalone` is mandatory. Without it the CLI starts (or reuses) a detached background server,
  one per state directory, which keeps the environment it was first started with, so a per-agent
  config is silently ignored.
- The TUI has no `--model` flag; the model is the config's `model` field.
- `-s <id>` with an id that does not exist yet creates the session with exactly that id when the
  first prompt is sent. tx can therefore choose the session id at `start`, and `resume` is the
  same command with the same id. `-c` continues the last session.
- The TUI has no fork flag. `POST /api/session/{id}/fork` on the server forks (and emits
  `session.forked`); `opencode run --fork` and `opencode mini --fork` exist but were not run.

**Config and isolation.**

- `OPENCODE_CONFIG=<file>` adds a config file; `OPENCODE_CONFIG_CONTENT` adds inline JSON;
  `OPENCODE_CONFIG_DIR` replaces the global config directory.
- `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME` redirect every file,
  including the SQLite session store (`$XDG_DATA_HOME/opencode/opencode.db`, table `session_v2`).
  Every invocation must set them: even `opencode --version` creates `~/.config/opencode`,
  `~/.local/share/opencode`, `~/.local/state/opencode` and `~/.cache/opencode` otherwise.
- `OPENCODE_DISABLE_CLAUDE_CODE=1` stops it reading `~/.claude/CLAUDE.md` and `~/.claude/skills`.
  `~/.agents/skills` is still loaded; no switch was found.
- `OPENCODE_DISABLE_AUTOUPDATE=1` and `"update": "disable"` turn off self-update.

**Config schema (v2).** Providers under `providers`, with `package:
"aisdk:@ai-sdk/openai-compatible"` and `settings.baseURL`; `instructions: [file…]`; `skills:
[directory…]` holding `<name>/SKILL.md` (verified loaded); `mcp.servers.<name>` with `type:
"local"` and a `command` array; `permissions: [{action, resource, effect}]` with effect allow,
ask or deny; `plugins: [directory…]`, each directory holding `index.js`. So every part of a tx
agent definition has a home in the generated config. See `spike/opencode-mlx.json`.

**Plugins and events.** A v2 plugin is a default export `{ id, setup(api) }`; the v1 style is
rejected. It runs inside the (standalone) server process, inherits its environment and can spawn
subprocesses. Events come from `api.event.subscribe()`, each with the session id in
`data.sessionID`. The ones that map to tx states:

| OpenCode event | tx change |
|---|---|
| `session.created` | starting → working; record the session id |
| `session.execution.started` | working |
| `session.execution.succeeded` | idle (finished) |
| `permission.asked` | idle (waiting) |
| `permission.replied` | working |
| `session.execution.failed` | idle, error in `data.error` (e.g. `provider.no-route`) |

There is no `session.idle` in v2. A turn also emits about a hundred streaming events
(`session.step.*`, `session.text.*`, `session.tool.*`, `session.usage.updated`); the bridge should
forward only the ones above. `api.tool.hook("execute.before", …)` fires; `api.permission.hook("ask",
…)` never fired, so its real name is unknown (the event is enough for state). Commands run by the
shell tool get `OPENCODE_SESSION_ID`, `OPENCODE=1` and `OPENCODE_TERMINAL=1`. See
`spike/tx-events/index.js`, the plugin used to record all of this.

**Known problem.** With Qwen 3.5, thinking text leaked into the hidden title-generation agent, and
once that generation ran away and held the GPU for ten minutes. The config can give the `title`
agent its own model or disable it; untested.

## mlx-vlm 0.8.0 facts

**Serving.** `python -m mlx_vlm.server --host 127.0.0.1 --port 8090 --model <path>
[--top-logprobs-k N]` loaded `Qwen3-4B-Instruct-2507-MLX-4bit` from a local folder in about 2 s
(the 1.7B 8-bit as well). A local model's id is its **absolute path**; any other name makes the
server try to download it from Hugging Face. So the OpenCode model is `mlx/<absolute path>`.

**Tool calls.** `/v1/chat/completions` with `tools` returns `finish_reason: "tool_calls"` and
OpenAI-shaped `tool_calls`; streamed, the arguments arrive as deltas that join correctly.
`/v1/messages` (Anthropic) also works with tool use, streaming and not, and
`/v1/messages/count_tokens` works; OpenCode was not tried against it.

**OpenCode through it** (4B, one file-writing task):

| run | result | time | notes |
|---|---|---|---|
| defaults | file written | 121 s | turn 1: 9,856 prompt tokens, first token 56.8 s, prefill 185 tok/s, decode 17 tok/s; turn 2: 10,371 tokens, 0 cached, first token 60.1 s |
| prefix caching on, zero reserve | file written | 87 s | turn 2 reused 8,608 of 10,373 tokens; prefill 18.6 s instead of 60 s |
| prefix caching on, default reserve | failed in 3.5 s | | server refused: "cannot fit the APC prefill memory budget even alone", sent as an error inside an HTTP 200 stream, which OpenCode reports only as `provider.invalid-output` |

Prefix caching is enabled at runtime with `PATCH /v1/settings {"apc_enabled": true,
"apc_disk_enabled": false, "apc_memory_reserve_gb": 0}`. The automatic budget on the M2 was
1.27 GB, which is why only part of the prompt was reused.

**Observability.**

- `GET /metrics` (= `/v1/metrics`): `summary` (requests started, completed, failed, in flight,
  token totals, average request and decode rates, last error); `latest` and `recent` (last 100)
  per request: prompt and completion tokens, `prompt_eval_time_s`, `prefill_tok_s`, `ttft_s`,
  `decode_tok_s` (also over the first and last 32 and 64 tokens), `request_elapsed_s`,
  `finish_reason`, `tool_calls`, flags; `server`: loaded models, queue depth, prefix-cache status.
- `/v1/cache/stats`: prefix-cache hits, misses, matched tokens, hit rate, evictions, memory.
- Each response's `timings` (llama.cpp names): `prompt_n`, `cache_n`, `predicted_n`, prompt and
  predicted milliseconds and rates, `peak_memory`, and `draft_*` counters with a drafter loaded.
- Log-probabilities: `logprobs: true`, with alternatives when started with `--top-logprobs-k`.
- The server log narrates each request: prefill progress, time to first token, decode progress
  every ten tokens, final rates.
- Gaps: no GPU metric (use `ioreg -r -d 1 -c IOAccelerator`, `Device Utilization %`, no sudo; it
  read 98–100 % during a turn). `peak_memory_gb` is the process's lifetime MLX peak, never reset.
  RSS misses Metal buffers; `footprint <pid>` is the honest number (up to 7.5 GB here).

**Hooks inside the model** (read from source, not tried).

- `mlx_vlm/models/qwen3/language.py`: `Qwen3Model.__call__` already takes `capture_layer_ids` and
  a `hidden_sink` collecting chosen layers' hidden states (built for speculative decoding).
  Wrapping each transformer block is the general route to activation statistics.
- `generate/ar.py`: `generate_step(..., logits_processors=[...])`, a per-token callback with the
  logits; `GenerationBatch._step` and `BatchGenerator.prefill_step` on the server path.
- `server/generation.py`: `ServerMetricsStore.record_success` and
  `GenerationMetrics.record_chunk` are where per-request metrics are made.
- Per-layer timing needs `mx.eval` after each layer (MLX is lazy), which serializes and slows
  generation; reasoning, not measured.

## Design

**`engine-opencode`** (manifest shaped like `plugins/engine-codex/plugin.yaml`, `needs:
[opencode]`):

- `configure`: `model` (OpenCode `provider/model`), provider base URL, `effort` (mapped to a
  reasoning variant where the model has one, else noted and ignored).
- `start`: a session id chosen by tx; per-agent XDG directories under the plugin's data
  directory; a generated config (provider, model, definition prompt as `instructions`, skills,
  MCP servers, permissions, the title agent tamed, the bridge plugin); launch `opencode
  --standalone -s <id> --prompt <prompt> <worktree>` with `OPENCODE_CONFIG` and the `XDG_*` /
  `OPENCODE_DISABLE_*` variables.
- `resume`: the same launch without `--prompt`.
- `on_hook`: the event table above.
- `fork`: refused at first; later through the server's fork endpoint on the source agent's data
  directory.
- `check`: `opencode` installed (run with the XDG redirection), endpoint answers `/v1/models`,
  model listed, prefix caching enabled without the reserve problem.

**The bridge plugin** (`index.js`, written into the agent's config directory): subscribes to
events and runs tx's `hook_command` with the event name for the six events above.

**Stats.** A collector polling `/metrics` and `/v1/cache/stats`, sampling `ioreg` and
`footprint`, plus the per-agent token counts the engine keeps in `ext.opencode`; a ratatui view
in the style of `timeline` / `graph`. Later, a thin wrapper around `mlx_vlm.server` registering
layer and logits hooks and serving their statistics next to `/metrics`.

**tx-ide change.** The prefix+n model list is hard-coded per engine in `plugins/spawn/src/lib.rs`.
An optional engine method (e.g. `models`) would let the form list what the server serves.

## Hardware (estimates, not measured)

Prefill is compute-bound (scales with GPU cores), decode is bandwidth-bound. A Max-class chip has
3–4× the M2's GPU cores and 4–5× its 100 GB/s bandwidth; an Ultra about twice that again. Expect
the 10k-token prefill to fall from about 55 s to 10–20 s on a Max, lower on an Ultra, and decode
to rise to roughly 60–100 tok/s for a 4B model. With 128 GB, large mixture-of-experts coding
models (100B+ total, a few billion active) fit at 4 bits with tens of GB left for the prefix
cache, or several 30B-class models run side by side.

To confirm on the 128 GB machine: the chip; which current MLX coding models mlx-vlm loads (some
text-only architectures may only be in mlx-lm, which serves a similar API); whether the prefix
cache budget needs the zero-reserve workaround there.

## Open questions

- Switch for `~/.agents/skills` in OpenCode.
- Whether configuring or disabling the `title` agent stops the runaway generation.
- The plugin API's permission hook name.
- Whether `TMPDIR` redirects OpenCode's temp directory.
- OpenCode against mlx-vlm's `/v1/messages`.

## Reproducing

`spike/` holds the files that worked: `launch.sh` (isolated OpenCode launcher), `opencode-mlx.json`
(v2 config for `mlx_vlm.server`), `tx-events/index.js` (the event-recording plugin). Install
mlx-vlm into a private environment and start the server:

```sh
uv venv .venv --python 3.12 && uv pip install --python .venv/bin/python mlx-vlm
HF_HOME=$PWD/hf .venv/bin/python -m mlx_vlm.server --host 127.0.0.1 --port 8090 \
  --model /absolute/path/to/an/MLX/model --top-logprobs-k 5
curl -X PATCH localhost:8090/v1/settings -H 'Content-Type: application/json' \
  -d '{"apc_enabled": true, "apc_disk_enabled": false, "apc_memory_reserve_gb": 0}'
```

Put the model's absolute path into `opencode-mlx.json` (both places), then from a scratch git
repository: `SPIKE_DIR=$PWD/state ./launch.sh opencode-mlx.json run --standalone -m
"mlx/<absolute path>" --format json "Create a file named hello.txt containing 'Hello from MLX'."`
