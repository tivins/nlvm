//! Cross-file class/interface table — built once from every `SourceFile` in
//! a program so codegen can resolve `new`, field access, and instance method
//! calls that reference a class defined in a different file. Mirrors the
//! (deliberately lenient) approach `nl-sema` takes for cross-file lookups:
//! this crate owns its own view rather than depending on `nl-sema`.

use std::collections::HashMap;

use nl_syntax::ast::{Arg, Expr, MethodKind, SourceFile, SourceItem, Type};

#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub name: String,
    /// Resolved (FQCN, not source-simple-name) type.
    pub ty: Type,
    /// The field's declared initializer, if any. For an enum case constant
    /// (`nl_syntax::parser::parse_enum_decl` always gives one an `init`),
    /// `ClassName.CaseName` re-compiles this expression at each reference
    /// site rather than reading real static storage (see
    /// `crate::expr::Emitter::compile_field_access`'s enum branch — case
    /// values are compile-time constants). For an ordinary `static` field on
    /// a non-enum class, this is instead compiled once into a synthetic
    /// `<clinit>` method (see `crate::compile_file`) that runs at program
    /// load time and writes through `SET_STATIC`.
    pub init: Option<Expr>,
    /// `static` modifier — see `nl_syntax::ast::FieldDecl::is_static`.
    /// Non-enum statics are backed by real per-class storage
    /// (`GET_STATIC`/`SET_STATIC`, `nl_vm::Program`'s static table); an
    /// enum's own case "fields" also carry this flag at the bytecode level
    /// but are never read back through it — see `init`'s doc comment.
    pub is_static: bool,
}

#[derive(Debug, Clone)]
pub struct CtorInfo {
    /// Resolved parameter types.
    pub params: Vec<Type>,
    /// Parallel to `params` — parameter names, needed to bind named
    /// arguments (compiler.md § Named and optional parameter rules).
    pub param_names: Vec<String>,
    /// Parallel to `params` — a parameter's default value expression, if
    /// it's optional. nl-sema has already validated (E026) that every
    /// present one is a compile-time constant, so it's safe to compile
    /// directly wherever the call site omits that argument.
    pub defaults: Vec<Option<Expr>>,
    /// Parallel to `params` — compiler.md § Ref parameter rules; vm.md §
    /// Ref parameters (boxing). nl-sema has already validated E020-E022.
    pub is_ref: Vec<bool>,
}

#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub name: String,
    /// Resolved parameter types.
    pub params: Vec<Type>,
    /// See `CtorInfo::param_names`.
    pub param_names: Vec<String>,
    /// See `CtorInfo::defaults`.
    pub defaults: Vec<Option<Expr>>,
    /// See `CtorInfo::is_ref`.
    pub is_ref: Vec<bool>,
    /// Resolved return type.
    pub return_ty: Type,
    pub is_static: bool,
}

/// How many leading parameters have no default value — everything past
/// this index is optional (specs.md § Optional parameters).
fn required_count(defaults: &[Option<Expr>]) -> usize {
    defaults.iter().take_while(|d| d.is_none()).count()
}

/// Whether a call supplying `argc` total arguments (positional + named)
/// could bind against a callee with `required`/`total` parameters — see
/// `nl_sema::class_table::arity_in_range` (same rationale, independent
/// copy — this crate doesn't depend on `nl-sema`).
fn arity_in_range(required: usize, total: usize, argc: usize) -> bool {
    required <= argc && argc <= total
}

