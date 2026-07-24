# nlvm — repères rapides

Implémentation Rust du langage **NL** (specs : [nlvm-lang/nlvm-specs](https://github.com/nlvm-lang/nlvm-specs)).
Détails projet/install/usage → [README.md](README.md). Version specs ciblée → [SPECS_VERSION](SPECS_VERSION).

## Crates (`crates/`)

| Crate | Rôle |
|---|---|
| `nl-syntax` | lexer, parser, AST |
| `nl-sema` | analyse sémantique (résolution, typage, checks E0xx) |
| `nl-bytecode` | format module `.nlm` (encodage/décodage partagé) |
| `nl-codegen` | AST → bytecode |
| `nl-vm` | interpréteur (frames, stack, opcodes, GC) |
| `nlc` | binaire CLI compilateur |
| `nlvm` | binaire CLI VM |
| `nl-test-runner` | binaire `nltest`, exécute les tests YAML de `tests/` |

## Tests

`tests/*.yaml` — fixtures organisées par phase (`phaseN_...`). Lancer via `nltest` (`cargo run -p nl-test-runner` ou binaire `nltest`).

## Suivi du projet

- État d'avancement / TODO courant → [Next.md](Next.md) (fichier de notes perso de l'utilisateur — ne pas modifier sans demande explicite)
- Historique des changements → [CHANGELOG.md](CHANGELOG.md)
- Décisions/investigations passées détaillées → [journal/](journal/)
- Écarts vs specs suivis comme issues GitHub (labels `spec-gap`, `component:*`, `optimization`) sur nlvm-lang/nlvm

## Maintenance de ce fichier

À tenir à jour par Claude quand la structure change (ajout/suppression de crate, changement d'organisation des tests, etc.) — garder concis, ne pas dupliquer le contenu des fichiers référencés.
