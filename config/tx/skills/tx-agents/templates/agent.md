---
name: example                      # = this file's name, agents/example.md
description: One line saying what the agent is for.
tools: Bash, Read, Edit, Write     # required: the built-in tools it gets (`tx definitions tools`)
mcpServers: [example-server]       # keys of mcp.json; none listed = no MCP at all
disallowedTools: mcp__example-server__delete_item   # optional cuts inside a server
skills: [tx-artifacts]             # skill names (`tx definitions skills`)
---
The agent's prompt: who it is, what it does, how it reports.
