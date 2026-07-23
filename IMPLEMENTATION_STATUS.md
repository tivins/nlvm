# État d'implémentation vs. nlvm-specs

Photographie de l'écart entre les spécifications (`nlvm-specs`, `SPECS_VERSION` = 0.8.47 côté ce dépôt) et l'implémentation Rust actuelle (nlvm v0.9.0). Établi par lecture croisée de `specs.md`, `compiler.md`, `vm.md`, `stdlib.md`, `optimizations.md`, `tests.md` contre `crates/nl-syntax`, `crates/nl-sema`, `crates/nl-codegen`, `crates/nl-bytecode`, `crates/nl-vm`, `crates/nl-test-runner`, `Next.md` et `journal/`.

Ne liste que les écarts. Tout ce qui n'apparaît pas ici a été vérifié conforme.

## Résumé

Les milestones 1 à 8 (lexer/parser, sémantique, bytecode, VM core, objets, exceptions/closures, stdlib, test runner) sont **globalement en place et fonctionnels** — 14/14 tests officiels des specs passent, 157/157 tests internes passent. Mais plusieurs fonctionnalités documentées sont soit absentes, soit déclarées/parsées sans être réellement câblées au runtime — ce qui est plus trompeur qu'une simple absence, car le code compile mais se comporte différemment de la spec. Le milestone 9 (optimisations) est entièrement optionnel et n'a pas été commencé, comme prévu par le plan des milestones.

---

## 1. Écarts à fort impact (touchent des fonctionnalités de base déjà "réputées" complètes)

