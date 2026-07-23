//! Cross-file class/interface table, mirroring `nl_codegen::class_table`
//! (kept as a separate, independent view rather than a shared dependency —
//! sema only needs enough of it for lenient existence/type-shape checks).

use std::collections::{HashMap, HashSet};

use nl_syntax::ast::{MethodKind, SourceFile, SourceItem, Type, Visibility};

use crate::types;

#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub name: String,
    pub ty: Type,
    pub visibility: Visibility,
    pub readonly: bool,
    pub is_static: bool,
}

#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub name: String,
    pub params: Vec<Type>,
    /// Parallel to `params` — parameter names, needed to bind named
    /// arguments (compiler.md § Named and optional parameter rules).
    pub param_names: Vec<String>,
    /// How many leading parameters have no default value — everything past
    /// this index is optional. Used for range-based (rather than exact)
    /// arity matching, and for E023 ("required parameter not provided").
    pub required_count: usize,
    /// Parallel to `params` — compiler.md § Ref parameter rules
    /// (E020-E022).
    pub is_ref: Vec<bool>,
    pub return_ty: Type,
    pub is_static: bool,
    pub is_const: bool,
    pub visibility: Visibility,
    /// specs.md § Abstract classes and methods — E032/E033/E034.
    pub is_abstract: bool,
    /// specs.md § Final classes and methods — E036.
    pub is_final: bool,
    /// specs.md § Nodiscard — compiler.md § Warnings, W001. Always `false`
    /// for interface method signatures (`nl_syntax::ast::MethodSig` doesn't
    /// carry this modifier — nodiscard is checked at concrete call sites
    /// only, not through an interface-typed receiver).
    pub is_nodiscard: bool,
    /// Resolved (FQCN) `throws` clause — compiler.md § Exception checking.
    pub throws: Vec<Type>,
}

#[derive(Debug, Clone)]
pub struct CtorInfo {
    pub params: Vec<Type>,
    /// See `MethodInfo::param_names`.
    pub param_names: Vec<String>,
    /// See `MethodInfo::required_count`.
    pub required_count: usize,
    /// See `MethodInfo::is_ref`.
    pub is_ref: Vec<bool>,
    pub throws: Vec<Type>,
    pub visibility: Visibility,
}

/// How many leading parameters of `params` have no default value —
/// everything past this index is optional (specs.md § Optional parameters:
/// "must be placed after all required parameters").
pub fn required_count(params: &[nl_syntax::ast::Param]) -> usize {
    params.iter().take_while(|p| p.default.is_none()).count()
}

/// Whether a call supplying `argc` total arguments (positional + named)
/// could possibly bind against `params` — a range check rather than an
/// exact-arity one now that optional parameters exist. Best-effort, like
/// the rest of this checker's arity-only overload resolution: named
/// arguments can make the true binding non-contiguous, but `argc` alone is
/// enough to pick the right overload in every case this codebase exercises.
pub fn arity_in_range(required: usize, total: usize, argc: usize) -> bool {
    required <= argc && argc <= total
}

#[derive(Debug, Clone)]
pub struct ClassInfo {
    /// Resolved FQCN of the direct superclass (`extends`), if any.
    pub extends: Option<String>,
    /// Resolved FQCNs of directly implemented interfaces (classes only, no
    /// transitivity through interface-`extends` — out of scope this phase).
    pub implements: Vec<String>,
    pub fields: Vec<FieldInfo>,
    pub methods: Vec<MethodInfo>,
    pub ctors: Vec<CtorInfo>,
    /// compiler.md § Readonly classes and properties — E013.
    pub is_readonly: bool,
    /// specs.md § Abstract classes and methods — E032/E033.
    pub is_abstract: bool,
    /// specs.md § Final classes and methods — E035.
    pub is_final: bool,
    /// specs.md § Enums; see `nl_syntax::ast::ClassDecl::is_enum`.
    pub is_enum: bool,
    /// Case names, in declaration order — empty for a non-enum class. See
    /// `nl_syntax::ast::ClassDecl::enum_cases`.
    pub enum_cases: Vec<String>,
}

/// Whether `sub` is `sup` itself or (transitively) extends it — used for
/// unreachable-catch-clause detection (compiler.md § Unreachable catch
/// clauses, E048): an earlier `catch (sup ...)` already catches everything
/// a later `catch (sub ...)` would.
pub fn is_subclass_or_same(classes: &ClassTable, sub: &str, sup: &str) -> bool {
    let mut current = sub.to_string();
    loop {
        if current == sup {
            return true;
        }
        match classes.get(&current).and_then(|c| c.extends.clone()) {
            Some(parent) => current = parent,
            None => return false,
        }
    }
}

