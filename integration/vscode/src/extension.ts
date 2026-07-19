import * as vscode from 'vscode';
import { execFile } from 'child_process';
import * as path from 'path';

const diagnostics = vscode.languages.createDiagnosticCollection('nl');

// nlc -l reports two shapes on a single stderr line, one per error kind
// (nlc only ever reports the first error it hits, see crates/nlc/src/main.rs):
//   - syntax errors:   path:line:col: message
//   - semantic errors: path:line: E### — message   (LocatedError's Display)
const SYNTAX_ERROR_RE = /^(.+):(\d+):(\d+): (.+)$/;
const SEMANTIC_ERROR_RE = /^(.+):(\d+): (E\d+) — (.+)$/;

function addDiagnostic(
  byFile: Map<string, vscode.Diagnostic[]>,
  file: string,
  line: number,
  col: number,
  message: string
) {
  const zeroLine = Math.max(0, line - 1);
  const zeroCol = Math.max(0, col - 1);
  const range = new vscode.Range(zeroLine, zeroCol, zeroLine, zeroCol + 1);
  const diagnostic = new vscode.Diagnostic(range, message, vscode.DiagnosticSeverity.Error);
  diagnostic.source = 'nlc';
  const list = byFile.get(file) ?? [];
  list.push(diagnostic);
  byFile.set(file, list);
}

function parseDiagnostics(output: string): Map<string, vscode.Diagnostic[]> {
  const byFile = new Map<string, vscode.Diagnostic[]>();
  for (const rawLine of output.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line) continue;

    const syntaxMatch = SYNTAX_ERROR_RE.exec(line);
    if (syntaxMatch) {
      const [, file, lineNo, colNo, message] = syntaxMatch;
      addDiagnostic(byFile, file, Number(lineNo), Number(colNo), message);
      continue;
    }

    const semanticMatch = SEMANTIC_ERROR_RE.exec(line);
    if (semanticMatch) {
      const [, file, lineNo, code, message] = semanticMatch;
      addDiagnostic(byFile, file, Number(lineNo), 1, `${code} — ${message}`);
    }
  }
  return byFile;
}

function lintDocument(document: vscode.TextDocument) {
  if (document.languageId !== 'nl') return;

  const config = vscode.workspace.getConfiguration('nl');
  const compilerPath = config.get<string>('compilerPath', 'nlc');
  const cwd =
    vscode.workspace.getWorkspaceFolder(document.uri)?.uri.fsPath ?? path.dirname(document.fileName);

  execFile(compilerPath, ['-l', document.fileName], { cwd }, (error, _stdout, stderr) => {
    diagnostics.delete(document.uri);

    if (!error) {
      return;
    }

    if ((error as NodeJS.ErrnoException).code === 'ENOENT') {
      vscode.window.showErrorMessage(
        `NL: could not find compiler '${compilerPath}'. Set 'nl.compilerPath' in settings.`
      );
      return;
    }

    const byFile = parseDiagnostics(stderr);
    for (const [file, fileDiagnostics] of byFile) {
      const uri = path.isAbsolute(file) ? vscode.Uri.file(file) : vscode.Uri.file(path.resolve(cwd, file));
      diagnostics.set(uri, fileDiagnostics);
    }
  });
}

export function activate(context: vscode.ExtensionContext) {
  context.subscriptions.push(diagnostics);

  context.subscriptions.push(
    vscode.commands.registerCommand('nl.lintCurrentFile', () => {
      const editor = vscode.window.activeTextEditor;
      if (editor) {
        lintDocument(editor.document);
      }
    })
  );

  context.subscriptions.push(
    vscode.workspace.onDidSaveTextDocument((document) => {
      if (vscode.workspace.getConfiguration('nl').get<boolean>('lintOnSave', true)) {
        lintDocument(document);
      }
    })
  );

  context.subscriptions.push(
    vscode.workspace.onDidOpenTextDocument((document) => {
      if (vscode.workspace.getConfiguration('nl').get<boolean>('lintOnOpen', true)) {
        lintDocument(document);
      }
    })
  );

  context.subscriptions.push(
    vscode.workspace.onDidCloseTextDocument((document) => {
      diagnostics.delete(document.uri);
    })
  );

  for (const document of vscode.workspace.textDocuments) {
    if (document.languageId === 'nl') {
      lintDocument(document);
    }
  }
}

export function deactivate() {
  diagnostics.dispose();
}
