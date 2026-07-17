# NLVM — Historique de construction initiale

Ce document retrace, phase par phase, la construction du compilateur `nlc`, de la VM `nlvm`, du runner de tests `nltest` et de la stdlib native `system.*` du langage NL (spécifié dans [`nlvm-specs`](https://github.com/tivins/nlvm-specs)).

Il consolide en un seul fil linéaire les documents de planification `PLAN.md`, `PLAN_phase5.md` et `PLAN_phase6.md`.

---

## Contexte et décisions initiales

Décisions actées avant tout code :

- **Langage d'implémentation : Rust** (workspace Cargo multi-crates).
- **Stratégie : tranche verticale** — faire passer un programme minimal de bout en bout (lexer → parser → bytecode → VM → exit code) en premier, avec le test runner YAML comme pilote dès le début, puis élargir milestone par milestone.

Référence roadmap officielle : `nlvm-specs/docs/milestones.md` (9 phases, lexer → optimisations).

### Architecture — workspace Cargo

```
nlvm/
├── Cargo.toml            # workspace
├── crates/
│   ├── nl-syntax/        # lexer + parser + AST (M1)
│   ├── nl-sema/          # résolution de noms, typage, les 49 checks (M2)
│   ├── nl-bytecode/      # format .nlm : encodage ET décodage partagés (M3 + loader VM)
│   ├── nl-codegen/       # AST validé → bytecode (M3)
│   ├── nl-vm/            # interpréteur, modèle objet, exceptions, GC, natives (M4–M7)
│   ├── nlc/              # binaire CLI compilateur
│   ├── nlvm/             # binaire CLI VM
│   └── nl-test-runner/   # runner YAML (M8) — binaire `nltest`
└── tests/                # tests YAML supplémentaires (mêmes conventions que les specs)
```

Point clé : `nl-bytecode` est partagé entre compilateur et VM — une seule définition du format `.nlm`, des opcodes et des descripteurs de types, donc pas de dérive entre les deux moitiés. Le runner vit dans le même workspace et lit directement `nlvm-specs/tests/` (chemin configurable).

### Points de vigilance transverses

- La monomorphisation des templates et le `readonly` runtime touchent à la fois le compilateur et la VM — à concevoir dans `nl-bytecode` dès qu'on y arrive, pas après coup.
- **GC : comptage de références** (tranché 2026-07-17). Le refcount d'`Arc` est le GC (voir Phase 7 § « Destructeurs appelés par le GC »). Limitation assumée : les cycles d'objets ne sont jamais collectés, donc leurs destructeurs ne tournent pas — un collecteur de cycles serait un ajout ultérieur (Phase 8).
- Les tests `m7_0030` (use-after-close) montrent que les specs incluent des exigences de sûreté runtime, pas seulement compile-time.

---

## Phase 0 — Bootstrap

Workspace Cargo, CI (`cargo test` + runner YAML), CLAUDE.md pointant vers les specs.

---

## Phase 1 — Tranche minimale de bout en bout ⭐ (phase critique)

Objectif : faire passer `m1_0010_parse_minimal` et `m4_0010_minimal_main` (un `main` qui retourne un exit code). Force à poser *tous* les squelettes : lexer/parser minimal, un semblant de sema, émission d'un module `.nlm` valide (magic, constant pool, SHA-256), loader + boucle d'interprétation avec frames, et surtout le **test runner v0** (front matter YAML, blocs `#NLFILE`, comparaison exit code/stdout). À partir de là, chaque avancée se mesure en tests qui passent.

---

## Phase 2 — VM primitive + expressions (M4)

Arithmétique int/float, comparaisons, contrôle de flux, strings et concaténation, `INVOKE_STATIC`. Cible : `m4_0020` et une batterie de tests maison.

Concrètement : variables locales typées/`auto`, `if`/`else if`/`else`, `while`, `for` C-style avec `break`/`continue`, affectations composées, `++`/`--`, appels de méthodes statiques y compris récursifs. 12 tests maison dans `tests/`, tous verts. L'essentiel du jeu d'opcodes M4 était déjà dans la VM, le travail restant portait sur `nl-syntax`/`nl-codegen`.

---

## Phase 3 — Sémantique noyau (M2 partiel)

Typage, résolution de noms, null safety (E003/E004), definite assignment, smart casts — les checks nécessaires aux programmes qu'on sait déjà exécuter. Scope volontairement restreint aux programmes déjà exécutables (méthodes statiques, une classe par fichier, pas encore d'objets/interfaces/exceptions/match).

### AST/parser

Types union `T1|T2|...|null` (`Type::Union`, `Type::NullT`), déclarations locales sans initialiseur (`int x;`) pour permettre l'analyse de definite assignment.

### `nl-sema`

Résolution de noms + scopes imbriqués avec shadowing, analyse de definite assignment flow-sensitive (E001, if/else/while/for selon compiler.md), null safety (E003/E004), `auto` sans initialiseur (E005), concaténation de chaînes (E008), compatibilité d'opérateurs (E009), méthodes dupliquées (E041), classes dupliquées (E042).

### `nl-codegen`

Les types nullable/union s'effacent en `ExprTy` du membre non-null (les valeurs sont taguées dynamiquement à l'exécution, cf. vm.md) ; `==`/`!=` non numériques (string/bool/null) compilés séparément de la comparaison numérique.

### Bilan Phase 3

9 nouveaux tests maison dans `tests/` (`phase3_00*`), un par code d'erreur + un test d'exécution avec union nullable de bout en bout. Tous verts (21/21 avec Phase 2).

`m2_0010`/`m2_0020` de `nlvm-specs/tests` passent désormais via de vrais checks (plus un stub). `m2_0030` (interfaces/E044), `m2_0040` (match/E047), `m2_0050` (try/catch/E048) et `m2_0060` (smart casts + instanceof) restent bloqués par des constructions syntaxiques pas encore supportées (interfaces, `match`, `try`/`catch`, `instanceof`, méthodes d'instance) — reportés aux phases 4-6 où ces constructions arrivent.

Le *type narrowing* (smart casts, compiler.md § Type narrowing) n'est pas implémenté : actuellement un `T|null` non explicitement re-typé reste `T|null`, donc son usage direct comme `T` (hors passage à un paramètre/retour lui-même nullable) est rejeté en E004 — à traiter avec `instanceof`/interfaces.

E030 (mot-clé réservé comme identifiant) déjà garanti par le lexer/parser (un mot-clé ne peut pas être tokenisé comme identifiant) — pas de check dédié nécessaire.

---

## Phase 4 — Objets, tableaux, dispatch (M5)

Scope volontairement restreint : pas d'héritage de classe, pas de `super`, pas de champs statiques accessibles, pas de closures/génériques/exceptions.

### AST/parser

`SourceFile` porte désormais un `SourceItem` (`Class` ou `Interface`) et la liste des `use` résolue ; `ClassDecl` gagne `fields`/`implements` ; nouveaux `FieldDecl`, `InterfaceDecl`/`MethodSig`, `MethodKind` (constructeurs `<construct>`/destructeurs `<destruct>` nommés directement au parsing) ; `LValue` (`Local`/`Field`/`Index`) remplace le `String` brut d'`Expr::Assign` ; nouveaux `Expr` (`This`, `New`, `NewArray`, `FieldAccess`, `MethodCall`, `Index`, `InstanceOf`) et `Stmt::ThisCall`.

Chaînage postfixe générique (`.`/`[]`) en plus de la précédence existante ; `instanceof` au niveau relationnel (specs.md § Operator precedence). Un mot-clé réservé peut apparaître comme segment de namespace/`use` (`test.class`, `test.instanceof` — requis par les fixtures officielles).

### `nl-sema`/`nl-codegen`

Chacun construit sa propre table de classes/interfaces inter-fichiers (FQCN → champs/constructeurs/méthodes/`implements`, résolution `use` → FQCN) — pas de dépendance partagée, cohérent avec la séparation existante. Résolution de surcharge *best-effort* par arité uniquement (suffisant pour le seul cas du scope : le chaînage de constructeurs). Nouveaux codes E045 (`this(...)` doit être la première instruction) et E046 (cycle de délégation) ; références croisées inconnues (classe/champ/méthode) laissées indulgentes, déférées à `nl-codegen` comme le reste des appels non résolus.

### `nl-vm`

`Program` (registre multi-modules `FQCN → Module`, remplace le `&Module` unique) ; `Value::Object` (champs par nom, pas par offset) ; implémentation de `NEW`/`INSTANCEOF`/`GET_FIELD`/`SET_FIELD`/`INVOKE_INSTANCE`/`INVOKE_SPECIAL`/`NEW_ARRAY`/`ARRAY_LOAD`/`ARRAY_STORE`/`ARRAY_LENGTH` (tous `Unsupported` avant cette phase). Dispatch virtuel = résolution par (classe *runtime* du receveur, nom+descripteur) — pas de vraie vtable, équivalent tant qu'il n'y a pas d'héritage de classe.

### CLI

`nlc`/`nlvm`/`nl-test-runner` recompilent/exécutent tout un programme (plusieurs fichiers liés) en un seul passage via `nl_codegen::compile_program`/`nl_vm::run_program(&[Module], …)` au lieu d'un fichier/module isolé.

### Bilan Phase 4

6 nouveaux tests maison (`phase4_00*`, tous verts, 27/27 avec les phases précédentes) : champs+constructeur+accès champ, constructeurs surchargés + `this(...)`, méthodes d'instance s'appelant via `this` avec état indépendant par objet, interface + `implements` + `instanceof`, tableaux (création/index/`length()`), `IndexOutOfBoundsException` non rattrapée.

`m5_0010_class_instantiation.yaml` (nlvm-specs) passe désormais. `m5_0020_instanceof.yaml` et `m5_0030_constructor_chaining.yaml` restent bloqués uniquement sur `system.Out.print` (stdlib, Phase 6) — confirmé par les messages d'erreur (plus aucune erreur de parsing/modèle objet).

Hors scope explicite (documenté dans le code, à traiter plus tard) : héritage de classe (`extends` entre classes)/`super`/vraies vtables, `abstract`/`final`, application runtime de `readonly`/`const`, énumérations, appel des destructeurs par le GC, tableaux multi-dimensionnels/liste d'initialisation/`map`/`filter`/`forEach`/`sort`/`find` (closures), `CHECKCAST`, champs statiques accessibles via `Classe.champ` (GET_STATIC/SET_STATIC non câblés côté codegen), initialiseurs de champs déclarés (valeur par défaut du type uniquement), résolution de surcharge précise (arité seule).

---

## Phase 5 — Exceptions + closures + génériques

### Prérequis : héritage de classe simple

Non prévu explicitement par le plan, mais requis par la hiérarchie `Exception` du spec.

- **AST/parser** : `ClassDecl.extends: Option<String>`, `Expr::Super`, `Stmt::SuperCall` (`super(...)`, même règle E045/E046 que `this(...)`), `super.method(...)` réutilise le chaînage postfixe générique.
- **`nl-sema`/`nl-codegen`** : `ClassInfo.extends`, `field_ty`/`method_return_ty`/`find_method`/`find_field` remontent la chaîne `extends` ; `is_object_assignable`/`is_subclass_or_same` transitifs. `nl-codegen` : `Module.super_class` câblé.
- **`nl-vm`** : `NEW` fusionne les champs de toute la chaîne d'ancêtres (`entry().or_insert_with` — un champ de sous-classe masque un champ homonyme hérité) ; `INVOKE_INSTANCE`/`INVOKE_SPECIAL` résolvent via `resolve_virtual` (remonte `super_class` si la méthode n'est pas déclarée exactement sur la classe visée) au lieu d'un lookup exact.

