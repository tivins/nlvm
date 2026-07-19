//! Template class monomorphization — specs.md § Template class, vm.md §
//! Templates (monomorphization). Pure AST-to-AST rewriting, run once ahead
//! of both `nl-sema` and `nl-codegen` (both call `expand` on the same input
//! — see their `check_compile`/`compile_program` entry points — so they
//! always agree on the expanded program):
//!
//! 1. Collect every distinct `(template class, concrete type arguments)`
//!    combination actually used anywhere in the program (`Type::Generic` in
//!    a field/param/return/local-variable type, or `new Vector<int>(...)`).
//! 2. For each, synthesize a monomorphized `SourceFile`: the template's
//!    `ClassDecl` with every occurrence of a type parameter substituted for
//!    its concrete argument, named `"Vector<int>"` (matching vm.md's
//!    native-template mangling, e.g. `"system.List<int>"`).
//! 3. Rewrite every reference to the generic form into a reference to the
//!    mangled name, and drop the original (uninstantiable on its own)
//!    template `ClassDecl`s from the file list.
//!
//! After `expand` returns, neither `Type::Generic` nor a non-empty type-arg
//! list on `Expr::New` should appear anywhere in the result.
//!
//! **Native template classes** (`system.List<T>`, `system.Map<K,V>` — vm.md
//! § Templates (monomorphization), stdlib.md § system.List/system.Map) go
//! through the exact same name-mangling here (`is_native_generic`) even
//! though there is no `ClassDecl` to substitute into: no `SourceFile` is
//! synthesized for them (`nl_vm::native` provides the implementation
//! directly, keyed by the mangled FQCN), only the rewrite step runs, same
//! mangled-name format as user templates. This is why `expand` no longer
//! short-circuits when there are zero user templates in the program — a
//! program with only `system.List`/`system.Map` usages and no user
//! `template class` still needs the rewrite pass.
//!
//! Deliberately out of scope (see PLAN.md's generics gap): bounded type
//! parameters are parsed but not enforced, template *methods* are not
//! supported (only template classes), and there is no `Self`/`type`
//! contextual sugar inside a template body — bodies must spell out the type
//! parameter's own name. Native templates nested inside another native or
//! user template's type argument (e.g. `system.List<system.List<int>>`) are
//! not exercised/tested.

use std::collections::HashMap;

use crate::ast::{
    Arg, Block, ClassDecl, ClosureBody, Expr, FieldDecl, LValue, MethodDecl, SourceFile,
    SourceItem, Stmt, StmtKind, Type,
};

struct TemplateInfo {
    namespace: Vec<String>,
    decl: ClassDecl,
    path: String,
}

/// Every distinct `(template class, concrete type arguments)` combination
/// actually used anywhere in `files`, keyed by mangled FQCN — step 1 of
/// `expand`, exposed standalone so `nl-sema` can check bounded type
/// parameters (compiler.md § Template instantiation, E037) against the
/// *original* (pre-expansion) `Type::Generic`/`new T<...>(...)` sites, which
/// no longer exist once `expand` has rewritten them away.
pub fn collect_instantiations(files: &[SourceFile]) -> HashMap<String, (String, Vec<Type>)> {
    let mut templates: HashMap<String, TemplateInfo> = HashMap::new();
    for file in files {
        if let SourceItem::Class(class) = &file.item {
            if !class.type_params.is_empty() {
                templates.insert(
                    fqcn_of(file),
                    TemplateInfo {
                        namespace: file.namespace.clone(),
                        decl: class.clone(),
                        path: file.path.clone(),
                    },
                );
            }
        }
    }
    let mut instantiations: HashMap<String, (String, Vec<Type>)> = HashMap::new();
    for file in files {
        let imports = import_map(file, files);
        collect_file(file, &imports, &templates, &mut instantiations);
    }
    instantiations
}

pub fn expand(files: Vec<SourceFile>) -> Vec<SourceFile> {
    let mut templates: HashMap<String, TemplateInfo> = HashMap::new();
    for file in &files {
        if let SourceItem::Class(class) = &file.item {
            if !class.type_params.is_empty() {
                templates.insert(
                    fqcn_of(file),
                    TemplateInfo {
                        namespace: file.namespace.clone(),
                        decl: class.clone(),
                        path: file.path.clone(),
                    },
                );
            }
        }
    }

    // mangled FQCN ("ns.Vector<int>") -> (template FQCN, resolved concrete args).
    let mut instantiations: HashMap<String, (String, Vec<Type>)> = HashMap::new();
    for file in &files {
        let imports = import_map(file, &files);
        collect_file(file, &imports, &templates, &mut instantiations);
    }

    let mut out = Vec::with_capacity(files.len() + instantiations.len());
    for file in &files {
        if let SourceItem::Class(class) = &file.item {
            if !class.type_params.is_empty() {
                continue; // template classes are never compiled as-is
            }
        }
        let imports = import_map(file, &files);
        out.push(rewrite_file(file, &imports, &templates));
    }

    let mut generated: Vec<SourceFile> = Vec::with_capacity(instantiations.len());
    for (mangled_fqcn, (template_fqcn, args)) in &instantiations {
        generated.push(generate_instantiation(
            &templates,
            mangled_fqcn,
            template_fqcn,
            args,
        ));
    }

    // vm.md § Ref parameters (boxing) — synthesize a `Box<T>` instantiation
    // (`nl_syntax::prelude::box_class`) for every concrete `T` used as a
    // `ref` parameter's type, across both the already-expanded plain
    // classes (`out`) and the just-generated template instantiations
    // (`generated`). A `ref` parameter inside a template body only has a
    // concrete type *after* substitution, so this can't run any earlier
    // than here — but by this point every parameter type in both lists is
    // fully concrete (`out` went through `rewrite_file`, `generated`
    // through `subst_class`), so no `imports` resolution is needed.
    let mut box_instantiations: HashMap<String, (String, Vec<Type>)> = HashMap::new();
    for file in out.iter().chain(generated.iter()) {
        if let SourceItem::Class(class) = &file.item {
            collect_ref_box_requests(class, &mut box_instantiations);
        }
    }
    for (mangled_fqcn, (template_fqcn, args)) in &box_instantiations {
        generated.push(generate_instantiation(
            &templates,
            mangled_fqcn,
            template_fqcn,
            args,
        ));
    }

    out.extend(generated);
    out
}

