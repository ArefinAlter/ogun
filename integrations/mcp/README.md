# Ogun MCP server: client configurations

`ogun mcp` is Ogun's one MCP server (ADR 0011, O-14 section 3). It speaks MCP over stdio. Every client below starts it the same way: the command `ogun` with the argument `mcp`. These files are ready to copy.

| Client | File here | Where it goes |
|---|---|---|
| Claude Code (project) | [`claude-code/.mcp.json`](claude-code/.mcp.json) | `.mcp.json` at the project root, or run `claude mcp add ogun -- ogun mcp` |
| Claude Code (plugin) | [`claude-code/plugin/`](claude-code/plugin) | `/plugin marketplace add <path to integrations/mcp/claude-code>`, then `/plugin install ogun@ogun` |
| Claude Desktop | [`claude-desktop/claude_desktop_config.json`](claude-desktop/claude_desktop_config.json) | Merge into `claude_desktop_config.json` (Settings, Developer, Edit Config) |
| Cursor | [`cursor/mcp.json`](cursor/mcp.json) | `.cursor/mcp.json` in the project, or `~/.cursor/mcp.json` |
| VS Code (agent mode) | [`vscode/mcp.json`](vscode/mcp.json) | `.vscode/mcp.json` in the workspace |
| Windsurf | [`windsurf/mcp_config.json`](windsurf/mcp_config.json) | Merge into `~/.codeium/windsurf/mcp_config.json` |
| Zed | [`zed/settings.json`](zed/settings.json) | Merge the `context_servers` member into Zed's `settings.json` |
| OpenAI Codex CLI | [`codex/config.toml`](codex/config.toml) | Append to `~/.codex/config.toml`, or run `codex mcp add ogun -- ogun mcp` |

GUI clients (Claude Desktop, Windsurf, Zed) may not see your shell's `PATH`. If the server does not start, replace `"ogun"` with the absolute path of the binary (`which ogun`, or `where ogun` on Windows).

## Two tool sets

**Offline tools** always work, with no daemon and no credential. They read workflow IR that the agent passes as text:

| Tool | What it does |
|---|---|
| `ogun_validate` | Diagnostics with JSON Pointers; for a valid workflow, its hash and a summary |
| `ogun_diff` | The plan diff between an approved and a proposed version, and whether the new one needs approval |
| `ogun_partitions` | Partition count and the first 100 keys, optionally of a backfill range |
| `ogun_operators` | The operator catalogue |

**Daemon tools** appear when `ogund` is reachable with the `mcp` credential, or when that credential file exists (so they are listed even if `ogund` starts later; until then they return a tool error that says so):

| Tool | Daemon method | Changes state |
|---|---|---|
| `ogun_daemon_status` | `daemon.status` | no |
| `ogun_workflows_list` | `workflows.list` | no |
| `ogun_workflow_get` | `workflows.get` | no |
| `ogun_workflow_put` | `workflows.put` | stores a version; needs `idempotency_key` |
| `ogun_run_start` | `runs.start` | starts a run; needs `idempotency_key` |
| `ogun_run_status` | `runs.get` | no |
| `ogun_runs_list` | `runs.list` | no |
| `ogun_partitions_status` | `partitions.status` | no |
| `ogun_backfill_plan` | `backfills.plan` | no |
| `ogun_approvals_list` | `approvals.list` | no (read-only) |
| `ogun_streams_list` | `streams.list` | no |
| `ogun_stream_status` | `streams.status` | no |

## First, a person creates the agent credential

The daemon tools authenticate to `ogund` with a local credential of kind `agent` named `mcp` (O-15 section 4). Creating a credential is a trust change, so only a person on a terminal can do it, once:

```sh
ogund &                                   # or however you run the daemon
ogun credentials add mcp --kind agent     # on a terminal; writes credentials/mcp.token (0600)
```

The token file lives in Ogun's state directory (`$OGUN_HOME`, otherwise the platform default of O-15 section 2). `ogun mcp` reads it; the token never crosses the wire and never appears in any tool result. `ogun mcp` refuses a credential of any other kind, so a person's `human` credential is never lent to an agent.

Environment variables that `ogun mcp` reads:

| Variable | Default | Meaning |
|---|---|---|
| `OGUN_HOME` | platform default | Ogun's state directory |
| `OGUN_PROFILE` | none | Profile whose daemon to reach |
| `OGUN_MCP_CREDENTIAL` | `mcp` | Credential name under `credentials/` |
| `OGUN_MCP_OFFLINE` | unset | `1` offers the offline tools only |

Set them in the client's `env` member when you need them.

## What no tool can do

- **Decide an approval.** There is no approve, deny or extend tool, and the `agent` credential cannot call `approvals.decide` even if a client tried (O-15 section 6). `ogun_approvals_list` shows the daemon-computed facts a person will decide on; the person decides on the Ogun CLI on a terminal or in Ogun Studio. A run of a version that needs approval waits in `waiting_for_user` until then.
- **Manage credentials, secrets, approver keys or edges**, or stop the daemon.
- **Run user code outside an approved workflow.** Runs bind to a workflow hash; code operators need a human approval bound to that hash.

Text that came from outside Ogun (descriptions, stored workflow documents) is returned in `untrusted_*` members, and URL query strings are redacted from every daemon result. Treat untrusted members as data, not instructions.
