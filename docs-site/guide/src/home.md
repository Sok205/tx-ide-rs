# Startup and the home

This page follows `tx ui attach` to the first screen, the home. As of this branch, the `home`
plugin (crate `tx-home`) replaces the earlier start menu (`menu`, now a retired id).

## The attach path

`tx ui attach` belongs to the `ide` plugin in `tx-client`. It does three things:

1. **Ensure the `home` view.** If there is no session named `home`, it creates one: a view of
   kind `home` running your shell, window named `home`, status bar and pane-border labels on.
2. **Hand the pane to the home plugin.** If the `home` plugin is active (the core has a verb named
   `_home-install`), attach runs it in-process with the argument `home`, on every attach. A
   failure here is reported and never stops the attach.
3. **Attach.** From a bare terminal it execs `tmux attach-session` to `home` with the client
   flag `no-detach-on-destroy`, so the terminal stays in tmux when a session ends. Inside tx's
   server it switches the client instead.

Attach no longer starts the assistant itself; the home does that.

## The floor: `_home-install`

The home TUI is the **floor** of the home pane: whatever runs there, when it ends, the TUI comes
back. tmux holds that guarantee, not a parent process. `_home-install home` finds the floor pane (the one already marked
`@tx-home-floor`, else the first pane of the first window) and sets four **pane-scoped** options
on it:

```text
@tx-home-floor          1
remain-on-exit          on        (the pane stays, dead, when its program exits)
remain-on-exit-format   "tx home: working…"
pane-died  (hook)       run-shell -b "tx … _home-perform #{hook_pane}"
```

Because the options are pane-scoped, any other pane you split off in the home window keeps its
own life. Then, if the pane is dead or runs a plain shell (`zsh`, `bash`, `sh`, `fish`, `dash`),
`_home-install` clears stale choice files and respawns it with `tx _home-tui <pane> --first`. It
does the same to a TUI an earlier attach left while the assistant is down, so every attach retries
the assistant. A pane running an editor or a nested tmux client is left alone; the floor takes
over when it ends.

## The loop: `_home-tui` and `_home-perform`

The TUI draws, polls, and exits with a **choice**: one line written to
`<data>/plugins/home/choice.<pane digits>`. The choices are `attach <name>`, `resume <id>`,
`new <cwd>`, `stop <id>`, `ask <text>`, `artifacts <id>`, `timeline`, `tutor`, `warm`,
`layout <name> <id or -> <cwd>` and `new-layout <name> <cwd><TAB><description>`. In the last one the
directory runs up to the last tab, so it may hold spaces. The pane dies,
the `pane-died` hook fires, and `_home-perform <pane>` reads and deletes the choice:

- `attach <name>` respawns the pane with a **nested attach** of that session and nothing after
  it (`TMUX= tmux -S <socket> attach -t <target>`), and records the name in `last.<pane>`.
  When you detach, or the session ends, the pane dies again; with no choice waiting,
  `_home-perform` brings the TUI back with `--returned-from <name>`, so the row you left is
  selected.
- `resume <id>` runs `tx respawn <id>` and nests into the agent; `stop <id>` runs `tx stop`;
  `new <cwd>` opens the new-agent form with the project preset; `artifacts`, `timeline` and
  `tutor` open that plugin's popup or view; `warm` runs `tx assistant warm`; `layout` runs
  `tx layout open <name> --cwd <cwd> --detach`, with `--agent <id>` unless the id is `-`, and
  switches the client to the view it prints (a failure shows `layout: <message>` on the status
  line). Each then respawns the TUI.
- `new-layout` runs `tx create layout-<name> --cwd <cwd> --agent layout-author --tag layouts
  --prompt "Write layout <name>: <description>"` (a `.` in the name becomes `-`, since agent
  names have none). Model, effort and engine come from `config.yaml`. Like `ask`, it then nests
  into the agent (the popup switches to it), so you can watch it and answer its questions. A
  failure shows `new layout: <message>` on the status line and brings the TUI back. The agent
  writes `<config>/layouts/<name>.yaml`, runs `tx layout check <name>` until the file passes, and
  reports. The new layout shows up in `w` at the home's next refresh.