/// Mangled name of a fully-resolved (post-`nl_syntax::monomorphize::expand`)
/// type. Must match `nl_syntax::monomorphize`'s own (private) `mangle_type`
/// exactly for the flat shapes handled here — that's the name a `ref`
/// parameter's `Box<T>` was actually monomorphized/compiled under (see that
/// module's synthesized `Box<T>` instantiations, one per distinct `ref`
/// parameter type used anywhere in the program). Restricted to the shapes
/// that can appear post-expansion (no `Type::Generic`/`Type::Union` — a
/// bare scalar, `string`, a resolved class FQCN, or an array of one of
/// those); anything else would mean nl-sema failed to reject a `ref`
/// parameter of a type that was never expected to reach here.
pub fn mangle_flat_type(ty: &Type) -> String {
    match ty {
        Type::Int => "int".to_string(),
        Type::Float => "float".to_string(),
        Type::Bool => "bool".to_string(),
        Type::Byte => "byte".to_string(),
        Type::StringT => "string".to_string(),
        Type::Named(name) => name.clone(),
        Type::Array(inner) => format!("{}[]", mangle_flat_type(inner)),
        other => format!("{other:?}"),
    }
}

/// The mangled FQCN of the `Box<T>` used to pass `inner` by `ref` — vm.md §
/// Ref parameters (boxing).
pub fn box_fqcn(inner: &Type) -> String {
    format!("Box<{}>", mangle_flat_type(inner))
}

/// compiler.md § Ref parameter rules; vm.md § Ref parameters (boxing). The
/// calling-convention parameter types for a signature: same as `params`
/// except a `ref` parameter's *physical* type on the stack/in the method
/// descriptor is `Box<T>`, not `T` — the callee reads/writes through the
/// box, and only the caller (immediately after the call returns) unboxes
/// the result back into its own variable.
pub fn calling_convention_params(params: &[Type], is_ref: &[bool]) -> Vec<Type> {
    params
        .iter()
        .zip(is_ref)
        .map(|(ty, r)| {
            if *r {
                Type::Named(box_fqcn(ty))
            } else {
                ty.clone()
            }
        })
        .collect()
}

