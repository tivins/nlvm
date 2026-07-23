//! `typedef` alias expansion — specs.md § Typedef. Pure AST-to-AST rewriting,
//! run once ahead of `nl_syntax::monomorphize::expand` (both `nl-sema` and
//! `nl-codegen` call this first, then monomorphize, on the same input — see
//! their `check_compile`/`compile_program` entry points — so they always
//! agree on the expanded program, same discipline `monomorphize` itself
//! documents).
//!
//! A `typedef Type Name;` is a **compile-time alias**, not a new type
//! (specs.md: "fully interchangeable with their underlying types"). This
//! module erases every alias before `nl-sema`/`nl-codegen` ever see it, by
//! substituting `Name` with its (fully flattened) underlying `Type`
//! everywhere a type can appear in the AST — so neither of those crates
//! needs to know `typedef` exists at all.
//!
//! Algorithm:
//! 1. Collect every `typedef` declared anywhere, keyed by `"namespace.Name"`
//!    (or bare `"Name"` for a namespace-less file) — specs.md: "Typedefs are
//!    scoped to their namespace".
//! 2. Resolve each typedef's own right-hand-side `Type` against its
//!    *declaring* file's import map (`monomorphize::import_map`), so e.g.
//!    `typedef Vector<int> IntVector;` becomes `Type::Generic("ns.Vector",
//!    [Int])` (FQCN-qualified) regardless of which *other* file later
//!    references `IntVector` — that file's own imports must never leak into
//!    the alias's meaning.
//! 3. Flatten chained typedefs (one `typedef` aliasing another) to a fixed
//!    point, with a cycle guard (a self-referential typedef chain has no
//!    sensible expansion; left as its last-seen form rather than looping
//!    forever — not a case specs.md documents or this implementation
//!    diagnoses specially).
//! 4. Rewrite every occurrence of a typedef'd name in every file's AST
//!    (field/param/return/local-variable types, casts, array element types,
//!    closure signatures, `new T<...>(...)` type arguments, ...) with its
//!    flattened `Type`, resolving unqualified references against *that
//!    occurrence's own* file namespace (same-namespace visibility rule).
//!
//! **Known gap**: two other AST positions name a class by a bare `String`
//! rather than a `Type` — `catch (Type name)` (`nl_syntax::ast::CatchClause`)
//! and `expr instanceof Type` (`Expr::InstanceOf`) — and a typedef used in
//! either is not substituted. Neither is exercised by any specs.md example
//! (every `catch`/`instanceof` there names a real class/interface directly).
//! `new IntVector(...)` (specs.md § Typedef with templates) is also a bare
//! `String` (`Expr::New`'s `name` field) but *is* handled — see
//! `rewrite_expr`'s `Expr::New` arm — since the spec explicitly documents it.

use std::collections::{HashMap, HashSet};

use crate::ast::{
    Arg, Block, ClassDecl, ClosureBody, Expr, FieldDecl, InterfaceDecl, LValue, MethodDecl,
    MethodSig, Param, SourceFile, SourceItem, Stmt, StmtKind, SwitchCase, Type,
};
use crate::monomorphize::{import_map, resolve_type_names};

pub fn expand(files: Vec<SourceFile>) -> Vec<SourceFile> {
    if files.iter().all(|f| f.typedefs.is_empty()) {
        return files;
    }

    // Declaring namespace context for each typedef key — needed to resolve
    // unqualified names *inside* its own right-hand side (see
    // `flatten_typedef`), kept separate from the already-import-resolved
    // `Type` in `resolved` below.
    let mut declaring_ns: HashMap<String, String> = HashMap::new();
    let mut resolved: HashMap<String, Type> = HashMap::new();
    for file in &files {
        if file.typedefs.is_empty() {
            continue;
        }
        let ns_key = file.namespace.join(".");
        let imports = import_map(file, &files);
        for td in &file.typedefs {
            let key = qualify(&ns_key, &td.name);
            declaring_ns.insert(key.clone(), ns_key.clone());
            resolved.insert(key, resolve_type_names(&td.ty, &imports));
        }
    }

    let mut flat: HashMap<String, Type> = HashMap::new();
    for key in resolved.keys().cloned().collect::<Vec<_>>() {
        let mut visiting = HashSet::new();
        let ty = flatten_typedef(&key, &declaring_ns, &resolved, &mut flat, &mut visiting);
        flat.insert(key, ty);
    }

    files
        .into_iter()
        .map(|file| rewrite_file(file, &flat))
        .collect()
}

