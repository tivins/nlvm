# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.12.3]

### Fixed
- Implicit same-class static calls (`foo(x)` without a receiver, resolved against the current class's own static methods) now pick the correct overload by argument type instead of always resolving to whichever same-name overload was declared last. Previously a class declaring two static methods `show(int)` and `show(string)` silently kept only the last declared one in the per-name signature table, so `show(42)` and `show("hi")` both resolved to `show(string)` — the first call failing to compile with `E004` or, worse, targeting the wrong method. Same argument-type-based resolution as the Phase 5 fix already applied to `new T(...)`, `this(...)`/`super(...)` delegation, and dotted method calls (see [issue #7](https://github.com/nlvm-lang/nlvm/issues/7)).

## [0.12.2]

### Fixed
- Objects that only reference each other (a reference cycle — e.g. two objects each holding a field pointing back at the other, or a self-referencing object) are now reclaimed and have their `destruct()` called, closing a previously-documented gap where the `Arc`-refcounting GC could never collect them because their reference count never reached zero. A synchronous trial-deletion pass runs alongside the existing refcounting whenever a reference is dropped from a field, array element, local variable, or `static` field, and again once at program exit; it only reclaims a group once no reference to any of its members exists from outside the group, so an object still reachable through a live variable or a `static` field is never collected while reachable. Collection isn't always instantaneous — reassigning every variable that pointed into a cycle may not free it until the enclosing function returns, a documented limitation — but a cycle is always eventually collected, at the latest by the time the program exits.

## [0.12.1]

### Fixed
- The bytecode `ABSTRACT`/`FINAL` flags (`class_flags`/`method_flags`) are now actually emitted by the compiler and enforced by the VM, closing a gap where they were defined in the module format but never written or checked. An abstract method (interface methods included) is now compiled to a proper code-less stub (`code_length = 0`, no locals/stack/exception/line table) instead of being silently omitted from the module; `abstract`/`final` classes and `final` methods now carry their flag in the compiled bytecode. Loading a module now rejects one with `ABSTRACT`+`FINAL` set together (class or method) or an abstract method with non-empty code — previously undetected malformed bytecode. Loading a program now also rejects a class extending a `final` class or overriding a `final` method, and instantiating an `abstract` class is rejected at runtime as a VM-level safety net — protections that previously existed only at compile time (E032/E035/E036) and gave no defense for a hand-written or corrupted `.nlm`.

## [0.12.0]

### Added
- Prefix `++`/`--` (`++x`, `--x`) is now parsed and compiled — previously only the postfix forms existed. Both prefix and postfix are now real expressions with the value specs.md § Operator precedence documents: postfix evaluates to the pre-mutation value, prefix to the post-mutation one, for a plain `int` local, a `ref int` parameter, and a closure-captured-and-mutated `int` local alike. An overloaded `operator++`/`operator--` on an object still follows specs.md's "Postfix note": both forms evaluate to the same mutated object reference. The target is still restricted to a plain variable name (`obj.field++`/array-element `++`/`--` remain unsupported); `++`/`--` applied to anything else is now a clear parse-time error instead of silently misparsing or failing deep in codegen.

### Fixed
- Method/constructor overload resolution now considers argument types, not just argument count. Previously, when two overloads of the same method or constructor shared the same arity (e.g. `show(int)` and `show(string)`), the compiler always picked whichever was declared first — regardless of the actual argument's type — so calling the "wrong-positioned" overload either failed to compile with a confusing type error or, in rarer cases, silently compiled a call against the wrong method. Resolution now scores each arity-compatible candidate by how well its parameters match the call's argument types (exact match, then numeric widening/subclass/interface compatibility) and picks the best match; ties still fall back to the first declared, a documented limitation. Covers `new T(...)`, `this(...)`/`super(...)` constructor delegation (specs.md § Constructor chaining), and instance/static method calls.
- `this(...)` constructor-delegation cycle detection (E046) also resolved its target by arity only, which could report a false cycle for a same-arity overloaded constructor whose `this(...)` argument was a literal or one of the constructor's own parameters (the delegation target looked like it delegated to itself). It now uses the same argument-type-aware resolution for that common case.

## [0.11.2]

### Fixed
- A non-abstract class that implements an interface (directly, via an ancestor `extends`, or transitively through `interface ... extends ...`) without providing all of its methods is now rejected with `E033 — Class must be declared abstract`, matching specs.md § Interface inheritance ("a class implementing an interface must implement all methods") and § Abstract classes and methods ("interface methods are implicitly abstract"). Previously such a class compiled successfully and only failed at runtime, with an unhelpful error, the first time virtual dispatch tried to find the missing method.
- `nl-test-runner` no longer defaults to a machine-specific absolute path (`/data/projects/nlvm-specs/tests`) when run without an argument; it now defaults to this repo's own `tests/` directory, which always exists relative to the invocation.

## [0.11.1]

### Fixed
- `obj++`/`obj--` on a type with no matching `operator++`/`operator--` overload (or a non-`int` primitive, e.g. `string`) now reports `E009` at compile time instead of silently passing semantic analysis and failing later with an unstructured codegen error.
- `system.Out.print`/`println`/`system.Err.print`/`println` now accept an argument whose static type implements `Stringable`, calling its `toString()` by virtual dispatch — matching the `+` concatenation and `(string)` cast behavior (specs.md § Stringable interface lists all three as consumers). Previously rejected at compile time with `E004`.

## [0.11.0]

### Added
- `typedef Type Name;` (specs.md § Typedef): namespace-scoped compile-time type aliases, usable anywhere a type is expected — including as a `new` target for a template alias (`typedef Vector<int> IntVector; new IntVector(...)`) and for function-type aliases (`typedef (int, int) => int BinaryOperation;`). Fully erased before semantic analysis/codegen, so aliases are completely interchangeable with their underlying type.
- `switch`/`case`/`default` statement (specs.md § Switch/Match) with C-like fall-through semantics: execution continues into the next `case` without an explicit `break`. `break` exits the nearest `switch` or loop; `continue` inside a `switch` still targets the nearest enclosing loop.
- `interface A extends B, C` (interface inheritance, compiler.md § Interface inheritance): an interface may extend any number of parent interfaces, inheriting all their method declarations. `instanceof`/upcast and the const-correctness check (E044) both now work transitively through the whole interface hierarchy, not just directly-implemented interfaces.
- `for (const auto item : collection)` — an explicit `const` on a for-each loop variable is now enforced (E039), independent of whether the iterated collection is itself read-only.

### Fixed
- Assigning an object to an interface-typed variable/field/parameter (`Disposable d = someCloseable;`) now correctly recognizes an interface implemented indirectly through another implemented interface's own `extends` chain — previously only directly-`implements`-ed interfaces were recognized at assignment sites (E004), even though `instanceof` already handled deeper class hierarchies correctly.
- Using a reserved keyword where an identifier is expected now reports the documented `E030` message instead of a generic parse error.

## [0.10.0]

### Added
- `static` fields on ordinary (non-enum) classes are now backed by real per-class storage (`GET_STATIC`/`SET_STATIC`), previously unimplemented (`VmError::Unsupported`). A declared initializer (`public static int counter = 0;`) runs once at program load time, before `main`. Accessed and assigned via `ClassName.field`, including through a subclass name when the field is inherited.
- `system.Map`/`system.List` key/element lookup (`get`/`set`/`remove`/`has`/`contains`) now calls a `ValueEquatable`-implementing type's `valueEquals` for structural equality instead of always falling back to reference identity.
- `Stringable.toString()` is now called implicitly by string concatenation (`+`) and the `(string)` cast when an operand's static type implements `Stringable`, instead of being rejected at compile time (E008/E007).

### Fixed
- A class property declared with an inline initializer (`public int x = 42;`) is now actually assigned that value at construction — previously the initializer expression was parsed and accepted by the compiler but silently never applied, leaving the field at its type's plain default (`0`/`""`/`false`/`null`) regardless of what was written.

## [0.9.0]

### Added
- `Self`/`type` contextual keywords are now also usable inside an **interface body** (specs.md § Self in interfaces), e.g. `interface Spawner { public Self spawn(); }` — previously a parse error. An implementing class writes `Self`/`type` (or its own name) in its own method declaration to get the covariant return type; this compiler doesn't verify interface method conformance beyond existing const-correctness checking (E044), so covariance itself relies on the implementer writing the signature correctly, same as before this change.
- Built-in `Cloneable` interface (specs.md § Cloneable interface): `public Self clone();`. Implement it and call `clone()` as an ordinary instance method for a shallow copy.
- Built-in `ValueEquatable` interface (specs.md § ValueEquatable interface): `public bool valueEquals(const Self|null other); public int valueHash();`. Implement it for structural equality distinct from `==` (reference identity). `system.Map`/`List` key lookup does not yet call into `valueEquals`/`valueHash` (still reference identity for object keys) — see `Next.md`.

## [0.8.0]

### Added
- Operator overloading (specs.md § Operator Overloading): classes can now define `operator+ operator- operator* operator/ operator%`, compound assignment (`operator+= operator-= operator*= operator/= operator%=`), comparisons (`operator< operator> operator<= operator>=`), three-way comparison (`operator<=>`), unary `operator-`/`operator!`, and `operator++`/`operator--`. Resolved by exact parameter type, so a class can overload the same operator for several parameter types (e.g. `operator+` for both another instance and `int`) without ambiguity.
- `type`/`Self` contextual return-type keywords (specs.md § Self and type keywords) inside a class/enum body — `type` for methods that construct and return a new instance of the enclosing class (including `new type(...)`), `Self` for methods that mutate and return `this`. (Not yet supported inside interface bodies — see `Next.md`.)

## [0.7.1]

### Fixed
- `+` between two field-access expressions of static type `string` (e.g. `page.root + item.href`), with no string literal or local variable anywhere in the chain to anchor the fast path, no longer fails codegen with "unsupported construct: arithmetic/comparison between StringT and StringT". String concatenation's static-type peek now also resolves through field accesses and method calls, not just literals and local variables.

## [0.7.0]

### Added
- `Exception.printStackTrace()` (specs.md § Exception class hierarchy, v0.8.47) — writes `message` to `system.Err`, followed by one `"    at " + file + ":" + line` line per `stackTrace` frame, in throw-site-first order. Implemented as an ordinary inherited method on the prelude's root `Exception` class, so every built-in and user-defined exception subclass gets it for free.

## [0.6.0]

### Added
- `nodiscard` method modifier (specs.md § Nodiscard) — previously a parse error whenever used. Calling a `nodiscard` method and discarding its return value as a bare statement now reports warning `W001` (compiler.md § Warnings) instead of failing compilation. `nlc` prints reported warnings to stderr without aborting the build.

## [0.5.10]

### Added
- One-line install: `curl -fsSL https://nlvm.dev/install.sh | bash` downloads the latest prebuilt `nlc`/`nlvm` (Linux x86_64, macOS arm64) and verifies it against a published `SHA256SUMS`. Running `./install.sh` from a clone still builds from source, unchanged.
- `release.yml` now generates and publishes a `SHA256SUMS` file alongside each release's binary tarballs.

## [0.5.9]

### Added
- GitHub Actions workflow (`release.yml`) that builds `nlc`/`nlvm` release binaries for Linux and macOS (Intel + Apple Silicon) and publishes them as a GitHub Release on version tags, laying the groundwork for a one-line install script.

### Changed
- Project moved to the `nlvm-lang` GitHub organization; README and VS Code extension links updated accordingly. The documentation site moved to its own `nlvm.dev` repository.

## [0.5.8]

### Fixed
- A closure nested two or more levels deep, referencing a variable captured by an enclosing closure (rather than its own parameters/locals), now compiles instead of failing with "undefined variable" — the capture is correctly re-propagated (including its shared box, if mutated) through every level of nesting.

## [0.5.7]

Closures now capture variables by reference, matching specs.md § Variable capture.

### Fixed
- Anonymous functions capturing a variable that's mutated after capture — either by the enclosing scope or by the closure itself (`counter++` inside the closure body) — now see/produce the same shared value instead of a stale snapshot taken at closure-creation time.

## [0.5.6]

Website & branding: logo assets. No toolchain changes.

### Added
- Brand assets under `docs/assets/brand/`: a master `logo.svg` (the "nl" glyph as drawn paths, no font dependency) and PNG exports from 16 to 1024 px, ready for the future `nlvm-lang` GitHub organization avatar.
- A 1280×640 social preview card (`social-preview.svg` + rendered PNG) for GitHub social previews and Open Graph, using JetBrains Mono / Inter to match the site's own `--mono` / `--sans` type system instead of generic fallback fonts.
- `docs/assets/brand/generate.py`, a single script that builds all of the above from one set of constants — regenerate everything with `python3 generate.py`.

### Changed
- The site favicon now uses `logo.svg` instead of the inline font-dependent data URI, so it renders identically everywhere.
- The header wordmark (all pages) and the home hero brand now display `logo.svg` instead of the CSS-styled text glyph.

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
