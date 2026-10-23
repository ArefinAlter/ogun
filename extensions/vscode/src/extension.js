'use strict';
// The Ogun VS Code extension. Structural checking and completion come from the JSON Schema
// (contributes.jsonValidation). Semantic checking always runs the real validator, so the editor
// never disagrees with the runtime:
//
// - language-server mode: when `ogun-lsp` (or `ogun lsp`) is found or configured, the extension
//   starts it and uses its diagnostics, hover, completion, go-to-definition, symbols and the
//   workflow-hash code lens;
// - CLI mode, the fallback: `ogun validate --json` on open and on save.
//
// Read-only either way: the extension never runs workflows and is never an approval surface
// (ADR 0011).

const vscode = require('vscode');
const { execFile } = require('child_process');
const { locateNearest } = require('./locate');
const { LspClient, resolveServerCommand } = require('./lsp');

const isWorkflow = (doc) => doc.uri.scheme === 'file' && doc.fileName.endsWith('.workflow.json');
const SELECTOR = { scheme: 'file', pattern: '**/*.workflow.json' };
const config = () => vscode.workspace.getConfiguration('ogun');

function runOgun(args, input) {
  const bin = config().get('path') || 'ogun';
  return new Promise((resolve, reject) => {
    const child = execFile(bin, args, { maxBuffer: 16 * 1024 * 1024 }, (err, stdout) => {
      if (err && err.code === 'ENOENT') return reject(new Error(`ogun not found at "${bin}"; set ogun.path`));
      try { resolve(JSON.parse(stdout)); } catch (e) { reject(new Error('unexpected output from ogun')); }
    });
    if (input !== undefined) child.stdin.end(input);
  });
}

async function validateWithCli(doc, collection) {
  if (!isWorkflow(doc)) return null;
  // The validator reads files; validate the saved file so positions match the disk contents.
  const report = await runOgun(['validate', '--json', doc.fileName]);
  const text = doc.getText();
  const diagnostics = (report.diagnostics || []).map((d) => {
    const r = locateNearest(text, d.path || '');
    const range = new vscode.Range(doc.positionAt(r.start), doc.positionAt(Math.max(r.end, r.start + 1)));
    const severity = d.severity === 'warning' ? vscode.DiagnosticSeverity.Warning : vscode.DiagnosticSeverity.Error;
    const diag = new vscode.Diagnostic(range, d.message, severity);
    diag.code = d.code;
    diag.source = 'ogun';
    return diag;
  });
  collection.set(doc.uri, diagnostics);
  return report;
}

// ---- LSP to VS Code conversions ----

const toRange = (r) => new vscode.Range(r.start.line, r.start.character, r.end.line, r.end.character);
const toPosition = (p) => ({ line: p.line, character: p.character });
const docId = (doc) => ({ uri: doc.uri.toString() });
const SEVERITY = {
  1: vscode.DiagnosticSeverity.Error,
  2: vscode.DiagnosticSeverity.Warning,
  3: vscode.DiagnosticSeverity.Information,
  4: vscode.DiagnosticSeverity.Hint,
};

function toMarkdown(contents) {
  if (!contents) return null;
  if (Array.isArray(contents)) return contents.map(toMarkdown).filter(Boolean);
  if (typeof contents === 'string') return new vscode.MarkdownString(contents);
  if (contents.kind === 'plaintext') return new vscode.MarkdownString().appendText(contents.value);
  if (contents.value !== undefined) return new vscode.MarkdownString(contents.value);
  return null;
}

function toDiagnostic(d) {
  const diag = new vscode.Diagnostic(toRange(d.range), d.message, SEVERITY[d.severity] || vscode.DiagnosticSeverity.Error);
  if (d.code !== undefined) diag.code = d.code;
  diag.source = d.source || 'ogun';
  return diag;
}

function toCompletionItem(item) {
  // LSP kinds are 1-based, VS Code's 0-based, in the same order.
  const out = new vscode.CompletionItem(item.label, typeof item.kind === 'number' ? item.kind - 1 : undefined);
  if (item.detail) out.detail = item.detail;
  if (item.documentation) out.documentation = toMarkdown(item.documentation);
  if (item.filterText) out.filterText = item.filterText;
  if (item.sortText) out.sortText = item.sortText;
  if (item.textEdit) {
    out.range = toRange(item.textEdit.range || item.textEdit.replace);
    out.insertText = item.textEdit.newText;
  } else if (item.insertText) {
    out.insertText = item.insertText;
  }
  return out;
}

function toSymbol(s) {
  const out = new vscode.DocumentSymbol(s.name, s.detail || '', s.kind - 1, toRange(s.range), toRange(s.selectionRange));
  out.children = (s.children || []).map(toSymbol);
  return out;
}

// ---- the language-server mode ----