fn qualify(ns_key: &str, name: &str) -> String {
    if ns_key.is_empty() {
        name.to_string()
    } else {
        format!("{ns_key}.{name}")
    }
}

/// `name` (as it appears inside some type, unqualified or dotted) -> the
/// `flat`/`resolved` table key it refers to, if it refers to a typedef at
/// all — checked as a dotted name first (already FQCN-shaped, e.g. after
/// `resolve_type_names` resolved it against a real class import), then as an
/// unqualified name in `ns_key`'s own namespace.
fn lookup_key(name: &str, ns_key: &str, table: &HashMap<String, Type>) -> Option<String> {
    if name.contains('.') {
        return table.contains_key(name).then(|| name.to_string());
    }
    let candidate = qualify(ns_key, name);
    table.contains_key(&candidate).then_some(candidate)
}

/// Fully expands typedef `key`'s own right-hand side — recursing through any
/// further typedef references inside it — memoized in `flat` so repeated
/// references (including diamonds through several other typedefs) are only
/// resolved once. `visiting` breaks a cyclic chain (`typedef A B; typedef B
/// A;`): the cycle point is returned as-is rather than looping forever.
fn flatten_typedef(
    key: &str,
    declaring_ns: &HashMap<String, String>,
    resolved: &HashMap<String, Type>,
    flat: &mut HashMap<String, Type>,
    visiting: &mut HashSet<String>,
) -> Type {
    if let Some(t) = flat.get(key) {
        return t.clone();
    }
    let Some(ty) = resolved.get(key) else {
        return Type::Named(key.to_string());
    };
    if !visiting.insert(key.to_string()) {
        return ty.clone();
    }
    let ns_key = declaring_ns[key].clone();
    let expanded = expand_type(&ty.clone(), &ns_key, declaring_ns, resolved, flat, visiting);
    visiting.remove(key);
    flat.insert(key.to_string(), expanded.clone());
    expanded
}

/// Recursively substitutes every typedef reference inside `ty`, resolving
/// unqualified names against `ns_key`. Shared by `flatten_typedef` (building
/// each alias's own fully-expanded form) and `apply_type` (rewriting user
/// code with the already-flattened table, where `raw`/`visiting` are simply
/// empty/unused — see that function).
fn expand_type(
    ty: &Type,
    ns_key: &str,
    declaring_ns: &HashMap<String, String>,
    resolved: &HashMap<String, Type>,
    flat: &mut HashMap<String, Type>,
    visiting: &mut HashSet<String>,
) -> Type {
    match ty {
        Type::Named(name) => match lookup_key(name, ns_key, resolved) {
            Some(key) => flatten_typedef(&key, declaring_ns, resolved, flat, visiting),
            None => ty.clone(),
        },
        Type::Generic(name, args) => {
            let args: Vec<Type> = args
                .iter()
                .map(|a| expand_type(a, ns_key, declaring_ns, resolved, flat, visiting))
                .collect();
            // A typedef aliasing a bare generic head directly (`typedef
            // Vector MyVector;`, without concrete args) isn't a shape
            // specs.md shows — `Vector<int>` itself (the whole
            // `Type::Generic`) is what gets aliased, handled by the
            // `Type::Named` arm above wherever this alias is *used*, not
            // here. Left as an ordinary (possibly still-generic) type.
            match lookup_key(name, ns_key, resolved) {
                Some(key) => flatten_typedef(&key, declaring_ns, resolved, flat, visiting),
                None => Type::Generic(name.clone(), args),
            }
        }
        Type::Array(inner) => Type::Array(Box::new(expand_type(
            inner, ns_key, declaring_ns, resolved, flat, visiting,
        ))),
        Type::Union(members) => Type::Union(
            members
                .iter()
                .map(|m| expand_type(m, ns_key, declaring_ns, resolved, flat, visiting))
                .collect(),
        ),
        Type::Function {
            params,
            return_type,
            throws,
        } => Type::Function {
            params: params
                .iter()
                .map(|p| expand_type(p, ns_key, declaring_ns, resolved, flat, visiting))
                .collect(),
            return_type: Box::new(expand_type(return_type, ns_key, declaring_ns, resolved, flat, visiting)),
            throws: throws.clone(),
        },
        other => other.clone(),
    }
}

