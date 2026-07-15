# nlvm

Implementation of the **NL** language, specified in [`nlvm-specs`](https://github.com/tivins/nlvm-specs): compiler (`nlc`), bytecode virtual machine (`nlvm`), and YAML test runner (`nltest`).

See [PLAN.md](PLAN.md) for the detailed roadmap (phases, decisions, progress tracking).

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
cargo build
```

## Usage

```sh
# Compile a .nl file into an .nlm module
cargo run -p nlc -- -o out/ Main.nl

# Run a compiled module
cargo run -p nlvm -- out/Main.nlm
```

## Tests

The YAML test suite lives in [`nlvm-specs/tests`](../nlvm-specs/tests) (not in this repository). The runner executes it directly:

```sh
cargo run -p nl-test-runner -- /data/projects/nlvm-specs/tests
```

Each `m{N}_*.yaml` file corresponds to a milestone from [`nlvm-specs/docs/milestones.md`](../nlvm-specs/docs/milestones.md). See [`nlvm-specs/docs/tests.md`](../nlvm-specs/docs/tests.md) for the format.