/// Whether `iface` (an interface FQCN) is `target` itself or, transitively,
/// `extends`-es it — compiler.md § Interface inheritance: "the extending
/// interface inherits all method declarations of its parents", and "an
/// implementing class can be upcast to any interface in the hierarchy".
/// Interfaces store their `extends` parents in `ClassInfo::implements` (see
/// `build_class_table`'s `SourceItem::Interface` arm) — reusing the same
/// field classes use for `implements`, since both express "the set of
/// interfaces this satisfies". `seen` guards against a diamond
/// (`Resource extends Closeable, Stringable` where both ultimately extend a
/// common ancestor) re-walking the same interface repeatedly; cheap enough
/// to allocate fresh per top-level call given typical interface hierarchy
/// depth.
fn interface_extends(classes: &ClassTable, iface: &str, target: &str, seen: &mut HashSet<String>) -> bool {
    if iface == target {
        return true;
    }
    if !seen.insert(iface.to_string()) {
        return false;
    }
    let Some(info) = classes.get(iface) else {
        return false;
    };
    info.implements
        .iter()
        .any(|parent| interface_extends(classes, parent, target, seen))
}

/// BFS closure of interfaces reachable from `direct` (each interface plus,
/// transitively, anything it `extends` — see `interface_extends`'s doc
/// comment). Shared by E044's const-correctness check and E033's interface
/// conformance check (`check_abstract_final` in `checker.rs`), so both agree
/// on exactly which interface methods a class is on the hook for.
pub fn interface_closure(classes: &ClassTable, direct: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut queue: Vec<String> = direct.to_vec();
    let mut result = Vec::new();
    while let Some(iface_fqcn) = queue.pop() {
        if !seen.insert(iface_fqcn.clone()) {
            continue;
        }
        if let Some(info) = classes.get(&iface_fqcn) {
            queue.extend(info.implements.iter().cloned());
        }
        result.push(iface_fqcn);
    }
    result
}

/// Whether `fqcn` (or, transitively, any of its `extends` ancestors)
/// declares `target` in its `implements` list, or one of those directly
/// implemented interfaces itself (transitively) `extends`-es `target` — used
/// for `Stringable` dispatch (E008/E007's concat/cast operand check) so a
/// subclass of a `Stringable`-implementing class counts too, not just the
/// exact declaring class, and so does a class implementing an interface that
/// itself extends `Stringable`. Matches `is_object_assignable`'s existing
/// leniency.
pub fn implements_interface(classes: &ClassTable, fqcn: &str, target: &str) -> bool {
    let mut current = fqcn;
    loop {
        let Some(info) = classes.get(current) else {
            return false;
        };
        if info
            .implements
            .iter()
            .any(|i| interface_extends(classes, i, target, &mut HashSet::new()))
        {
            return true;
        }
        match &info.extends {
            Some(parent) => current = parent,
            None => return false,
        }
    }
}

pub type ClassTable = HashMap<String, ClassInfo>;

pub fn fqcn_of(file: &SourceFile) -> String {
    let name = match &file.item {
        SourceItem::Class(c) => c.name.as_str(),
        SourceItem::Interface(i) => i.name.as_str(),
    };
    if file.namespace.is_empty() {
        name.to_string()
    } else {
        format!("{}.{}", file.namespace.join("."), name)
    }
}

/// Simple name -> FQCN, from this file's own declaration, every other class
/// in the same namespace (see `nl-codegen`'s equivalent for the fixture
/// that confirms this — `m5_0020`'s `Dog implements Animal` with no `use`),
/// plus explicit `use` imports.
pub fn import_map(file: &SourceFile, all_files: &[SourceFile]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    // Built-in exception classes are globally visible without a `use` — see
    // nl_syntax::prelude. Seeded first so a file's own declarations/`use`s
    // (checked below) can still shadow a same-named builtin.
    for prelude_file in nl_syntax::prelude::files() {
        map.insert(fqcn_of(&prelude_file), fqcn_of(&prelude_file));
    }
    // `system.io.IOException` and friends resolve to the same prelude
    // classes — see nl_syntax::prelude::NAMESPACED_ALIASES.
    for (alias, target) in nl_syntax::prelude::NAMESPACED_ALIASES {
        map.insert((*alias).to_string(), (*target).to_string());
    }
    for other in all_files {
        if other.namespace == file.namespace {
            let simple = match &other.item {
                SourceItem::Class(c) => c.name.clone(),
                SourceItem::Interface(i) => i.name.clone(),
            };
            map.insert(simple, fqcn_of(other));
        }
    }
    let fqcn = fqcn_of(file);
    let simple = match &file.item {
        SourceItem::Class(c) => c.name.clone(),
        SourceItem::Interface(i) => i.name.clone(),
    };
    map.insert(simple, fqcn);
    for u in &file.uses {
        let simple = u.alias.clone().unwrap_or_else(|| {
            u.path
                .rsplit('.')
                .next()
                .expect("use path is never empty")
                .to_string()
        });
        map.insert(simple, u.path.clone());
    }
    map
}

