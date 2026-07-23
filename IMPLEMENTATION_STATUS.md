# État d'implémentation vs. nlvm-specs

Photographie de l'écart entre les spécifications (`nlvm-specs`, `SPECS_VERSION` = 0.8.47 côté ce dépôt) et l'implémentation Rust actuelle (nlvm v0.10.0). Établi par lecture croisée de `specs.md`, `compiler.md`, `vm.md`, `stdlib.md`, `optimizations.md`, `tests.md` contre `crates/nl-syntax`, `crates/nl-sema`, `crates/nl-codegen`, `crates/nl-bytecode`, `crates/nl-vm`, `crates/nl-test-runner`, `Next.md` et `journal/`.

Ne liste que les écarts. Tout ce qui n'apparaît pas ici a été vérifié conforme.

## Résumé

Les milestones 1 à 8 (lexer/parser, sémantique, bytecode, VM core, objets, exceptions/closures, stdlib, test runner) sont **globalement en place et fonctionnels** — 14/14 tests officiels des specs passent, 170/170 tests internes passent (157 + 4 ajoutés en Phase 1 + 9 ajoutés en Phase 2, un ou plusieurs par écart résolu). Le milestone 9 (optimisations) est entièrement optionnel et n'a pas été commencé, comme prévu par le plan des milestones.

Les trois écarts à fort impact précédemment identifiés (`static` runtime, `ValueEquatable` dans Map/List, `Stringable` dans la concaténation/cast) ont été traités en Phase 1. Cinq écarts de syntaxe/sémantique (`typedef`, `switch`/`case`, `interface ... extends ...`, `for (const ...)` explicite, E030) ont été traités en Phase 2 — voir les deux sections dédiées ci-dessous. Il reste plusieurs écarts de moindre impact (§ 2-6).

---

## Phase 1 — Écarts à fort impact : résolus

Les trois écarts qui **compilaient et semblaient marcher** mais se comportaient silencieusement autrement que documenté ont été câblés. 157/157 tests internes + 14/14 tests officiels des specs passent toujours après ces changements (aucune régression détectée). Quatre fixtures YAML ont été ajoutées à `tests/` (`phase10_0040` à `phase10_0070`, une par écart résolu plus une pour le bug d'initialiseur d'instance ci-dessous) — chacune vérifiée pour échouer contre le code d'avant cette passe (rétabli temporairement via `git stash`) avant d'être validée contre le code corrigé, pour confirmer qu'elle couvre bien le comportement changé et pas autre chose.

### Champs `static` sur classes non-enum
`GET_STATIC`/`SET_STATIC` sont maintenant implémentés. `nl_vm::Program` porte une table de stockage statique par classe (`HashMap<fqcn, HashMap<field_name, Value>>`), pré-remplie avec la valeur par défaut de chaque champ `static` au chargement (`Program::new`). Un champ déclaré avec un initialiseur (`public static int counter = 0;`) est assigné une seule fois, avant `main`, par une méthode synthétique `<clinit>` que `nl-codegen` génère automatiquement (nom réservé, jamais en collision avec du code utilisateur) et que `nl_vm::program::run_static_initializers` exécute pour chaque classe chargée, dans l'ordre de chargement (déterministe, mais pas d'initialisation paresseuse à la Java — documenté comme simplification, cohérent avec le reste de l'implémentation). L'accès `ClassName.field` (lecture et écriture) résout la classe *déclarante* du champ même quand on y accède via une sous-classe (`find_field_owner`, côté `nl-sema` et `nl-codegen`), pour que le stockage soit partagé correctement en cas d'héritage.

**Problème rencontré pendant le développement** : en creusant l'exécution des initialiseurs statiques, il est apparu que les initialiseurs de champs *d'instance* (`public int x = 42;`) n'étaient eux non plus **jamais appliqués** — un champ non-static avec initialiseur gardait silencieusement la valeur par défaut de son type après `new`, quel que soit ce qui était écrit dans le code source. Ce n'était pas l'un des trois écarts listés, mais un bug préexistant plus large découvert en cherchant où (et si) un mécanisme équivalent existait déjà pour les champs static — aucun des 157 tests internes ni des 14 tests officiels n'exerçait ce cas assez précisément pour le détecter (confirmé par un test manuel avant/après : `new Foo().getX()` retournait `0` au lieu de `42`).
**Solution** : `nl-codegen` désucre maintenant tout champ avec initialiseur (statique ou non) en une assignation ordinaire (`this.field = init;` / `ClassName.field = init;`), injectée soit au début de chaque `construct` (juste après un éventuel appel `super(...)`, jamais dupliquée dans un `construct` qui délègue via `this(...)`), soit dans le `<clinit>` synthétique. Corrigé dans le même changement puisque static et instance partagent exactement le même désucrage.