Hors scope : héritage multiple, `abstract`/`final`, vraies vtables (toujours résolution par nom+descripteur).

### Hiérarchie d'exceptions intégrée

`nl_syntax::prelude` construit directement en Rust (pas de source `.nl` à parser) les `SourceFile` de `Exception`/`RuntimeException`/`ArithmeticException`/`IndexOutOfBoundsException`/`NullPointerException`/`InvalidCastException`/`NumberFormatException`/`IllegalArgumentException`/`StackOverflowException`/`IOException`/`FileNotFoundException`/`FormatException`/`InterruptedException`, sans namespace (donc FQCN = nom simple, visible sans `use`).

`nl-sema::check_compile` et `nl-codegen::compile_program` préfixent systématiquement ces fichiers ; `import_map` (sema et codegen) les seede par nom identité. `nlc` écrit désormais tous les modules retournés par `compile_program` (plus seulement un par fichier source zippé — sinon désynchronisé par les modules du prélude en tête de liste).

Pas de capture de stack trace (`Exception.stackTrace` absent) ni de vérification statique des exceptions vérifiées (E016/E017, `throws` juste parsé/stocké en métadonnée bytecode `throws_types`, non appliqué à ce stade — traité plus loin dans la phase).

### `throw`/`try`/`catch`/`finally`