/// compiler.md § Named and optional parameter rules — E023-E026 already
/// validated (by nl-sema, which always runs first) that `args` binds
/// against `param_names`/`defaults`. Resolves that binding into the fully
/// positional `Vec<Expr>` (one per parameter, in declared order) the rest
/// of codegen's call-emission code expects — a parameter's own expression
/// if `args` supplies it (by position or by name), otherwise its default.
pub fn resolve_positional_args(
    param_names: &[String],
    defaults: &[Option<Expr>],
    args: &[Arg],
) -> Vec<Expr> {
    let mut out: Vec<Option<Expr>> = vec![None; param_names.len()];
    for (i, arg) in args.iter().enumerate() {
        match &arg.name {
            None => {
                if i < out.len() {
                    out[i] = Some(arg.value.clone());
                }
            }
            Some(name) => {
                if let Some(p_idx) = param_names.iter().position(|n| n == name) {
                    out[p_idx] = Some(arg.value.clone());
                }
            }
        }
    }
    out.into_iter()
        .enumerate()
        .map(|(i, v)| {
            v.unwrap_or_else(|| {
                defaults[i]
                    .clone()
                    .expect("nl-sema already validated every required parameter is bound")
            })
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct ClassInfo {
    /// Resolved FQCN of the direct superclass (`extends`), if any.
    pub extends: Option<String>,
    /// Resolved FQCNs of directly implemented interfaces (a class) or
    /// directly extended parent interfaces (an interface) — compiler.md §
    /// Interface inheritance. Reuses one field for both, like
    /// `nl_sema::class_table::ClassInfo::implements` (see that field's doc
    /// comment for why): used by `interface_closure` to flatten the whole
    /// transitive interface set into a class's compiled `Module.interfaces`
    /// list, so `nl_vm::interpreter::is_instance_of`'s exact-FQCN interface
    /// scan (no interface-`extends` awareness of its own) still resolves
    /// `instanceof`/upcasts correctly against an interface's ancestors.
    pub implements: Vec<String>,
    pub fields: Vec<FieldInfo>,
    pub ctors: Vec<CtorInfo>,
    pub methods: Vec<MethodInfo>,
    /// specs.md § Enums; see `nl_syntax::ast::ClassDecl::is_enum`. Codegen
    /// never needs the case-name list itself (unlike `nl-sema`, which uses
    /// it for match exhaustiveness — see `nl_sema::class_table::ClassInfo`)
    /// — case constants are recompiled from `fields[i].init` by name at
    /// each reference site instead (see `FieldInfo::init`).
    pub is_enum: bool,
}

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
/// in the same namespace (specs.md § Imports: "another type in the same
/// namespace" can conflict with an import, which only makes sense if
/// same-namespace types are already in scope without one — see
/// `m5_0020`'s `Dog implements Animal` with no `use`), plus explicit `use`
/// imports.
pub fn import_map(file: &SourceFile, all_files: &[SourceFile]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    // Built-in exception classes are globally visible without a `use` — see
    // nl_syntax::prelude. Seeded first so a file's own declarations/`use`s
    // (below) can still shadow a same-named builtin.
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

/// Resolves every `Named` component of `ty` from a simple name to its FQCN
/// using `imports`; unresolvable names are left as-is (lenient — surfaces as
/// a clear "unknown class" error at the point of use, not here).
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

pub fn build_class_table(files: &[SourceFile]) -> HashMap<String, ClassInfo> {
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
                        init: f.init.clone(),
                        is_static: f.is_static,
                    })
                    .collect();

                let mut ctors = Vec::new();
                let mut methods = Vec::new();
                for m in &class.methods {
                    let params: Vec<Type> = m
                        .params
                        .iter()
                        .map(|p| resolve_type(&p.ty, &imports))
                        .collect();
                    let param_names: Vec<String> =
                        m.params.iter().map(|p| p.name.clone()).collect();
                    let defaults: Vec<Option<Expr>> =
                        m.params.iter().map(|p| p.default.clone()).collect();
                    let is_ref: Vec<bool> = m.params.iter().map(|p| p.is_ref).collect();
                    match m.kind {
                        MethodKind::Constructor => ctors.push(CtorInfo {
                            params,
                            param_names,
                            defaults,
                            is_ref,
                        }),
                        MethodKind::Destructor => {}
                        MethodKind::Normal => methods.push(MethodInfo {
                            name: m.name.clone(),
                            params,
                            param_names,
                            defaults,
                            is_ref,
                            return_ty: resolve_type(&m.return_type, &imports),
                            is_static: m.is_static,
                        }),
                    }
                }

                let extends = class
                    .extends
                    .as_ref()
                    .map(|n| imports.get(n).cloned().unwrap_or_else(|| n.clone()));
                let implements = class
                    .implements
                    .iter()
                    .map(|n| imports.get(n).cloned().unwrap_or_else(|| n.clone()))
                    .collect();
                ClassInfo {
                    extends,
                    implements,
                    fields,
                    ctors,
                    methods,
                    is_enum: class.is_enum,
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
                        defaults: m.params.iter().map(|p| p.default.clone()).collect(),
                        is_ref: m.params.iter().map(|p| p.is_ref).collect(),
                        return_ty: resolve_type(&m.return_type, &imports),
                        is_static: false,
                    })
                    .collect();
                // compiler.md § Interface inheritance — `extends` parents,
                // stored in `implements` (see that field's doc comment).
                let implements = iface
                    .extends
                    .iter()
                    .map(|n| imports.get(n).cloned().unwrap_or_else(|| n.clone()))
                    .collect();
                ClassInfo {
                    extends: None,
                    implements,
                    fields: Vec::new(),
                    ctors: Vec::new(),
                    methods,
                    is_enum: false,
                }
            }
        };
        table.insert(fqcn, info);
    }
    table
}

/// Every interface FQCN in `start`'s (a class's directly-`implements`-ed
/// interfaces) transitive `extends` closure, flattened — compiler.md §
/// Interface inheritance: "an implementing class can be upcast to any
/// interface in the hierarchy, and `instanceof` returns true for all of
/// them". Used to populate a class's compiled `Module.interfaces` (see
/// `compile_file` in `lib.rs`) with every ancestor interface, not just the
/// ones written directly after `implements` — `nl_vm::interpreter`'s
/// `is_instance_of`/`implements_interface` only ever does an exact-FQCN scan
/// of that list, with no interface-`extends` awareness of its own, so the
/// flattening has to happen here, at compile time.
pub fn interface_closure<'a>(
    classes: &'a HashMap<String, ClassInfo>,
    start: impl IntoIterator<Item = &'a String>,
) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut queue: Vec<String> = start.into_iter().cloned().collect();
    let mut out = Vec::new();
    while let Some(fqcn) = queue.pop() {
        if !seen.insert(fqcn.clone()) {
            continue;
        }
        out.push(fqcn.clone());
        if let Some(info) = classes.get(&fqcn) {
            queue.extend(info.implements.iter().cloned());
        }
    }
    out
}

