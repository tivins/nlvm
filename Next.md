# TODO

## Tasks

* Implement `typedef`.
* Operator overloading (operator+)	❌ parse error: expected type, found Keyword(TypeKw) — le mot-clé operator est tokenisé mais jamais consommé par le parser. Aucun test ni fichier .nl dans tout le repo ne l'utilise.
* Nodiscard	❌ parse error: expected type, found Keyword(Nodiscard) — même situation, token seul.
* Cloneable / ValueEquatable / Self en général	❌ parse error: expected type, found Keyword(SelfType) — le mot-clé Self n'est référencé nulle part dans le sema checker. ValueEquatable a même un commentaire explicite dans le code source du compilateur (nl-vm/src/native.rs:57) disant "ValueEquatable itself is not implemented". Cloneable n'existe nulle part dans le compilateur.

## DONE

See `journal/*.md` for previous "devlogs".

