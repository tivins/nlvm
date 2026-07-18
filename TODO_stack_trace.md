# Chantier : stack trace info

Objectif : satisfaire compiler.md/vm.md/specs.md (Milestone 6 — Exceptions & closures) sur `Exception.stackTrace` et `StackOverflowException`. Actuellement rien n'est implémenté (cf. `crates/nl-syntax/src/prelude.rs:9-13`).

Références specs (nlvm-specs) :
* `docs/specs.md:2429-2439` — `Exception.stackTrace: ExecutionPoint[]`, `ExecutionPoint { line, file }`
* `docs/specs.md:2486-2490`, `docs/vm.md:696-705` — capture native par la VM pendant `super(...)` du constructeur `Exception` (pas de bypass `readonly`), frames du chaînage de constructeurs de l'exception elle-même exclues
* `docs/vm.md:286-297` — line-number table par méthode (`{start_pc, line}`), lignes à `0` si absente
* `docs/vm.md:311-324`, `docs/specs.md:2466` — dépassement de profondeur d'appel → `StackOverflowException`
* `docs/vm.md:688,1042,1073` — exception non attrapée sortant de `main` : message + trace sur stderr, exit code 1
* `review/security-audit.md:352-362` (SEC-17) — ne pas exposer les traces en prod (fuite de chemins internes) ; le design "capture dans le constructeur" est un choix de sécurité, à ne pas contourner

## Étapes

### 1. Peupler `line_table` en codegen — ~~FAIT~~
- [x] `crates/nl-codegen/src/lib.rs:305` — remplacer `line_table: Vec::new()` par la vraie table construite pendant l'émission des instructions
- [x] `crates/nl-codegen/src/expr.rs:756` — idem (méthode `invoke` synthétique des closures)
- [x] Vérifier le round-trip lecture/écriture déjà supporté par `crates/nl-bytecode/src/module.rs:71,178-179,296-313` (`LineTableEntry{start_pc,line}`)
- [x] Test : `crates/nl-codegen/src/lib.rs` (`mod tests::line_table_tracks_source_lines`) compile un petit programme et vérifie `start_pc` strictement croissant + lignes exactes

Implémentation : `Emitter` (`crates/nl-codegen/src/expr.rs`) porte désormais `line_table: Vec<LineTableEntry>` + `last_line: u32`, peuplés par `record_line(line)` appelée en tête de `compile_stmt` (`crates/nl-codegen/src/stmt.rs`). Granularité **statement** (l'AST ne porte de `line` qu'au niveau `Stmt`, pas `Expr` — donc un closure à corps expression `() => 42` n'a toujours aucune entrée, cf. limitation notée dans le code). Dédoublonnage sur changement de ligne uniquement, conforme à vm.md § Method descriptor ("entries sorted by ascending start_pc, each covering up to the next entry's start_pc"). 130 tests maison + 14/14 nlvm-specs toujours verts, aucune régression fmt/clippy (les warnings existants dans `expr.rs:1567+` sont préexistants, non liés à ce changement).

### 2. Pile de frames NL walkable dans l'interpréteur — ~~FAIT~~
- [x] Stratégie retenue : pile parallèle légère (`thread_local!`), pas une vraie VM-stack — suffit pour le tracing, et donne l'isolation par thread OS réel (`native::construct_thread`) gratuitement
- [x] Nouveau module `crates/nl-vm/src/call_stack.rs` : `push_frame(class_fqcn, method_name) -> FrameGuard` (RAII, pop au drop — couvre tous les chemins de sortie de `run_frame`, y compris `?`/erreur), `set_current_line(line_table, pc)` (résout la ligne courante via la `line_table` de l'étape 1, `partition_point`), `snapshot(skip)` (liste `(class_fqcn, method_name, line)` innermost-first, `skip` pour exclure les frames du chaînage de constructeurs d'exception — pas encore appelé, prévu étape 4)
- [x] Intégré dans `crates/nl-vm/src/interpreter.rs::run_frame` : guard poussé une fois en tête, `set_current_line` appelé à chaque itération de la boucle avant `exec_step`
- [x] Tests unitaires dans `call_stack.rs` (résolution ligne, push/pop/imbrication, `skip`)

Limitation assumée : `snapshot`/`skip` sont actuellement du code mort (`#[allow(dead_code)]`) — rien ne les appelle encore, ce sera fait à l'étape 4. 130 tests maison + 14/14 nlvm-specs toujours verts.

### 3. Garde de profondeur d'appel → `StackOverflowException`
- [ ] Ajouter un compteur de profondeur (ou détection de la profondeur de pile Rust) dans `crates/nl-vm/src/interpreter.rs`
- [ ] Choisir un seuil raisonnable et lever `StackOverflowException` (déjà déclarée dans la hiérarchie prelude, `crates/nl-syntax/src/prelude.rs:37`, mais jamais levée) plutôt que de crasher le process Rust
- [ ] Test : programme récursif infini → doit lever/afficher `StackOverflowException`, pas paniquer

### 4. Champ `stackTrace` + capture native au constructeur `Exception`
- [ ] Ajouter `ExecutionPoint { line: int, file: string }` et `Exception.stackTrace: ExecutionPoint[]` dans `crates/nl-syntax/src/prelude.rs`
- [ ] Capturer la pile (issue de l'étape 2) au moment de l'exécution du constructeur de base `Exception`, avant retour de `super(...)` — exclure les frames du chaînage de constructeurs de l'exception elle-même
- [ ] `crates/nl-vm/src/error.rs:9-19` (`VmError::Thrown(Value)`) : vérifier si la trace doit transiter par là ou si elle est déjà portée par l'objet exception NL construit
- [ ] Sortie sur exception non attrapée : message + trace sur stderr, exit code 1 (vm.md:688,1042,1073)

### 5. Tests
- [ ] Ajouter des fixtures maison (aucune fixture yaml nlvm-specs n'existe encore pour le format de trace)
- [ ] Vérifier lignes correctes (via étape 1), profondeur correcte (étape 2/3), exclusion des frames de constructeur d'exception (étape 4)

## Notes
- Ne pas faire de shim provisoire (pas de trace "vide mais présente à l'API") — soit c'est fait proprement (lignes réelles), soit ça reste absent du prelude comme aujourd'hui.
- Chantier à traiter dans l'ordre 1 → 2 → 3/4 en parallèle → 5, chaque étape dépendant de la précédente pour être testable de bout en bout.