async function startLanguageServer(context, output, onHash) {
  const settings = {
    mode: config().get('languageServer.mode') || 'auto',
    lspPath: config().get('languageServer.path') || '',
    ogunPath: config().get('path') || 'ogun',
  };
  const target = resolveServerCommand(settings);
  if (!target) {
    if (settings.mode === 'on') output.appendLine('ogun-lsp not found; using `ogun validate --json`.');
    return null;
  }
  const cwd = vscode.workspace.workspaceFolders && vscode.workspace.workspaceFolders[0]
    ? vscode.workspace.workspaceFolders[0].uri.fsPath
    : undefined;
  const client = new LspClient(target.command, target.args, { cwd });
  client.on('stderr', (text) => output.append(text));
  try {
    await client.initialize({
      processId: process.pid,
      clientInfo: { name: 'ogun-vscode', version: context.extension ? context.extension.packageJSON.version : '0' },
      rootUri: cwd ? vscode.Uri.file(cwd).toString() : null,
      capabilities: {
        general: { positionEncodings: ['utf-16'] },
        textDocument: {
          synchronization: { dynamicRegistration: false },
          hover: { contentFormat: ['markdown', 'plaintext'] },
          completion: { completionItem: { snippetSupport: false, documentationFormat: ['markdown', 'plaintext'] } },
          definition: {},
          documentSymbol: { hierarchicalDocumentSymbolSupport: true },
          codeLens: {},
          publishDiagnostics: { versionSupport: true },
        },
      },
    });
  } catch (e) {
    output.appendLine(`Could not start ${target.command} ${target.args.join(' ')}: ${e.message}. Using \`ogun validate --json\`.`);
    client.stop().catch(() => {});
    return null;
  }
  output.appendLine(`Using the Ogun language server (${target.source}): ${target.command} ${target.args.join(' ')}`);

  const collection = vscode.languages.createDiagnosticCollection('ogun');
  const hashOf = (uri) => client.request('ogun/workflowHash', { textDocument: { uri } }).catch(() => null);

  client.on('notification', (method, params) => {
    if (method === 'textDocument/publishDiagnostics') {
      const uri = vscode.Uri.parse(params.uri);
      collection.set(uri, (params.diagnostics || []).map(toDiagnostic));
      hashOf(params.uri).then((h) => onHash(uri, h));
    } else if (method === 'window/showMessage' && params && params.message) {
      vscode.window.showInformationMessage(`Ogun: ${params.message}`);
    } else if (method === 'window/logMessage' && params && params.message) {
      output.appendLine(params.message);
    }
  });

  const open = (doc) => {
    if (!isWorkflow(doc)) return;
    client.notify('textDocument/didOpen', {
      textDocument: { uri: doc.uri.toString(), languageId: doc.languageId, version: doc.version, text: doc.getText() },
    });
  };

  context.subscriptions.push(
    collection,
    vscode.workspace.onDidOpenTextDocument(open),
    vscode.workspace.onDidChangeTextDocument((e) => {
      if (!isWorkflow(e.document) || e.contentChanges.length === 0) return;
      client.notify('textDocument/didChange', {
        textDocument: { uri: e.document.uri.toString(), version: e.document.version },
        contentChanges: [{ text: e.document.getText() }],
      });
    }),
    vscode.workspace.onDidCloseTextDocument((doc) => {
      if (isWorkflow(doc)) client.notify('textDocument/didClose', { textDocument: docId(doc) });
    }),
    vscode.languages.registerHoverProvider(SELECTOR, {
      async provideHover(doc, position) {
        const h = await client.request('textDocument/hover', { textDocument: docId(doc), position: toPosition(position) }).catch(() => null);
        if (!h || !h.contents) return null;
        return new vscode.Hover(toMarkdown(h.contents), h.range ? toRange(h.range) : undefined);
      },
    }),
    vscode.languages.registerCompletionItemProvider(SELECTOR, {
      async provideCompletionItems(doc, position) {
        const r = await client.request('textDocument/completion', { textDocument: docId(doc), position: toPosition(position) }).catch(() => null);
        const items = Array.isArray(r) ? r : (r && r.items) || [];
        return items.map(toCompletionItem);
      },
    }, '"', '.', '@'),
    vscode.languages.registerDefinitionProvider(SELECTOR, {
      async provideDefinition(doc, position) {
        const r = await client.request('textDocument/definition', { textDocument: docId(doc), position: toPosition(position) }).catch(() => null);
        const list = r ? (Array.isArray(r) ? r : [r]) : [];
        return list.map((l) => new vscode.Location(vscode.Uri.parse(l.uri || l.targetUri), toRange(l.range || l.targetSelectionRange)));
      },
    }),
    vscode.languages.registerDocumentSymbolProvider(SELECTOR, {
      async provideDocumentSymbols(doc) {
        const r = await client.request('textDocument/documentSymbol', { textDocument: docId(doc) }).catch(() => null);
        return (r || []).filter((s) => s.selectionRange).map(toSymbol);
      },
    }),
    vscode.languages.registerCodeLensProvider(SELECTOR, {
      async provideCodeLenses(doc) {
        const r = await client.request('textDocument/codeLens', { textDocument: docId(doc) }).catch(() => null);
        return (r || [])
          .filter((l) => l.command && l.command.command === 'ogun.workflowHash')
          .map((l) => new vscode.CodeLens(toRange(l.range), {
            title: l.command.title,
            command: 'ogun.copyHash',
            arguments: l.command.arguments,
          }));
      },
    }),
    { dispose: () => { client.stop().catch(() => {}); } },
  );
  vscode.workspace.textDocuments.forEach(open);
  return { client, collection, hashOf };
}

