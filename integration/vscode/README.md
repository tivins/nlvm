# NL Language for VS Code

Syntax highlighting and linting for the [NL language](../../README.md).

## Features

- Syntax highlighting (`syntaxes/nl.tmLanguage.json`) — keywords, primitive types,
  strings, comments, numbers.
- Bracket/comment matching and auto-closing pairs (`language-configuration.json`).
- Linting: runs `nlc -l <file>` on open/save and reports syntax and semantic
  errors in the Problems panel. Also available as the `NL: Lint Current File`
  command.

## Settings

- `nl.compilerPath` (default `"nlc"`) — path to the `nlc` binary. When working
  inside the `nlvm` repo itself, point this at `target/debug/nlc` (or
  `target/release/nlc`) after `cargo build -p nlc`.
- `nl.lintOnSave` (default `true`)
- `nl.lintOnOpen` (default `true`)

## Development

```sh
npm install
npm run compile   # or: npm run watch
```

Then press F5 (or "Run NL Extension" in the Run panel) to launch an Extension
Development Host with the extension loaded, and open a `.nl` file.
