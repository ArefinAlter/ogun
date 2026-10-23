# ADR 0011: Editor and agent extension surfaces

| | |
|---|---|
| Status | Proposed |
| Date | 2026-10-23 |
| Research | `docs/research/OR5-edge-streams-platforms.md` section 4 |

## Context

The maintainer asked for extensions for VS Code, other coding agents and other platforms. If each editor carried its own validator, those validators would drift from the runtime. And any surface an agent can reach must never be able to approve.

## Decision

1. **One validator.** Every editor integration runs the real validator: today `ogun validate --json`, later `ogun lsp`, a language server over stdio. The published JSON Schema handles completion and structure. JSON Schema cannot express the O-02 rules, so semantic checks always come from `ogun-ir`.
2. **VS Code.** `extensions/vscode` provides:
   - schema checking for `*.workflow.json`;
   - validator diagnostics placed at the JSON Pointer they name;
   - the workflow hash in the status bar.

   It has no npm dependencies. JetBrains IDEs and Zed use `ogun lsp` once it exists.
3. **One MCP server.** `ogun mcp` is the single agent server. Today it runs on stdio with read-only tools: `ogun_validate`, `ogun_diff`, `ogun_partitions` and `ogun_operators`. Packaging per client (Claude Code, Codex, Cursor, Windsurf, Zed, VS Code) is configuration only (O-14 section 3).
4. **Never an approval surface.** Editors, MCP clients and agents can draft workflows and see the plan diff a person will be asked to approve. They can never approve, extend a budget or run user code.

## Consequences

- Agents get exactly the information a reviewer gets, so they can propose changes that need no approval, or explain the approval they need.
- Run, status and backfill tools for MCP wait for `ogund`. They will start runs only within approvals that already exist.

## Alternatives considered

- **A validator per editor:** rejected. They drift.
- **Exposing the daemon API directly to extensions:** rejected. It grants too much authority to code running inside editors.