- **AST** : `Stmt::Throw`/`Stmt::Try`/`CatchClause`.
- **`nl-codegen`** : une entrée `ExceptionTableEntry` par `catch` (ordre de déclaration = ordre de spécificité, cohérent avec E048) plus, si `finally` est présent, une entrée catch-all (`catch_type = 0`) couvrant le corps du `try` *et* tous les `catch`, dont le handler stocke l'exception, exécute `finally`, puis la relance ; une seconde copie de `finally` est émise sur le chemin de sortie normale.
- **`nl-vm`** : `run_frame` délègue chaque instruction à `exec_step` (qui exécute une seule opcode) pour pouvoir intercepter `VmError::Thrown` entre deux instructions et consulter `method.exception_table` via `find_handler` (position du *pc* de l'instruction fautive) avant de soit reprendre à un handler soit laisser l'erreur se propager — la propagation inter-frames se fait "gratuitement" via `?` sur les appels récursifs `call_static`/`call_instance`, qui correspond exactement à l'algorithme du spec.
- Les exceptions implicites (division par zéro, déréférencement null, accès hors bornes, `throw null`) sont maintenant de vraies `Value::Object` (`throw_native`), attrapables comme n'importe quelle exception ; le message d'erreur non rattrapée conserve le format historique (`Unhandled exception: Classe: message`).

Lacune documentée à ce stade : `return`/`break`/`continue` à l'intérieur d'un `try`/`catch` sortent directement sans dupliquer `finally` — traité juste après (voir « `finally` dupliqué… »).

### `match` (specs.md § Switch/Match)

- **AST** : `Expr::Match`/`MatchArm` (motif `None` = `default`).
- **`nl-sema`** : E047 exhaustivité (bool sans `default` accepté seulement si `true`/`false` tous deux présents ; tout le reste exige `default`) et E047 doublon de motif littéral.
- **`nl-codegen`** : chaîne `DUP`+comparaison+branchement par bras, conforme au pseudo-bytecode de vm.md ; le dernier bras sert de secours implicite quand il n'y a pas de `default` explicite (exhaustivité déjà garantie par nl-sema).

### `finally` dupliqué sur `return`/`break`/`continue`

`Emitter::finally_stack` (dans `nl-codegen`) empile un clone du bloc `finally` en entrée de `compile_try` (avant le corps du `try` *et* les `catch`, cohérent avec le fait qu'un `catch` reste protégé par le `finally` englobant) et le dépile juste avant d'émettre le code du `finally` lui-même (pour qu'un `return` À L'INTÉRIEUR du `finally` ne se re-déclenche pas lui-même, mais déclenche toujours un `finally` *englobant* s'il y en a un).

`Stmt::Return` rejoue tout `finally_stack` (du plus interne au plus externe) ; `Stmt::Break`/`Continue` ne rejouent que les entrées poussées depuis le début de la boucle ciblée (`LoopCtx.finally_depth`, capturé à l'entrée de la boucle), pour ne pas re-déclencher un `finally` qui englobe la boucle entière sans que le `break`/`continue` en sorte réellement. Test dédié (`phase5_0100`) vérifiant l'ordre d'exécution exact sur un `return` et un `break` dans une boucle.

### Vérification statique des exceptions vérifiées — E015/E016/E017

`nl-sema` distingue désormais une exception *vérifiée* (checked) — tout ce qui dérive d'`Exception` sans dériver de `RuntimeException` — via `is_subclass_or_same(fqcn, "Exception") && !is_subclass_or_same(fqcn, "RuntimeException")`.

- **E015** : `MethodChecker` accumule `method_throws` (throws clause résolue de la méthode courante) et `catch_stack` (types capturés par chaque `try` actuellement ouvert, empilé/dépilé dans `check_try`) ; `require_handled(fqcn)` (appelé sur un `throw` direct, un appel à une méthode/constructeur qui déclare `throws`) vérifie que le type est soit capturé par un `catch` englobant soit couvert par le `throws` de la méthode courante, sinon E015.
- **E016/E017** : comparent, pour une méthode d'instance qui override une méthode exacte (nom + types de paramètres, `class_table::find_method_exact`, nouveau) de la classe parente : chaque exception vérifiée du parent doit être couverte par le sous-type déclaré côté enfant (E016 sinon) et chaque exception vérifiée déclarée côté enfant doit être couverte par le parent (E017 sinon) — les exceptions runtime sont exemptées des deux côtés, conformément au spec.

`class_table::{MethodInfo,ClassInfo}` (côté sema) gagnent `throws`/`is_static`/`ctors` (les constructeurs, jusqu'ici totalement absents de la table sema, étaient nécessaires pour vérifier les `throws` d'un constructeur appelé via `new`).

Deux tests maison existants (`phase5_0030`, `phase5_0090`) ont dû être corrigés : ils levaient une exception vérifiée sans la capturer ni la déclarer, ce qui était permissif avant cette phase mais est maintenant une véritable E015 (conforme au spec) — `throws MyException` ajouté à leurs signatures. 4 nouveaux tests maison (E015 positif/négatif, E016, E017).

### Closures / fonctions anonymes

Capture **par valeur uniquement** (pas de boxing, écart documenté ci-dessous vs. le spec).

#### Lexer/parseur

Nouveau token `Punct::FatArrow` (`=>`, absent jusqu'ici — `Punct::Arrow` existant est `->`, inutilisé par le parseur). `Expr::Closure { params, return_type, throws, body }` (`ClosureBody::Block`/`Expr`) ; `(` en position primaire tente d'abord `parse_closure` (avec retour en arrière sur `self.pos` si ça échoue) avant de retomber sur le groupement `(expr)` existant — seule façon de distinguer `(int a, int b) => ...` d'une expression parenthésée sans lookahead illimité.

Seul un type de retour *primitif* explicite immédiatement suivi de `{` est supporté (`(int a) => float { ... }`) ; un type de retour nommé (classe) après `=>` est ambigu avec le début d'un corps-expression (`(int a) => a` — `a` est-il un type en attente d'un corps, ou le corps lui-même ?) et n'est pas supporté. `const` sur un paramètre de closure est parsé et ignoré (comme partout ailleurs dans cette implémentation).

#### `nl-codegen`

Chaque closure génère une **classe synthétique** (`crate::closure`, un champ par variable capturée + une méthode `invoke`), exactement comme le prescrit vm.md. Analyse des variables libres (`closure::referenced_names`, parcours récursif de l'AST du corps) puis, pour chaque nom candidat, vérification qu'il résout bien comme variable locale de la méthode *englobante* au point de création de la closure (`Emitter::lookup_local`) — sinon ce n'est pas une capture (référence de classe, nom déclaré à l'intérieur du corps de la closure, etc.).

`Emitter` gagne `captured_fields`/`closures`/`closure_counter`/`closure_name_prefix`/`inferred_return_ty` ; `resolve_ident` (nouveau) essaie d'abord `self.scopes` (une déclaration interne masque toujours une capture de même nom — vraie sémantique lexicale) puis retombe sur `captured_fields` (`GET_FIELD`/`SET_FIELD` sur `this` plutôt que `LOAD`/`STORE` sur un slot). Le site de création émet `NEW` puis un `SET_FIELD` par capture (copie de la valeur *actuelle*, donc snapshot à la création — pas de `Box<T>` partagé).

`compile_call` détecte un appel `nom(args)` où `nom` résout comme variable locale/capturée de type `ExprTy::Closure` et émet `INVOKE_CLOSURE` au lieu d'un `INVOKE_STATIC` (avant de retomber sur la résolution `static_sigs` existante). `compile_file`/`compile_method`/`compile_program` renvoient désormais `Vec<Module>` (le module de la classe plus les modules synthétiques de ses closures, éventuellement imbriquées) au lieu d'un seul `Module`.

#### `nl-vm`

`INVOKE_CLOSURE` — une closure est un simple objet dont la classe synthétique a une méthode `invoke` ; dispatch identique à `INVOKE_INSTANCE`.

#### `nl-sema`

`Expr::Closure` vérifié comme un bloc imbriqué avec ses propres paramètres déclarés (la portée capture donc naturellement l'assignation définitive : une variable capturée doit être assignée *au moment de la création* de la closure). `return_ty`/`skip_return_check` de `MethodChecker` sont sauvegardés/remplacés/restaurés le temps de vérifier le corps (sinon un `return` dans la closure était vérifié contre le type de retour de la méthode *englobante*, bug révélé par le premier test écrit).

Ajout d'une règle de laxisme dans `check_assignable` : `Type::Void` côté valeur est maintenant un joker toujours assignable (une closure n'a pas de vrai type statique modélisé — pas de `Type::Function` cette phase — donc son type synthétique est `Type::Void`, comme plusieurs autres formes non modélisées de ce checker ; sans ce joker, `int result = add(5, 3);` déclenchait un faux E004).

#### Écart documenté vs. le spec

Capture **par valeur** (snapshot à la création), pas par référence/boxing — muter une variable capturée depuis la closure ne se propage pas vers l'extérieur, et une mutation externe après création n'est pas visible depuis la closure (testé explicitement, `phase5_0160`). `++`/`--` sur une variable capturée n'est pas supporté (erreur de compilation explicite plutôt qu'une mauvaise génération de code). Pas de type fonction (`(int,int) => int` en position de type, `typedef` de closures), donc pas de closures passées à des paramètres/retournées avec un type explicite ni de closures passées aux futures méthodes natives de tableaux. `Self`/`type` en sucre syntaxique non supportés (sans objet ici).

### Génériques / templates (monomorphisation)

**Classes template uniquement** (pas de méthodes template, pas de bornes de type appliquées, pas de sucre `Self`/`type`, pas d'opérateurs surchargés `operator+`).

#### AST

`ClassDecl.type_params: Vec<String>` ; `Type::Generic(String, Vec<Type>)` (référence `Vector<int>`) ; `Expr::New` gagne un champ `Vec<Type>` (arguments de type de `new Vector<int>(...)`).

#### Parseur

`template <type T [extends Bound], ...>` avant `class` (la borne est parsée et jetée — non appliquée). `<TypeArgs>` reconnu après un type nommé à la fois dans `parse_type_atom` (déclarations/champs/paramètres) et `parse_new_base_type` (`new Vector<int>(...)`) — sans ambiguïté dans ces positions puisque `<` n'y a pas d'autre sens possible.

La vraie ambiguïté est au niveau instruction : `Box<int> a = ...;` est indiscernable de la comparaison chaînée `Box < int > a` sans lookahead ; `looks_like_var_decl`/`looks_like_generic_type_decl` (nouveau) scrute en avant depuis le `<` en comptant la profondeur d'imbrication jusqu'au `>` correspondant, puis vérifie que ce qui suit est un identifiant.

#### `nl_syntax::monomorphize` (nouveau module, partagé)

Appelé à l'identique par `nl_sema::check_compile` et `nl_codegen::compile_program` avant même le prélude, pour qu'ils voient toujours exactement le même programme étendu.

Réécriture pure AST→AST en deux passes :
1. Parcourt tout le programme pour collecter chaque combinaison distincte `(classe template, arguments concrets résolus)` réellement utilisée (`Type::Generic` en position de champ/paramètre/retour/variable locale, ou `new Vector<int>(...)`).
2. Pour chacune, substitue chaque paramètre de type par son type concret dans tout le `ClassDecl` du template (champs, signatures, corps des méthodes) et synthétise un nouveau `SourceFile` nommé `"Vector<int>"` (même namespace que le template d'origine) — mangling identique à celui déjà utilisé par vm.md pour les templates natifs (`"system.List<int>"`).

Toute référence à la forme générique ailleurs dans le programme est réécrite vers ce nom mangled (`Type::Generic` → `Type::Named`, `Expr::New` avec arguments de type → `Expr::New` sans). Les `ClassDecl` template eux-mêmes sont retirés de la liste de fichiers (jamais compilés tels quels — incomplets sans substitution). Une fois `expand` passé, ni `nl-sema` ni `nl-codegen` n'ont besoin de savoir que les génériques existent : chaque classe monomorphisée est une classe ordinaire.

Conséquence de cette approche « tout en amont » : aucun changement nécessaire dans `nl-codegen`/`nl-vm` pour le mécanisme cœur (l'`Emitter` ne sait toujours pas ce qu'est un générique) — seuls les `match` exhaustifs sur `Type`/`Expr::New` dans `nl-sema`/`nl-codegen` ont dû gagner un bras pour `Type::Generic` (`unreachable!` défensif, puisque `expand` est censé l'avoir déjà éliminé partout).

### Bilan Phase 5

21 nouveaux tests maison au total sur cette phase (`phase5_00*`, tous verts, 47/47 avec les phases précédentes) : héritage + `super`, `try`/`catch` avec ordre spécifique-avant-général, exception personnalisée `extends Exception` avec champ additionnel, `finally` qui s'exécute même en cas de propagation puis rattrapage par un appelant, `match` sur `int` avec `default`, `match` sur `bool` exhaustif sans `default`, E047 (motif dupliqué), E048 (même type capturé deux fois), exception non rattrapée (message qualifié par FQCN), etc.

`m2_0040_compile_e047_match_not_exhaustive.yaml`, `m2_0050_compile_e048_unreachable_catch.yaml`, `m5_0030_constructor_chaining.yaml` (nlvm-specs) passent désormais (9/14, contre 6/14 avant le début de la Phase 5). `m2_0030` (E044, const-correctness), `m2_0060` (smart cast/`const` en position de type), `m7_0030` (stdlib fichiers) restent bloqués sur des lacunes sans rapport avec cette phase.

`m5_0020`/`m7_0040` progressent (ternaire et stdlib de base ne bloquent plus) mais restent rouges sur deux lacunes distinctes et plus profondes, découvertes en Phase 6 : `m5_0020` sur une question de nullabilité implicite des types référence/interface qui semble contredire compiler.md § Null safety tel qu'implémenté (pas corrigé faute de certitude sur la règle réelle) ; `m7_0040` sur le *type narrowing* (smart casts), toujours hors scope depuis la Phase 3.

Bug corrigé en passant (préexistant, révélé par un test de cette phase) : `nl-sema` ne résolvait pas les types `Named` des paramètres/retour d'une méthode statique dans la table `sigs` locale au fichier (comparait `"Counter"` non résolu à `"ns.Counter"` résolu → faux E004 dès qu'une méthode statique du même fichier prend un paramètre objet).

---

## Phase 6 — Stdlib

### Opérateur ternaire

Prérequis ajouté hors plan initial mais nécessaire à plusieurs fixtures `nlvm-specs` (`m5_0020`, `m7_0040`, `showcase.md`) : l'opérateur ternaire `cond ? then : else` (specs.md § Operator precedence, niveau 10). AST `Expr::Ternary` ; parseur inséré entre `parse_assignment` et `parse_or` (right-associatif, la branche `then` accepte une expression complète). `nl-sema` exige juste une condition `bool` et reste indulgent sur le type des deux branches (comme les bras de `match`) ; `nl-codegen` émet un branchement classique et coerce la branche `else` vers le type de la branche `then`. `??`/`?:` (elvis, niveau 11) ne sont pas implémentés.

### Classes natives `system.*` (base)

Les classes natives `system.*` (vm.md § Standard library binding) n'ont pas de source `.nl` ni de `Module` bytecode — la VM intercepte `INVOKE_STATIC` directement par nom de classe. Table de signatures dupliquée indépendamment dans `nl_sema::stdlib`/`nl_codegen::stdlib` (même pattern que `class_table`, volontairement non partagé).

`system.Out`/`system.Err.print`/`println` acceptent `int|float|bool|string` : côté sema via un type union (réutilise `is_assignable` sans cas spécial), côté codegen en normalisant l'argument avec l'opcode `TO_STRING` existant puis en appelant un unique overload natif `(string) -> void` — évite d'avoir à distinguer des overloads par type au niveau de la résolution de méthode (qui ne sait comparer que par arité, cf. limitation déjà documentée pour les constructeurs). `system.In.readLine` lit vraiment `stdin` (non exercé par un test pour l'instant). `system.Int`/`Float`/`Bool` : `parse` (lève `NumberFormatException`/`IllegalArgumentException`), `tryParse` (retourne `null`), `toString`.

#### Détection d'un appel `system.X.method(...)`

Un nouveau helper `dotted_path` (dupliqué dans `nl-sema::checker` et `nl-codegen::expr`) reconstruit le chemin pointé (`"system.Out"`) à partir de la chaîne `Ident`/`FieldAccess` produite par le parseur (qui ne distingue pas syntaxiquement un accès de champ d'un chemin de classe qualifié) ; si le premier segment n'est pas une variable locale connue, le chemin est cherché dans la table stdlib avant de retomber sur la résolution habituelle.

#### Sortie standard réellement branchée

`nl_vm::Program` porte désormais `stdout`/`stderr` (`RefCell<String>`, accumulés par les natifs `system.Out`/`system.Err` puis renvoyés dans `RunOutcome`) — auparavant toujours vides.

#### Bug de portée corrigé en passant

Révélé par `m5_0020`, indépendant du reste de cette phase : `import_map` (dans `nl-sema` et `nl-codegen`) ne rendait visible, sans `use`, que les classes du prélude d'exceptions — une classe/interface du **même namespace** nécessitait quand même un `use` explicite, ce que documentaient (à tort) les commentaires du code. Or specs.md § Imports indique qu'un import peut entrer en conflit avec "another type in the same namespace", ce qui n'a de sens que si ce type est déjà visible sans import — confirmé par la fixture `m5_0020` (`Dog implements Animal` sans `use` dans le même namespace `test.instanceof`). `import_map` prend maintenant `all_files: &[SourceFile]` en plus et seed toute classe/interface partageant le même `namespace`.

### `system.String`

Méthodes d'instance sur `string` (`text.trim()`) et leurs équivalents statiques documentés comme équivalents (`system.String.trim(text)`) : `length`, `charAt`, `substring(start)`/`substring(start,end)`, `indexOf(s)`/`indexOf(s,fromIndex)`, `contains`, `toUpperCase`, `toLowerCase`, `replace`, `startsWith`, `endsWith`, `trim`, `split`.

- **`nl-codegen`/`nl-sema`** : les deux formes (instance et statique) compilent vers exactement le même `INVOKE_STATIC system.String.<name>`, avec la valeur receveuse comme premier argument implicite — une seule entrée de table par méthode, indexée par l'arité *totale* (receveur inclus), partagée par les deux sites d'appel. Pas de vraie classe/vtable pour `string` : ce n'est pas un `Value::Object`, donc pas de dispatch virtuel, juste un alias vers l'unique native `system.String`.
- **`nl-vm`** : `native::dispatch` récupère les arguments par position. Comptage/indexation en **caractères** (`chars().collect::<Vec<char>>()`), pas en octets, conformément à specs.md ("A character is represented as a string of length 1"). `charAt`/`substring` hors bornes lèvent une vraie `IndexOutOfBoundsException`. `split` construit un `Value::Array` de `Value::Str` directement, sans passer par l'opcode `NEW_ARRAY`.

### `system.List<T>` / `system.Map<K,V>`

Contrairement à tout ce qui précède dans cette phase, ce sont de vrais objets tas créés via `new`, pas des classes utilitaires statiques.

#### Parseur

`new system.List<int>(...)`/`new system.Map<K,V>(...)` nécessitaient un nom de type à points après `new`, jusqu'ici non supporté (`parse_new_base_type` ne lisait qu'un seul identifiant). Ajout d'une boucle `while . ident` gloutonne — sans ambiguïté à cette position précise. `parse_type_atom` n'a pas été étendu de la même façon — hors scope, les tests utilisent `auto`.

#### `nl_syntax::monomorphize`

Les deux classes sont reconnues (`is_native_generic`) et mangleés exactement comme un template utilisateur (`"system.List<int>"`, `"system.Map<string, int>"`) par les passes `rewrite_type`/`rewrite_expr`, mais **sans** synthèse de `ClassDecl` — `nl_vm::native` fournit l'implémentation directement, câblée sur le nom mangled. Conséquence : `expand` ne peut plus s'arrêter tôt quand `templates.is_empty()`.

#### `nl_sema::native_generics`/`nl_codegen::native_generics`

Nouveaux modules, même duplication volontaire que `stdlib.rs`. Puisque `nl_syntax::monomorphize` a déjà mangled la référence en FQCN concret avant que sema/codegen ne la voient, ces modules **reparsent** les arguments de type concrets directement depuis le nom mangled (split top-level *sensible à la profondeur* des `<...>` imbriqués, puis segment → `Type` primitif/tableau/`Named`) plutôt que de les faire transiter séparément — c'est la seule information disponible à chaque site d'appel.

#### `nl_vm::native`

Un `List<T>` est un `Value::Object` ordinaire avec un seul champ `"__data__"` (`Value::Array`) ; un `Map<K,V>` a deux champs parallèles `"__keys__"`/`"__values__"` (même index = une entrée) — préféré à une vraie table de hachage car l'égalité de clé suit `values_equal` (primitifs/`string` par valeur, sinon identité de référence — `ValueEquatable` non implémenté), qui n'est pas compatible `Hash`, et les tailles de programmes de test sont petites (lookup O(n) sans conséquence). `keys()`/`values()` retournent une **copie** (nouveau `Rc`), jamais la vue interne.

`interpreter::exec_step` intercepte `NEW`, `INVOKE_SPECIAL` sur `<construct>` (seul `List<T>(T[] initial)` fait quelque chose), et `INVOKE_INSTANCE` (`dispatch_instance`, indexé par la classe *runtime* du receveur). `values_equal` (jusqu'ici privée à `interpreter.rs`, utilisée par `CMP_EQ`) est passée `pub(crate)` pour être réutilisée telle quelle.

Hors scope explicite : `entries()`/`forEach` sur `Map` (traités plus loin dans la phase), la boucle for-each sur `List`/`Map` (traité plus loin), `ValueEquatable`, générique natif imbriqué dans un autre générique.

### `system.io.*`

`File` (statique : `exists`/`open` 1-arg/`readAllText`/`writeAllText`), `FileHandle` (instance : `close`/`read`/`readLine`/`write(string)`/`write(byte[],off,len)`/`flush`), `Directory` (`list`/`create`/`remove`/`exists`), `Path` (`join`/`dirname`/`basename`/`extension`/`normalize`).

#### Prérequis parseur

Noms qualifiés à points partout où `.` n'a pas d'autre sens — position de type (`parse_type_atom`, donc aussi `system.io.FileHandle h;` via `looks_like_var_decl` qui saute le préfixe pointé), clause `catch`, clause `throws` — factorisé en `parse_dotted_name`. `Dot` accepté dans le lookahead d'arguments génériques.

#### Résolution des exceptions qualifiées

Le prélude déclare `IOException`/`FileNotFoundException` sans namespace, mais stdlib.md/les fixtures les référencent comme `system.io.IOException` — `nl_syntax::prelude::NAMESPACED_ALIASES` (consommé par les deux `import_map`) fait résoudre les deux orthographes vers la même classe.

#### Tables de signatures

Mêmes tables dupliquées sema/codegen que le reste du stdlib, plus deux nouveautés — `throws()` côté sema (les méthodes fichier déclarent des exceptions *vérifiées*, branchées sur `require_handled` : appeler `File.writeAllText` sans `catch`/`throws` est un vrai E015 désormais) et `instance_lookup`/`instance_signature` pour `system.io.FileHandle`, **première classe native à instances non générique**.

#### `nl-vm`

L'objet handle ne porte qu'un index `"__fd__"` dans `Program::file_handles` (`RefCell<Vec<Option<std::fs::File>>>`) ; `close()` vide le slot (idempotent, conforme stdlib.md), toute opération ultérieure lève `IOException` — c'est exactement le scénario CWE-416 de `m7_0030`. `read`/`write(byte[],off,len)` font le bounds-check du spec (avant toute I/O, insensible à l'overflow via `checked_add`). `readLine` lit octet par octet (pas de `BufReader` : la position OS du handle doit rester exacte pour les `read`/`write` entrelacés). `open` 1-arg = mode `ReadWrite` du spec. `Path.normalize` est purement lexical. `Directory.list` trie les entrées (ordre `read_dir` non déterministe sinon).

Limites documentées : pas de `FileMode` (les enums n'existent pas — seul `open` 1-arg à ce stade, `FileMode` ajouté plus loin dans la phase), pas de `glob` (ajouté plus loin), pas de destructeur appelant `close()`.

`m7_0030_read_after_close_cwe416.yaml` passe désormais.

### Boucle for-each

`for ([const] (auto|Type) item : collection)` sur les tableaux `T[]` et `system.List<T>`.

- **AST** : `Stmt::ForEach { ty: Option<Type>, var, iterable, body }` (`const` parsé et jeté).
- **Parseur** : essai spéculatif avec retour en arrière depuis `parse_for_stmt` — seul le `:` après la variable distingue le for-each d'un en-tête C-style.
- **`nl-sema`** : type de l'élément déduit du type de la collection (`T[]` → `T`, `system.List<T>` → retour de `get(int)`) ; type explicite vérifié assignable depuis l'élément ; variable de boucle déclarée dans une portée propre.
- **`nl-codegen`** : désucrage en boucle indexée strictement conforme au pseudo-bytecode de vm.md — deux locaux synthétiques, `ARRAY_LENGTH`/`ARRAY_LOAD` pour les tableaux, `INVOKE_INSTANCE size()`/`get(i)` pour `List` ; `break`/`continue` via le `LoopCtx` existant.

Hors scope à ce stade : itération de `system.Map<K,V>` (traité juste après).

### `system.MapEntry<K,V>` + `Map.entries()` + for-each sur `Map`

`MapEntry` est un troisième générique natif, jamais construit par l'utilisateur — `entries()` retourne côté VM un tableau d'objets à deux champs publics `key`/`value` (ordre = celui de `keys()`, « consistent » comme exigé). Les accès `entry.key`/`entry.value` passent par un nouveau `native_generics::field_ty` consulté avant la table de classes. Le for-each sur une map appelle `entries()` en tête (conforme au pseudo-bytecode vm.md) puis retombe exactement sur le chemin tableau existant de `compile_foreach` ; `foreach_element_ty` (sema) choisit `T` pour `List<T>` et `MapEntry<K,V>` pour `Map<K,V>`.

### `system.Random` / `system.SecureRandom` / `system.Uuid`

`Random` est le premier — et jusqu'ici seul — cas d'une classe native *instanciable directement* par l'utilisateur (`new system.Random()`/`new system.Random(int seed)`), à la différence de `system.io.FileHandle` (produit uniquement par `File.open`) ; `SecureRandom`/`Uuid` restent des classes utilitaires 100% statiques.

- **`nl-codegen::stdlib`** gagne `ctor_param_types(fqcn, argc)`, consultée par `Emitter::compile_new` juste après `native_generics::ctor_param_types`.
- **`nl_vm::native`** : un `Random` est un `Value::Object` à un seul champ `"__state__"` (état 64 bits du PRNG). Générateur : SplitMix64 ; la graine par défaut mélange l'horloge système et un compteur atomique process-wide. `nextInt(bound)` utilise un simple modulo. `bound <= 0` lève `IllegalArgumentException`.
- **`SecureRandom`/`Uuid`** : source d'entropie `/dev/urandom` (pas de dépendance externe type `getrandom`/`rand`, le workspace n'en a aucune, et le spec cite `/dev/urandom` comme exemple ; portabilité Windows non traitée). `nextInt(bound)` fait du rejection sampling. `Uuid.random()` tire 16 octets, positionne les nibbles de version (`0100`) et de variant (`10xx`) selon RFC 4122.

#### Lacune corrigée en passant — comparaisons/arithmétique sur `byte`

Les opérateurs de comparaison (`==`/`!=`/`<`/…) sur des valeurs `byte` échouaient en codegen (`promote_numeric` ne gérait que les paires `(Int,Int)`/`(Float,Float)`/mixte Int-Float — un couple `(Byte,Byte)` ou `(Byte,Int)` tombait dans le bras d'erreur générique).

Correction : `nl-codegen::expr::promote_numeric` gagne les 5 combinaisons manquantes (`(Byte,Byte)`, `(Byte,Int)`/`(Int,Byte)`, `(Byte,Float)`/`(Float,Byte)`), toutes ramenées à `Int`/`Float` via `B2I` (et `I2F` en cascade). Bug connexe : `byte + byte` était typé `byte` par `nl-sema::checker::check_numeric_or_eq` alors que la VM produit toujours un `int` faute d'opcode `byte` — corrigé en spécialisant uniquement le bras `Add|Sub|Mul|Div|Mod`.

### `system.io.FileMode` + `File.open` 2-arg + `File.glob`

#### `FileMode`

`Read`/`Write`/`Append`/`ReadWrite`/`ReadWriteTruncate`/`ReadWriteAppend` — pas un vrai enum utilisateur (toujours hors scope) : modélisé comme des constantes entières sur une fausse "classe" stdlib, résolues à un accès de champ pointé (`system.io.FileMode.Read`) exactement comme `system.Out.print(...)` est reconnu à un appel de méthode pointé. `nl_sema::stdlib::enum_const_ty`/`FILE_MODES` (type `Type::Named("system.io.FileMode")`, qui passe `check_assignable` gratuitement) et `nl_codegen::stdlib::enum_const_value` (même liste, la position = le tag entier).

`File.open` gagne une 2e entrée de signature `(string, FileMode) -> FileHandle`. `nl_vm::native` construit désormais un `std::fs::OpenOptions` d'après le tag entier (1-argument = toujours `ReadWrite`, comportement inchangé).

#### `File.glob(basePath, pattern)`

Le pattern est traité comme une **regex** (pas un glob `*`/`**`) — le seul exemple concret de stdlib.md est lui-même une regex, malgré la description "glob or regex" ; implémenter les deux moteurs aurait été disproportionné pour une fonctionnalité sans fixture officielle.

Nouveau module `nl_vm::mini_regex` (aucune dépendance externe, cohérent avec le choix déjà fait pour `system.SecureRandom`/`Uuid`) : parseur récursif-descendant + matcher par backtracking avec continuations, supportant littéraux/`.`/`*`/`+`/`?`/classes de caractères/groupes/alternance/`\d`/`\w`/`\s`, mais ni répétition comptée `{m,n}` ni ancres (le matching est toujours pleine-chaîne à ce stade — les ancres et groupes capturants sont ajoutés plus tard pour `system.text.Regex`).

`collect_glob_matches` parcourt récursivement `basePath`, ne matche que les fichiers, sur leur chemin relatif à `basePath` (toujours `/`, même sous Windows), et renvoie le chemin complet trié.

### `system.net.*` — TcpListener/TcpStream/UdpSocket/Http

Première dépendance externe non-stdlib du workspace (`rustls`/`webpki-roots`, ajoutées après discussion avec l'utilisateur : le spec exige une validation TLS réelle du certificat pour `https://`, jugée trop risquée à réimplémenter à la main comme `mini_regex`/le CSPRNG de `SecureRandom`).

#### Sockets TCP/UDP

`TcpListener`/`UdpSocket` sont instanciables directement (`new system.net.TcpListener(host, port)`/`new system.net.UdpSocket()`) : même interception `NEW`/`INVOKE_SPECIAL <construct>` que `system.Random`. `TcpStream`, à l'inverse, n'est jamais construit par `new` — seulement via le `static TcpStream.connect(...)` ou `TcpListener.accept()`, qui construisent l'objet directement (comme `File.open` construit un `FileHandle`).

Les trois classes stockent leur état hors de l'objet (un index `"__fd__"` dans une nouvelle table `Program::{tcp_listeners,tcp_streams,udp_sockets}`, même mécanisme que `file_handles`) puisqu'un vrai `std::net::TcpListener`/`TcpStream`/`UdpSocket` ne peut pas vivre dans un champ de `Value::Object`. `UdpSocket::construct()` (sans arguments) lie quand même immédiatement un vrai socket OS à un port éphémère ; `bind(host, port)` ne peut pas rebinder en place, donc remplace le socket de la table par un nouveau.

Bornes `read`/`write` sur `TcpStream` : même règle et même code que `system.io.FileHandle`. `UdpSocket.receive` n'a pas d'offset/length et la troncature d'un datagramme plus grand que le buffer est gérée gratuitement par l'OS.

#### Bug découvert et corrigé pendant cette tâche

La première version de `dispatch_native_instance` faisait `match obj.borrow().class_name.as_str() { "system.Random" => return dispatch_random(...), ... }` — un scrutinee de `match` prolonge la durée de vie du temporaire `Ref` jusqu'à la fin de tout le `match` (contrairement à un simple `if`), donc le `obj.borrow_mut()` fait par `dispatch_random` plus loin dans l'appel paniquait ("RefCell already borrowed"). Corrigé en clonant `class_name` en `String` possédée avant le `match`.

#### `system.net.Http.get`/`post`

Nouveau module `nl_vm::net_http` : parseur d'URL minimal (`scheme://host[:port]/path`, pas d'IPv6 entre crochets ni d'userinfo/query) ; requête HTTP/1.1 brute avec `Connection: close` puis lecture jusqu'à EOF ; dé-chunking minimal de secours si le serveur envoie quand même `Transfer-Encoding: chunked`.

`http://` passe par un `std::net::TcpStream` brut ; `https://` enveloppe le même flux dans un `rustls::StreamOwned<ClientConnection, TcpStream>` avec le magasin de racines Mozilla embarqué (`webpki-roots::TLS_SERVER_ROOTS`, choisi plutôt que `rustls-native-certs` pour rester indépendant du trust store de l'OS) — aucune option pour désactiver la validation (conforme au MUST de stdlib.md), un échec de handshake/certificat remonte comme `IOException`.

`system.net.HttpResponse` (`statusCode`/`body`/`headers`) est un type de résultat natif non générique construit directement (`Value::Object`), sur le modèle de `system.MapEntry<K,V>` mais sans nom mangled à parser : nouvelle table `stdlib::result_field_ty` (dupliquée nl-sema/nl-codegen), consultée dans `Expr::FieldAccess` juste après `native_generics::field_ty`.

#### Tests

Les scénarios TCP/UDP sont testables en YAML pur malgré la VM mono-thread : `connect()` réussit dès que le handshake TCP est mis en file d'attente par l'OS (`backlog`), donc un programme NL séquentiel peut faire `connect()` puis `accept()` puis lire/écrire des deux côtés sans jamais avoir besoin de deux threads réels. **`system.net.Http.get`/`post` n'a en revanche aucun test YAML** (nécessiterait un vrai serveur concurrent) — résolu à la place par des tests Rust `#[test]` dans `nl_vm::net_http` qui démarrent un vrai thread OS côté serveur, plus un test `#[ignore]` qui vérifie une vraie poignée de main TLS contre `https://example.com/`.

Hors scope explicite : TLS côté serveur, TLS custom/pinning pour `TcpStream`, IPv6 littéral, trailers HTTP.

### `system.thread.Thread` / `Mutex` / `Semaphore`

La seule fonctionnalité de Phase 6 qui touche autre chose que `nl-vm` de façon *architecturale* : vm.md exige un vrai parallélisme, or toute la VM reposait jusqu'ici sur `Rc<RefCell<..>>` (`Value::Str/Array/Object`, tables internes de `Program`), qui ne sont ni `Send` ni `Sync`. Décidé avec l'utilisateur : vrais threads OS plutôt qu'une simulation mono-thread, quitte à payer une refonte plus large.

#### Refonte `Rc`/`RefCell` → `Arc`/`Mutex` (préalable)

Aucun changement de comportement observable. `crate::value::Value::Str(Arc<String>)`/`Array(Arc<Mutex<Vec<Value>>>)`/`Object(Arc<Mutex<Object>>)` ; nouvelle fonction `value::lock(&Mutex<T>) -> MutexGuard<T>` qui récupère un mutex empoisonné (`unwrap_or_else(|e| e.into_inner())`) plutôt que de paniquer — un thread qui panique sur un bug interne ne doit pas faire paniquer en cascade tous les autres threads.

`Program` : `stdout`/`stderr`/`file_handles`/`tcp_*` passent de `RefCell` à `Mutex` ; toute la chaîne d'appel (`call_static`/`call_instance`/`run_frame`/`exec_step`, et les fonctions de `nl_vm::native`) prend désormais `&Arc<Program>` au lieu de `&Program`, pour qu'un `Thread.start()` puisse cloner l'`Arc` et le déplacer dans la fermeture `'static` de `std::thread::spawn`. Migration mécanique (~150 sites) mais entièrement vérifiée par le compilateur.

#### `Thread(() => void task)`

Premier site d'appel de toute cette implémentation où une closure est passée en argument (jusqu'ici les closures n'étaient qu'assignées puis invoquées directement) ; `Emitter::coerce_value` gagne un bras dédié acceptant `ExprTy::Closure`.

`construct_thread` stocke la closure dans un champ `"__task__"` ; le thread OS n'est réellement lancé qu'à `start()`. `start()` clone l'`Arc<Program>`, déplace la closure dans `std::thread::spawn`, et appelle `invoke_task` qui résout la méthode synthétique `invoke` de la classe de la closure par nom seul puis délègue à `interpreter::call_instance`. Une exception non rattrapée est formatée exactement comme par `run_program` pour le thread principal et écrite dans `Program::stderr`.

#### `join()`/`join(int)`/`isAlive()`

`Program::threads: Mutex<Vec<Option<JoinHandle<()>>>>`. `join()` fait un `Option::take` sur le slot (idempotent : rejoindre un thread déjà joint est un no-op). `join(int timeoutMillis)` : `std::thread::JoinHandle` n'a pas de join borné dans le temps, donc c'est un polling de `JoinHandle::is_finished()` par pas de 1 ms — suffisant pour les tests, mais occupe activement un cœur pendant l'attente. `InterruptedException` est déclarée par `join`/`join(int)`/`Thread.sleep` mais rien dans cette implémentation ne la lève jamais (gap documenté).

#### `Mutex`/`Semaphore`

Backées par le même type interne `Program::Counter` (`Mutex<i64>` + `Condvar`, pas un `MutexGuard` conservé entre `lock()`/`unlock()` — un guard ne peut pas survivre à un seul appel natif, alors que le verrou *logique* doit rester posé across un nombre arbitraire d'autres appels natifs). Implémentation manuelle classique verrou-par-condvar. `Mutex` = un `Counter` plafonné à 1 ; `Semaphore` = le même compteur sans plafond, initialisé à `initialCount` (`IllegalArgumentException` si négatif).

Hors scope explicite : vraie interruption de thread, attente non-bornée efficiente pour `join(int)`, threads "non-daemon".

### `system.ps.Process`

`Process` (classe 100% statique, comme `system.io.File`/`Path`) plus deux types de résultat natifs non génériques (`ProcessInfo`, `ProcessResult`).

`run(string[] args)`/`run(string command)` partagent la même arité (1) — la seule paire d'overloads de tout le stdlib où deux formes distinctes (pas une simple union de primitifs comme `print`) collisionnent sur la clé `(fqcn, name, argc)`. Côté `nl-sema`, un type paramètre `Union(string, string[])` suffit tel quel. Côté `nl-codegen`, `Emitter::compile_stdlib_call` court-circuite ce seul appel avant la table générique, en inspectant l'`ExprTy` réellement compilé ; `nl_vm::native::dispatch` fait le même aiguillage à l'exécution en filtrant sur la variante de `Value`.

`list()`/`list(pid)` lisent `/proc` directement (portabilité Linux uniquement assumée) : `command`/`args` viennent de `/proc/<pid>/cmdline` ; `user` vient du premier champ `Uid:` de `/proc/<pid>/status`, résolu en nom via une lecture de `/etc/passwd` maison plutôt qu'une dépendance externe (`libc::getpwuid`).

`exit(int code)` — stdlib.md le documente comme un « terminal statement ». Un vrai `std::process::exit` aurait tué le process hôte tout entier, pas seulement le programme NL en cours (`nl-test-runner` exécute tous les fichiers `tests/*.yaml` dans un seul process OS). Résolu avec un nouveau `VmError::Exit(i32)`, distinct de `VmError::Thrown` : il se propage par `?` mais `run_frame` ne le fait jamais correspondre à une entrée de la table d'exceptions (aucun `try`/`catch` NL ne peut l'attraper), et `nl_vm::program::run_program` le convertit directement en `RunOutcome.exit_code`.

Effet de bord sur `nl-sema` : `Stmt::Expr`'s bras dans `check_stmt` reconnaît structurellement l'appel `system.ps.Process.exit(...)` et renvoie `terminated = true`, exactement comme `throw`/`return` — sans ça, un `if`/`else` où une branche appelle `exit(...)` aurait déclenché un faux E001.

### `system.text.Regex` / `system.text.Encoding`

Deux classes 100% statiques, plus `system.text.RegexMatch` (résultat natif non générique).

#### Prérequis parseur

`system.text.Regex.match(...)` échouait au parsing (`expected identifier, found Keyword(Match)`) — `match` est un mot-clé (expression `match` de la Phase 5), et `parse_postfix`'s membre-après-`.` exigeait un `eat_ident` strict. Renommé `eat_namespace_segment` (jusqu'ici réservé aux segments de `namespace`/`use`) en `eat_ident_or_keyword` et réutilisé pour tout nom de membre après un `.` — position sans ambiguïté.

#### `mini_regex` étendu

Ancres `^`/`$` (position absolue dans la chaîne complète, pas de mode multi-ligne), groupes capturants `(...)` (toujours capturants, pas de `(?:...)`), et deux nouvelles méthodes `find`/`find_all` (recherche *partielle*, contrairement à `is_match` qui reste un match *pleine chaîne* réservé à `File.glob`). Le moteur est passé d'un style « slice de suffixe » à un style « position absolue + continuation » (`match_node(node, chars, pos, caps, k)`).

`escape()` (nouvelle fonction libre) échappe les métacaractères un par un.

#### `crate::text` (nouveau module `nl-vm`)

Construit un `system.text.RegexMatch` (`fullMatch`/`groups`, `groups[0]` dupliquant `fullMatch` — stdlib.md documente la disposition exacte comme *implementation-defined*), et implémente base64 (RFC 4648, avec padding `=`) à la main.

`match`/`matchFirst`/`replace`/`split` compilent un pattern (throw `IllegalArgumentException` — non documenté par stdlib.md, mais cohérent avec les autres « entrée invalide sans `throws` déclaré »). `encodeUtf8`/`base64Encode`/`base64Decode` passent par deux nouveaux helpers `bytes_from_array`/`array_from_bytes` ; `decodeUtf8` est *lossy* (`String::from_utf8_lossy`), `base64Decode` déclare `throws FormatException`.

#### Bug corrigé en passant

`nl-sema`'s `Expr::FieldAccess` exigeait strictement `Type::Named` pour consulter les tables de champs, donc accéder à un champ d'un type nullable (`RegexMatch|null`) retombait sur le repli permissif `Type::Void` — rendait `m.fullMatch == "..."` un faux E009. Corrigé en faisant collapser `Type::Union` vers son premier membre `Type::Named` avant la résolution du champ ; un vrai `null` continue de lever `NullPointerException` à l'exécution.

### `system.time.DateTime` / `system.time.TimeZone`

Même stance "pas de dépendance externe, lire les données du système directement" déjà prise pour `/proc` (`system.ps`) et `/dev/urandom` (`SecureRandom`/`Uuid`) : nouveau module `nl_vm::mini_tz` qui lit la base `/usr/share/zoneinfo` (format binaire TZif, RFC 8536) directement plutôt que de vendre une bibliothèque de fuseaux horaires (`chrono-tz`/`tzdata`) ou de réimplémenter les règles IANA à la main.

#### `mini_tz`

Parseur TZif (bloc 64 bits v2/v3 avec repli sur v1 sinon — seule la table de transitions est lue, pas la chaîne POSIX de fin de fichier qui extrapole au-delà de la dernière transition explicite, lacune documentée) ; calcul calendaire exact par l'algorithme de Howard Hinnant (`days_from_civil`/`civil_from_days`, domaine public) ; identifiant de fuseau soit un nom IANA (`"Europe/Paris"`, avec garde-fou anti-traversée), soit `"UTC"`, soit un décalage fixe pseudo-fuseau `"±HH:MM"` (produit par `DateTime.parse` sur une chaîne à décalage explicite plutôt que `Z`).

`default_zone_id()` : variable d'environnement `TZ`, sinon cible du lien symbolique `/etc/localtime`, sinon `"UTC"`.

12 tests unitaires Rust, y compris deux qui vérifient de vraies transitions DST 2023 sur `Europe/Paris`/`America/New_York` lues depuis le `/usr/share/zoneinfo` réel de la machine.

#### `nl_vm::native`

Ni `DateTime` ni `TimeZone` ne sont jamais construits par `new` — ce sont des objets construits directement par les statiques `now`/`parse`/`getDefault`/`get` (même schéma que `File.open` construisant un `FileHandle`), puis dispatchés par `INVOKE_INSTANCE` via `is_native_instance_class`/`dispatch_native_instance`. État porté directement sur les champs de l'objet (`"__epoch__"`/`"__zone__"` pour `DateTime`, `"__id__"` pour `TimeZone`).

`getYear`/…/`getSecond`/`format` recalculent le décalage UTC courant via `mini_tz::zone_offset_seconds(zone, epoch)` à chaque appel (pas de cache). `TimeZone.get` valide l'id en résolvant réellement un décalage (échec → `IllegalArgumentException`). `DateTime.parse` lève `FormatException` (checked) sur une entrée malformée.

Hors scope documenté : extrapolation au-delà de la dernière transition TZif explicite, secondes fractionnaires/bissextiles, tout autre format que celui des deux exemples de stdlib.md, portabilité non-Linux.

### `system.Env`

Dernière classe stdlib listée par milestones.md, classe 100% statique (`get`/`set`/`remove`/`list`), même famille que `system.SecureRandom`/`Uuid`.

`get(name)` : `std::env::var` ; absent *ou* non-UTF8 (cas non documenté) traités identiquement en `null`. `list()` : `std::env::vars()` triée.

`set`/`remove` appellent `std::env::set_var`/`remove_var`, marquées `unsafe` par Rust lui-même depuis 1.82 précisément à cause du risque de data race documenté par stdlib.md elle-même (§ Thread safety : modifier l'environnement pendant qu'un autre thread le lit est UB sur la plupart des plateformes) — première utilisation d'`unsafe` dans tout le workspace. Pas de synchronisation ajoutée côté VM (responsabilité de l'appelant NL) ; le bloc `unsafe` se contente de documenter cette limite en commentaire.

### Méthodes de tableaux à callback (`slice`/`map`/`filter`/`forEach`/`sort`/`find`) et `system.Map.forEach`

Premiers native natifs à recevoir une closure *en argument* et à la rappeler eux-mêmes, plutôt qu'une closure simplement stockée puis invoquée par le bytecode appelant (`INVOKE_CLOSURE`) comme partout ailleurs jusqu'ici.

#### `nl-sema::checker`

L'arme `Type::Array(_) if name == "length"` devient `Type::Array(elem) => match (name, argc) { ... }`, une entrée par méthode. `slice`/`filter` gardent le même type d'élément que le receveur ; `find` retourne `T|null` ; `map`/`forEach`/`sort` retournent `Type::Void` — le joker habituel, puisque `map`'s `U` n'a pas de représentation statique.

#### `nl-codegen::expr`

`compile_array_method_call` (nouveau) émet les arguments puis un `INVOKE_INSTANCE` dont le nom de classe en constant pool est un simple placeholder (`"system.Array"`, jamais une vraie classe) — les tableaux n'ont pas de classe à eux, la VM dispatche sur la variante `Value::Array` du receveur. Contrairement à `filter`/`slice`/`sort`/`forEach`/`find`, le type d'élément réel du résultat de `map` (`U`) est récupéré directement depuis le type *déduit* de la closure littérale elle-même (`ExprTy::Closure`'s `return_ty`) — plus précis que le joker `Type::Void` que nl-sema utilise.

#### `nl-vm`

`INVOKE_INSTANCE` intercepte désormais un receveur `Value::Array` (avant la destructuration `Value::Object` existante). Nouveau `invoke_closure` (généralisation de l'`invoke_task` existant de `system.thread.Thread`). `dispatch_array` clone le `Vec<Value>` sous-jacent avant d'itérer plutôt que de garder le verrou pendant le rappel — un callback qui touche le même tableau (ex. `arr.forEach((v) => arr.pushBack(v))`) déadlockerait sinon. `sort` utilise un tri par insertion (O(n²)) plutôt que `slice::sort_by`, qui ne peut pas propager un `Result` depuis un comparateur faillible.

Pas de support pour l'opérateur spaceship `<=>` à ce stade (ajouté plus loin) : le test de `sort` utilise `(int a, int b) => a - b`.

`List` n'a pas de `forEach` propre : stdlib.md n'en définit pas (seul `Map.forEach` existe).

### `new T[]{...}` — liste d'initialisation

L'opcode `NEW_ARRAY_INIT` existait déjà dans `nl-bytecode` (`u16 type_index, u16 count`, encodage/décodage/`from_u8` déjà câblés) mais restait `Unsupported` côté VM, et rien ne l'émettait — l'AST n'avait même pas de variante dédiée.

- **AST** : `Expr::NewArrayInit(Box<Type>, Vec<Expr>)`, à côté de l'`Expr::NewArray(Box<Type>, Box<Expr>)` existant (taille fixe).
- **Parseur** : `parse_new_expr` bifurque juste après avoir consommé `[` — `]` immédiat bascule sur la forme liste (`{` obligatoire, éléments séparés par `,`, `}` final ; `new T[]{}` vide accepté).
- **`nl-sema`** : nouveau bras qui type-check chaque élément (pas de vérification d'assignabilité élément-par-élément à `elem_ty`, même laxisme que le reste du checker) et retourne `Type::Array(elem_ty)`.
- **`nl-codegen`** : `compile_new_array_init` compile chaque élément puis le coerce vers le type d'élément résolu avant d'émettre `NEW_ARRAY_INIT` avec un nouvel helper `op_u16_u16` ; l'effet de pile de l'instruction est `1 - count`.
- **`nl-vm`** : `NEW_ARRAY_INIT` lit `type_index` (ignoré, gardé seulement pour le format d'encodage documenté par vm.md) et `count`, puis `stack.split_off(stack.len() - count)` (préserve l'ordre e₀..eₙ₋₁).

### Types de champs/paramètres pointés-génériques

Vérification d'un item "reste à faire" noté précédemment (`system.thread.Mutex` comme type de champ de classe supposé échouer au parsing) : en fait déjà résolu *sans travail supplémentaire* par le commit qui a ajouté `parse_dotted_name` pour `system.io.*` — ce chemin est commun aux champs, paramètres *et* variables locales. Un test dédié fige le comportement.

### Opérateur spaceship `<=>`

Le token `Punct::Spaceship` était déjà lexé mais jamais parsé ni compilé — seul `CMP_THREE_WAY` existait déjà côté `nl-bytecode`/`nl-vm`, jamais émis.

- **AST** : nouveau `BinOp::Cmp3`.
- **Parseur** : nouvelle fonction `parse_spaceship` insérée entre `parse_relational` (niveau 6) et `parse_additive` (niveau 4) — respecte exactement l'ordre du tableau de précédence.
- **`nl-sema`** : nouveau bras dans `check_numeric_or_eq`, même règle que `<`/`>`/`<=`/`>=` mais le type de résultat est `Type::Int` (pas `Bool`) puisque `<=>` retourne `-1`/`0`/`1`.
- **`nl-codegen`** : émet `Opcode::CmpThreeWay` et retourne `ExprTy::Int` (`promote_numeric` avant l'émission, donc `byte`/`int`/`float` déjà widened en `Int`/`Float` identiques avant que la VM ne voie l'opcode — aucun changement requis côté VM).

Bug pré-existant repéré en passant, **hors scope** (non corrigé) : une concaténation de chaînes à trois opérandes ou plus (`s + toString(x) + ","` où `s` n'est pas un littéral) échoue en codegen — `peek_type` ne reconnaît apparemment pas un sous-`Expr::Binary` imbriqué comme étant lui-même de type `string`.

### `system.io.Grep`

Deux surcharges statiques — `search(string pattern, string path) throws IOException` (un seul fichier) et `search(string pattern, string dirPath, bool recursive) throws IOException` (répertoire) — départagées par arité comme `File.open`'s `FileMode`.

Backées par `crate::mini_regex` appliqué ligne par ligne via `Regex::find` — match partiel "comme grep" (stdlib.md le dit explicitement pour `Regex.match`), pas `Regex::is_match` qui est réservé au match plein-chemin de `File.glob`. Type de résultat `system.io.GrepMatch` (`path: string`, `lineNumber: int`, `line: string`) : un `Value::Object` construit directement en Rust, sans `ClassInfo` ni source `.nl`.

`grep_path` parcourt `std::fs::read_dir` trié avec récursion optionnelle, délègue à `grep_file` sur chaque fichier.

### Opérateur de cast explicite `(T) expr`

Dernier point du "reste à faire" de Phase 6, seule source de `Value::Byte` jusqu'ici passait par une native (`system.text.Encoding.encodeUtf8`) faute de cette syntaxe.

#### AST

Nouveau `Expr::Cast(Box<Type>, Box<Expr>)` (contrairement à `Expr::InstanceOf`, qui ne garde qu'un nom de classe brut, la cible d'un cast peut être n'importe quel `Type` — tableau, union — d'où le type complet plutôt qu'une `String`).

#### Parseur

`(T) expr` est au même niveau de précédence que les opérateurs unaires (niveau 2), donc tenté dans `parse_unary` (nouvelle `try_parse_cast`, même stratégie de retour-arrière que `try_parse_closure`) avant de retomber sur `parse_postfix`.

Aucune table de `typedef` n'existe pour lever l'ambiguïté C classique entre un cast et une valeur parenthésée (`(a) - b`) — nouvelle `can_start_cast_operand` retient l'interprétation "cast" seulement si le token qui suit `)` ne peut démarrer qu'une expression unaire/primaire (identifiant, littéral, `(`, `!`, `new`, `match`, `this`, `super`) ; un `-`/`+` juste après est traité comme continuant l'ancienne valeur parenthésée en soustraction/addition, **sauf** quand la cible est un type primitif numérique/`bool`/`string`, auquel cas `(byte) -1`/`(int) -5.9` sont acceptés sans ambiguïté réelle (comportement calqué sur celui de Java pour les primitives).

#### `nl-sema`

Nouveau code **E007** (`Cannot cast '%s' to '%s'`). `check_cast` implémente la table de compiler.md : identité/élargissement/rétrécissement numérique ; cast vers `string` restreint aux mêmes types que la concaténation (`is_concat_operand`, réutilisée) puisque `Stringable` n'est toujours pas implémenté ; cast classe↔classe accepté dans les deux sens dès qu'une relation `extends`/`implements` existe ; `T|null → T` explicitement rejeté (seul cas où la nullabilité elle-même bloque, pas le simple littéral `null` : `(T) null` reste accepté).

Le type cible est résolu via `self.resolve_ty` (résolution des imports) avant validation — sans ça, un cast vers une classe importée via `use` échouait à tort en E007.

#### `nl-codegen`

`compile_cast` choisit l'opcode selon la paire `ExprTy` source/cible — `I2F`/`F2I`/`I2B`/`B2I` pour les conversions numériques directes ; `byte↔float` composé via `int` (`B2I`+`I2F`, `F2I`+`I2B`) ; `ToString` pour un cast vers `string` ; `CheckCast` pour un cast objet↔objet, y compris un upcast — redondant à l'exécution dans ce cas mais inoffensif.

#### `nl-vm`

`Opcode::CheckCast` (jusqu'ici `Unsupported`) — même schéma que `INSTANCEOF` déjà en place, mais lève `InvalidCastException` au lieu de pousser `false` ; `null` traverse le contrôle sans vérification.

### Bilan Phase 6

Phase 6 complète — tous les points du "reste à faire" traités (byte, types pointés-génériques, `<=>`, `system.io.Grep`, cast). Environ 82/82 tests maison, 10/14 sur nlvm-specs.

---

## Phase 7 — Complétude sémantique (49 codes d'erreur)

**49/49 implémentés et testés** (2026-07-17).

Avant cette phase, 19 codes étaient déjà implémentés en passant (E001, E003-E005, E007-E009, E015-E017, E027-E029, E041-E042, E045-E048) ; E030 (mot-clé réservé) n'a jamais eu besoin de check dédié (déjà garanti par le lexer/parser, cf. Phase 3). Cette phase en a ajouté **21 de plus** : E002, E006, E010-E014, E018-E019, E031-E037, E039-E040, E043-E044, E049. Plus l'ajout de E020-E026 et E038 traités dans des sessions dédiées.

### E018/E019 — visibilité

`Visibility`/`is_static` étaient déjà dans l'AST mais jamais vérifiés. Ajout de `visibility_explicit: bool` sur `FieldDecl`/`MethodDecl` (le parseur défaultait silencieusement à `Public` en l'absence de mot-clé — désormais une omission déclenche E019). `class_table::{FieldInfo,MethodInfo,CtorInfo}` gagnent `visibility` ; nouvelles `find_field_owner`/`find_method_owner` (comme `find_method_exact` mais renvoient aussi la classe *déclarante*, nécessaire pour vérifier `private`/`protected` contre la bonne classe plutôt que la classe du type statique) et `is_accessible`. Câblé sur les quatre sites de référence à un membre.

### E040 — contexte statique

`this_ty` était déjà `None` dans une méthode statique — il suffisait d'émettre l'erreur au lieu de retomber sur le joker `Type::Void`. Limité à `this`/`super` (testable) ; `Self` n'est de toute façon pas implémenté comme expression dans ce parseur.

### E043 — import dupliqué

Nouveau `check_duplicate_imports` dans `nl-sema/src/lib.rs`. Piège découvert en écrivant le test : `m5_0010` (`use test.class.ClassTest;` importé depuis son propre namespace, cf. bugfix Phase 6) est un **no-op légitime**, pas un conflit — un conflit n'existe que quand le nom importé est déjà lié à une entité *différente* (comparaison par FQCN, pas juste par nom simple).

### E002 — propriété non initialisée

Nouvelle mini-analyse de flux dédiée dans `checker.rs` (`field_assigned_after`/`field_assigned_stmt`, distincte de l'analyse E001 existante) — nécessaire car la question posée est différente : "le champ est-il assigné à *chaque point de sortie réel*" (chaque `return` explicite + la fin de méthode), et un `throw` n'impose *aucune* exigence (l'objet à moitié construit est jeté). Un `this(...)` en tête de constructeur est crédité de la garantie du constructeur ciblé sans recomputation récursive.

### E010/E011 — méthodes const

`MethodDecl.is_const` était déjà parsé (Phase 4) mais jamais lu. `MethodChecker` gagne `is_const_method`. E010 : `this.champ = ...` rejeté avant même de résoudre le champ. E011 : appel à `this.methode()` non-const, résolu via `find_method_owner`.

### E044 — const-correctness d'interface

Bloquait `m2_0030` depuis Phase 3. Cause racine : `Stringable` (specs.md, `public string toString() const;`) n'existait dans aucun prélude — ajouté à `nl_syntax::prelude::files()` comme `InterfaceDecl` synthétique (comme la hiérarchie d'exceptions). `MethodSig` (signatures d'interface) gagne `is_const`, jusque-là parsé et jeté. Nouveau `check_const_interface_impl` compare, pour chaque interface implémentée, chaque méthode `const` déclarée contre la méthode de même nom+arité de la classe.

### E012/E039 — const params/locals, boucle for-each en contexte const

Plus gros morceau de plomberie — `const` sur un paramètre/une variable locale était jusqu'ici *parsé et jeté partout* (fermetures, `for-each`, params). Ajout de `Param.is_const`/`Stmt::VarDecl.is_const` réellement porteurs ; `const T x = expr;` reconnu comme forme de statement dédiée dans `parse_stmt`. `MethodChecker` gagne `const_vars`/`readonly_loop_vars` (deux `HashSet<u32>` sur le même espace d'ids que `assigned`).

E012 vérifié à l'assignation (`LValue::Local`), `++`/`--`, et à l'appel d'une méthode non-const sur une variable const porteuse d'un objet. E039 : la variable de boucle `for-each` hérite de `readonly_loop_vars` quand l'itérable est `this.champ` dans une méthode const, ou un paramètre/local déjà const.

### E037/E006 — bornes de template, opérateur non supporté

La borne `extends Bound` d'un paramètre de template était parsée puis jetée depuis la Phase 5 — changé en `Vec<TypeParam { name, bound }>`. Nouveau `nl_syntax::monomorphize::collect_instantiations` (extrait de la première moitié de `expand`, exposé publiquement) permet à `nl-sema` de vérifier chaque `(gabarit, arguments concrets)` réellement utilisé *avant* que `expand` ne réécrive tout — `class_table::satisfies_bound` (extends transitif + un seul niveau d'`implements`) tranche.

E006 n'a paradoxalement nécessité **aucune** vraie surcharge d'opérateur (toujours hors scope) : une fois un gabarit monomorphisé, le corps substitué est vérifié comme une classe ordinaire, donc un opérateur non supporté par le type concret échoue déjà "gratuitement" en E009 — `relabel_template_operator_error` détecte juste (via le nom mangled `"Template<Args>"`) qu'on est dans une instanciation et recode E009→E006 après coup.

### E031 — tableau taille fixe, type non-nullable

`Expr::NewArray` existait depuis la Phase 4 ; simple ajout d'un check dans `nl-sema` (élément résolu en `Type::Named` non-union ⇒ pas de défaut valide ⇒ erreur).

### E013/E014 — readonly

`FieldDecl.readonly` était déjà parsé (Phase 4) mais jamais lu ; ajout de `ClassDecl.is_readonly` (`class readonly Name`, syntaxe confirmée dans specs.md — l'ordre canonique place `readonly` *après* `class`, avant le nom). La hiérarchie d'exceptions du prélude est maintenant marquée `is_readonly: true` (conforme à specs.md qui déclare chaque exception `class readonly ...`), sans risque : la seule assignation de champ s'y produit toujours dans son propre `<construct>`.

Règle : assignation autorisée seulement dans le `construct` de la classe *déclarante* elle-même, via `this.champ` littéral — une sous-classe qui assignerait directement un champ readonly hérité (plutôt que de passer par `super(...)`) reste rejetée, conforme à la note de specs.md.

### Abstract/final — E032-E036, E049

Plus grosse nouvelle grammaire de cette phase — `abstract`/`final` étaient lexés depuis le début mais jamais câblés au parseur. `ClassDecl`/`MethodDecl` gagnent `is_abstract`/`is_final` (class : avant `class`, ordre flexible pour que `abstract final class` parse quand même et se fasse rejeter *sémantiquement* en E049 plutôt que par une erreur de parsing brute).

Une méthode abstraite parse un corps `{ ... }` OU `;` (permissif, pour que fournir un corps déclenche E034 proprement au lieu d'un parse error). `nl-codegen` saute purement et simplement la compilation des méthodes abstraites (jamais appelées directement : E032 bloque `new` sur la classe, E033 garantit qu'une classe concrète a toujours une vraie override, donc le dispatch virtuel n'atteint jamais la déclaration abstraite).

E033 : parcourt toute la chaîne `extends` à la recherche de méthodes abstraites, et pour chacune vérifie via `find_method_exact` que la plus proche déclaration trouvée en repartant de la classe courante n'est plus abstraite.

### E038 — tableaux multi-dimensionnels

Traité dans une session séparée : `Expr::NewArray(Box<Type>, Box<Expr>)` (une seule taille) devient `Expr::NewArray(Box<Type>, Vec<Option<Expr>>)` — une entrée par paire de crochets, `None` pour une taille omise (`[]`).

- **Parseur** : `parse_extra_array_dims` boucle sur les crochets suivants après la première dimension ; la forme `new T[]{...}` (liste d'initialisation) reste reconnue uniquement quand `[]` est immédiatement suivi de `{`.
- **`nl-sema`** : `m` = nombre de tailles fournies en tête ; E038 si une taille réapparaît après la première omission (`dims[m..].any(is_some)`) ; E031 seulement si `m == k` (aucune omission — dès qu'une dimension est omise, le niveau alloué le plus profond a des éléments de type tableau, donc toujours nullable) ; nouveau `build_new_array_type` calcule le type statique en injectant *une seule* union nullable au niveau de la première omission.
- **`nl-codegen`** : `compile_new_array`/`emit_new_array_level` (récursif) implémentent exactement le désucrage décrit par compiler.md — `NEW_ARRAY` par niveau alloué (0..m), boucle `ARRAY_STORE` (locals scratch pour la taille/le tableau/l'index) pour peupler chaque niveau sauf le dernier alloué.

Trois autres sites mettaient à jour `Expr::NewArray` à la main (walkers plats) : `nl-codegen/src/closure.rs`, `nl-syntax/src/monomorphize.rs` (×3 passes) — mécaniques, aucun changement de logique.

### E023-E026 — paramètres nommés/optionnels

Refonte des sites d'appel à travers les trois crates.

- **`nl-syntax`** : nouveau `Arg { name: Option<String>, value: Expr }` remplace `Vec<Expr>` comme type des arguments dans `Expr::Call`/`Expr::New`/`Expr::MethodCall`/`Stmt::ThisCall`/`Stmt::SuperCall` ; `Param` gagne `default: Option<Expr>`. `parse_arg` reconnaît `nom: expr` par un lookahead simple. `parse_param_default` ajoute `= expr` optionnel après un paramètre.

- **`nl-sema`** : la résolution de surcharge par arité (« best-effort » depuis la Phase 4) devient une résolution *par plage* — `class_table::arity_in_range(required, total, argc)` remplace chaque comparaison `params.len() == argc`. `MethodInfo`/`CtorInfo` gagnent `param_names`/`required_count`. Nouveau `bind_call_args` (fonction pure) : liaison générique arguments↔paramètres partagée par les quatre sites d'appel — E024 dès qu'un positionnel suit un nommé, E025 si un paramètre est déjà lié, E023 pour tout paramètre requis non lié en sortie ; un nom d'argument inconnu ou un positionnel excédentaire reste indulgent. E026 vérifié une seule fois à la déclaration via un nouveau `is_const_expr` structurel (littéraux + `-` unaire sur littéral numérique — pas d'évaluateur de constantes général).

- **`nl-codegen`** : les *trois* représentations de signature indépendantes (`class_table::MethodInfo`/`CtorInfo` propres à ce crate, plus le troisième cache `expr.rs::MethodSig` pour les appels same-class) gagnent chacune `param_names`/`defaults: Vec<Option<Expr>>`. Nouveau `class_table::resolve_positional_args(names, defaults, args) -> Vec<Expr>` réalise la liaison nommé/positionnel/défaut déjà validée par nl-sema en une liste positionnelle complète, consommée par `compile_call_args` (inchangé) à chacun des cinq points d'émission — chacun bascule aussi son delta de pile de `args.len()` vers `positional.len()` (bug de comptage de pile qui aurait été silencieux sinon). Les appels natifs (aucun `Param` source) rejettent explicitement un argument nommé.

### E020-E022 — paramètres `ref`

Le morceau volontairement laissé de côté aux deux sessions précédentes — seul restant de toute la Phase 7. Contrairement à E023-E026 (résolu entièrement à la compilation), une vraie sémantique par référence exige un vrai mécanisme runtime, implémenté exactement comme vm.md § « Ref parameters (boxing) » le décrit — le compilateur boxe la variable de l'appelant dans un objet `Box<T>` à un seul champ (`value`), le callee lit/écrit `Box<T>.value` à la place du paramètre, l'appelant déboxe après l'appel.

#### `nl-syntax`

`Param`/`Arg` gagnent `is_ref: bool` (parsé via un `Keyword::Ref` déjà lexé mais jamais câblé depuis le début du projet — ordre `const ref T param` au paramètre, `ref` optionnel juste avant l'expression à l'argument, composable avec `nom: ref expr`).

`Box<T>` est une nouvelle classe *template* synthétique dans `nl_syntax::prelude` (`box_class()`, un champ public `value: T` + un constructeur trivial) — jamais écrite par l'utilisateur, instanciée automatiquement pour chaque type concret `T` utilisé comme paramètre `ref` quelque part dans le programme.

Câblage dans `nl_syntax::monomorphize::expand` : un template dans un corps de méthode générique n'a un type *concret* pour `T` qu'*après* substitution, donc la collecte des besoins `Box<T>` (nouveau `collect_ref_box_requests`) tourne en un second temps, sur l'union des classes déjà réécrites (`out`) et des instanciations de templates tout juste générées (`generated`).

#### Piège découvert en écrivant le test positif

`nl_sema::check_compile`/`nl_codegen::compile_program` appelaient `monomorphize::expand` sur les fichiers *utilisateur seuls*, et ne préfixaient le prélude qu'*après* — sans conséquence tant qu'aucune classe du prélude n'était elle-même un template, mais `Box<T>` casse cette hypothèse : `expand` ne voyait jamais sa déclaration, donc ne pouvait jamais l'instancier. Fix : préfixer le prélude *avant* `expand` dans les deux crates, pour que le prélude soit vu par le pipeline de monomorphisation comme n'importe quel fichier utilisateur.

#### `nl-sema`

Nouveau `check_ref_arg` (E020 si l'argument n'est pas un `Expr::Ident` non-const, E021 si `Arg.is_ref` est faux) appelé partout où `bind_call_args` liait déjà un paramètre. E022 vérifié une fois à la déclaration (à côté d'E026 : `param.is_ref && param.default.is_some()`).

**Lacune assumée, non traitée** : `Foo.method(...)` où `Foo` est un chemin de classe pointé (pas `this`) suit un chemin de résolution *distinct* dans `checker.rs`, déjà documenté indulgent pour l'arité/les types — E020/E021 n'y sont donc pas non plus vérifiés (confirmé : `Utils.swap(x, y)` sans `ref` compile sans erreur E021, bien que la sémantique runtime reste correcte si l'appelant respecte spontanément la convention).

#### `nl-codegen`

`class_table::MethodInfo`/`CtorInfo` et le troisième cache `expr.rs::MethodSig` gagnent chacun `is_ref: Vec<bool>`. Nouveaux `class_table::mangle_flat_type`/`box_fqcn`/`calling_convention_params` — un paramètre `ref` a un type *physique* `Box<T>` dans le descripteur de méthode et sur la pile, jamais `T` directement.

Côté appelant, `compile_call_args` boxe chaque argument `ref` *avant* de pousser quoi que ce soit (`emit_new_box` : `NEW Box<T>`/`DUP`/`LOAD var`/`SET_FIELD`/`STORE box_local`, ordre imposé par l'exemple de vm.md), pousse la box à la place de l'argument, puis chaque site d'émission appelle `emit_unbox_ref_args` juste après l'instruction `INVOKE_*`.

Cas particulier géré : transmettre un paramètre `ref` déjà boxé à un autre appel `ref` (`ref`-forwarding) réutilise directement la box existante (`RefPlan::Forward`).

Côté callee, `LocalSlot` gagne `boxed: Option<ExprTy>` ; `declare_ref_param` marque le slot d'un paramètre `ref` ; les trois points de lecture/écriture d'un local basculent sur une séquence `GET_FIELD`/`SET_FIELD` explicite quand `boxed.is_some()`.

### Bilan Phase 7

**121/121 (tests maison, Phase 7 terminée : 49/49 codes) + 11/14 (nlvm-specs, inchangé)**.

---

## Écarts spec/implémentation traités en cours de route

Repérés en écrivant des démos externes (`nlvm-demos`), donc côté usage réel plutôt que tests internes.

### Alias d'import (`use x.Y as Z;`)

Traité 2026-07-17 : `parse_source_file` consomme désormais un `as <ident>` optionnel après le chemin pointé ; `SourceFile.uses` est passé de `Vec<String>` à `Vec<UseDecl { path, alias }>` ; les quatre `import_map` lient désormais l'alias (quand présent) plutôt que le dernier segment du chemin, et `check_duplicate_imports`/E043 compare par nom lié (alias ou simple).

### Appel de méthode statique sur classe utilisateur (`Foo.method()`)

Traité 2026-07-17 : `nl_codegen::class_table::MethodInfo` gagne `is_static`. `compile_method_call` reconnaît désormais, juste après le chemin dédié `system.*`, le cas où `dotted_path(target)` résout à une classe connue portant une méthode statique de ce nom/arité — émet un `INVOKE_STATIC` construit à la volée au lieu de tomber dans `unsupported construct: undefined variable`. `nl-sema` n'a nécessité aucun changement (déjà indulgent sur ce chemin).

### `readonly` runtime enforcement

Traité 2026-07-17 : `vm.md` § Field access exige que `SET_FIELD` rejette à l'exécution toute écriture sur un champ `readonly` hors constructeur, comme filet de sécurité.

Deux trous : `SET_FIELD` ne consultait jamais le flag `readonly` du champ, *et* `nl-codegen` ne propageait jamais `ClassDecl.is_readonly` dans `Module.class_flags`. Fix : `class_flags: if class.is_readonly { class_flags::READONLY } else { 0 }` en plus du `field_flags::READONLY` par champ déjà câblé ; nouveau `resolve_field_owner` dans `nl-vm/src/interpreter.rs` (même schéma que `resolve_virtual`) qui remonte la chaîne `extends` depuis la classe *runtime* du receveur jusqu'à trouver la déclaration du champ.

`SET_FIELD` rejette (`VmError::Malformed`, comme les autres invariants "should never happen given a valid compiler" — cohérent avec la formulation vm.md « as a safety net; the compiler should have caught this ») sauf si l'écriture a lieu dans le `<construct>` de la classe *déclarante* elle-même sur le receveur exact `this` (`Arc::ptr_eq` avec `locals[0]`).

### Destructeurs appelés par le GC

Traité 2026-07-17. Décision GC actée du même coup : **comptage de références**, c'est-à-dire que le refcount des `Arc<Mutex<Object>>` existants *est* le GC — la mémoire était déjà libérée à la chute de la dernière référence, il ne manquait que le crochet `<destruct>`.

Implémentation : `impl Drop for Object` (`nl-vm/src/value.rs`) — quand le dernier `Arc` tombe, résolution de `<destruct>` (descripteur `() -> void`) via `resolve_virtual` (passé `pub(crate)`), donc en remontant la chaîne `extends` : un destructeur hérité tourne pour les sous-classes, seul le plus dérivé tourne (la spec ne définit pas de chaînage à la C++).

`Drop` n'ayant que `&mut self` (l'`Arc` est déjà mort, impossible de fabriquer un `this`), les champs sont déplacés dans un objet « résurrection » marqué `destroyed: true` qui sert de receveur — le flag garantit « at most once » même si le destructeur re-fait échapper `this` dans une structure vivante. Une exception jetée par le destructeur est silencieusement jetée (conforme vm.md).

`Object` gagne `program: Weak<Program>` (renseigné seulement par `NEW` dans l'interpréteur — seuls les objets de classes utilisateur ont un `<destruct>` bytecode ; `Weak` et pas `Arc` car les closures de `Thread` capturent `Arc<Program>` *et* des objets, ce qui ferait un cycle). Les ~20 sites de construction natifs passent par un nouveau `Object::native(...)` (hook mort, jamais de lookup).

Deux corrections d'ordre de verrous en découlent : `SET_FIELD`/`ARRAY_STORE` relâchent désormais le verrou de l'objet/du tableau *avant* de dropper la valeur remplacée (dont le destructeur pourrait rappeler l'objet encore verrouillé → interblocage, `std::sync::Mutex` non réentrant) ; et `run_program` consomme/droppe le résultat de `main` (dont un éventuel objet d'exception non rattrapée, qui peut avoir un destructeur) *avant* de capturer stdout/stderr, sinon la sortie de ce destructeur serait perdue.

Timing obtenu : « prompt » au sens vm.md (sortie de scope = drop des locals du frame, réassignation, `POP`, fin de `main`) ; les cycles ne sont jamais collectés (limitation refcounting assumée), et les objets retenus par un thread abandonné au retour de `main` ne sont pas détruits non plus.

---

## Suivi d'avancement

- [x] Phase 0 — Bootstrap
- [x] Phase 1 — Tranche minimale de bout en bout (m1_0010, m4_0010, m4_0020 passent ; `nlc`/`nlvm` fonctionnent en CLI)
- [x] Phase 2 — VM primitive + expressions
- [x] Phase 3 — Sémantique noyau (scope restreint)
- [x] Phase 4 — Objets, tableaux, dispatch (scope restreint)
- [x] Phase 5 — Exceptions + closures + génériques
- [x] Phase 6 — Stdlib (complète)
- [x] Phase 7 — Complétude sémantique (49/49 codes d'erreur)
- [ ] Phase 8 — Optimisations (optionnel)

État final : **121/121 tests maison, 11/14 tests `nlvm-specs`**. Les 3 restants sur nlvm-specs (`m2_0060`, `m5_0020`, `m7_0040`) sont bloqués par des lacunes documentées (type narrowing jamais implémenté depuis Phase 3, nullabilité implicite des interfaces).