### `ValueEquatable` dans `system.Map`/`system.List`
`get`/`set`/`remove`/`has` (Map) et `contains` (List) appellent maintenant `valueEquals` (dispatch virtuel, via `nl_vm::native::equatable_equals`) quand le type de la clé/élément implémente `ValueEquatable`, au lieu de toujours retomber sur l'identité de référence. `valueHash` reste déclarable et appelable comme une méthode ordinaire mais n'est toujours pas réellement utilisé pour le hachage : `Map<K,V>` reste un stockage par tableaux parallèles à recherche O(n) (changement hors scope — refonte de la structure de données, pas juste du câblage d'interface).

**Problème rencontré** : les recherches par clé (`get`/`set`/`remove`/`has`) tenaient le verrou (`Mutex`) du tableau de clés pendant la comparaison d'égalité. Appeler `valueEquals` (du bytecode utilisateur, potentiellement ré-entrant) pendant que ce verrou est tenu risquait un deadlock si le code utilisateur touchait la même map.
**Solution** : les clés sont copiées (`clone()` du `Vec<Value>`, le verrou est relâché) avant toute comparaison d'égalité — même précaution que celle déjà documentée ailleurs dans `nl-vm` pour `SET_FIELD` (ne jamais tenir un verrou pendant un rappel dans du code utilisateur).

### `Stringable.toString()` dans la concaténation `+` et le cast `(string)`
`nl-sema` accepte maintenant un opérande de concaténation/cast dont le type statique est une classe implémentant `Stringable` (directement ou via héritage). Côté VM, l'opcode `TO_STRING` (partagé par `+`, `(string)`, et la normalisation `system.Out.print`/`println`) appelle `toString()` par dispatch virtuel quand l'objet implémente `Stringable`, au lieu de toujours utiliser la représentation `[object ClassName]`.

**Problème rencontré** : aucun — en creusant `nl-codegen`, l'émission de bytecode pour `+`/`(string)` était déjà entièrement générique (elle émet `TO_STRING` pour *tout* opérande qui n'est pas déjà une `string`, sans jamais distinguer les primitives des objets). Tout le déficit était donc concentré dans (a) la vérification de type `nl-sema`, qui rejetait catégoriquement tout type `Named`, et (b) l'opcode `TO_STRING` côté VM, qui ne savait produire que la représentation par défaut. Aucun changement de `nl-codegen` n'a été nécessaire — plus simple que prévu initialement.

---

## Phase 2 — Syntaxe / sémantique absente : résolu

Les cinq écarts listés dans l'ancienne § 2 comme "absents" (`typedef`, `switch`/`case`, `interface ... extends ...`, `for (const ...)` explicite, E030) ont été implémentés. 170/170 tests internes + 14/14 tests officiels des specs passent toujours après ces changements (aucune régression détectée). Neuf fixtures YAML ont été ajoutées à `tests/` (`phase11_0010` à `phase11_0090`) plus un test Rust unitaire (`nl_syntax::parser::tests::reserved_keyword_as_identifier_reports_e030`, pour le seul cas — le texte exact du message E030 — qu'un fixture YAML ne peut pas vérifier, puisque `expected_parse_error` ne teste qu'un booléen, pas le contenu du message).

