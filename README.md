# tx-ide

A plain core for a swarm of interactive coding agents in tmux: each agent is an ordinary engine
process (Claude Code, Codex) in its own worktree with a durable record; views (nvim, shells,
diffviews) sit beside them; a picker, a timeline, artifacts, a tutor and a one-line assistant
are plugins. The front end is `txc`.

Everything lives under [`poc/`](poc/README.md): the `tx-core` library, the `tx-client` tmux/nvim
layer, the plugins, the `txc`/`txi` binaries, the shipped roles (`poc/tx-core/agents/`) and the
operator's own nvim and tmux config (`poc/config/`).

## Install

```bash
git clone <this-repo> ~/tx-ide-rs
~/tx-ide-rs/poc/install            # cargo install txc and txi; txc ui install; link ~/.config/nvim and ~/.tmux.conf
~/tx-ide-rs/poc/install --dry-run  # print every change first
txc ui attach                      # the IDE
```

Needs Rust 1.92+, tmux, fzf, git, jq, and at least one engine CLI (`claude`, `codex`); `nvim` for
views. `poc/README.md` is the full reference (directories, verbs, keys, plugins, the install).

## Development

```bash
cd poc
cargo test --workspace
cargo clippy --workspace --all-targets
```

Design notes and plans: `docs/superpowers/`. The rustdoc structure graph: `scripts/structure/`.

The previous implementation (the `tx` binary under `crates/`, its bash helpers and installer) was
removed after the switch to `txc`; it is reachable at the tag `old-tx-final`.

## License

MIT.