/// Whether `sub` is `sup` itself or (transitively) extends it — independent
/// copy of `nl_sema::class_table::is_subclass_or_same` (this crate doesn't
/// depend on `nl-sema` — see this module's doc comment).
pub fn is_subclass_or_same(classes: &HashMap<String, ClassInfo>, sub: &str, sup: &str) -> bool {
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

/// Whether `fqcn` (transitively, via `extends`) implements interface
/// `target` — independent copy of
/// `nl_sema::class_table::implements_interface`, built on `interface_closure`
/// (already flattens an interface's own `extends` ancestors into
/// `ClassInfo::implements`, see that field's doc comment).
pub fn implements_interface(classes: &HashMap<String, ClassInfo>, fqcn: &str, target: &str) -> bool {
    let mut current = fqcn;
    loop {
        let Some(info) = classes.get(current) else {
            return false;
        };
        if interface_closure(classes, &info.implements)
            .iter()
            .any(|i| i == target)
        {
            return true;
        }
        match info.extends.as_deref() {
            Some(parent) => current = parent,
            None => return false,
        }
    }
}

/// How well a call argument's type matches a declared parameter, for
/// overload resolution — independent copy of
/// `nl_sema::class_table::overload_param_score`'s rule (`Some(0)` exact,
/// `Some(1)` compatible via numeric widening/subtyping/a nullable target,
/// `None` incompatible; `arg = None` — this pass couldn't confidently type
/// that argument without emitting bytecode, see `Emitter::overload_arg_ty`
/// — always scores `Some(0)`, never disqualifying a candidate it can't
/// actually evaluate). Must stay in lockstep with the nl-sema copy: see
/// `Emitter::overload_arg_ty`'s doc comment for why.
fn overload_param_score(classes: &HashMap<String, ClassInfo>, arg: Option<&Type>, param: &Type) -> Option<u32> {
    let arg = arg?;
    if arg == param {
        return Some(0);
    }
    if matches!(arg, Type::NullT) {
        let nullable = matches!(param, Type::Union(members) if members.iter().any(|m| matches!(m, Type::NullT)));
        return if nullable { Some(1) } else { None };
    }
    match (arg, param) {
        (Type::Named(from), Type::Named(to)) => {
            if is_subclass_or_same(classes, from, to) || implements_interface(classes, from, to) {
                Some(1)
            } else {
                None
            }
        }
        (Type::Int | Type::Float | Type::Byte, Type::Int | Type::Float | Type::Byte) => Some(1),
        _ => None,
    }
}

/// Picks the best-matching candidate among `candidates` (already filtered to
/// same-name/arity-compatible overloads by the caller) — independent copy of
/// `nl_sema::class_table::best_overload`'s tie-breaking rule (first declared
/// wins a tie or an all-incompatible outcome; trivial passthrough for the
/// overwhelmingly common single-candidate case).
fn best_overload<'c, T>(
    classes: &HashMap<String, ClassInfo>,
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

/// Arity-compatible constructor resolution, picking the best-matching
/// overload by `arg_tys` rather than just the first declared — the
/// codegen-side half of the
/// "arity-only" fix (see `nl_sema::class_table::find_ctor_overload`, which
/// this must always agree with: nl-sema already validated the call against
/// whichever overload *it* picked, so if this picks a different one here,
/// the emitted bytecode targets a method nl-sema never actually checked
/// this call against).
pub fn find_ctor_overload<'c>(
    classes: &'c HashMap<String, ClassInfo>,
    fqcn: &str,
    argc: usize,
    arg_tys: &[Option<Type>],
) -> Option<&'c CtorInfo> {
    let candidates: Vec<&CtorInfo> = classes
        .get(fqcn)?
        .ctors
        .iter()
        .filter(|c| arity_in_range(required_count(&c.defaults), c.params.len(), argc))
        .collect();
    best_overload(classes, &candidates, |c| &c.params, arg_tys)
}