/// Synthesizes the monomorphized `SourceFile` for one `(template, concrete
/// args)` instantiation — the template's `ClassDecl` with every type
/// parameter substituted, renamed to the mangled FQCN (e.g. `"Vector<int>"`,
/// or `"Box<int>"` for a ref-parameter box). Shared by ordinary user/native
/// template instantiations and the synthetic `Box<T>` ones.
fn generate_instantiation(
    templates: &HashMap<String, TemplateInfo>,
    mangled_fqcn: &str,
    template_fqcn: &str,
    args: &[Type],
) -> SourceFile {
    let template = &templates[template_fqcn];
    let subst: HashMap<String, Type> = template
        .decl
        .type_params
        .iter()
        .map(|tp| tp.name.clone())
        .zip(args.iter().cloned())
        .collect();
    let mut decl = subst_class(&template.decl, &subst);
    decl.type_params = Vec::new();
    decl.name = mangled_fqcn
        .strip_prefix(&format!("{}.", template.namespace.join(".")))
        .unwrap_or(mangled_fqcn)
        .to_string();
    // For a namespace-less template, `strip_prefix` above has nothing to
    // strip (empty namespace never produces a `"."`-joined prefix), so
    // `decl.name` is already the whole mangled FQCN in that case —
    // consistent with `fqcn_of` treating the bare name as the FQCN.
    SourceFile {
        namespace: template.namespace.clone(),
        uses: Vec::new(),
        item: SourceItem::Class(decl),
        path: template.path.clone(),
    }
}

/// See `expand`'s "Ref parameters (boxing)" comment. `class`'s parameter
/// types are already fully concrete by the time this runs.
fn collect_ref_box_requests(class: &ClassDecl, out: &mut HashMap<String, (String, Vec<Type>)>) {
    for m in &class.methods {
        for p in &m.params {
            if p.is_ref {
                let mangled = format!("Box<{}>", mangle_type(&p.ty));
                out.insert(mangled, ("Box".to_string(), vec![p.ty.clone()]));
            }
        }
    }
}

// ---------------------------------------------------------------------
// Minimal self-contained name resolution (mirrors nl-sema/nl-codegen's
// `class_table::{fqcn_of, import_map}`, duplicated here since this crate
// can't depend on either of them — this module only needs enough to
// resolve a template class *reference* to its FQCN, not general class-table
// lookups).
// ---------------------------------------------------------------------

