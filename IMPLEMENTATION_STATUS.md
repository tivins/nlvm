# État d'implémentation vs. nlvm-specs

Photographie de l'écart entre les spécifications (`nlvm-specs`, `SPECS_VERSION` = 0.8.47 côté ce dépôt) et l'implémentation Rust actuelle (nlvm v0.12.4). Établi par lecture croisée de `specs.md`, `compiler.md`, `vm.md`, `stdlib.md`, `optimizations.md`, `tests.md` contre `crates/nl-syntax`, `crates/nl-sema`, `crates/nl-codegen`, `crates/nl-bytecode`, `crates/nl-vm`, `crates/nl-test-runner`, `Next.md` et `journal/`.

Ne liste que les écarts. Tout ce qui n'apparaît pas ici a été vérifié conforme.

## Résumé

Les milestones 1 à 8 (lexer/parser, sémantique, bytecode, VM core, objets, exceptions/closures, stdlib, test runner) sont **globalement en place et fonctionnels** — 14/14 tests officiels des specs passent, 189/189 tests internes (fixtures YAML de `tests/`) passent. Le milestone 9 (optimisations) est entièrement optionnel et n'a pas été commencé, comme prévu par le plan des milestones.

Sept phases de comblement d'écarts ont été réalisées et sont documentées en détail dans [`journal/journal_05_implementation_gap_phases.md`](journal/journal_05_implementation_gap_phases.md) :

| Phase | Sujet | Tests ajoutés |
|-------|-------|---------------|
| 1 | `static` runtime, `ValueEquatable` dans Map/List, `Stringable` dans `+`/cast | 4 fixtures YAML |
| 2 | `typedef`, `switch`/`case`, `interface extends`, `for (const ...)`, E030 | 9 fixtures YAML + 1 test Rust |
| 3 | Validation E009 pour `++`/`--`, `Stringable` dans `print`/`println` | 4 fixtures YAML |
| 4 | Conformance d'interface E033, chemin par défaut `nl-test-runner` | 4 fixtures YAML |
| 5 | `++`/`--` préfixe + valeur d'expression, résolution de surcharge par type | 6 fixtures YAML |
| 6 | Drapeaux `ABSTRACT`/`FINAL` (émission, validation, garde-fou runtime) | 14 tests Rust |
| 7 | Collecteur de cycles (suppression d'essai) | 5 fixtures YAML + 3 tests Rust |

Les écarts restants sont suivis comme **issues GitHub** sur [nlvm-lang/nlvm/issues](https://github.com/nlvm-lang/nlvm/issues) — voir les labels [`spec-gap`](https://github.com/nlvm-lang/nlvm/issues?q=is%3Aissue+label%3Aspec-gap), [`component:sema`](https://github.com/nlvm-lang/nlvm/issues?q=is%3Aissue+label%3A%22component%3Asema%22), [`component:vm`](https://github.com/nlvm-lang/nlvm/issues?q=is%3Aissue+label%3A%22component%3Avm%22), [`component:stdlib`](https://github.com/nlvm-lang/nlvm/issues?q=is%3Aissue+label%3A%22component%3Astdlib%22), [`optimization`](https://github.com/nlvm-lang/nlvm/issues?q=is%3Aissue+label%3Aoptimization) pour filtrer.

## Ordre de valeur/risque recommandé

1. **[#1 `system.text.json`](https://github.com/nlvm-lang/nlvm/issues/1)** et **[#2 `system.db`](https://github.com/nlvm-lang/nlvm/issues/2)** — gros chantiers autonomes (nouvelles dépendances, nouveau binding natif complet).
2. ~~Limites Phase 5 : **[#7 appel statique implicite même-classe](https://github.com/nlvm-lang/nlvm/issues/7)** et **[#8 conformance E033/E044 par type](https://github.com/nlvm-lang/nlvm/issues/8)**.~~ Traité v0.12.3 (#7) et v0.12.4 (#8).
3. ~~**[#9 Fusion par diamant d'interfaces (E041)](https://github.com/nlvm-lang/nlvm/issues/9)**.~~ Traité v0.12.5.
4. **[#16 Vérificateur statique `NEW`](https://github.com/nlvm-lang/nlvm/issues/16)** (limite Phase 6) — à considérer si un besoin concret apparaît.
5. **[#17 Limites collecteur de cycles Phase 7](https://github.com/nlvm-lang/nlvm/issues/17)**.
6. **[#18 Milestone 9 — optimisations](https://github.com/nlvm-lang/nlvm/issues/18)** — à laisser pour la fin.
