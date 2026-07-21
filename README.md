# nlvm

Implementation of the **NL** language, specified in [`nlvm-specs`](https://github.com/nlvm-lang/nlvm-specs): compiler (`nlc`), bytecode virtual machine (`nlvm`), and YAML test runner (`nltest`).

The `nlvm-specs` release currently targeted is tracked in [`SPECS_VERSION`](SPECS_VERSION) (bumped whenever new specs are implemented) and reported by `nlc --version` / `nlvm --version`.

See [CHANGELOG.md](CHANGELOG.md) for a history of notable changes, and [Next.md](Next.md) for open items and implementation notes.

## Example

```
namespace hello;
class Main {
    public static int main(string[] args) {
        system.Out.println("Hello, world!");
        return 0;
    }
}
```

## Structure

```
crates/
├── nl-syntax/       # lexer + parser + AST
├── nl-sema/         # semantic analysis (name resolution, typing, checks)
├── nl-bytecode/     # .nlm module format (shared encoding/decoding)
├── nl-codegen/      # AST -> bytecode
├── nl-vm/           # interpreter (frames, stack, opcodes)
├── nlc/             # compiler CLI binary
├── nlvm/            # VM CLI binary
└── nl-test-runner/  # `nltest` binary, runs YAML tests
```

## Build

```sh
cargo build -r
```

## Install

One-liner (downloads the latest prebuilt `nlc`/`nlvm` for Linux x86_64 or macOS arm64 into `~/.local/bin`, which must be on `$PATH`):

```sh
curl -fsSL https://nlvm.dev/install.sh | bash
```

From a clone (builds from source instead, same `~/.local/bin` target — use this on other platforms):

```sh
./install.sh
```

## Usage

```sh
# Compile .nl sources into a single .nlp program (named after the entry class)
nlc -o out/ Main.nl

# ...or to an explicit path
nlc -o out/prog.nlp Main.nl

# Run a compiled program
nlvm out/prog.nlp

# Legacy layout: one .nlm module per class/interface
nlc --emit-modules -o out/ Main.nl
nlvm out/   # loads every .nlm/.nlp under the directory
```

## Tests

This repository ships its own YAML test suite under [`tests/`](tests) (`phase{N}_*.yaml`, one file per language feature), which is what CI runs:

```sh
cargo test --workspace
cargo run -p nl-test-runner -- tests
```

The canonical spec suite lives in [`nlvm-specs/tests`](https://github.com/nlvm-lang/nlvm-specs/tree/main/tests) (not in this repository) and can be run the same way:

```sh
cargo run -p nl-test-runner -- /local-path-to/nlvm-specs/tests
```

Each `m{N}_*.yaml` file there corresponds to a milestone from [`nlvm-specs/docs/milestones.md`](https://github.com/nlvm-lang/nlvm-specs/blob/main/docs/milestones.md). See [`nlvm-specs/docs/tests.md`](https://github.com/nlvm-lang/nlvm-specs/blob/main/docs/tests.md) for the format.

## License

[MIT](LICENSE)