fn fqcn_of(file: &SourceFile) -> String {
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

fn import_map(file: &SourceFile, all_files: &[SourceFile]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for other in all_files {
        if other.namespace == file.namespace {
            let simple = match &other.item {
                SourceItem::Class(c) => c.name.clone(),
                SourceItem::Interface(i) => i.name.clone(),
            };
            map.insert(simple, fqcn_of(other));
        }
    }
    let simple = match &file.item {
        SourceItem::Class(c) => c.name.clone(),
        SourceItem::Interface(i) => i.name.clone(),
    };
    map.insert(simple, fqcn_of(file));
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

fn resolve_name(name: &str, imports: &HashMap<String, String>) -> String {
    imports
        .get(name)
        .cloned()
        .unwrap_or_else(|| name.to_string())
}

/// `system.List`/`system.Map` — the two native template classes (vm.md §
/// Templates (monomorphization)). Unlike a user `template class`, there is
/// no `ClassDecl` to substitute into; only the name gets mangled the same
/// way, and `nl_vm::native`/`nl_sema`/`nl_codegen`'s own `native_generics`
/// modules recognize the mangled FQCN directly.
fn is_native_generic(fqcn: &str) -> bool {
    matches!(fqcn, "system.List" | "system.Map")
}

/// Canonical string form of a type, used both for the mangled class name
/// (`"Vector<int>"`) and to key/deduplicate instantiations.
fn mangle_type(ty: &Type) -> String {
    match ty {
        Type::Int => "int".to_string(),
        Type::Float => "float".to_string(),
        Type::Bool => "bool".to_string(),
        Type::Byte => "byte".to_string(),
        Type::StringT => "string".to_string(),
        Type::Void => "void".to_string(),
        Type::NullT => "null".to_string(),
        Type::Array(inner) => format!("{}[]", mangle_type(inner)),
        Type::Named(name) => name.clone(),
        Type::Union(members) => members
            .iter()
            .map(mangle_type)
            .collect::<Vec<_>>()
            .join("|"),
        Type::Generic(name, args) => format!(
            "{name}<{}>",
            args.iter().map(mangle_type).collect::<Vec<_>>().join(", ")
        ),
        Type::Function {
            params,
            return_type,
            ..
        } => format!(
            "({}) => {}",
            params.iter().map(mangle_type).collect::<Vec<_>>().join(", "),
            mangle_type(return_type)
        ),
    }
}

/// Resolves every bare `Named` component of `ty` to an FQCN via `imports`
/// (needed so two references to the same instantiation, spelled with
/// different `use`-driven simple names, still mangle identically).
fn resolve_type_names(ty: &Type, imports: &HashMap<String, String>) -> Type {
    match ty {
        Type::Named(name) => Type::Named(resolve_name(name, imports)),
        Type::Array(inner) => Type::Array(Box::new(resolve_type_names(inner, imports))),
        Type::Union(members) => Type::Union(
            members
                .iter()
                .map(|m| resolve_type_names(m, imports))
                .collect(),
        ),
        Type::Generic(name, args) => Type::Generic(
            resolve_name(name, imports),
            args.iter()
                .map(|a| resolve_type_names(a, imports))
                .collect(),
        ),
        Type::Function {
            params,
            return_type,
            throws,
        } => Type::Function {
            params: params
                .iter()
                .map(|p| resolve_type_names(p, imports))
                .collect(),
            return_type: Box::new(resolve_type_names(return_type, imports)),
            throws: throws.clone(),
        },
        other => other.clone(),
    }
}

// ---------------------------------------------------------------------
// Pass 1: collect every `(template, concrete args)` combination used
// anywhere in the program.
// ---------------------------------------------------------------------

fn collect_file(
    file: &SourceFile,
    imports: &HashMap<String, String>,
    templates: &HashMap<String, TemplateInfo>,
    out: &mut HashMap<String, (String, Vec<Type>)>,
) {
    let SourceItem::Class(class) = &file.item else {
        return;
    };
    for f in &class.fields {
        collect_type(&f.ty, imports, templates, out);
        if let Some(e) = &f.init {
            collect_expr(e, imports, templates, out);
        }
    }
    for m in &class.methods {
        for p in &m.params {
            collect_type(&p.ty, imports, templates, out);
        }
        collect_type(&m.return_type, imports, templates, out);
        collect_block(&m.body, imports, templates, out);
    }
}

fn collect_type(
    ty: &Type,
    imports: &HashMap<String, String>,
    templates: &HashMap<String, TemplateInfo>,
    out: &mut HashMap<String, (String, Vec<Type>)>,
) {
    match ty {
        Type::Generic(name, args) => {
            for a in args {
                collect_type(a, imports, templates, out);
            }
            let fqcn = resolve_name(name, imports);
            if templates.contains_key(&fqcn) {
                let resolved_args: Vec<Type> = args
                    .iter()
                    .map(|a| resolve_type_names(a, imports))
                    .collect();
                let mangled = format!(
                    "{fqcn}<{}>",
                    resolved_args
                        .iter()
                        .map(mangle_type)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                out.insert(mangled, (fqcn, resolved_args));
            }
        }
        Type::Array(inner) => collect_type(inner, imports, templates, out),
        Type::Union(members) => {
            for m in members {
                collect_type(m, imports, templates, out);
            }
        }
        Type::Function {
            params,
            return_type,
            ..
        } => {
            for p in params {
                collect_type(p, imports, templates, out);
            }
            collect_type(return_type, imports, templates, out);
        }
        _ => {}
    }
}

fn collect_block(
    block: &Block,
    imports: &HashMap<String, String>,
    templates: &HashMap<String, TemplateInfo>,
    out: &mut HashMap<String, (String, Vec<Type>)>,
) {
    for stmt in block {
        collect_stmt(stmt, imports, templates, out);
    }
}

fn collect_stmt(
    stmt: &Stmt,
    imports: &HashMap<String, String>,
    templates: &HashMap<String, TemplateInfo>,
    out: &mut HashMap<String, (String, Vec<Type>)>,
) {
    match &stmt.kind {
        StmtKind::Return(Some(e)) | StmtKind::Throw(e) => collect_expr(e, imports, templates, out),
        StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue => {}
        StmtKind::Expr(e) => collect_expr(e, imports, templates, out),
        StmtKind::VarDecl { ty, init, .. } => {
            if let Some(t) = ty {
                collect_type(t, imports, templates, out);
            }
            if let Some(e) = init {
                collect_expr(e, imports, templates, out);
            }
        }
        StmtKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_expr(cond, imports, templates, out);
            collect_block(then_branch, imports, templates, out);
            if let Some(b) = else_branch {
                collect_block(b, imports, templates, out);
            }
        }
        StmtKind::While { cond, body } => {
            collect_expr(cond, imports, templates, out);
            collect_block(body, imports, templates, out);
        }
        StmtKind::ForEach {
            ty, iterable, body, ..
        } => {
            if let Some(t) = ty {
                collect_type(t, imports, templates, out);
            }
            collect_expr(iterable, imports, templates, out);
            collect_block(body, imports, templates, out);
        }
        StmtKind::For {
            init,
            cond,
            step,
            body,
        } => {
            for s in init {
                collect_stmt(s, imports, templates, out);
            }
            if let Some(c) = cond {
                collect_expr(c, imports, templates, out);
            }
            for e in step {
                collect_expr(e, imports, templates, out);
            }
            collect_block(body, imports, templates, out);
        }
        StmtKind::Block(b) => collect_block(b, imports, templates, out),
        StmtKind::ThisCall(args) | StmtKind::SuperCall(args) => {
            for a in args {
                collect_expr(&a.value, imports, templates, out);
            }
        }
        StmtKind::Try {
            body,
            catches,
            finally,
        } => {
            collect_block(body, imports, templates, out);
            for c in catches {
                collect_block(&c.body, imports, templates, out);
            }
            if let Some(f) = finally {
                collect_block(f, imports, templates, out);
            }
        }
    }
}

fn collect_expr(
    expr: &Expr,
    imports: &HashMap<String, String>,
    templates: &HashMap<String, TemplateInfo>,
    out: &mut HashMap<String, (String, Vec<Type>)>,
) {
    match expr {
        Expr::IntLit(_)
        | Expr::FloatLit(_)
        | Expr::BoolLit(_)
        | Expr::StringLit(_)
        | Expr::NullLit
        | Expr::This
        | Expr::Super
        | Expr::Ident(_)
        | Expr::PostIncr(_)
        | Expr::PostDecr(_) => {}
        Expr::Assign(target, value) => {
            collect_lvalue(target, imports, templates, out);
            collect_expr(value, imports, templates, out);
        }
        Expr::Call(_, args) => {
            for a in args {
                collect_expr(&a.value, imports, templates, out);
            }
        }
        Expr::New(name, type_args, args) => {
            if !type_args.is_empty() {
                for a in type_args {
                    collect_type(a, imports, templates, out);
                }
                let fqcn = resolve_name(name, imports);
                if templates.contains_key(&fqcn) {
                    let resolved_args: Vec<Type> = type_args
                        .iter()
                        .map(|a| resolve_type_names(a, imports))
                        .collect();
                    let mangled = format!(
                        "{fqcn}<{}>",
                        resolved_args
                            .iter()
                            .map(mangle_type)
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    out.insert(mangled, (fqcn, resolved_args));
                }
            }
            for a in args {
                collect_expr(&a.value, imports, templates, out);
            }
        }
        Expr::NewArray(elem_ty, dims) => {
            collect_type(elem_ty, imports, templates, out);
            for size in dims.iter().flatten() {
                collect_expr(size, imports, templates, out);
            }
        }
        Expr::NewArrayInit(elem_ty, elements) => {
            collect_type(elem_ty, imports, templates, out);
            for e in elements {
                collect_expr(e, imports, templates, out);
            }
        }
        Expr::FieldAccess(target, _) | Expr::InstanceOf(target, _) => {
            collect_expr(target, imports, templates, out)
        }
        Expr::Cast(ty, inner) => {
            collect_type(ty, imports, templates, out);
            collect_expr(inner, imports, templates, out);
        }
        Expr::MethodCall(target, _, args) => {
            collect_expr(target, imports, templates, out);
            for a in args {
                collect_expr(&a.value, imports, templates, out);
            }
        }
        Expr::Index(target, index) => {
            collect_expr(target, imports, templates, out);
            collect_expr(index, imports, templates, out);
        }
        Expr::Unary(_, inner) => collect_expr(inner, imports, templates, out),
        Expr::Binary(_, lhs, rhs) => {
            collect_expr(lhs, imports, templates, out);
            collect_expr(rhs, imports, templates, out);
        }
        Expr::Match(subject, arms) => {
            collect_expr(subject, imports, templates, out);
            for arm in arms {
                if let Some(p) = &arm.pattern {
                    collect_expr(p, imports, templates, out);
                }
                collect_expr(&arm.value, imports, templates, out);
            }
        }
        Expr::Ternary(cond, then_e, else_e) => {
            collect_expr(cond, imports, templates, out);
            collect_expr(then_e, imports, templates, out);
            collect_expr(else_e, imports, templates, out);
        }
        Expr::Coalesce(lhs, rhs) | Expr::Elvis(lhs, rhs) => {
            collect_expr(lhs, imports, templates, out);
            collect_expr(rhs, imports, templates, out);
        }
        Expr::Closure {
            params,
            return_type,
            body,
            ..
        } => {
            for p in params {
                collect_type(&p.ty, imports, templates, out);
            }
            if let Some(t) = return_type {
                collect_type(t, imports, templates, out);
            }
            match body {
                ClosureBody::Block(b) => collect_block(b, imports, templates, out),
                ClosureBody::Expr(e) => collect_expr(e, imports, templates, out),
            }
        }
    }
}

fn collect_lvalue(
    lvalue: &LValue,
    imports: &HashMap<String, String>,
    templates: &HashMap<String, TemplateInfo>,
    out: &mut HashMap<String, (String, Vec<Type>)>,
) {
    match lvalue {
        LValue::Local(_) => {}
        LValue::Field(target, _) => collect_expr(target, imports, templates, out),
        LValue::Index(target, index) => {
            collect_expr(target, imports, templates, out);
            collect_expr(index, imports, templates, out);
        }
    }
}

// ---------------------------------------------------------------------
// Pass 2a: rewrite an ordinary (non-template) file's `Type::Generic`/
// `Expr::New(with type args)` references to the mangled monomorphized name.
// ---------------------------------------------------------------------

fn rewrite_file(
    file: &SourceFile,
    imports: &HashMap<String, String>,
    templates: &HashMap<String, TemplateInfo>,
) -> SourceFile {
    let item = match &file.item {
        SourceItem::Class(class) => SourceItem::Class(rewrite_class(class, imports, templates)),
        SourceItem::Interface(iface) => SourceItem::Interface(iface.clone()),
    };
    SourceFile {
        namespace: file.namespace.clone(),
        uses: file.uses.clone(),
        item,
        path: file.path.clone(),
    }
}

fn rewrite_class(
    class: &ClassDecl,
    imports: &HashMap<String, String>,
    templates: &HashMap<String, TemplateInfo>,
) -> ClassDecl {
    let rw_ty = |t: &Type| rewrite_type(t, imports, templates);
    ClassDecl {
        name: class.name.clone(),
        type_params: class.type_params.clone(),
        extends: class.extends.clone(),
        implements: class.implements.clone(),
        fields: class
            .fields
            .iter()
            .map(|f| FieldDecl {
                ty: rw_ty(&f.ty),
                init: f.init.as_ref().map(|e| rewrite_expr(e, imports, templates)),
                ..f.clone()
            })
            .collect(),
        methods: class
            .methods
            .iter()
            .map(|m| rewrite_method(m, imports, templates))
            .collect(),
        is_readonly: class.is_readonly,
        is_abstract: class.is_abstract,
        is_final: class.is_final,
        decl_line: class.decl_line,
        is_enum: class.is_enum,
        enum_cases: class.enum_cases.clone(),
    }
}

fn rewrite_method(
    m: &MethodDecl,
    imports: &HashMap<String, String>,
    templates: &HashMap<String, TemplateInfo>,
) -> MethodDecl {
    MethodDecl {
        params: m
            .params
            .iter()
            .map(|p| crate::ast::Param {
                name: p.name.clone(),
                ty: rewrite_type(&p.ty, imports, templates),
                is_const: p.is_const,
                is_ref: p.is_ref,
                default: p
                    .default
                    .as_ref()
                    .map(|e| rewrite_expr(e, imports, templates)),
            })
            .collect(),
        return_type: rewrite_type(&m.return_type, imports, templates),
        body: rewrite_block(&m.body, imports, templates),
        ..m.clone()
    }
}

fn rewrite_type(
    ty: &Type,
    imports: &HashMap<String, String>,
    templates: &HashMap<String, TemplateInfo>,
) -> Type {
    match ty {
        Type::Generic(name, args) => {
            let args: Vec<Type> = args
                .iter()
                .map(|a| rewrite_type(a, imports, templates))
                .collect();
            let fqcn = resolve_name(name, imports);
            if templates.contains_key(&fqcn) || is_native_generic(&fqcn) {
                let resolved_args: Vec<Type> = args
                    .iter()
                    .map(|a| resolve_type_names(a, imports))
                    .collect();
                Type::Named(format!(
                    "{fqcn}<{}>",
                    resolved_args
                        .iter()
                        .map(mangle_type)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            } else {
                Type::Generic(name.clone(), args)
            }
        }
        Type::Array(inner) => Type::Array(Box::new(rewrite_type(inner, imports, templates))),
        Type::Union(members) => Type::Union(
            members
                .iter()
                .map(|m| rewrite_type(m, imports, templates))
                .collect(),
        ),
        Type::Function {
            params,
            return_type,
            throws,
        } => Type::Function {
            params: params
                .iter()
                .map(|p| rewrite_type(p, imports, templates))
                .collect(),
            return_type: Box::new(rewrite_type(return_type, imports, templates)),
            throws: throws.clone(),
        },
        other => other.clone(),
    }
}

fn rewrite_block(
    block: &Block,
    imports: &HashMap<String, String>,
    templates: &HashMap<String, TemplateInfo>,
) -> Block {
    block
        .iter()
        .map(|s| rewrite_stmt(s, imports, templates))
        .collect()
}

fn rewrite_stmt(
    stmt: &Stmt,
    imports: &HashMap<String, String>,
    templates: &HashMap<String, TemplateInfo>,
) -> Stmt {
    let kind = match &stmt.kind {
        StmtKind::Return(e) => {
            StmtKind::Return(e.as_ref().map(|e| rewrite_expr(e, imports, templates)))
        }
        StmtKind::Expr(e) => StmtKind::Expr(rewrite_expr(e, imports, templates)),
        StmtKind::VarDecl {
            ty,
            name,
            init,
            is_const,
        } => StmtKind::VarDecl {
            ty: ty.as_ref().map(|t| rewrite_type(t, imports, templates)),
            name: name.clone(),
            init: init.as_ref().map(|e| rewrite_expr(e, imports, templates)),
            is_const: *is_const,
        },
        StmtKind::If {
            cond,
            then_branch,
            else_branch,
        } => StmtKind::If {
            cond: rewrite_expr(cond, imports, templates),
            then_branch: rewrite_block(then_branch, imports, templates),
            else_branch: else_branch
                .as_ref()
                .map(|b| rewrite_block(b, imports, templates)),
        },
        StmtKind::While { cond, body } => StmtKind::While {
            cond: rewrite_expr(cond, imports, templates),
            body: rewrite_block(body, imports, templates),
        },
        StmtKind::ForEach {
            ty,
            var,
            iterable,
            body,
        } => StmtKind::ForEach {
            ty: ty.as_ref().map(|t| rewrite_type(t, imports, templates)),
            var: var.clone(),
            iterable: rewrite_expr(iterable, imports, templates),
            body: rewrite_block(body, imports, templates),
        },
        StmtKind::For {
            init,
            cond,
            step,
            body,
        } => StmtKind::For {
            init: init
                .iter()
                .map(|s| rewrite_stmt(s, imports, templates))
                .collect(),
            cond: cond.as_ref().map(|c| rewrite_expr(c, imports, templates)),
            step: step
                .iter()
                .map(|e| rewrite_expr(e, imports, templates))
                .collect(),
            body: rewrite_block(body, imports, templates),
        },
        StmtKind::Break => StmtKind::Break,
        StmtKind::Continue => StmtKind::Continue,
        StmtKind::Block(b) => StmtKind::Block(rewrite_block(b, imports, templates)),
        StmtKind::ThisCall(args) => StmtKind::ThisCall(
            args.iter()
                .map(|a| rewrite_arg(a, imports, templates))
                .collect(),
        ),
        StmtKind::SuperCall(args) => StmtKind::SuperCall(
            args.iter()
                .map(|a| rewrite_arg(a, imports, templates))
                .collect(),
        ),
        StmtKind::Throw(e) => StmtKind::Throw(rewrite_expr(e, imports, templates)),
        StmtKind::Try {
            body,
            catches,
            finally,
        } => StmtKind::Try {
            body: rewrite_block(body, imports, templates),
            catches: catches
                .iter()
                .map(|c| crate::ast::CatchClause {
                    ty: c.ty.clone(),
                    var: c.var.clone(),
                    body: rewrite_block(&c.body, imports, templates),
                })
                .collect(),
            finally: finally
                .as_ref()
                .map(|b| rewrite_block(b, imports, templates)),
        },
    };
    Stmt {
        kind,
        line: stmt.line,
    }
}

fn rewrite_expr(
    expr: &Expr,
    imports: &HashMap<String, String>,
    templates: &HashMap<String, TemplateInfo>,
) -> Expr {
    match expr {
        Expr::IntLit(_)
        | Expr::FloatLit(_)
        | Expr::BoolLit(_)
        | Expr::StringLit(_)
        | Expr::NullLit
        | Expr::This
        | Expr::Super
        | Expr::Ident(_)
        | Expr::PostIncr(_)
        | Expr::PostDecr(_) => expr.clone(),
        Expr::Assign(target, value) => Expr::Assign(
            rewrite_lvalue(target, imports, templates),
            Box::new(rewrite_expr(value, imports, templates)),
        ),
        Expr::Call(name, args) => Expr::Call(
            name.clone(),
            args.iter()
                .map(|a| rewrite_arg(a, imports, templates))
                .collect(),
        ),
        Expr::New(name, type_args, args) => {
            let rw_args: Vec<Type> = type_args
                .iter()
                .map(|a| rewrite_type(a, imports, templates))
                .collect();
            let fqcn = resolve_name(name, imports);
            if !type_args.is_empty() && (templates.contains_key(&fqcn) || is_native_generic(&fqcn))
            {
                let resolved_args: Vec<Type> = rw_args
                    .iter()
                    .map(|a| resolve_type_names(a, imports))
                    .collect();
                let mangled = format!(
                    "{fqcn}<{}>",
                    resolved_args
                        .iter()
                        .map(mangle_type)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                Expr::New(
                    mangled,
                    Vec::new(),
                    args.iter()
                        .map(|a| rewrite_arg(a, imports, templates))
                        .collect(),
                )
            } else {
                Expr::New(
                    name.clone(),
                    rw_args,
                    args.iter()
                        .map(|a| rewrite_arg(a, imports, templates))
                        .collect(),
                )
            }
        }
        Expr::NewArray(elem_ty, dims) => Expr::NewArray(
            Box::new(rewrite_type(elem_ty, imports, templates)),
            dims.iter()
                .map(|d| d.as_ref().map(|e| rewrite_expr(e, imports, templates)))
                .collect(),
        ),
        Expr::NewArrayInit(elem_ty, elements) => Expr::NewArrayInit(
            Box::new(rewrite_type(elem_ty, imports, templates)),
            elements
                .iter()
                .map(|e| rewrite_expr(e, imports, templates))
                .collect(),
        ),
        Expr::FieldAccess(target, name) => Expr::FieldAccess(
            Box::new(rewrite_expr(target, imports, templates)),
            name.clone(),
        ),
        Expr::MethodCall(target, name, args) => Expr::MethodCall(
            Box::new(rewrite_expr(target, imports, templates)),
            name.clone(),
            args.iter()
                .map(|a| rewrite_arg(a, imports, templates))
                .collect(),
        ),
        Expr::Index(target, index) => Expr::Index(
            Box::new(rewrite_expr(target, imports, templates)),
            Box::new(rewrite_expr(index, imports, templates)),
        ),
        Expr::InstanceOf(target, type_name) => Expr::InstanceOf(
            Box::new(rewrite_expr(target, imports, templates)),
            type_name.clone(),
        ),
        Expr::Cast(ty, inner) => Expr::Cast(
            Box::new(rewrite_type(ty, imports, templates)),
            Box::new(rewrite_expr(inner, imports, templates)),
        ),
        Expr::Unary(op, inner) => {
            Expr::Unary(*op, Box::new(rewrite_expr(inner, imports, templates)))
        }
        Expr::Binary(op, lhs, rhs) => Expr::Binary(
            *op,
            Box::new(rewrite_expr(lhs, imports, templates)),
            Box::new(rewrite_expr(rhs, imports, templates)),
        ),
        Expr::Match(subject, arms) => Expr::Match(
            Box::new(rewrite_expr(subject, imports, templates)),
            arms.iter()
                .map(|a| crate::ast::MatchArm {
                    pattern: a
                        .pattern
                        .as_ref()
                        .map(|p| rewrite_expr(p, imports, templates)),
                    value: rewrite_expr(&a.value, imports, templates),
                })
                .collect(),
        ),
        Expr::Ternary(cond, then_e, else_e) => Expr::Ternary(
            Box::new(rewrite_expr(cond, imports, templates)),
            Box::new(rewrite_expr(then_e, imports, templates)),
            Box::new(rewrite_expr(else_e, imports, templates)),
        ),
        Expr::Coalesce(lhs, rhs) => Expr::Coalesce(
            Box::new(rewrite_expr(lhs, imports, templates)),
            Box::new(rewrite_expr(rhs, imports, templates)),
        ),
        Expr::Elvis(lhs, rhs) => Expr::Elvis(
            Box::new(rewrite_expr(lhs, imports, templates)),
            Box::new(rewrite_expr(rhs, imports, templates)),
        ),
        Expr::Closure {
            params,
            return_type,
            throws,
            body,
        } => Expr::Closure {
            params: params
                .iter()
                .map(|p| crate::ast::Param {
                    name: p.name.clone(),
                    ty: rewrite_type(&p.ty, imports, templates),
                    is_const: p.is_const,
                    is_ref: p.is_ref,
                    default: p
                        .default
                        .as_ref()
                        .map(|e| rewrite_expr(e, imports, templates)),
                })
                .collect(),
            return_type: return_type
                .as_ref()
                .map(|t| rewrite_type(t, imports, templates)),
            throws: throws.clone(),
            body: match body {
                ClosureBody::Block(b) => ClosureBody::Block(rewrite_block(b, imports, templates)),
                ClosureBody::Expr(e) => {
                    ClosureBody::Expr(Box::new(rewrite_expr(e, imports, templates)))
                }
            },
        },
    }
}

fn rewrite_arg(
    arg: &Arg,
    imports: &HashMap<String, String>,
    templates: &HashMap<String, TemplateInfo>,
) -> Arg {
    Arg {
        name: arg.name.clone(),
        is_ref: arg.is_ref,
        value: rewrite_expr(&arg.value, imports, templates),
    }
}

fn rewrite_lvalue(
    lvalue: &LValue,
    imports: &HashMap<String, String>,
    templates: &HashMap<String, TemplateInfo>,
) -> LValue {
    match lvalue {
        LValue::Local(name) => LValue::Local(name.clone()),
        LValue::Field(target, name) => LValue::Field(
            Box::new(rewrite_expr(target, imports, templates)),
            name.clone(),
        ),
        LValue::Index(target, index) => LValue::Index(
            Box::new(rewrite_expr(target, imports, templates)),
            Box::new(rewrite_expr(index, imports, templates)),
        ),
    }
}

// ---------------------------------------------------------------------
// Pass 2b: substitute a template's own type parameters with concrete types
// throughout its `ClassDecl` (fields, method signatures, method bodies).
// ---------------------------------------------------------------------

fn subst_class(class: &ClassDecl, subst: &HashMap<String, Type>) -> ClassDecl {
    ClassDecl {
        name: class.name.clone(),
        type_params: class.type_params.clone(),
        extends: class.extends.clone(),
        implements: class.implements.clone(),
        fields: class
            .fields
            .iter()
            .map(|f| FieldDecl {
                ty: subst_type(&f.ty, subst),
                init: f.init.as_ref().map(|e| subst_expr(e, subst)),
                ..f.clone()
            })
            .collect(),
        methods: class
            .methods
            .iter()
            .map(|m| subst_method(m, subst))
            .collect(),
        is_readonly: class.is_readonly,
        is_abstract: class.is_abstract,
        is_final: class.is_final,
        decl_line: class.decl_line,
        is_enum: class.is_enum,
        enum_cases: class.enum_cases.clone(),
    }
}

fn subst_method(m: &MethodDecl, subst: &HashMap<String, Type>) -> MethodDecl {
    MethodDecl {
        params: m
            .params
            .iter()
            .map(|p| crate::ast::Param {
                name: p.name.clone(),
                ty: subst_type(&p.ty, subst),
                is_const: p.is_const,
                is_ref: p.is_ref,
                default: p.default.as_ref().map(|e| subst_expr(e, subst)),
            })
            .collect(),
        return_type: subst_type(&m.return_type, subst),
        body: subst_block(&m.body, subst),
        ..m.clone()
    }
}

fn subst_type(ty: &Type, subst: &HashMap<String, Type>) -> Type {
    match ty {
        Type::Named(name) => subst.get(name).cloned().unwrap_or_else(|| ty.clone()),
        Type::Array(inner) => Type::Array(Box::new(subst_type(inner, subst))),
        Type::Union(members) => Type::Union(members.iter().map(|m| subst_type(m, subst)).collect()),
        Type::Generic(name, args) => Type::Generic(
            name.clone(),
            args.iter().map(|a| subst_type(a, subst)).collect(),
        ),
        Type::Function {
            params,
            return_type,
            throws,
        } => Type::Function {
            params: params.iter().map(|p| subst_type(p, subst)).collect(),
            return_type: Box::new(subst_type(return_type, subst)),
            throws: throws.clone(),
        },
        other => other.clone(),
    }
}

fn subst_block(block: &Block, subst: &HashMap<String, Type>) -> Block {
    block.iter().map(|s| subst_stmt(s, subst)).collect()
}

fn subst_stmt(stmt: &Stmt, subst: &HashMap<String, Type>) -> Stmt {
    let kind = match &stmt.kind {
        StmtKind::Return(e) => StmtKind::Return(e.as_ref().map(|e| subst_expr(e, subst))),
        StmtKind::Expr(e) => StmtKind::Expr(subst_expr(e, subst)),
        StmtKind::VarDecl {
            ty,
            name,
            init,
            is_const,
        } => StmtKind::VarDecl {
            ty: ty.as_ref().map(|t| subst_type(t, subst)),
            name: name.clone(),
            init: init.as_ref().map(|e| subst_expr(e, subst)),
            is_const: *is_const,
        },
        StmtKind::If {
            cond,
            then_branch,
            else_branch,
        } => StmtKind::If {
            cond: subst_expr(cond, subst),
            then_branch: subst_block(then_branch, subst),
            else_branch: else_branch.as_ref().map(|b| subst_block(b, subst)),
        },
        StmtKind::While { cond, body } => StmtKind::While {
            cond: subst_expr(cond, subst),
            body: subst_block(body, subst),
        },
        StmtKind::ForEach {
            ty,
            var,
            iterable,
            body,
        } => StmtKind::ForEach {
            ty: ty.as_ref().map(|t| subst_type(t, subst)),
            var: var.clone(),
            iterable: subst_expr(iterable, subst),
            body: subst_block(body, subst),
        },
        StmtKind::For {
            init,
            cond,
            step,
            body,
        } => StmtKind::For {
            init: init.iter().map(|s| subst_stmt(s, subst)).collect(),
            cond: cond.as_ref().map(|c| subst_expr(c, subst)),
            step: step.iter().map(|e| subst_expr(e, subst)).collect(),
            body: subst_block(body, subst),
        },
        StmtKind::Break => StmtKind::Break,
        StmtKind::Continue => StmtKind::Continue,
        StmtKind::Block(b) => StmtKind::Block(subst_block(b, subst)),
        StmtKind::ThisCall(args) => {
            StmtKind::ThisCall(args.iter().map(|a| subst_arg(a, subst)).collect())
        }
        StmtKind::SuperCall(args) => {
            StmtKind::SuperCall(args.iter().map(|a| subst_arg(a, subst)).collect())
        }
        StmtKind::Throw(e) => StmtKind::Throw(subst_expr(e, subst)),
        StmtKind::Try {
            body,
            catches,
            finally,
        } => StmtKind::Try {
            body: subst_block(body, subst),
            catches: catches
                .iter()
                .map(|c| crate::ast::CatchClause {
                    ty: c.ty.clone(),
                    var: c.var.clone(),
                    body: subst_block(&c.body, subst),
                })
                .collect(),
            finally: finally.as_ref().map(|b| subst_block(b, subst)),
        },
    };
    Stmt {
        kind,
        line: stmt.line,
    }
}

fn subst_expr(expr: &Expr, subst: &HashMap<String, Type>) -> Expr {
    match expr {
        Expr::IntLit(_)
        | Expr::FloatLit(_)
        | Expr::BoolLit(_)
        | Expr::StringLit(_)
        | Expr::NullLit
        | Expr::This
        | Expr::Super
        | Expr::Ident(_)
        | Expr::PostIncr(_)
        | Expr::PostDecr(_) => expr.clone(),
        Expr::Assign(target, value) => Expr::Assign(
            subst_lvalue(target, subst),
            Box::new(subst_expr(value, subst)),
        ),
        Expr::Call(name, args) => Expr::Call(
            name.clone(),
            args.iter().map(|a| subst_arg(a, subst)).collect(),
        ),
        Expr::New(name, type_args, args) => Expr::New(
            name.clone(),
            type_args.iter().map(|t| subst_type(t, subst)).collect(),
            args.iter().map(|a| subst_arg(a, subst)).collect(),
        ),
        Expr::NewArray(elem_ty, dims) => Expr::NewArray(
            Box::new(subst_type(elem_ty, subst)),
            dims.iter()
                .map(|d| d.as_ref().map(|e| subst_expr(e, subst)))
                .collect(),
        ),
        Expr::NewArrayInit(elem_ty, elements) => Expr::NewArrayInit(
            Box::new(subst_type(elem_ty, subst)),
            elements.iter().map(|e| subst_expr(e, subst)).collect(),
        ),
        Expr::FieldAccess(target, name) => {
            Expr::FieldAccess(Box::new(subst_expr(target, subst)), name.clone())
        }
        Expr::MethodCall(target, name, args) => Expr::MethodCall(
            Box::new(subst_expr(target, subst)),
            name.clone(),
            args.iter().map(|a| subst_arg(a, subst)).collect(),
        ),
        Expr::Index(target, index) => Expr::Index(
            Box::new(subst_expr(target, subst)),
            Box::new(subst_expr(index, subst)),
        ),
        // `instanceof T` inside a template body isn't substitutable this
        // phase (`type_name` is a bare `String`, not a `Type`) — left as-is
        // (rare inside template bodies; not exercised by tests).
        Expr::InstanceOf(target, type_name) => {
            Expr::InstanceOf(Box::new(subst_expr(target, subst)), type_name.clone())
        }
        Expr::Cast(ty, inner) => Expr::Cast(
            Box::new(subst_type(ty, subst)),
            Box::new(subst_expr(inner, subst)),
        ),
        Expr::Unary(op, inner) => Expr::Unary(*op, Box::new(subst_expr(inner, subst))),
        Expr::Binary(op, lhs, rhs) => Expr::Binary(
            *op,
            Box::new(subst_expr(lhs, subst)),
            Box::new(subst_expr(rhs, subst)),
        ),
        Expr::Match(subject, arms) => Expr::Match(
            Box::new(subst_expr(subject, subst)),
            arms.iter()
                .map(|a| crate::ast::MatchArm {
                    pattern: a.pattern.as_ref().map(|p| subst_expr(p, subst)),
                    value: subst_expr(&a.value, subst),
                })
                .collect(),
        ),
        Expr::Ternary(cond, then_e, else_e) => Expr::Ternary(
            Box::new(subst_expr(cond, subst)),
            Box::new(subst_expr(then_e, subst)),
            Box::new(subst_expr(else_e, subst)),
        ),
        Expr::Coalesce(lhs, rhs) => Expr::Coalesce(
            Box::new(subst_expr(lhs, subst)),
            Box::new(subst_expr(rhs, subst)),
        ),
        Expr::Elvis(lhs, rhs) => {
            Expr::Elvis(Box::new(subst_expr(lhs, subst)), Box::new(subst_expr(rhs, subst)))
        }
        Expr::Closure {
            params,
            return_type,
            throws,
            body,
        } => Expr::Closure {
            params: params
                .iter()
                .map(|p| crate::ast::Param {
                    name: p.name.clone(),
                    ty: subst_type(&p.ty, subst),
                    is_const: p.is_const,
                    is_ref: p.is_ref,
                    default: p.default.as_ref().map(|e| subst_expr(e, subst)),
                })
                .collect(),
            return_type: return_type.as_ref().map(|t| subst_type(t, subst)),
            throws: throws.clone(),
            body: match body {
                ClosureBody::Block(b) => ClosureBody::Block(subst_block(b, subst)),
                ClosureBody::Expr(e) => ClosureBody::Expr(Box::new(subst_expr(e, subst))),
            },
        },
    }
}

fn subst_arg(arg: &Arg, subst: &HashMap<String, Type>) -> Arg {
    Arg {
        name: arg.name.clone(),
        is_ref: arg.is_ref,
        value: subst_expr(&arg.value, subst),
    }
}

fn subst_lvalue(lvalue: &LValue, subst: &HashMap<String, Type>) -> LValue {
    match lvalue {
        LValue::Local(name) => LValue::Local(name.clone()),
        LValue::Field(target, name) => {
            LValue::Field(Box::new(subst_expr(target, subst)), name.clone())
        }
        LValue::Index(target, index) => LValue::Index(
            Box::new(subst_expr(target, subst)),
            Box::new(subst_expr(index, subst)),
        ),
    }
}
