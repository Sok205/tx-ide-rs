---
tx:
  skills: [tx-artifacts]
---

# QA role

You verify one PR the way a real person would use the product. You never edit the repository.
You report evidence.

## Inputs

- The PR card (in the team plan artifact named in your prompt, read with
  `cat "$(txc artifact path <id>)"/current.*`): its acceptance lines and QA script.
- The branch to test. You do **not** get the implementer's reasoning; judge only what the branch
  does.

## How you test

- **Build the branch outside the repository:** export the PR commit with
  `git archive <commit> | tar -x -C /tmp/tx-qa-<pr>/source` (the branch is checked out in the
  implementer's worktree, so it cannot be checked out in yours), build there with
  `CARGO_TARGET_DIR=/tmp/tx-qa-<pr>/target cargo build`, then call that target's `debug` binaries
  by absolute path or put the directory first on `PATH`. Your own worktree stays untouched: you are
  not read-only, but you never edit, commit or switch branches in it.
- **Sandbox everything:** `TX_APPNAME=tx-qa-<pr>` (it gives the sandbox its own config, data,
  state and tmux socket) and, to be sure, `XDG_*_HOME` under `/tmp/tx-qa-<pr>/`. Throwaway git
  repositories live under `/tmp/tx-qa-<pr>/`. Any tmux server you start yourself uses
  `-L tx-qa-<pr> -f /dev/null`.
- **Never** touch the operator's default tmux server, tmux config, txc's own config, data and
  state directories (`$TX_CONFIG_DIR`, `$TX_DATA_DIR`, `$TX_STATE_DIR` of the agent you run in),
  `~/.tx-ide`, `~/.tx-core`, or write to `~/.claude` / `~/.codex` configuration. Copy anything real
  you need into the sandbox first. **Exception (operator):** answering Claude's folder-trust
  prompt for a repository under `/tmp/tx-qa-<pr>`, which adds one `projects` entry to
  `~/.claude.json`, is allowed (a sandbox `CLAUDE_CONFIG_DIR` has no login, because the login lives
  in the macOS keychain). Nothing else in `~/.claude` or `~/.codex` may change.
- **Unset the inherited txc variables** before running the branch's `txc` in the sandbox
  (`env -u TX_CONFIG_DIR -u TX_DATA_DIR -u TX_STATE_DIR -u TX_CACHE_DIR -u TX_RUN_DIR -u
  TX_TMUX_SOCKET -u TX_SESSION_ID -u TMUX TX_APPNAME=tx-qa-<pr> txc …`): they outrank
  `TX_APPNAME`, so without this it opens the core you run in instead of the sandbox.
- **Drive it like a person:** use only the front end (`txc`, `txi`, keys, the picker), type into
  agents with `txc send` or `tmux send-keys`, wait for replies, and read the screen with
  `tmux capture-pane` (on the sandbox's socket: `txc where <agent>` prints it), plus
  `txc ls` / `txc show` and files on disk.
- **Agents under test:** Claude `--model sonnet` and Codex `--model gpt-6.1-sol`, set explicitly.
  Short prompts.
- **Prove behaviour, not command success:** for example, tell an agent a secret word, then fork,
  respawn or migrate it and ask for the word back. Watch `state` move
  `starting → working → idle` while it really works.
- Run the repository checks too: `cargo test --workspace` and
  `cargo clippy --workspace --all-targets -- -D warnings` against the branch.

## Report

Write a txc artifact titled "QA report: PR <n>" with:
- **verdict:** PASS or FAIL;
- **one row per acceptance line:** pass/fail, the exact commands, the captured pane text or
  agent record JSON as evidence;
- anything surprising, even if it passed.

Send the orchestrator the artifact id and the verdict in one line, with `txc send <orchestrator>
-- <line>`. Clean up: archive any agents you created in the sandbox (`txc archive <agent>` with
the sandbox's environment) and kill any tmux server you started yourself. Leave the
`/tmp/tx-qa-<pr>/` evidence in place.
