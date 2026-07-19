//! Cross-file class/interface table, mirroring `nl_codegen::class_table`
//! (kept as a separate, independent view rather than a shared dependency —
//! sema only needs enough of it for lenient existence/type-shape checks).

use std::collections::HashMap;

use nl_syntax::ast::{MethodKind, SourceFile, SourceItem, Type, Visibility};

#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub name: String,
    pub ty: Type,
    pub visibility: Visibility,
    pub readonly: bool,
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

/// Best-effort constructor resolution by arity — mirrors `nl_codegen`'s
/// `find_ctor` (constructor overloads are only distinguished by arity this
/// phase; see PLAN.md). Range-based rather than exact since optional
/// parameters (compiler.md § Named and optional parameter rules) let a
/// single constructor accept a span of argument counts.
pub fn find_ctor<'c>(classes: &'c ClassTable, fqcn: &str, argc: usize) -> Option<&'c CtorInfo> {
    classes
        .get(fqcn)?
        .ctors
        .iter()
        .find(|c| arity_in_range(c.required_count, c.params.len(), argc))
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

/// compiler.md § Template instantiation, "Bounded type parameters" — E037.
/// Whether `concrete_fqcn` (or an ancestor, via `extends`) satisfies
/// `bound_fqcn` — either by being a subclass of it, or by `implements`-ing it
/// directly at some point in the `extends` chain (interfaces don't
/// themselves `extend` other interfaces in this class table — see
/// `ClassInfo::implements`'s doc comment — so no further transitivity is
/// needed there).
pub fn satisfies_bound(classes: &ClassTable, concrete_fqcn: &str, bound_fqcn: &str) -> bool {
    if is_subclass_or_same(classes, concrete_fqcn, bound_fqcn) {
        return true;
    }
    let mut current = concrete_fqcn;
    loop {
        let Some(info) = classes.get(current) else {
            return false;
        };
        if info.implements.iter().any(|i| i == bound_fqcn) {
            return true;
        }
        match info.extends.as_deref() {
            Some(parent) => current = parent,
            None => return false,
        }
    }
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
                        throws: Vec::new(),
                    })
                    .collect();
                ClassInfo {
                    extends: None,
                    implements: Vec::new(),
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
