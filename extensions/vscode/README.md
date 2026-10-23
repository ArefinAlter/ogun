# Ogun for VS Code

Editing support for Ogun workflow IR (`*.workflow.json`, spec O-02):

- **Schema checking.** The JSON Schema of the IR is bundled, so VS Code flags structural mistakes as you type.
- **Validator diagnostics.** Every diagnostic the runtime would give (references, cycles, budgets, inline secrets and code, stream rules) appears in the Problems panel at the member it names.
- **Language server features**, when the Ogun language server is available: hover with an operator's kind, outputs and versions; completion of `op` references (`name@major`), `inputs` references (`node.output`), route targets, policy, budget and predicate members and the closed value sets; go to definition from an input reference to its node; an outline of the nodes; and a code lens with the workflow hash.
- **Workflow hash.** The status bar shows the hash an approval would bind to. **Ogun: Copy workflow hash** (or the code lens) copies it.

The extension is read-only. It never runs a workflow, and it is never an approval surface: approvals happen on the Ogun CLI or in Studio.

## Two modes

| Mode | When | What runs |
|---|---|---|
| Language server | `ogun-lsp` is on `PATH`, `ogun.languageServer.path` names a server, or `ogun lsp` starts | One long-lived server over stdio; diagnostics as you type |
| CLI | No server found, it fails to start, or `ogun.languageServer.mode` is `off` | `ogun validate --json` on open and on save |

Both use the same validator (`ogun-ir`), so they never disagree with each other or with `ogund`. The language-server client is plain JavaScript in `src/lsp.js` (Content-Length framed JSON-RPC over the child's stdio); the extension still has no npm dependencies.

## Settings

| Setting | Default | Meaning |
|---|---|---|
| `ogun.path` | `ogun` | The `ogun` command-line tool. |
| `ogun.validateOnSave` | `true` | CLI mode: validate on save. |
| `ogun.languageServer.mode` | `auto` | `auto`, `on` (log why the server cannot start) or `off`. |
| `ogun.languageServer.path` | empty | Command that starts the server. Empty: `ogun-lsp` on `PATH`, then `ogun lsp`. |

## Develop

```sh
node --test                                   # locator, schema copy, LSP framing and client
OGUN_LSP_BIN=../../target/debug/ogun-lsp node --test   # also against the real server
npx @vscode/vsce package                      # builds ogun-0.1.0.vsix
```

The bundled schema must equal `docs/spec/schema/workflow-ir.v0.schema.json`; the test checks it. After changing the schema, copy it here.