/// Walks `fqcn`'s `extends` chain, so a method declared on an ancestor class
/// resolves from a subclass reference too (instance calls, `super.m(...)`).
pub fn find_method<'c>(
    classes: &'c HashMap<String, ClassInfo>,
    fqcn: &str,
    name: &str,
    argc: usize,
) -> Option<&'c MethodInfo> {
    let mut current = fqcn;
    loop {
        let info = classes.get(current)?;
        if let Some(m) = info.methods.iter().find(|m| {
            m.name == name && arity_in_range(required_count(&m.defaults), m.params.len(), argc)
        }) {
            return Some(m);
        }
        current = info.extends.as_deref()?;
    }
}

/// Like `find_method`, but picks the best-matching overload by `arg_tys`
/// instead of just the first declared at each class level — see
/// `find_ctor_overload`'s doc comment for why this must always agree with
/// `nl_sema::class_table::find_method_owner_overload`.
pub fn find_method_overload<'c>(
    classes: &'c HashMap<String, ClassInfo>,
    fqcn: &str,
    name: &str,
    argc: usize,
    arg_tys: &[Option<Type>],
) -> Option<&'c MethodInfo> {
    let mut current = fqcn;
    loop {
        let info = classes.get(current)?;
        let candidates: Vec<&MethodInfo> = info
            .methods
            .iter()
            .filter(|m| {
                m.name == name && arity_in_range(required_count(&m.defaults), m.params.len(), argc)
            })
            .collect();
        if let Some(m) = best_overload(classes, &candidates, |m| &m.params, arg_tys) {
            return Some(m);
        }
        current = info.extends.as_deref()?;
    }
}

/// Like `find_method`, but for operator overloads (specs.md § Operator
/// Overloading): matches `name` (an `operator<sym>` canonical name — see
/// `nl_syntax::parser::parse_operator_symbol`) against an *exact* parameter
/// type list rather than arity alone, so e.g. `Vector2` can overload
/// `operator+` once for `Vector2` and once for `int` without either call
/// site becoming ambiguous — mirrors `nl_sema::class_table::find_operator_method`.
/// Static methods are skipped (operator overloads are instance methods
/// only).
pub fn find_operator_method<'c>(
    classes: &'c HashMap<String, ClassInfo>,
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
            .find(|m| m.name == name && !m.is_static && m.params == params)
        {
            return Some(m);
        }
        current = info.extends.as_deref()?;
    }
}

/// Like `find_method`, for fields.
pub fn find_field<'c>(
    classes: &'c HashMap<String, ClassInfo>,
    fqcn: &str,
    name: &str,
) -> Option<&'c FieldInfo> {
    let mut current = fqcn;
    loop {
        let info = classes.get(current)?;
        if let Some(f) = info.fields.iter().find(|f| f.name == name) {
            return Some(f);
        }
        current = info.extends.as_deref()?;
    }
}

/// Like `find_field`, but also returns the FQCN of the class that actually
/// *declares* the field — needed for `GET_STATIC`/`SET_STATIC`, whose
/// constant-pool `FieldRef` must name the declaring class (where
/// `nl_vm::Program`'s static storage lives), not whatever subclass name a
/// reference happened to spell out.
pub fn find_field_owner<'c>(
    classes: &'c HashMap<String, ClassInfo>,
    fqcn: &str,
    name: &str,
) -> Option<(String, &'c FieldInfo)> {
    let mut current = fqcn;
    loop {
        let info = classes.get(current)?;
        if let Some(f) = info.fields.iter().find(|f| f.name == name) {
            return Some((current.to_string(), f));
        }
        current = info.extends.as_deref()?;
    }
}