/// Rewrites every `Type::Named("Self")` inside `ty` to `Type::Named(fqcn)`
/// (recursively through arrays, unions, generics, function types). See the
/// parser's `parse_interface_decl` doc comment: `Self` in an interface method
/// signature is stored as the literal placeholder `Type::Named("Self")` and
/// only resolves to the implementing class's own FQCN at conformance-check
/// time — used by E033/E044's interface-vs-impl signature match so a
/// `Cloneable.clone(): Self` interface method matches a class-side
/// `clone(): <ThisClass>` (which the parser has already substituted).
pub fn substitute_self(ty: &Type, fqcn: &str) -> Type {
    match ty {
        Type::Named(name) if name == "Self" => Type::Named(fqcn.to_string()),
        Type::Array(inner) => Type::Array(Box::new(substitute_self(inner, fqcn))),
        Type::Union(members) => {
            Type::Union(members.iter().map(|m| substitute_self(m, fqcn)).collect())
        }
        Type::Generic(name, args) => Type::Generic(
            name.clone(),
            args.iter().map(|a| substitute_self(a, fqcn)).collect(),
        ),
        Type::Function {
            params,
            return_type,
            throws,
        } => Type::Function {
            params: params.iter().map(|p| substitute_self(p, fqcn)).collect(),
            return_type: Box::new(substitute_self(return_type, fqcn)),
            throws: throws.clone(),
        },
        other => other.clone(),
    }
}

pub fn resolve_type(ty: &Type, imports: &HashMap<String, String>) -> Type {
    match ty {
        Type::Named(name) => {
            Type::Named(imports.get(name).cloned().unwrap_or_else(|| name.clone()))
        }
        Type::Array(inner) => Type::Array(Box::new(resolve_type(inner, imports))),
        Type::Union(members) => {
            Type::Union(members.iter().map(|m| resolve_type(m, imports)).collect())
        }
        Type::Function {
            params,
            return_type,
            throws,
        } => Type::Function {
            params: params.iter().map(|p| resolve_type(p, imports)).collect(),
            return_type: Box::new(resolve_type(return_type, imports)),
            throws: throws.clone(),
        },
        other => other.clone(),
    }
}


/// Walks `fqcn`'s direct-superclass chain (starting at `fqcn` itself)
/// looking for a method with the exact same name and parameter types.
/// Unlike `find_method`'s arity-only matching (good enough for resolving a
/// call site), overriding requires an exact signature match — used by
/// E016/E017 to find the specific parent method a subclass method overrides.
pub fn find_method_exact<'c>(
    classes: &'c ClassTable,
    fqcn: &str,
    name: &str,
    params: &[Type],
) -> Option<&'c MethodInfo> {
    let mut current = fqcn;
    loop {
        let info = classes.get(current)?;
        if let Some(m) = info
            .methods
            .iter()
            .find(|m| m.name == name && m.params == params)
        {
            return Some(m);
        }
        current = info.extends.as_deref()?;
    }
}

/// Walks `fqcn`'s `extends` chain (arity-only matching, like `find_ctor`) and
/// returns the field/method together with the FQCN of the class that
/// actually *declares* it — needed by compiler.md § Visibility enforcement
/// (E018) to check `private`/`protected` against the declaring class, not
/// whichever subclass the reference happened to be typed as.
pub fn find_field_owner(
    classes: &ClassTable,
    fqcn: &str,
    name: &str,
) -> Option<(String, FieldInfo)> {
    let mut current = fqcn.to_string();
    loop {
        let info = classes.get(&current)?;
        if let Some(f) = info.fields.iter().find(|f| f.name == name) {
            return Some((current, f.clone()));
        }
        current = info.extends.clone()?;
    }
}