// ---- activation ----

async function activate(context) {
  const output = vscode.window.createOutputChannel('Ogun');
  const status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
  context.subscriptions.push(output, status);

  const showHash = (hash) => {
    if (hash) {
      status.text = `Ogun ${hash.slice(0, 4)}…${hash.slice(-4)}`;
      status.tooltip = `Workflow hash ${hash}`;
    } else {
      status.text = 'Ogun: invalid';
      status.tooltip = 'The workflow has errors; see the Problems panel';
    }
    status.show();
  };

  let lsp = null;
  const onHash = (uri, info) => {
    const editor = vscode.window.activeTextEditor;
    if (!editor || editor.document.uri.toString() !== uri.toString() || !info) return;
    showHash(info.workflow_hash);
  };
  try {
    lsp = await startLanguageServer(context, output, onHash);
  } catch (e) {
    output.appendLine(`Language server unavailable: ${e.message}`);
    lsp = null;
  }

  const cliCollection = vscode.languages.createDiagnosticCollection('ogun-cli');
  context.subscriptions.push(cliCollection);
  const runCli = async (doc) => {
    try {
      const report = await validateWithCli(doc, cliCollection);
      if (report) showHash(report.workflow_hash);
    } catch (e) {
      vscode.window.showWarningMessage(`Ogun: ${e.message}`);
    }
  };
  const usingLsp = () => lsp && !lsp.client.exited;

  if (lsp) {
    lsp.client.on('exit', () => {
      output.appendLine('The Ogun language server stopped; using `ogun validate --json` until the window reloads.');
      lsp.collection.clear();
      vscode.workspace.textDocuments.filter(isWorkflow).forEach(runCli);
    });
  }

  const currentHash = async (doc) => {
    if (usingLsp()) {
      const info = await lsp.hashOf(doc.uri.toString());
      return info && info.workflow_hash;
    }
    const out = await runOgun(['hash', '--json', doc.fileName]).catch(() => ({}));
    return out.workflow_hash;
  };

  context.subscriptions.push(
    vscode.commands.registerCommand('ogun.validate', () => {
      const doc = vscode.window.activeTextEditor && vscode.window.activeTextEditor.document;
      if (!doc || !isWorkflow(doc)) return;
      if (usingLsp()) {
        lsp.client.notify('textDocument/didChange', {
          textDocument: { uri: doc.uri.toString(), version: doc.version },
          contentChanges: [{ text: doc.getText() }],
        });
      } else {
        runCli(doc);
      }
    }),
    vscode.commands.registerCommand('ogun.copyHash', async (hashArg) => {
      let hash = typeof hashArg === 'string' && /^[0-9a-f]{64}$/.test(hashArg) ? hashArg : null;
      if (!hash) {
        const doc = vscode.window.activeTextEditor && vscode.window.activeTextEditor.document;
        if (!doc || !isWorkflow(doc)) return;
        hash = await currentHash(doc);
      }
      if (hash) {
        await vscode.env.clipboard.writeText(hash);
        vscode.window.showInformationMessage(`Copied ${hash}`);
      } else {
        vscode.window.showWarningMessage('Ogun: the workflow is not valid; nothing to hash');
      }
    }),
    vscode.workspace.onDidSaveTextDocument((doc) => {
      if (!usingLsp() && isWorkflow(doc) && config().get('validateOnSave')) runCli(doc);
    }),
    vscode.workspace.onDidOpenTextDocument((doc) => { if (!usingLsp() && isWorkflow(doc)) runCli(doc); }),
    vscode.window.onDidChangeActiveTextEditor(async (editor) => {
      if (!editor || !isWorkflow(editor.document)) return;
      if (usingLsp()) {
        const info = await lsp.hashOf(editor.document.uri.toString());
        if (info) showHash(info.workflow_hash);
      }
    }),
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration('ogun.languageServer') || e.affectsConfiguration('ogun.path')) {
        vscode.window.showInformationMessage('Ogun: reload the window to apply the language server settings.');
      }
    }),
  );
  if (!usingLsp()) vscode.workspace.textDocuments.filter(isWorkflow).forEach(runCli);
}

function deactivate() {}

module.exports = { activate, deactivate };
