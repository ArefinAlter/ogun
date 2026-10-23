# Ogun for VS Code

Editing support for Ogun workflow IR (`*.workflow.json`, spec O-02):

- **Schema checking and completion.** The JSON Schema of the IR is bundled, so VS Code completes members and flags structural mistakes as you type.
- **Validator diagnostics.** On open and on save, the extension runs `ogun validate --json` and shows every diagnostic in the Problems panel at the member it names. These are the same diagnostics the runtime would give: references, cycles, budgets, inline secrets and code, stream rules.
- **Workflow hash.** The status bar shows the hash an approval would bind to. **Ogun: Copy workflow hash** copies it.

The extension is read-only. It never runs a workflow, and it is never an approval surface: approvals happen on the Ogun CLI or in Studio.

## Requirements

The `ogun` command-line tool on your `PATH`, or set `ogun.path`.

## Develop

```sh
node --test                   # pointer locator and schema copy
npx @vscode/vsce package      # builds ogun-0.1.0.vsix
```

The bundled schema must equal `docs/spec/schema/workflow-ir.v0.schema.json`; the test checks it. After changing the schema, copy it here.
