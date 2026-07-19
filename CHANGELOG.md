# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.0]

Track the `nlvm-specs` baseline explicitly.

### Added
- `SPECS_VERSION`: single source of truth for the `nlvm-specs` release this implementation targets, bumped whenever new specs are implemented.
- `nlc --version`, reporting the crate version and the tracked `nlvm-specs` version (`nlc` had no version flag before).

### Changed
- `nlvm --version` now also reports the tracked `nlvm-specs` version alongside the crate version.
- `tools/Release.nl` now tags releases as `<version>+<specs version>` (e.g. `0.5.0+0.8.44`) instead of the changelog version alone.

## [0.4.0]

Single-file program output: `.nlp` container format.

### Added
- `.nlp` program container format (`nl-bytecode::program`): one file bundling every module of a compiled program, each embedded as a complete `.nlm` image.
- `nlvm` runs `.nlp` files (and still accepts `.nlm` files, directories, or a mix); containers are detected by magic number, not extension.
- `nlc --emit-modules`: opt back into the previous one-`.nlm`-per-class output layout.

### Changed
- `nlc` now produces a single `.nlp` program by default — `-o` may name the file directly (`-o prog.nlp`) or a directory, in which case the file is named after the entry class (the one defining a static `main`).
- `nlvm --version` reports the real crate version instead of a hardcoded string.

## [0.3.0]

Release helper script written in NL itself.

### Added
- `tools/Release.nl`: reads the latest version from `CHANGELOG.md` and runs `git tag -a`/`git push` for it, demonstrating `system.io.File`, `system.text.Regex`, and `system.ps.Process` together in a real script.

## [0.2.0]

Explicit function type declarations.

### Added
- Explicit function types (`(int) => bool`, with optional `throws`) usable as a variable/field/parameter/return type, per specs.md § Function type assignment.

### Fixed
- `nl-vm`'s descriptor param-count parsing (`count_params`) miscounted a parameter whose own descriptor contains a comma (a function-type parameter, or a mangled generic like `system.Map<K, V>`) — now depth-aware.
- A closure literal with a union-typed parameter (e.g. `string|null`), called through a bare identifier, crashed at runtime (`invoke` not found) — its synthesized `invoke` method's descriptor is now built consistently with what every call site expects.

## [0.1.1]

Stack trace support. Detailed build journal in [docs/journal_02_stack_trace.md](docs/journal_02_stack_trace.md).

### Added
- Exception stack trace capture.
- `StackOverflowException` via call depth limit.
- Shadow stack for stack traces.
- Line-number table in codegen.

## [0.1.0]

Initial implementation of the NL language: compiler (`nlc`), bytecode VM (`nlvm`), and YAML test runner (`nltest`). Detailed build journal in [docs/journal_01_initial_build.md](docs/journal_01_initial_build.md).

### Added
- Lexer, parser, AST, and a shared `.nlm` bytecode format (`nl-bytecode`) between compiler and VM.
- Core semantics: typing, name resolution, null safety, definite assignment, smart-cast narrowing.
- Objects, arrays, interfaces, virtual dispatch, single inheritance.
- Exceptions (`throw`/`try`/`catch`/`finally`), checked-exception verification (E015-E017), `match` expressions.
- Closures (capture by value) and generic classes via monomorphization.
- Full `system.*` standard library: `Out`/`Err`/`In`, `String`, `List<T>`/`Map<K,V>`, `system.io.*` (files, directories, paths, `Grep`), `Random`/`SecureRandom`/`Uuid`, `system.net.*` (TCP/UDP/HTTP with TLS), `system.thread.*` (real OS threads, `Mutex`, `Semaphore`), `system.ps.Process`, `system.text.Regex`/`Encoding`, `system.time.DateTime`/`TimeZone`, `system.Env`.
- Array callback methods (`map`/`filter`/`forEach`/`sort`/`find`/`slice`), array initializer lists, ternary/nullish-coalescing/elvis/three-way-comparison (`<=>`) operators, explicit casts, ref parameters, named/optional/default arguments, readonly enforcement, abstract/final classes, enums.
- Reference-counting GC with destructor calls (`<destruct>`) on last-reference drop.
- Full semantic error-code coverage (49/49 checks from the spec).

### Notes
- Older, phase-by-phase history: `git log` or [docs/journal_01_initial_build.md](docs/journal_01_initial_build.md).
