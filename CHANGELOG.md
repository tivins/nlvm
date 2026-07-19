# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.5]

Website: home page identity pass. No toolchain changes.

### Changed
- Landing-page hero is now asymmetric: copy and CTAs on the left, the animated terminal demo on the right, so the compiler is on screen from the first second.
- Section kickers are numbered (`01 · Why NL` …) and the footer states how the site is built (hand-written HTML & CSS, no framework, no tracking).

### Added
- A subtle film-grain overlay across the site, a blinking caret in the header wordmark, and a "Devlog" section on the home page surfacing the three latest posts.

## [0.5.4]

Website: hero brand lockup. No toolchain changes.

### Added
- Landing page opens with an NL brand lockup (glyph + "The NL programming language" eyebrow) above the headline, so the language is named at first glance.

## [0.5.3]

Website: interactive terminal demo. No toolchain changes.

### Added
- Landing-page terminal now cycles through four real captured scenarios (build & run, compile checks, stack traces, spec & tests) with clickable tabs to pick one.

## [0.5.2]

Diagnostic formatting fix. No language changes.

### Fixed
- `nlc` no longer prints the compile-error code twice (e.g. `E003 — … (E003)`); the code now appears exactly once, matching `nlc --lint` output. The same duplication is removed from `nl-test-runner` failure messages.

## [0.5.1]

Project website. No toolchain changes.

### Added
- Project website under `docs/` (served by GitHub Pages from `main`/`docs`): landing page, language tour, getting-started guide, and an English devlog — static HTML/CSS/JS, dark theme.

### Changed
- Build journals moved from `docs/` to `journal/` (`docs/` is now the website root); links updated in `CHANGELOG.md` and `Next.md`.

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

Stack trace support. Detailed build journal in [journal/journal_02_stack_trace.md](journal/journal_02_stack_trace.md).

### Added
- Exception stack trace capture.
- `StackOverflowException` via call depth limit.
- Shadow stack for stack traces.
- Line-number table in codegen.

## [0.1.0]

Initial implementation of the NL language: compiler (`nlc`), bytecode VM (`nlvm`), and YAML test runner (`nltest`). Detailed build journal in [journal/journal_01_initial_build.md](journal/journal_01_initial_build.md).

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
- Older, phase-by-phase history: `git log` or [journal/journal_01_initial_build.md](journal/journal_01_initial_build.md).