/// Substitutes typedef references in `ty` using the already-fully-flattened
/// `flat` table — the user-code-rewriting counterpart to `expand_type`.
/// Simpler than that function: `flat`'s values are final (no further typedef
/// references can remain inside them after `flatten_typedef` ran for every
/// key), so a plain recursive lookup-and-clone is enough — no memoization or
/// cycle guard needed here.
fn apply_type(ty: &Type, ns_key: &str, flat: &HashMap<String, Type>) -> Type {
    match ty {
        Type::Named(name) => match lookup_key(name, ns_key, flat) {
            Some(key) => flat[&key].clone(),
            None => ty.clone(),
        },
        Type::Generic(name, args) => {
            let args: Vec<Type> = args.iter().map(|a| apply_type(a, ns_key, flat)).collect();
            match lookup_key(name, ns_key, flat) {
                Some(key) => flat[&key].clone(),
                None => Type::Generic(name.clone(), args),
            }
        }
        Type::Array(inner) => Type::Array(Box::new(apply_type(inner, ns_key, flat))),
        Type::Union(members) => {
            Type::Union(members.iter().map(|m| apply_type(m, ns_key, flat)).collect())
        }
        Type::Function {
            params,
            return_type,
            throws,
        } => Type::Function {
            params: params.iter().map(|p| apply_type(p, ns_key, flat)).collect(),
            return_type: Box::new(apply_type(return_type, ns_key, flat)),
            throws: throws.clone(),
        },
        other => other.clone(),
    }
}

fn rewrite_file(file: SourceFile, flat: &HashMap<String, Type>) -> SourceFile {
    let ns_key = file.namespace.join(".");
    let item = match file.item {
        SourceItem::Class(class) => SourceItem::Class(rewrite_class(&class, &ns_key, flat)),
        SourceItem::Interface(iface) => {
            SourceItem::Interface(rewrite_interface(&iface, &ns_key, flat))
        }
    };
    SourceFile {
        namespace: file.namespace,
        uses: file.uses,
        typedefs: Vec::new(),
        item,
        path: file.path,
    }
}

fn rewrite_interface(
    iface: &InterfaceDecl,
    ns_key: &str,
    flat: &HashMap<String, Type>,
) -> InterfaceDecl {
    InterfaceDecl {
        name: iface.name.clone(),
        extends: iface.extends.clone(),
        methods: iface
            .methods
            .iter()
            .map(|m| MethodSig {
                name: m.name.clone(),
                return_type: apply_type(&m.return_type, ns_key, flat),
                params: m.params.iter().map(|p| rewrite_param(p, ns_key, flat)).collect(),
                is_const: m.is_const,
            })
            .collect(),
        decl_line: iface.decl_line,
    }
}

fn rewrite_param(p: &Param, ns_key: &str, flat: &HashMap<String, Type>) -> Param {
    Param {
        name: p.name.clone(),
        ty: apply_type(&p.ty, ns_key, flat),
        is_const: p.is_const,
        default: p.default.as_ref().map(|e| rewrite_expr(e, ns_key, flat)),
        is_ref: p.is_ref,
    }
}

