# `Self` in interfaces, `Cloneable`, `ValueEquatable`

Suite de [`journal_03_phase9_language_features.md`](journal_03_phase9_language_features.md) (§ "Operator overloading et mots-clés `type`/`Self`"), qui avait volontairement laissé `Self` non résolu à l'intérieur d'un corps d'**interface** — c'était le dernier item resté ouvert sur `Next.md` : "Cloneable / ValueEquatable / `Self` en interface".

## `Self`/`type` dans un corps d'interface

`parser::parse_interface_decl` (nl-syntax) ne posait jamais `Parser::current_class`, contrairement à `parse_class_decl`/`parse_enum_decl` — donc `parse_type_atom` retombait systématiquement sur son erreur "'type'/'Self' can only be used inside a class body" dès qu'on écrivait `public Self clone();` dans une interface.

Le mécanisme existant (`current_class: Option<String>` posé une fois, consommé par `parse_type_atom`/`parse_new_base_type`) résout `Self`/`type` **directement vers le nom de la classe englobante** — ça marche pour une classe parce qu'il n'y a qu'un seul nom possible. Une interface n'a pas cette propriété : specs.md § Self in interfaces précise que `Self` doit s'instancier **par classe implémentante** ("like a built-in one-parameter template"), et à l'endroit où `parse_interface_decl` parse la déclaration, aucune classe implémentante n'est encore connue (il peut y en avoir zéro, une, ou plusieurs dans la même compilation).

Décision : plutôt que d'ajouter une vraie substitution différée (nouvelle variante `Type`, propagée jusqu'à la construction de `ClassInfo` par classe implémentante), on réutilise le mécanisme `current_class` tel quel avec un nom **littéral placeholder** : `parse_interface_decl` pose `self.current_class = Some("Self".to_string())` pour la durée du corps de l'interface. `Self`/`type` produisent donc `Type::Named("Self")` dans le `MethodSig` de l'interface.

Ça tient parce que `resolve_type` (dupliqué nl-sema/nl-codegen, `class_table.rs`) est **lenient par construction** : `imports.get("Self").unwrap_or_else(|| "Self".to_string())` — comme aucune classe ne s'appelle jamais `Self`, le placeholder traverse `resolve_type` inchangé, sans erreur ni panique. Et rien en aval n'essaie réellement de le résoudre : ce compilateur ne vérifie pas la conformance d'une classe à une interface au-delà d'E044 (`check_const_interface_impl`, const-correctness par nom+arité) — il n'y a tout simplement aucun code qui compare le `return_ty`/les types de paramètres d'un `MethodSig` d'interface à ceux de la méthode d'implémentation. Donc `Type::Named("Self")` reste un type inerte sur le `MethodInfo` de l'interface elle-même ; il ne fuit jamais vers un appel réel, parce que la résolution d'un appel de méthode se fait toujours via le type statique **concret** du receiver (`Point.clone()`, jamais via l'interface `Cloneable.clone()` avec une substitution `Self = Point`).

Ce que ça implique concrètement pour l'implémenteur : il doit lui-même réécrire `Self`/`type` (ou le nom de sa propre classe, les deux sont acceptés par specs.md) dans sa **propre** déclaration de méthode — mécanisme déjà en place depuis `journal_03`. C'est cette réécriture côté classe, pas une résolution côté interface, qui produit la covariance. Limitation assumée et cohérente avec le reste du checker (permissif, cf. `is_object_assignable`, E044) : appeler une méthode `Self`-typée à travers une variable **de type interface** (`Cloneable c = new Point(...); c.clone()`) n'est pas un cas couvert — non testé, non garanti — mais aucun exemple de specs.md n'en a besoin (l'exemple canonique est `auto copy = original.clone();` avec `original` de type concret).

## `Cloneable` et `ValueEquatable`

Comme `Stringable` (`nl_syntax::prelude::stringable`), ces deux interfaces n'existent nulle part en `.nl` source — il n'y a aucune infrastructure de stdlib chargée depuis des fichiers `.nl` dans ce compilateur, seulement `nl_syntax::prelude` (AST Rust construit à la main, injecté globalement dans tout `import_map`). `cloneable()`/`value_equatable()` suivent exactement le même patron, ajoutées à `prelude::files()`.

- `Cloneable { public Self clone(); }` — une fois l'interface déclarable, `clone()` est une méthode d'instance ordinaire ; aucune infra VM n'est nécessaire (pas de clonage natif, pas de nouvel opcode). L'implémenteur écrit `public Self clone() { return new type(...); }` comme n'importe quelle méthode fluide/covariante déjà supportée.
- `ValueEquatable { public bool valueEquals(const Self|null other); public int valueHash(); }` — même chose, méthodes ordinaires. Le paramètre `const Self|null other` exerce à la fois le placeholder `Self` en position de paramètre *et* à l'intérieur d'un `Type::Union`, déjà géré par `resolve_type`/`parse_type` sans changement.

**Limitation assumée, documentée dans le commentaire de `value_equatable()`** (cohérente avec le gap déjà noté dans `nl_vm::native` et `native_generics.rs` avant ce chantier) : `system.Map`/`List` ne consultent toujours pas `valueEquals`/`valueHash` pour la comparaison de clés/éléments objets — `values_equal` (nl-vm/src/interpreter.rs) reste identité de référence pour tout ce qui n'est pas primitif/`string`. Câbler ça demanderait un rappel depuis `dispatch_map`/`List.contains` (nl-vm/src/native.rs) vers l'interpréteur pour invoquer dynamiquement une méthode utilisateur — chantier séparé, non nécessaire pour que `p1.valueEquals(p2)` fonctionne (l'exemple canonique de specs.md), donc laissé de côté ici.

## Tests

3 nouveaux tests (`tests/phase10_00{10,20,30}_*.yaml`) :
- `phase10_0010` : interface utilisateur avec `Self` en type de retour, deux classes implémentantes différentes (`Sparrow`/`Labrador`), retour covariant vérifié en appelant une méthode spécifique à chaque classe sur le résultat de `spawn()` sans cast.
- `phase10_0020` : `Cloneable` — `clone()` produit une copie indépendante (mutation de la copie sans effet sur l'original).
- `phase10_0030` : `ValueEquatable` — `valueEquals`/`valueHash` structurels, distincts de `==` (identité).

Piège rencontré en écrivant ces tests (rien à voir avec le chantier lui-même, pur détail de l'outillage) : `nl-test-runner` construit son YAML frontmatter en joignant les lignes internes par `\n` **sans newline finale** (`testfile.rs::parse_test_file`) — si `expected_stdout: |` est la **dernière** clé avant le `---` fermant, le bloc YAML perd sa newline finale de "clip" et `expected_stdout` n'a plus le `\n` de fin qu'un `println` final produit réellement. Fix (déjà le patron dans les tests existants comme `phase7_0260`) : toujours faire suivre un bloc `expected_stdout: |` d'une autre clé (`expected_stderr: ""`), jamais le laisser en dernier avant `---`.

157 tests maison (154 + 3) + 14/14 nlvm-specs toujours verts. Version bump 0.8.0 → 0.9.0 (nouvelle fonctionnalité rétrocompatible).