- `ask <text>` runs `tx assistant warm` for the assistant's name, nests into it at once, and sends
  the line with `tx assistant ask` in the background; the engine queues input typed while it
  starts.

Every action is a `tx` verb or a tmux command. Other plugins that respawn the home pane (the
picker's Enter) keep working; their program's end brings the TUI back.

## The screen

```text
tx-ide  2 running · 3 ended today · assistant ● ready

tx-ide-rs  ~/code/tx-ide-rs
  ● startup-ux     working  12m   feat/startup-ux
  ○ docs-site      ended  yesterday
wrangler  ~/w/wrangler
  ◐ fixer          idle  3m      fix/retry-backoff
    wrangler-p1-nvim · build-watch

enter attach · →/← unfold · n new here · / ask · f find · ? keys
```

The list is **projects → agents → views**. A project is a repository's main checkout, found
from each agent's worktree through git. Projects sort by newest activity, the assistant's last.
Running agents come first, then ended ones (greyed); past three ended agents the rest fold into
one row until Tab. Views sit under the agent in their `@tx-view-parent`, else under their project,
else under a trailing `views` group. Unfolding an agent (→ or `l`) shows its history: branch,
roles, model and effort, parent, the last exchange, artifact counts, start and end. With no agents
but the assistant, a first-run panel shows instead. Data comes from a worker thread every second
and after every action, so git and tmux never hold up a key or a frame.

## The assistant in the header

An attach-triggered open (`--first`, on the floor) runs `tx assistant warm` detached when the
assistant is down: absent, stopped (respawned in place, never replaced by `tx-assistant-2`), or
failed last time. The header reads the record, not `warm`'s output: `◐ starting · creating` (no
record yet), `◐ starting · claude`, `● ready`, `○ stopped · r restart`, or `✗ failed: <reason>`
from `<data>/plugins/assistant/warm.failed`. Enter works on the assistant's row as soon as its
session exists.

## Keys

For 400 ms after an attach only arrows, `j`/`k`, Enter, Esc and `q` are accepted, so a reflexive
keypress triggers nothing. Ctrl and Alt chords belong to tmux and never reach the home.

| key | does |
|---|---|
| `j`/`k`, ↓/↑ | move over agent and view rows (wraps) |
| Enter | attach (nest) a running agent or view; resume an ended agent |
| →/`l`, ←/`h` | unfold or fold an agent's history |
| Tab | show or fold all ended agents |
| `n` | new agent, its directory preset to the selected project |
| `x` | stop the selected agent (then `y`) |
| `r` | resume an ended agent; restart a stopped assistant; retry a failed one |
| `a` | the selected agent's artifacts |
| `t`/`T` | the timeline |
| `/` | ask the assistant one line |
| `f` | find: filter by name, tag, project, branch or view |
| `L` | the tutor |
| `w` | the layout list (only with the `layouts` plugin on); Enter opens the highlighted layout in the selected project, around the selected agent unless it ended; `n` asks `new layout › NAME DESCRIPTION` and starts a `layout-author` agent in the selected project that writes it (Esc cancels; a bad name, an existing name or an empty description keeps the line open with a hint) |
| `p` | the plugin list; `?`/`K` the key sheet |
| `q`, Esc | close (popup only; on the floor they do nothing) |

The footer changes with context: `enter resume …` on an ended agent,
`enter attach · n new · / ask · L tour · ? keys` on first run, `· q close` in the popup, and
`· w layouts` last when the `layouts` plugin is on.

## prefix+m and the disabled case

prefix+m runs `_home-open`: the same TUI in an 80% popup. There, `attach` is a `switch-client`
(the floor stays in its pane), and other choices run once the popup has closed.

With the `home` plugin disabled in `plugins.yaml`, `_home-install` does not exist: `ui attach`
leaves `home` as a plain shell, prefix+m is unbound after `ui install`, and nothing starts the
assistant until prefix+/ or `tx assistant warm`.