/// Walks `fqcn`'s `extends` chain looking for an instance method named
/// `name` (an `operator<sym>` canonical name — see
/// `nl_syntax::parser::parse_operator_symbol`) whose parameters match
/// `params` *exactly* — specs.md § Operator Overloading resolution needs
/// exact-type matching (not `find_method_owner`'s arity-only leniency) so
/// that e.g. `Vector2` can overload `operator+` once for `Vector2` and once
/// for `int` without either call site becoming ambiguous. Static methods
/// are skipped (operator overloads are instance methods only, per specs.md
/// "Rules").
pub fn find_operator_method<'c>(
    classes: &'c ClassTable,
    fqcn: &str,
    name: &str,
    params: &[Type],
) -> Option<(String, &'c MethodInfo)> {
    let mut current = fqcn;
    loop {
        let info = classes.get(current)?;
        if let Some(m) = info
            .methods
            .iter()
            .find(|m| m.name == name && !m.is_static && m.params == params)
        {
            return Some((current.to_string(), m));
        }
        current = info.extends.as_deref()?;
    }
}

pub fn find_method_owner(
    classes: &ClassTable,
    fqcn: &str,
    name: &str,
    argc: usize,
) -> Option<(String, MethodInfo)> {
    let mut current = fqcn.to_string();
    loop {
        let info = classes.get(&current)?;
        if let Some(m) = info
            .methods
            .iter()
            .find(|m| m.name == name && arity_in_range(m.required_count, m.params.len(), argc))
        {
            return Some((current, m.clone()));
        }
        current = info.extends.clone()?;
    }
}

/// How well a single call argument matches a single declared parameter, for
/// overload resolution (see `find_method_owner_overload`/`find_ctor_overload`
/// below): `Some(0)` for an exact type match, `Some(1)` for one that's only
/// compatible (numeric widening, a subclass/interface implementation, a
/// nullable target), `None` for outright incompatible. `arg = None` — this
/// checker couldn't confidently pin down that argument's static type (see
/// `Checker::overload_arg_ty`, which only probes a deliberately narrow set of
/// expression shapes) — always scores `Some(0)`: an argument this checker
/// can't reason about must never disqualify a candidate it can't actually
/// evaluate, matching the leniency the rest of this checker already extends
/// to shapes it doesn't fully model.
pub(crate) fn overload_param_score(
    classes: &ClassTable,
    arg: Option<&Type>,
    param: &Type,
) -> Option<u32> {
    let arg = arg?;
    if arg == param {
        return Some(0);
    }
    if matches!(arg, Type::Void) {
        return Some(0);
    }
    if matches!(arg, Type::NullT) {
        return if types::is_nullable(param) { Some(1) } else { None };
    }
    if let (Type::Named(from), Type::Named(to)) = (arg, param) {
        return if is_subclass_or_same(classes, from, to) || implements_interface(classes, from, to)
        {
            Some(1)
        } else {
            None
        };
    }
    if types::is_assignable(arg, param) {
        Some(1)
    } else {
        None
    }
}

/// Picks the best-matching candidate among `candidates` (already filtered to
/// same-name, arity-compatible overloads by the caller) by summing
/// `overload_param_score` over each candidate's parameters against
/// `arg_tys`. Falls back to the first candidate — same as the old
/// arity-only `.find()` this replaces — whenever there's at most one
/// candidate to begin with (the overwhelmingly common, non-overloaded case,
/// left provably unchanged), when no candidate's parameters are all
/// individually compatible, or when several candidates tie for the lowest
/// score: specs.md documents overload resolution "on the argument types"
/// for constructor delegation but never defines a tie-breaking rule or an
/// ambiguity diagnostic, so this mirrors this codebase's existing
/// leniency elsewhere (e.g. `nl_codegen::class_table::find_ctor`'s own doc
/// comment) rather than inventing a new error code the specs don't call for.
fn best_overload<'c, T>(
    classes: &ClassTable,
    candidates: &[&'c T],
    params_of: fn(&T) -> &[Type],
    arg_tys: &[Option<Type>],
) -> Option<&'c T> {
    if candidates.len() <= 1 {
        return candidates.first().copied();
    }
    let mut best: Option<(usize, u32)> = None;
    for (idx, cand) in candidates.iter().enumerate() {
        let params = params_of(cand);
        let mut total = 0u32;
        let mut compatible = true;
        for (i, param) in params.iter().enumerate() {
            let arg = arg_tys.get(i).and_then(|o| o.as_ref());
            match overload_param_score(classes, arg, param) {
                Some(score) => total += score,
                None => {
                    compatible = false;
                    break;
                }
            }
        }
        if !compatible {
            continue;
        }
        if best.is_none_or(|(_, best_score)| total < best_score) {
            best = Some((idx, total));
        }
    }
    match best {
        Some((idx, _)) => Some(candidates[idx]),
        None => candidates.first().copied(),
    }
}

