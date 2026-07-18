TODO Later

* ~~nlc -l (linting)~~
* stack trace info
* ~~enum~~ — basique + typé (int/string), méthodes/propriétés statiques et d'instance, `from`/`tryFrom`, exhaustivité de `match` (E047). Limitation assumée : pas de vraie storage statique VM (pas de `GET_STATIC`/`SET_STATIC`/`<clinit>`) — les constantes de cas sont recompilées (constantes de compilation) à chaque référence plutôt que lues depuis un stockage statique runtime ; les "static properties" custom au-delà des cas ne sont donc pas supportées (aucun exemple des specs n'en a besoin).
* ~~l'opérateur ?? (coalescing)~~ — `??` et `?:` (elvis) implémentés ensemble (même niveau de précédence, specs.md). Limitation assumée, cohérente avec `Ternary` déjà existant : nl-sema reste permissif sur le type (pas de nouveau code E vérifiant que l'opérande gauche est bien nullable), et le type résultat est approximé (opérande gauche sans `null`) plutôt que le vrai "common result type" à 5 règles — `nl-codegen` fait juste un `coerce_value` de l'opérande droit vers celui de gauche, comme pour le ternaire.
* les déclarations explicites de type fonction ((int) => bool x = ...)
* la mutation de variable capturée par une closure (counter++) — les closures capturent par valeur (snapshot), pas par référence comme l'indique la doc
* tests 11/14 nlvm-specs (smart-cast narrowing) - à traiter