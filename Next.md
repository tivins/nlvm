# TODO

## Tasks

* Implement `typedef`.
* Cloneable / ValueEquatable / `Self` en interface	❌ `Self`/`type` sont maintenant résolus (parse-time, vers `Type::Named(current_class)`) mais seulement à l'intérieur d'un corps de classe/enum (voir journal_03, section "Operator overloading") — pas dans un corps d'**interface**, où specs.md § Self in interfaces exige une résolution par classe implémentante (covariance). `ValueEquatable` a un commentaire explicite dans le code source du compilateur (nl-vm/src/native.rs:57) disant "ValueEquatable itself is not implemented". `Cloneable` n'existe nulle part dans le compilateur.

## DONE

See `journal/*.md` for previous "devlogs".