/// Like `find_method_owner`, but among every same-name, arity-compatible
/// overload declared directly on a given class in the `extends` chain,
/// picks the best match for `arg_tys` (see `best_overload`) instead of just
/// the first declared — the fix for the "arity-only" resolution gap
/// documented in `IMPLEMENTATION_STATUS.md`. Still only ever looks at one
/// class level at a time and falls through to the parent when that level has
/// no arity-compatible candidate at all, exactly like `find_method_owner`.
pub fn find_method_owner_overload(
    classes: &ClassTable,
    fqcn: &str,
    name: &str,
    argc: usize,
    arg_tys: &[Option<Type>],
) -> Option<(String, MethodInfo)> {
    let mut current = fqcn.to_string();
    loop {
        let info = classes.get(&current)?;
        let candidates: Vec<&MethodInfo> = info
            .methods
            .iter()
            .filter(|m| m.name == name && arity_in_range(m.required_count, m.params.len(), argc))
            .collect();
        if let Some(m) = best_overload(classes, &candidates, |m| &m.params, arg_tys) {
            return Some((current, m.clone()));
        }
        current = info.extends.clone()?;
    }
}

/// Like `find_ctor`, but picks the best-matching overload among every
/// arity-compatible constructor by `arg_tys` (see `best_overload`) instead
/// of just the first declared — specs.md § Constructor chaining: "the
/// target constructor is selected by overload resolution on the argument
/// types, like a regular call." Constructors are never inherited, so
/// (unlike `find_method_owner_overload`) there's no `extends` walk here,
/// same as the plain `find_ctor` this replaces.
pub fn find_ctor_overload<'c>(
    classes: &'c ClassTable,
    fqcn: &str,
    argc: usize,
    arg_tys: &[Option<Type>],
) -> Option<&'c CtorInfo> {
    let candidates: Vec<&CtorInfo> = classes
        .get(fqcn)?
        .ctors
        .iter()
        .filter(|c| arity_in_range(c.required_count, c.params.len(), argc))
        .collect();
    best_overload(classes, &candidates, |c| &c.params, arg_tys)
}

/// compiler.md § Template instantiation, "Bounded type parameters" — E037.
/// Whether `concrete_fqcn` (or an ancestor, via `extends`) satisfies
/// `bound_fqcn` — either by being a subclass of it, or by `implements`-ing it
/// directly at some point in the `extends` chain (interfaces don't
/// themselves `extend` other interfaces in this class table — see
/// `ClassInfo::implements`'s doc comment — so no further transitivity is
/// needed there).
pub fn satisfies_bound(classes: &ClassTable, concrete_fqcn: &str, bound_fqcn: &str) -> bool {
    is_subclass_or_same(classes, concrete_fqcn, bound_fqcn)
        || implements_interface(classes, concrete_fqcn, bound_fqcn)
}

/// compiler.md § Visibility enforcement — E018. `declaring_fqcn` is the
/// class that actually declares the member (see `find_field_owner`/
/// `find_method_owner`); `accessor_fqcn` is the class containing the
/// reference being checked.
pub fn is_accessible(
    classes: &ClassTable,
    visibility: Visibility,
    declaring_fqcn: &str,
    accessor_fqcn: &str,
) -> bool {
    match visibility {
        Visibility::Public => true,
        Visibility::Private => accessor_fqcn == declaring_fqcn,
        Visibility::Protected => {
            accessor_fqcn == declaring_fqcn
                || is_subclass_or_same(classes, accessor_fqcn, declaring_fqcn)
        }
    }
}

