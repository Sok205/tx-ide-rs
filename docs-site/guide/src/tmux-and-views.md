# The tmux layer and views

tx runs every agent and every view on a tmux server of its own. This page covers that server, how
its configuration is composed from plugins, what a view is, and how sessions nest inside each
other.

## tx's own server

The server listens on `<run>/tmux.sock` (the run directory is created mode 0700) and loads
`<data>/tmux.conf`. Every tmux call core or a plugin makes carries both `-S <socket>` and
`-f <conf>`, so your default tmux server and `~/.tmux.conf` are never touched. Targets are exact
(`=name`), user-controlled arguments come after `--`, and anything ending in `;` is escaped so
tmux cannot read it as a second command. The server exits when its last session ends.

Because the socket is not the default one, plain `tmux ls` will not show tx's sessions. For raw,
read-only inspection, ask tx where something lives:

```sh
tx where worker-1     # socket <TAB> session target <TAB> pane target
IFS=$'\t' read -r socket session pane < <(tx where worker-1)
tmux -S "$socket" capture-pane -p -t "$pane"
```

The shipped roles allow raw tmux for inspection, moving between panes and scrolling only. Anything
that creates, ends or changes an agent or a view goes through `tx`, or the records go stale.

## `tx ui install`: composing the configuration

The `ide` plugin (in `tx-client`) owns `tx ui install`. It materializes the core's roles and
every active plugin's resources under the data directory (`bin/` and `keys/` executable, `conf/`,
`roles/`, `definitions/` and the nvim runtime plain) and writes `<data>/tmux.conf`. That file sets three server
options, `@tx-bin` (the `tx` binary), `@tx-data` and `@tx-arguments` (the flags every `tx` a key
runs must carry), sources `conf/tmux.conf` (theme and status line), runs `keys/ide.tmux` (pane
navigation, paging, pane-border look) and then every other active plugin's `keys/<id>.tmux`.

Files under those directories that no active plugin provides are deleted, so the installed tree
always matches the enabled plugins and `tx` needs nothing from the source tree at run time.
Because paths are spliced into tmux config lines, `ui install` refuses a binary or data path
containing whitespace, quotes, `$`, `#`, `;`, a backslash or a backtick.

**Reconciliation.** Each key fragment declares, in a comment near its top, the prefix keys it
binds (`# keys: m`) and any global options or hooks it sets (`# unset: …`, `# unhook: …`).
`ui install` records these in `<data>/keys/.installed`. On a running server it compares the
previous record with the new one, unbinds keys, clears options and removes hooks that a now
disabled plugin declared, re-sources the configuration and prints `reloaded`. Disabling a plugin
in `plugins.yaml` and re-running `ui install` therefore removes its keys live. A server set up
before `.installed` existed gets a hint to restart.

## Views

A **view** is any tmux session on tx's server that is not an agent: the `home` base, editors,
diffviews, code tours, shells, the resident timeline and graph, the tutor and mockup viewers. A view has no
core record and never shows in `tx ls`. Its fields are tmux session options:

- `@tx-view` is `1` on every view; `@tx-view-kind` is one of `home`, `editor`, `diff`, `tour`,
  `shell`, `timeline`, `graph`, `tutor`, `mockup`.
- `@tx-view-parent` is the id of the agent it belongs to; `@tx-view-bound` the artifact it shows;
  `@tx-view-tags` and `@tx-view-created` complete it.

Because these live in tmux only, nothing goes stale when the server dies. Views are managed with
`tx ui view open <name> --kind <kind> [--parent <agent>] [--cwd <dir>] [-- program …]`,
`tx ui view ls` (which first closes views whose parent agent is gone) and
`tx ui view close <name>`. Closing and renaming go through a guard that refuses any session
carrying `@tx-id`: an agent is only ever stopped through core. This is the way to run a shell or
any non-LLM program on tx's server; `tx create` starts agents only.

## Nested attaches

Views host other sessions. A **nested attach** runs `TMUX= tmux -S <socket> attach -t <target>`
inside a view's pane, so the agent's terminal appears inside the view with the view's status bar
and border around it. The picker's Enter, the home screen's Enter and the artifacts menu all work
this way. Pane-navigation keys (`C-h`/`C-j`/`C-k`/`C-l`) bubble out of nested sessions, and the
pane border shows the nested session's name and tags.

`tx ui attach` from a bare terminal attaches to `home`. Inside another tmux whose socket is not
tx's it nests and warns that the outer prefix wins (press it twice). Inside tx's own server it
switches the current client to `home` instead.

## Keys

The prefix is `C-a`. Each key belongs to a plugin and disappears when that plugin is disabled:

| keys | does |
|---|---|
| prefix+m | the home screen in a popup (`home`) |
| prefix+t | the session picker; Enter nests into the session (`picker`) |
| prefix+n | new agent: the form, then the agent (`spawn`) |
| prefix+e | edit the session's name, tags and group (`edit`) |
| prefix+X | stop an agent or close a view, after a confirmation (`kill`) |
| prefix+f | fork the agent in the pane into a new worktree (`chat`) |
| prefix+s | refresh labels, then tmux's session tree (`labels`) |
| prefix+a | the artifacts an agent touched; in an artifact's view, back to the agent (`artifacts`) |
| prefix+T | the resident timeline in a popup (`timeline`) |
| prefix+g | the resident agent graph in a popup (`graph`) |
| prefix+/ | one line to the assistant, with the focus envelope (`assistant`) |
| C-h C-j C-k C-l, C-u C-d | move between panes; page the scrollback (`ide`) |

The home screen's `?` builds the same list live from the fragments' `# keys:` lines.