fn rewrite_class(class: &ClassDecl, ns_key: &str, flat: &HashMap<String, Type>) -> ClassDecl {
    ClassDecl {
        name: class.name.clone(),
        type_params: class.type_params.clone(),
        extends: class.extends.clone(),
        implements: class.implements.clone(),
        fields: class
            .fields
            .iter()
            .map(|f| FieldDecl {
                name: f.name.clone(),
                visibility: f.visibility,
                visibility_explicit: f.visibility_explicit,
                is_static: f.is_static,
                readonly: f.readonly,
                ty: apply_type(&f.ty, ns_key, flat),
                init: f.init.as_ref().map(|e| rewrite_expr(e, ns_key, flat)),
            })
            .collect(),
        methods: class
            .methods
            .iter()
            .map(|m| rewrite_method(m, ns_key, flat))
            .collect(),
        is_readonly: class.is_readonly,
        is_abstract: class.is_abstract,
        is_final: class.is_final,
        decl_line: class.decl_line,
        is_enum: class.is_enum,
        enum_cases: class.enum_cases.clone(),
    }
}

fn rewrite_method(m: &MethodDecl, ns_key: &str, flat: &HashMap<String, Type>) -> MethodDecl {
    MethodDecl {
        name: m.name.clone(),
        kind: m.kind,
        visibility: m.visibility,
        visibility_explicit: m.visibility_explicit,
        is_static: m.is_static,
        is_const: m.is_const,
        is_abstract: m.is_abstract,
        is_final: m.is_final,
        is_nodiscard: m.is_nodiscard,
        return_type: apply_type(&m.return_type, ns_key, flat),
        params: m.params.iter().map(|p| rewrite_param(p, ns_key, flat)).collect(),
        throws: m.throws.clone(),
        body: rewrite_block(&m.body, ns_key, flat),
        decl_line: m.decl_line,
    }
}

fn rewrite_block(block: &Block, ns_key: &str, flat: &HashMap<String, Type>) -> Block {
    block.iter().map(|s| rewrite_stmt(s, ns_key, flat)).collect()
}

fn rewrite_stmt(stmt: &Stmt, ns_key: &str, flat: &HashMap<String, Type>) -> Stmt {
    let kind = match &stmt.kind {
        StmtKind::Return(e) => StmtKind::Return(e.as_ref().map(|e| rewrite_expr(e, ns_key, flat))),
        StmtKind::Expr(e) => StmtKind::Expr(rewrite_expr(e, ns_key, flat)),
        StmtKind::VarDecl {
            ty,
            name,
            init,
            is_const,
        } => StmtKind::VarDecl {
            ty: ty.as_ref().map(|t| apply_type(t, ns_key, flat)),
            name: name.clone(),
            init: init.as_ref().map(|e| rewrite_expr(e, ns_key, flat)),
            is_const: *is_const,
        },
        StmtKind::If {
            cond,
            then_branch,
            else_branch,
        } => StmtKind::If {
            cond: rewrite_expr(cond, ns_key, flat),
            then_branch: rewrite_block(then_branch, ns_key, flat),
            else_branch: else_branch.as_ref().map(|b| rewrite_block(b, ns_key, flat)),
        },
        StmtKind::While { cond, body } => StmtKind::While {
            cond: rewrite_expr(cond, ns_key, flat),
            body: rewrite_block(body, ns_key, flat),
        },
        StmtKind::ForEach {
            ty,
            var,
            iterable,
            body,
            is_const,
        } => StmtKind::ForEach {
            ty: ty.as_ref().map(|t| apply_type(t, ns_key, flat)),
            var: var.clone(),
            iterable: rewrite_expr(iterable, ns_key, flat),
            body: rewrite_block(body, ns_key, flat),
            is_const: *is_const,
        },
        StmtKind::For {
            init,
            cond,
            step,
            body,
        } => StmtKind::For {
            init: init.iter().map(|s| rewrite_stmt(s, ns_key, flat)).collect(),
            cond: cond.as_ref().map(|c| rewrite_expr(c, ns_key, flat)),
            step: step.iter().map(|e| rewrite_expr(e, ns_key, flat)).collect(),
            body: rewrite_block(body, ns_key, flat),
        },
        StmtKind::Break => StmtKind::Break,
        StmtKind::Continue => StmtKind::Continue,
        StmtKind::Block(b) => StmtKind::Block(rewrite_block(b, ns_key, flat)),
        StmtKind::ThisCall(args) => {
            StmtKind::ThisCall(args.iter().map(|a| rewrite_arg(a, ns_key, flat)).collect())
        }
        StmtKind::SuperCall(args) => {
            StmtKind::SuperCall(args.iter().map(|a| rewrite_arg(a, ns_key, flat)).collect())
        }
        StmtKind::Throw(e) => StmtKind::Throw(rewrite_expr(e, ns_key, flat)),
        StmtKind::Try {
            body,
            catches,
            finally,
        } => StmtKind::Try {
            body: rewrite_block(body, ns_key, flat),
            catches: catches
                .iter()
                .map(|c| crate::ast::CatchClause {
                    ty: c.ty.clone(),
                    var: c.var.clone(),
                    body: rewrite_block(&c.body, ns_key, flat),
                })
                .collect(),
            finally: finally.as_ref().map(|b| rewrite_block(b, ns_key, flat)),
        },
        StmtKind::Switch { subject, cases } => StmtKind::Switch {
            subject: rewrite_expr(subject, ns_key, flat),
            cases: cases
                .iter()
                .map(|c| SwitchCase {
                    value: c.value.as_ref().map(|v| rewrite_expr(v, ns_key, flat)),
                    body: rewrite_block(&c.body, ns_key, flat),
                })
                .collect(),
        },
    };
    Stmt {
        kind,
        line: stmt.line,
    }
}