pub fn build_class_table(files: &[SourceFile]) -> ClassTable {
    let mut table = HashMap::with_capacity(files.len());
    for file in files {
        let fqcn = fqcn_of(file);
        let imports = import_map(file, files);
        let info = match &file.item {
            SourceItem::Class(class) => {
                let fields = class
                    .fields
                    .iter()
                    .map(|f| FieldInfo {
                        name: f.name.clone(),
                        ty: resolve_type(&f.ty, &imports),
                        visibility: f.visibility,
                        readonly: f.readonly,
                        is_static: f.is_static,
                    })
                    .collect();
                let resolve_throws = |m: &nl_syntax::ast::MethodDecl| -> Vec<Type> {
                    m.throws
                        .iter()
                        .map(|n| Type::Named(imports.get(n).cloned().unwrap_or_else(|| n.clone())))
                        .collect()
                };
                let methods = class
                    .methods
                    .iter()
                    .filter(|m| m.kind == MethodKind::Normal)
                    .map(|m| MethodInfo {
                        name: m.name.clone(),
                        params: m
                            .params
                            .iter()
                            .map(|p| resolve_type(&p.ty, &imports))
                            .collect(),
                        param_names: m.params.iter().map(|p| p.name.clone()).collect(),
                        is_ref: m.params.iter().map(|p| p.is_ref).collect(),
                        required_count: required_count(&m.params),
                        return_ty: resolve_type(&m.return_type, &imports),
                        is_static: m.is_static,
                        is_const: m.is_const,
                        visibility: m.visibility,
                        is_abstract: m.is_abstract,
                        is_final: m.is_final,
                        is_nodiscard: m.is_nodiscard,
                        throws: resolve_throws(m),
                    })
                    .collect();
                let ctors = class
                    .methods
                    .iter()
                    .filter(|m| m.kind == MethodKind::Constructor)
                    .map(|m| CtorInfo {
                        params: m
                            .params
                            .iter()
                            .map(|p| resolve_type(&p.ty, &imports))
                            .collect(),
                        param_names: m.params.iter().map(|p| p.name.clone()).collect(),
                        is_ref: m.params.iter().map(|p| p.is_ref).collect(),
                        required_count: required_count(&m.params),
                        throws: resolve_throws(m),
                        visibility: m.visibility,
                    })
                    .collect();
                let implements = class
                    .implements
                    .iter()
                    .map(|n| imports.get(n).cloned().unwrap_or_else(|| n.clone()))
                    .collect();
                let extends = class
                    .extends
                    .as_ref()
                    .map(|n| imports.get(n).cloned().unwrap_or_else(|| n.clone()));
                ClassInfo {
                    extends,
                    implements,
                    fields,
                    methods,
                    ctors,
                    is_readonly: class.is_readonly,
                    is_abstract: class.is_abstract,
                    is_final: class.is_final,
                    is_enum: class.is_enum,
                    enum_cases: class.enum_cases.clone(),
                }
            }
            SourceItem::Interface(iface) => {
                let methods = iface
                    .methods
                    .iter()
                    .map(|m| MethodInfo {
                        name: m.name.clone(),
                        params: m
                            .params
                            .iter()
                            .map(|p| resolve_type(&p.ty, &imports))
                            .collect(),
                        param_names: m.params.iter().map(|p| p.name.clone()).collect(),
                        is_ref: m.params.iter().map(|p| p.is_ref).collect(),
                        required_count: required_count(&m.params),
                        return_ty: resolve_type(&m.return_type, &imports),
                        is_static: false,
                        is_const: m.is_const,
                        // specs.md § Interfaces — interface methods are
                        // always public (a contract meant to be implemented
                        // and called from anywhere).
                        visibility: Visibility::Public,
                        // Interface method "abstractness" is a distinct,
                        // pre-existing mechanism (implements-conformance,
                        // handled leniently elsewhere in this checker) —
                        // unrelated to the `abstract class`/E033 rule below.
                        is_abstract: false,
                        is_final: false,
                        is_nodiscard: false,
                        throws: Vec::new(),
                    })
                    .collect();
                // compiler.md § Interface inheritance — `extends` parents
                // are stored in `implements` (see `interface_extends`'s doc
                // comment for why that field does double duty).
                let extends_ifaces = iface
                    .extends
                    .iter()
                    .map(|n| imports.get(n).cloned().unwrap_or_else(|| n.clone()))
                    .collect();
                ClassInfo {
                    extends: None,
                    implements: extends_ifaces,
                    fields: Vec::new(),
                    methods,
                    ctors: Vec::new(),
                    is_readonly: false,
                    is_abstract: false,
                    is_final: false,
                    is_enum: false,
                    enum_cases: Vec::new(),
                }
            }
        };
        table.insert(fqcn, info);
    }
    table
}