### Champs `static` sur classes non-enum : non fonctionnels au runtime
`GET_STATIC`/`SET_STATIC` retournent `VmError::Unsupported` (`crates/nl-vm/src/interpreter.rs:677-681`). Il n'existe aucune zone de stockage statique par classe, ni d'exécution des initialiseurs statiques au chargement. Seul le cas particulier des enums (constantes `static readonly` re-compilées à chaque site d'usage) fonctionne. Un champ `public static int counter` sur une classe ordinaire échoue à la compilation ou à l'exécution. `static` est pourtant documenté comme modificateur de champ standard (specs.md § Classes) et le milestone 5 (objets/dispatch) le liste comme livrable.

### `ValueEquatable` déclarable mais jamais consulté par `system.Map`/`system.List`
Confirmé indépendamment par 4 des 4 recherches. `valueEquals`/`valueHash` sont des méthodes utilisables normalement, mais les collections génériques retombent toujours sur l'identité de référence pour les clés/éléments non primitifs (`crates/nl-vm/src/native.rs`, `crates/nl-sema/src/native_generics.rs`, `crates/nl-codegen/src/native_generics.rs`). Un commentaire du code le reconnaît explicitement. Déjà noté dans CHANGELOG v0.9.0.

### `Stringable.toString()` non câblé dans la concaténation `+` et le cast `(string)`
L'interface se déclare et passe la vérification de const-correction (E044), mais `is_concat_operand` (`crates/nl-sema/src/checker.rs:2720`) et `Value::to_display_string` (`crates/nl-vm/src/value.rs:161`) ne gèrent que les primitives. Toute classe implémentant `Stringable` est **rejetée** (E008/E007) plutôt qu'acceptée avec appel implicite de `toString()`, contrairement à specs.md § String concatenation / § Cast to string.

---

## 2. Syntaxe / sémantique absente

- **`typedef`** — absent. Mot-clé lexé mais jamais parsé en déclaration ni utilisé comme alias. C'est l'unique tâche listée dans `Next.md`.
- **`switch`/`case`** — absent du parseur (`crates/nl-syntax/src/parser.rs` ne traite jamais `Keyword::Switch`). Seule l'expression `match(...)` existe. Confirmé par test manuel (`parse error ... expected expression, found Keyword(Switch)`).
- **`interface A extends B, C`** (héritage d'interfaces) — non parsé. `parse_interface_decl` va du nom directement à `{`. Corollaire runtime : `implements_interface` ne remonte pas transitivement les interfaces étendues.
- **`for (const auto item : collection)` explicite** — le `const` est consommé puis jeté ; `StmtKind::ForEach` n'a pas de champ `is_const`. Seuls les cas *implicites* (méthode `const`, paramètre `const`/`const ref`) sont vérifiés (E039) ; la forme explicite documentée n'est jamais appliquée.
- **`E030`** (identifiant = mot-clé réservé) — code absent de `crates/nl-sema/src/error.rs`. Seul diagnostic de la liste E001-E049/W001 manquant.
- **Validation de type pour `operator++`/`operator--`** — aucune vérification sémantique (pas d'E009) ; l'erreur ne surgit qu'au codegen sous forme d'erreur non structurée.
- **Résolution de surcharge par arité uniquement** (limitation de longue date, notée dans `journal/`) — sauf les opérateurs, qui matchent par type exact.
- **`++`/`--`** : forme postfixe uniquement, pas de valeur d'expression réelle.

## 3. VM / bytecode

- **Drapeaux `ABSTRACT`/`FINAL`** définis dans `crates/nl-bytecode/src/module.rs` mais jamais émis par `nl-codegen`, ni vérifiés par `nl-vm`. Les méthodes abstraites sont simplement omises du bytecode au lieu d'être codées `code_length = 0` comme le prescrit vm.md. La protection existe côté `nlc` (E032/E035/E036 au compile-time) mais pas comme filet de sécurité VM pour un `.nlm` généré autrement.
- **Garbage collector** = comptage de références (`Arc`), documenté et assumé dans le code, mais sans collecteur de cycles : un cycle d'objets n'est jamais réclamé et ses destructeurs ne s'exécutent jamais.
- **Dispatch virtuel** = recherche linéaire par nom+descripteur en remontant `extends`, pas de vtable précalculée comme le décrit vm.md § Method dispatch (comportement correct, juste pas l'implémentation documentée — impact perf non mesuré).
- **Traces de pile** : granularité par *statement* seulement (une closure à corps-expression n'a aucune entrée de ligne) ; pas de nom de méthode dans `ExecutionPoint`.
- **Target-typing des closures** (specs.md règle #5) non implémenté — pas de widening automatique.
- **Capture `auto` dans les closures** : seules les variables explicitement typées sont boxées ; une capture `auto` mutée reste par valeur.

## 4. Bibliothèque standard (stdlib.md)

### Namespaces entièrement absents
- **`system.text.json`** — `JsonValue` et toute la famille, `Json.parse/tryParse/stringify`, `JsonFormatException`. Aucune trace dans le code.
- **`system.db`** (+ `system.db.sqlite`, `system.db.mysql`) — `Connection`, `PreparedStatement`, `ResultSet`, `Row`, `ColumnType`, `SqlException`, drivers Sqlite/Mysql. Aucune dépendance driver dans les `Cargo.toml`.

### Écarts documentés dans le code
- **`system.io.File.glob`/`system.io.Grep`** — `mini_regex.rs` ne compile que des regex ; un pattern glob littéral (`"*.txt"`) ne matche pas comme un glob (`*` y est un quantificateur regex). Comportement volontaire et commenté, mais en décalage avec stdlib.md qui présente glob et regex comme équivalents.
- **`system.thread.Thread.join()/sleep()`** — déclarent `throws InterruptedException` pour le typage, mais aucun mécanisme d'interruption réel n'existe ; l'exception ne peut jamais être levée.
- **`system.ps.Process.list()`** — lit `/proc` directement, Linux uniquement.
- **`system.In.readLine`** — fonctionne mais n'est exercé par aucun test.

## 5. Optimisations (milestone 9 — optionnel, non commencé)

Aucune des optimisations listées dans `optimizations.md` n'est implémentée (constant folding/propagation, dead code elimination, devirtualization, inlining, tail call optimization, string literal concatenation, incremental compilation côté compilateur ; string interning, JIT, superinstructions, inline caching, GC tuning côté VM). Attendu : ce milestone est explicitement marqué optionnel et non commencé dans `journal/journal_01_initial_build.md`.

## 6. Divers mineurs (outillage, pas la spec elle-même)

- `crates/nl-test-runner/src/main.rs:10` a un chemin par défaut codé en dur vers `/data/projects/nlvm-specs/tests`, obsolète depuis la migration vers `nlvm-lang/nlvm-specs` (il faut le passer explicitement en argument).
- Aucune vérification de conformance d'interface au-delà d'E044 (const-correctness nom+arité) : les types de retour/paramètres d'une méthode d'interface ne sont pas comparés à son implémentation.
- `Self` en interface non testé pour l'appel via une variable de type interface (`Cloneable c = new Point(); c.clone()`).

---

## Étape suivante recommandée

Trois écarts de la section 1 (`static` runtime, `ValueEquatable` dans Map/List, `Stringable` dans la concaténation/cast) sont les plus prioritaires : ce sont des fonctionnalités qui **compilent et semblent marcher** mais se comportent silencieusement autrement que documenté — le risque pour un utilisateur du langage est bien plus élevé qu'une fonctionnalité simplement absente (qui, elle, échoue franchement à la compilation). Je recommande de traiter en premier le câblage de `Stringable`/`ValueEquatable` (les deux se ressemblent : "interface spéciale déclarée mais jamais consultée par le runtime/la stdlib générique", donc probablement un chantier commun côté `nl-vm`/`nl-codegen`), puis `GET_STATIC`/`SET_STATIC` qui est un trou de milestone 4/5 déjà censé être clos.

Ensuite, dans l'ordre de valeur/risque décroissant :
1. `typedef` — déjà la tâche unique de `Next.md`, donc alignée avec le plan existant.
2. `switch`/`case` — sucre syntaxique, `match` couvre déjà le besoin fonctionnel, donc moins urgent malgré sa visibilité dans les specs.
3. `system.db` et `system.text.json` — gros chantiers autonomes (nouvelles dépendances, nouveau binding natif complet) ; à traiter comme des mini-projets séparés une fois les fondations ci-dessus solidifiées.
4. Milestone 9 (optimisations) — à laisser pour la fin, conformément au plan des milestones lui-même.
