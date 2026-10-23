'use strict';
// The Ogun VS Code extension. Structural checking and completion come from the JSON Schema
// (contributes.jsonValidation); semantic checking runs the real validator, `ogun validate --json`,
// so the editor never disagrees with the runtime. Read-only: the extension never runs workflows
// and is never an approval surface (ADR 0011).

const vscode = require('vscode');
const { execFile } = require('child_process');
const { locateNearest } = require('./locate');

const isWorkflow = (doc) => doc.fileName.endsWith('.workflow.json');

function runOgun(args, input) {
  const bin = vscode.workspace.getConfiguration('ogun').get('path') || 'ogun';
  return new Promise((resolve, reject) => {
    const child = execFile(bin, args, { maxBuffer: 16 * 1024 * 1024 }, (err, stdout) => {
      if (err && err.code === 'ENOENT') return reject(new Error(`ogun not found at "${bin}"; set ogun.path`));
      try { resolve(JSON.parse(stdout)); } catch (e) { reject(new Error('unexpected output from ogun')); }
    });
    if (input !== undefined) child.stdin.end(input);
  });
}

async function validate(doc, collection) {
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

function activate(context) {
  const collection = vscode.languages.createDiagnosticCollection('ogun');
  const status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
  context.subscriptions.push(collection, status);

  const run = async (doc) => {
    try {
      const report = await validate(doc, collection);
      if (report && report.workflow_hash) {
        status.text = `Ogun ${report.workflow_hash.slice(0, 4)}…${report.workflow_hash.slice(-4)}`;
        status.tooltip = `Workflow hash ${report.workflow_hash}`;
        status.show();
      } else if (report) {
        status.text = 'Ogun: invalid';
        status.show();
      }
    } catch (e) {
      vscode.window.showWarningMessage(`Ogun: ${e.message}`);
    }
  };

  context.subscriptions.push(
    vscode.commands.registerCommand('ogun.validate', () => {
      const doc = vscode.window.activeTextEditor && vscode.window.activeTextEditor.document;
      if (doc) run(doc);
    }),
    vscode.commands.registerCommand('ogun.copyHash', async () => {
      const doc = vscode.window.activeTextEditor && vscode.window.activeTextEditor.document;
      if (!doc || !isWorkflow(doc)) return;
      const out = await runOgun(['hash', '--json', doc.fileName]).catch((e) => ({ error: e.message }));
      if (out.workflow_hash) {
        await vscode.env.clipboard.writeText(out.workflow_hash);
        vscode.window.showInformationMessage(`Copied ${out.workflow_hash}`);
      } else {
        vscode.window.showWarningMessage('Ogun: the workflow is not valid; nothing to hash');
      }
    }),
    vscode.workspace.onDidSaveTextDocument((doc) => {
      if (isWorkflow(doc) && vscode.workspace.getConfiguration('ogun').get('validateOnSave')) run(doc);
    }),
    vscode.workspace.onDidOpenTextDocument((doc) => { if (isWorkflow(doc)) run(doc); }),
  );
  vscode.workspace.textDocuments.filter(isWorkflow).forEach(run);
}

function deactivate() {}

module.exports = { activate, deactivate };