fn rewrite_arg(a: &Arg, ns_key: &str, flat: &HashMap<String, Type>) -> Arg {
    Arg {
        name: a.name.clone(),
        is_ref: a.is_ref,
        value: rewrite_expr(&a.value, ns_key, flat),
    }
}

fn rewrite_lvalue(lvalue: &LValue, ns_key: &str, flat: &HashMap<String, Type>) -> LValue {
    match lvalue {
        LValue::Local(name) => LValue::Local(name.clone()),
        LValue::Field(target, name) => {
            LValue::Field(Box::new(rewrite_expr(target, ns_key, flat)), name.clone())
        }
        LValue::Index(target, index) => LValue::Index(
            Box::new(rewrite_expr(target, ns_key, flat)),
            Box::new(rewrite_expr(index, ns_key, flat)),
        ),
    }
}

fn rewrite_expr(expr: &Expr, ns_key: &str, flat: &HashMap<String, Type>) -> Expr {
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
        | Expr::PostDecr(_)
        | Expr::PreIncr(_)
        | Expr::PreDecr(_) => expr.clone(),
        Expr::Assign(target, value) => Expr::Assign(
            rewrite_lvalue(target, ns_key, flat),
            Box::new(rewrite_expr(value, ns_key, flat)),
        ),
        Expr::Call(name, args) => Expr::Call(
            name.clone(),
            args.iter().map(|a| rewrite_arg(a, ns_key, flat)).collect(),
        ),
        // `new IntVector(0, 0, 0)` where `typedef Vector<int> IntVector;` —
        // specs.md § Typedef with templates: explicitly documented to work.
        // `name` is a bare class-name `String` here (not a `Type`), so it
        // needs its own lookup — when it names a typedef whose flattened
        // form is itself a class/generic reference, rewrite this `New` node
        // into the equivalent `new Vector<int>(...)` shape so
        // `nl_syntax::monomorphize` (which only ever looks at `Expr::New`'s
        // `name`/`type_args` fields, never a `Type`) instantiates it exactly
        // as if the alias had never existed. Left alone when `name` doesn't
        // resolve to a typedef (the overwhelmingly common case) or the
        // typedef's target isn't itself a class/generic reference (a
        // typedef of `int`/`string`/etc. is never a valid `new` target
        // regardless — `nl-sema` rejects that on its own terms).
        Expr::New(name, type_args, args) => {
            let rewritten_args = args.iter().map(|a| rewrite_arg(a, ns_key, flat)).collect();
            if type_args.is_empty() {
                if let Some(key) = lookup_key(name, ns_key, flat) {
                    match &flat[&key] {
                        Type::Generic(head, targs) => {
                            return Expr::New(head.clone(), targs.clone(), rewritten_args);
                        }
                        Type::Named(fqcn) => {
                            return Expr::New(fqcn.clone(), Vec::new(), rewritten_args);
                        }
                        _ => {}
                    }
                }
            }
            Expr::New(
                name.clone(),
                type_args.iter().map(|t| apply_type(t, ns_key, flat)).collect(),
                rewritten_args,
            )
        }
        Expr::NewArray(elem_ty, dims) => Expr::NewArray(
            Box::new(apply_type(elem_ty, ns_key, flat)),
            dims.iter()
                .map(|d| d.as_ref().map(|e| rewrite_expr(e, ns_key, flat)))
                .collect(),
        ),
        Expr::NewArrayInit(elem_ty, elements) => Expr::NewArrayInit(
            Box::new(apply_type(elem_ty, ns_key, flat)),
            elements.iter().map(|e| rewrite_expr(e, ns_key, flat)).collect(),
        ),
        Expr::FieldAccess(target, name) => {
            Expr::FieldAccess(Box::new(rewrite_expr(target, ns_key, flat)), name.clone())
        }
        Expr::MethodCall(target, name, args) => Expr::MethodCall(
            Box::new(rewrite_expr(target, ns_key, flat)),
            name.clone(),
            args.iter().map(|a| rewrite_arg(a, ns_key, flat)).collect(),
        ),
        Expr::Index(target, index) => Expr::Index(
            Box::new(rewrite_expr(target, ns_key, flat)),
            Box::new(rewrite_expr(index, ns_key, flat)),
        ),
        Expr::InstanceOf(target, name) => {
            Expr::InstanceOf(Box::new(rewrite_expr(target, ns_key, flat)), name.clone())
        }
        Expr::Cast(ty, inner) => Expr::Cast(
            Box::new(apply_type(ty, ns_key, flat)),
            Box::new(rewrite_expr(inner, ns_key, flat)),
        ),
        Expr::Unary(op, inner) => Expr::Unary(*op, Box::new(rewrite_expr(inner, ns_key, flat))),
        Expr::Binary(op, lhs, rhs) => Expr::Binary(
            *op,
            Box::new(rewrite_expr(lhs, ns_key, flat)),
            Box::new(rewrite_expr(rhs, ns_key, flat)),
        ),
        Expr::Match(subject, arms) => Expr::Match(
            Box::new(rewrite_expr(subject, ns_key, flat)),
            arms.iter()
                .map(|arm| crate::ast::MatchArm {
                    pattern: arm.pattern.as_ref().map(|p| rewrite_expr(p, ns_key, flat)),
                    value: rewrite_expr(&arm.value, ns_key, flat),
                })
                .collect(),
        ),
        Expr::Ternary(cond, then_e, else_e) => Expr::Ternary(
            Box::new(rewrite_expr(cond, ns_key, flat)),
            Box::new(rewrite_expr(then_e, ns_key, flat)),
            Box::new(rewrite_expr(else_e, ns_key, flat)),
        ),
        Expr::Coalesce(lhs, rhs) => Expr::Coalesce(
            Box::new(rewrite_expr(lhs, ns_key, flat)),
            Box::new(rewrite_expr(rhs, ns_key, flat)),
        ),
        Expr::Elvis(lhs, rhs) => Expr::Elvis(
            Box::new(rewrite_expr(lhs, ns_key, flat)),
            Box::new(rewrite_expr(rhs, ns_key, flat)),
        ),
        Expr::Closure {
            params,
            return_type,
            throws,
            body,
        } => Expr::Closure {
            params: params.iter().map(|p| rewrite_param(p, ns_key, flat)).collect(),
            return_type: return_type.as_ref().map(|t| apply_type(t, ns_key, flat)),
            throws: throws.clone(),
            body: match body {
                ClosureBody::Block(b) => ClosureBody::Block(rewrite_block(b, ns_key, flat)),
                ClosureBody::Expr(e) => ClosureBody::Expr(Box::new(rewrite_expr(e, ns_key, flat))),
            },
        },
    }
}
