---
name: tx-agents
description: Use when asked to create, change or inspect a tx agent definition, skill or MCP server — where the files live, their formats, how to check them, and how a running agent picks up a change.
---

# tx agent definitions

An agent gets **exactly** what its definition lists, plus any `--skill` / `--mcp` / `--tool`
extras given when it was created. tx adds nothing else. The one exception is Claude's named
baseline, `Skill` and `ToolSearch`, which `tx definitions` and `tx check` always print.

## Where the files are

| | Shipped (rewritten by `tx ui install`) | Yours (tx never writes here) |
|---|---|---|
| agents | `$TX_DATA_DIR/definitions/agents/<name>.md` | `$TX_CONFIG_DIR/definitions/agents/<name>.md` |
| skills | `$TX_DATA_DIR/definitions/skills/<name>/SKILL.md` | `$TX_CONFIG_DIR/definitions/skills/<name>/SKILL.md` |
| MCP servers | `$TX_DATA_DIR/definitions/mcp.json` | `$TX_CONFIG_DIR/definitions/mcp.json` |

Yours win by name: an agent, a skill, or a server key inside `mcp.json`. To change a shipped
definition, **copy it to the same relative path under `$TX_CONFIG_DIR/definitions/`** and edit
the copy. Never edit `$TX_DATA_DIR`: the next install overwrites it.

## Formats

Templates are next to this file: `templates/agent.md`, `templates/SKILL.md`, `templates/mcp.json`.

- **Agent**, `agents/<name>.md`: YAML frontmatter, then the prompt.
  - Keys, and no others:
    - `name`: required, equal to the file name.
    - `description`: required.
    - `tools`: **required**. The built-in tools, in Claude's names; the valid names come from
      `tx definitions tools`.
    - `mcpServers`: keys of `mcp.json`. None listed means no MCP at all.
    - `disallowedTools`: cuts, such as `mcp__<server>__<tool>`, or `mcp__<server>` for the whole
      server.
    - `skills`: skill names.
  - A list key takes a YAML list or a comma string.
  - Model and effort are not part of it; they are `tx create` flags.
- **Skill**, `skills/<name>/SKILL.md`: the Agent Skills standard. The frontmatter has `name`
  (equal to the folder name) and `description`; the body is the instructions. The folder may hold
  scripts and templates.
- **MCP servers**, `mcp.json`: Claude's `.mcp.json` format.
  - `{"mcpServers": {"<name>": {...}}}`, with `"type": "stdio"` (`command`, `args`, `env`) or
    `"type": "http"` (`url`, `headers`).
  - Secrets only as a whole `${NAME}` value in `env` or `headers`, or as
    `"Authorization": "Bearer ${NAME}"`.
  - The variable is read from the environment of the `tx` that launches the agent. A launch is
    refused when it is unset.

## Check, list, inspect

- `tx check [NAME]...`: validates everything, or the names given. It prints `path:line: problem`
  and exits 1 on any problem. Run it after every edit.
- `tx check --engine claude|codex [NAME]...`: adds what that engine always carries and no flag
  removes. Then, per agent, what it gets that its file does not say: the baseline, writes through
  `Bash`, tools that are one switch on Codex. On Claude it also refuses tool names Claude does
  not know.
- `tx definitions [agents|skills|mcp]`: names and descriptions.
- `tx definitions tools`: Claude's built-in tools, the Claude names Codex switches off, and every
  MCP server's tools. It starts the servers to ask them.

## Use and change

- **Start an agent from a definition:**
  `tx create <name> --agent <definition> [--skill S]... [--mcp M]... [--tool T]... --cwd <dir> --prompt <text>`.
  The default roles do not apply with `--agent`; only `--role` adds roles.
- **The definition is read again at every launch.** To apply an edit to a running agent, run
  `tx stop <agent>`, then `tx respawn <agent>`. The conversation and its cache survive, and the
  extras given at create are kept.
- Do not write into another agent's worktree to change what it gets. Edit the definition and
  respawn.