### `typedef`
`typedef Type Name;` est parsé (`nl_syntax::parser`, un ou plusieurs par fichier, juste après les `use` et avant la classe/interface/enum unique du fichier — même contrainte "un seul type par fichier" que le reste du parseur) puis entièrement résolu par une nouvelle passe `nl_syntax::typedef::expand`, exécutée avant `nl_syntax::monomorphize::expand` (même discipline que cette dernière : `nl-sema` et `nl-codegen` appellent les deux passes dans le même ordre sur la même entrée, donc toujours d'accord sur le programme étendu). C'est un alias de compilation pur : chaque occurrence de `Name` dans l'AST (champs, paramètres, types de retour, variables locales, cast, tableaux, closures, arguments de `new T<...>(...)`) est remplacée par le type sous-jacent *complètement résolu* (FQCN, alias chaînés aplatis) avant que `nl-sema`/`nl-codegen` ne voient le fichier — ni l'un ni l'autre n'a besoin de savoir que `typedef` existe. Portée : un typedef est visible sans qualification dans tout fichier du **même namespace** (comme une classe non importée du même namespace), conformément à specs.md ("Typedefs are scoped to their namespace"). `new IntVector(0, 0, 0)` (specs.md § Typedef with templates, alias d'une instanciation de template) est explicitement supporté : ce cas particulier stocke le nom de classe comme `String` brute plutôt que comme `Type` dans l'AST (`Expr::New`), donc nécessite une réécriture dédiée en plus de la substitution générique de `Type`.

**Problème rencontré** : un fichier `.nl` ne contient qu'une seule classe/interface/enum (contrainte déjà existante et volontaire du parseur), donc un typedef déclaré dans un fichier doit rester utilisable, sans qualification, depuis un *autre* fichier du même namespace — mais la résolution des noms bruts à l'intérieur de la définition d'un typedef (ex. `typedef Vector<int> IntVector;`, où `Vector` doit se résoudre via les imports du fichier *déclarant*, pas du fichier *utilisateur*) ne peut pas être faite paresseusement au point d'usage sans risquer de résoudre `Vector` avec le mauvais contexte d'imports.
**Solution** : résolution en deux temps — (1) chaque typedef est d'abord résolu une fois vers une forme FQCN-qualifiée en utilisant la table d'imports de *son* fichier déclarant (`nl_syntax::monomorphize::import_map`, dont les fonctions ont été rendues `pub(crate)` pour être réutilisées ici sans dupliquer la logique de résolution de noms) ; (2) les alias chaînés (`typedef A B; typedef B C;`) sont ensuite aplatis à point fixe, avec un garde-fou anti-cycle ; (3) seulement à ce stade, chaque fichier utilisateur substitue les références par nom simple, qualifiées par *son propre* namespace. `Expr::New`'s nom de classe brut (`String`, pas `Type`) a nécessité une réécriture séparée de la substitution générique de `Type` — repéré en écrivant le test `phase11_0070_typedef_template_alias.yaml` d'après l'exemple `new IntVector(...)` de specs.md, qui échouait initialement avec une classe introuvable ("IntBox" au lieu du FQCN mangled "...Box<int>") avant ce correctif.

**Limite documentée** : `catch (Type name)` et `expr instanceof Type` stockent eux aussi un nom de classe en `String` brute (pas en `Type`) et ne sont donc pas réécrits par cette passe — aucun exemple de specs.md n'utilise un typedef dans l'un ou l'autre de ces deux positions.

### `switch`/`case`
Ajouté comme une véritable instruction (`StmtKind::Switch { subject, cases }`, distincte de l'expression `Expr::Match` déjà existante) avec sémantique de *fall-through* (compiler.md/specs.md § Switch/Match : sans `break`, l'exécution continue dans le `case` suivant). Compilé (`nl_codegen::stmt::compile_switch`) en une seule passe : le sujet est évalué une fois dans un local scratch (contrairement à `compile_match`, qui garde le sujet sur la pile d'opérandes via `Dup`/`Pop` — approche non réutilisable ici puisqu'un corps de `case` est fait d'instructions arbitraires, pas d'une simple expression), puis chaque `case` compare ce local à sa valeur et saute vers le début de son propre corps si égal ; les corps sont ensuite émis en séquence plate dans l'ordre source — retomber d'un corps dans le suivant (fall-through) est donc une propriété *physique* du layout, sans instruction supplémentaire. `default` peut apparaître n'importe où parmi les `case` (comme en C) : c'est la cible du "aucun case ne correspond", indépendamment de sa position syntaxique.

**Problème rencontré** : `break`/`continue` réutilisent la pile `Emitter::loops` déjà utilisée par les boucles (`LoopCtx`), mais un `switch` n'est pas une boucle — `continue` à l'intérieur d'un `switch` imbriqué dans une boucle doit cibler la boucle *englobante*, pas le `switch` lui-même (aucune notion d'itération à "continuer" dans un `switch`), alors que `break` doit lui cibler la construction la plus proche, `switch` ou boucle indifféremment.
**Solution** : `LoopCtx` porte un nouveau champ `is_switch: bool`. `break` continue de cibler inconditionnellement le sommet de la pile (`self.loops.last()`) ; `continue` ignore désormais les frames marquées `is_switch` et cherche la plus proche frame de boucle réelle. Testé explicitement (`phase11_0020_switch_break_vs_continue_in_loop.yaml`) : un `switch` dans un `while`, avec un `break` qui ne doit *pas* sortir de la boucle et un `continue` qui doit sauter le reste du corps de boucle.

### `interface A extends B, C` (héritage d'interfaces)
Parsé (`InterfaceDecl.extends: Vec<String>`, noms simples séparés par virgules, même règle que `extends`/`implements` de classe). Stocké côté `nl-sema`/`nl-codegen` dans le même champ `ClassInfo::implements` qu'utilisent déjà les classes pour leurs interfaces directement implémentées (une interface qui `extends` un parent "implémente" ce parent au même sens fonctionnel). `class_table::implements_interface` marche désormais transitivement à travers la chaîne `extends` d'interface à interface (avec garde-fou anti-diamant), donc `instanceof`/upcast fonctionnent pour tout ancêtre de la hiérarchie (compiler.md : "an implementing class can be upcast to any interface in the hierarchy"). Côté codegen, `class_table::interface_closure` aplatit toute la fermeture transitive dans la liste `Module.interfaces` compilée d'une classe, puisque `nl_vm::interpreter::is_instance_of`/`implements_interface` ne fait qu'un scan direct de cette liste par FQCN exact, sans connaissance de `extends` d'interface. E044 (const-correctness d'une méthode d'interface implémentée) parcourt maintenant aussi la fermeture transitive des interfaces étendues, pas seulement les méthodes déclarées directement sur l'interface implémentée.

**Problème rencontré** : en testant `Disposable d = f;` (assignation d'un `FileHandle implements Closeable extends Disposable` à une variable typée `Disposable`), la vérification échouait avec E004 alors que `implements_interface` avait bien été corrigé pour la transitivité — le bug n'était pas dans `implements_interface` lui-même mais dans `nl_sema::checker::is_object_assignable`, qui contenait sa **propre** vérification directe-seulement de `info.implements` (`info.implements.iter().any(|i| i == to)`) au lieu de réutiliser `class_table::implements_interface`. C'était un bug préexistant plus large que le périmètre de cette tâche : même sans `interface extends`, deux interfaces sans lien de parenté auraient pu masquer ce chemin de code, mais rien dans les 161 tests d'avant Phase 2 n'exerçait une assignation typée-interface assez précisément pour le révéler.
**Solution** : `is_object_assignable` appelle maintenant `class_table::implements_interface` (transitif) au lieu de sa propre vérification directe dupliquée — corrigé dans le même changement, puisque c'est exactement le même écart de transitivité que celui visé par cette tâche, seulement dans un second point d'appel.

### `for (const auto item : collection)` explicite
`StmtKind::ForEach` porte maintenant un champ `is_const: bool`, effectivement parsé (au lieu d'être consommé puis jeté) et branché dans `nl_sema::checker`'s E039 : un `const` explicite sur la variable de boucle la rend non-modifiable, en plus (et indépendamment) des cas déjà couverts où c'est la collection itérée elle-même qui est en lecture seule (`this.<field>` dans une méthode `const`, variable/paramètre `const`).

**Problème rencontré** : aucun — le mécanisme de detection (`readonly_loop_vars: HashSet<u32>`) existait déjà pour les cas implicites ; il ne manquait que la propagation du booléen depuis le parseur jusqu'au point où `is_readonly_collection` est calculé.

### `E030` (mot-clé réservé utilisé comme identifiant)
`nl_syntax::parser::eat_ident` — le point unique où toute position "nom de variable/méthode/champ/classe/paramètre" du grammaire est acceptée — produit maintenant le message documenté (`E030 — '%s' is a reserved keyword and cannot be used as an identifier`) quand le jeton rencontré est un mot-clé, plutôt que le message générique `expected identifier, found Keyword(...)`.

**Problème rencontré** : chaque mot-clé de specs.md § Keywords (y compris `undefined`) est déjà lexé comme un jeton `Keyword`, jamais comme un `Ident` — il est donc *déjà* structurellement impossible d'utiliser un mot-clé réservé comme identifiant, à n'importe quel point de la grammaire, sans passer par `eat_ident`. Cela veut dire que la condition d'E030 ne peut jamais survivre à l'analyse syntaxique pour atteindre `nl-sema` (où vivent tous les autres codes E0xx, vérifiés via `expected_compile_error` dans le format de test YAML, qui ne matche que sur les erreurs remontées par `nl_sema::check_compile_with_warnings`). E030 reste donc une `SyntaxError` (rejetée à l'analyse syntaxique), pas une variante de `SemaError` avec son propre code testable par `expected_compile_error` — c'est pourquoi la couverture de test pour ce point précis passe par un test Rust unitaire (vérifiant le texte exact du message) plutôt qu'un fixture YAML classique (qui ne peut vérifier que `expected_parse_error: true`, sans le contenu du message).
**Solution** : message reformulé directement dans `eat_ident`, sans changement architectural — documenté comme un choix délibéré plutôt qu'un écart restant.

---

## 2. Syntaxe / sémantique restante

- **Validation de type pour `operator++`/`operator--`** — aucune vérification sémantique (pas d'E009) ; l'erreur ne surgit qu'au codegen sous forme d'erreur non structurée. Non traité en Phase 2 : nécessite d'ajouter une vérification de type dédiée dans `nl-sema` pour un opérateur qui n'a pas d'opérande explicite à typer (contrairement à `operator+`/`operator-`/etc.), donc un chantier à part plutôt qu'un ajout mineur.
- **Résolution de surcharge par arité uniquement** (limitation de longue date, notée dans `journal/`) — sauf les opérateurs, qui matchent par type exact. Non traité en Phase 2 : refonte transversale de la résolution d'appel dans `nl-sema`/`nl-codegen`, pas un écart de syntaxe/sémantique isolé.
- **`++`/`--`** : forme postfixe uniquement, pas de valeur d'expression réelle (`Expr::PostIncr`/`PostDecr` n'ont pas de valeur exploitable dans une expression composée, ex. `x = y++;`). Non traité en Phase 2 : changerait la représentation AST et la pile d'évaluation de codegen pour ces deux nœuds, risque de régression plus large que le reste de cette phase.

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
- Aucune vérification de conformance d'interface au-delà d'E044 (const-correctness nom+arité, désormais transitive à travers `interface ... extends ...` — voir § Phase 2) : les types de retour/paramètres d'une méthode d'interface ne sont pas comparés à son implémentation, et rien ne vérifie qu'une classe implémente réellement toutes les méthodes requises (directes ou héritées) — silencieux à la compilation, échouerait seulement au dispatch virtuel à l'exécution si la méthode est absente.
- Fusion par diamant de déclarations héritées de plusieurs interfaces `extends`-ées (compiler.md § Interface inheritance : deux déclarations identiques en nom+paramètres mais avec des types de retour différents devraient être un E041) — non vérifié ; `interface extends` (§ Phase 2) rend la hiérarchie transitivement visible pour `instanceof`/E044, mais ce cas de conflit spécifique n'est pas détecté.
- `Self` en interface non testé pour l'appel via une variable de type interface (`Cloneable c = new Point(); c.clone()`).

---

## Étape suivante recommandée

Les trois écarts à fort impact (Phase 1) et les cinq écarts de syntaxe/sémantique (Phase 2) étant traités et couverts par des tests, l'ordre de valeur/risque décroissant pour la suite :
1. `system.db` et `system.text.json` — gros chantiers autonomes (nouvelles dépendances, nouveau binding natif complet) ; à traiter comme des mini-projets séparés une fois les fondations ci-dessus solidifiées.
2. Validation de type `operator++`/`operator--` (E009), résolution de surcharge au-delà de l'arité, valeur d'expression réelle pour `++`/`--` — les trois écarts restants de la § 2, chacun un chantier isolé plutôt qu'un ajout mineur (voir le paragraphe de chacun pour le raisonnement).
3. Milestone 9 (optimisations) — à laisser pour la fin, conformément au plan des milestones lui-même.

Écart additionnel découvert en marge de la Phase 1 (toujours non traité) : `system.Out.print`/`println` continue de rejeter tout argument objet, y compris une classe `Stringable` — specs.md § Stringable interface le liste pourtant comme troisième consommateur (avec `+` et `(string)`). L'opcode VM sous-jacent (`TO_STRING`) est déjà corrigé ; seule la vérification côté `nl-codegen::compile_stdlib_call` (`is_printlike`) reste à assouplir.
