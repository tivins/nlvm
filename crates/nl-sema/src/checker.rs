//! Per-file semantic checker — name resolution, definite assignment (E001),
//! null safety (E003/E004), `auto` deduction (E005), string concatenation
//! (E008), operator compatibility (E009), duplicate methods (E041),
//! constructor delegation (E045/E046). See nlvm-specs/docs/compiler.md.
//!
//! Cross-file class/field/method references (objects, `new`, arrays,
//! interfaces) are checked leniently against the program-wide `ClassTable`:
//! an unknown class/field/method has no dedicated E-code yet and defers to
//! nl-codegen's harder failure — same pattern already used for unresolved
//! calls before this phase.

use std::collections::{HashMap, HashSet};

use nl_syntax::ast::{
    Arg, BinOp, Block, CatchClause, ClassDecl, Expr, LValue, MatchArm, MethodDecl, MethodKind,
    Param, SourceFile, SourceItem, Stmt, StmtKind, Type, UnOp,
};

use crate::class_table::{self, ClassTable};
use crate::error::{LocatedError, LocatedWarning, SemaError, SemaWarning};
use crate::types;

/// A method's signature, as seen from call sites within the same class
/// (bare, unqualified calls — always static, as before this phase). Keeps
/// the original `Param` list (not just resolved types) so bare calls can
/// bind named/optional arguments (compiler.md § Named and optional
/// parameter rules) the same way cross-class calls do via `class_table`.
type MethodSig = (Vec<Param>, Type, Vec<Type>);

/// A `SemaError` tagged with the line it was raised at, but not yet the
/// file — used internally while a check is still inside `MethodChecker`/the
/// free-function checks below, which don't have `SourceFile::path` in scope.
/// `check_source_file` is the single point that turns this into a
/// `LocatedError` by attaching `file.path`.
type Located = (u32, SemaError);

fn locate<T>(line: u32, r: Result<T, SemaError>) -> Result<T, Located> {
    r.map_err(|e| (line, e))
}

pub fn check_source_file(
    file: &SourceFile,
    all_files: &[SourceFile],
    classes: &ClassTable,
) -> Result<Vec<LocatedWarning>, LocatedError> {
    let SourceItem::Class(class) = &file.item else {
        // Interfaces declare signatures only — nothing to flow-check yet.
        return Ok(Vec::new());
    };

    let mut warnings: Vec<(u32, SemaWarning)> = Vec::new();
    let result: Result<(), Located> = (|| {
        locate(class.decl_line, check_duplicate_methods(class))?;
        locate(class.decl_line, check_constructor_delegation(class))?;
        locate(class.decl_line, check_visibility_modifiers(class))?;

        let imports = class_table::import_map(file, all_files);
        let this_fqcn = class_table::fqcn_of(file);
        locate(
            class.decl_line,
            check_property_initialization(class, &imports),
        )?;
        locate(
            class.decl_line,
            check_const_interface_impl(class, classes, &this_fqcn),
        )?;
        locate(
            class.decl_line,
            check_abstract_final(class, classes, &imports, &this_fqcn),
        )?;

        let mut sigs: HashMap<String, MethodSig> = HashMap::new();
        for m in &class.methods {
            if m.is_static && m.kind == MethodKind::Normal {
                let return_ty = class_table::resolve_type(&m.return_type, &imports);
                let throws = resolve_throws(m, &imports);
                sigs.insert(m.name.clone(), (m.params.clone(), return_ty, throws));
            }
        }

        for method in &class.methods {
            check_method(method, &sigs, classes, &imports, &mut warnings, &this_fqcn)
                .map_err(|(line, e)| (line, relabel_template_operator_error(e, &this_fqcn)))?;
            // compiler.md § Exception inheritance rules — E016/E017. Only
            // meaningful for instance methods (static methods hide, not
            // override) that actually override an ancestor's method.
            if !method.is_static && method.kind == MethodKind::Normal {
                locate(
                    method.decl_line,
                    check_exception_override(method, classes, &imports, &this_fqcn),
                )?;
            }
        }
        Ok(())
    })();

    result
        .map(|()| {
            warnings
                .into_iter()
                .map(|(line, warning)| LocatedWarning {
                    file: file.path.clone(),
                    line,
                    warning,
                })
                .collect()
        })
        .map_err(|(line, error)| LocatedError {
            file: file.path.clone(),
            line,
            error,
        })
}

/// compiler.md § Template instantiation — E006. This codebase has no
/// operator-overloading mechanism at all (see PLAN.md's generics gap), so
/// `nl_syntax::monomorphize` substituting a template's type parameter for an
/// unsupporting concrete type already fails "for free" as an ordinary E009
/// once the monomorphized class is type-checked like any other — the only
/// thing missing is the diagnostic's identity. A monomorphized class's FQCN
/// is always mangled `"TemplateName<Arg1, ...>"` (never produced any other
/// way), so that alone is enough to detect "this failure happened inside a
/// template instantiation" and re-code E009 as E006 without needing to track
/// provenance through the checker itself.
fn relabel_template_operator_error(err: SemaError, this_fqcn: &str) -> SemaError {
    let Some(template_name) = this_fqcn.split_once('<').map(|(name, _)| name.to_string()) else {
        return err;
    };
    match err {
        SemaError::BadBinaryOperator(op, t1, t2) => {
            let ty = if types::is_primitive_display(&t1) {
                t2
            } else {
                t1
            };
            SemaError::TemplateOperatorUnsupported(ty, op, template_name)
        }
        SemaError::BadUnaryOperator(op, t) => {
            SemaError::TemplateOperatorUnsupported(t, op, template_name)
        }
        other => other,
    }
}

fn resolve_throws(m: &MethodDecl, imports: &HashMap<String, String>) -> Vec<Type> {
    m.throws
        .iter()
        .map(|n| Type::Named(imports.get(n).cloned().unwrap_or_else(|| n.clone())))
        .collect()
}

/// compiler.md § Exception inheritance rules — E016/E017. Compares the
/// *checked* members only of `method`'s `throws` clause against the nearest
/// ancestor method it overrides (exact name + parameter-type match);
/// runtime exceptions are exempt from this rule on both sides.
fn check_exception_override(
    method: &MethodDecl,
    classes: &ClassTable,
    imports: &HashMap<String, String>,
    this_fqcn: &str,
) -> Result<(), SemaError> {
    let Some(super_fqcn) = classes.get(this_fqcn).and_then(|c| c.extends.clone()) else {
        return Ok(());
    };
    let params: Vec<Type> = method
        .params
        .iter()
        .map(|p| class_table::resolve_type(&p.ty, imports))
        .collect();
    let Some(parent) = class_table::find_method_exact(classes, &super_fqcn, &method.name, &params)
    else {
        return Ok(());
    };
    if parent.is_static {
        return Ok(());
    }
    let child_throws = resolve_throws(method, imports);
    let is_checked = |t: &Type| {
        let Type::Named(fqcn) = t else { return false };
        class_table::is_subclass_or_same(classes, fqcn, "Exception")
            && !class_table::is_subclass_or_same(classes, fqcn, "RuntimeException")
    };
    // "child throws type C covers parent throws type P" iff C is P or a
    // subclass of P — the single relation both E016 and E017 check, just
    // iterated in opposite directions.
    let covers = |c: &str, p: &str| class_table::is_subclass_or_same(classes, c, p);

    // E016: every checked exception the parent declares must be covered by
    // some type the child declares.
    for parent_exc in parent.throws.iter().filter(|t| is_checked(t)) {
        let Type::Named(parent_fqcn) = parent_exc else {
            continue;
        };
        let handled = child_throws
            .iter()
            .any(|c| matches!(c, Type::Named(child_fqcn) if covers(child_fqcn, parent_fqcn)));
        if !handled {
            return Err(SemaError::MissingThrowsInOverride(
                method.name.clone(),
                parent_fqcn.clone(),
            ));
        }
    }
    // E017: every checked exception the child declares must itself be
    // covered by something the parent already declares.
    for child_exc in child_throws.iter().filter(|t| is_checked(t)) {
        let Type::Named(child_fqcn) = child_exc else {
            continue;
        };
        let handled = parent
            .throws
            .iter()
            .any(|p| matches!(p, Type::Named(parent_fqcn) if covers(child_fqcn, parent_fqcn)));
        if !handled {
            return Err(SemaError::ExtraThrowsInOverride(
                method.name.clone(),
                child_fqcn.clone(),
            ));
        }
    }
    Ok(())
}

/// compiler.md § Duplicate definitions — E041. Signature = name + parameter
/// types only; return type does not distinguish methods. Applies equally to
/// overloaded constructors (all named `<construct>`).
fn check_duplicate_methods(class: &ClassDecl) -> Result<(), SemaError> {
    for i in 0..class.methods.len() {
        for j in (i + 1)..class.methods.len() {
            let a = &class.methods[i];
            let b = &class.methods[j];
            if a.name != b.name {
                continue;
            }
            let a_params: Vec<&Type> = a.params.iter().map(|p| &p.ty).collect();
            let b_params: Vec<&Type> = b.params.iter().map(|p| &p.ty).collect();
            if a_params == b_params {
                return Err(SemaError::DuplicateMethod(
                    a.name.clone(),
                    class.name.clone(),
                ));
            }
        }
    }
    Ok(())
}

/// compiler.md § Visibility enforcement — E019: every field/method/
/// constructor/destructor must carry an explicit `public`/`private`/
/// `protected` modifier (the parser otherwise defaults it to `Public` — see
/// `FieldDecl::visibility_explicit`/`MethodDecl::visibility_explicit`).
fn check_visibility_modifiers(class: &ClassDecl) -> Result<(), SemaError> {
    for f in &class.fields {
        if !f.visibility_explicit {
            return Err(SemaError::MissingVisibilityModifier(f.name.clone()));
        }
    }
    for m in &class.methods {
        if !m.visibility_explicit {
            return Err(SemaError::MissingVisibilityModifier(m.name.clone()));
        }
    }
    Ok(())
}

/// compiler.md § Class properties — E002. A non-nullable reference-typed
/// property with no initializer must be assigned on every path of every
/// `construct` overload. Scalars/`string`/nullable references all have a
/// valid default and are exempt (see the table there).
fn check_property_initialization(
    class: &ClassDecl,
    imports: &HashMap<String, String>,
) -> Result<(), SemaError> {
    let ctors: Vec<&MethodDecl> = class
        .methods
        .iter()
        .filter(|m| m.kind == MethodKind::Constructor)
        .collect();
    for field in &class.fields {
        if field.init.is_some() {
            continue;
        }
        let resolved = class_table::resolve_type(&field.ty, imports);
        if !matches!(resolved, Type::Named(_)) {
            continue;
        }
        if ctors.is_empty() {
            return Err(SemaError::PropertyNotInitialized(
                field.name.clone(),
                types::display(&resolved),
            ));
        }
        for ctor in &ctors {
            check_ctor_property_init(&ctors, ctor, &field.name, &resolved)?;
        }
    }
    Ok(())
}

/// Whether every path through `ctor` assigns `this.<field_name>` — a
/// `this(...)` delegation (necessarily the first statement, E045) is
/// credited with whatever its target constructor guarantees; the target is
/// independently checked on its own turn in `check_property_initialization`'s
/// loop, so no recursion is needed here (compiler.md § Constructor
/// delegation, "Definite assignment").
fn check_ctor_property_init(
    ctors: &[&MethodDecl],
    ctor: &MethodDecl,
    field_name: &str,
    field_ty: &Type,
) -> Result<(), SemaError> {
    let (start_assigned, rest): (bool, &[Stmt]) = match ctor.body.first().map(|s| &s.kind) {
        Some(StmtKind::ThisCall(args)) => {
            let argc = args.len();
            let credited = ctors.iter().any(|c| {
                class_table::arity_in_range(
                    class_table::required_count(&c.params),
                    c.params.len(),
                    argc,
                )
            });
            (credited, &ctor.body[1..])
        }
        _ => (false, &ctor.body[..]),
    };
    let (assigned, terminated) = field_assigned_after(rest, field_name, field_ty, start_assigned)?;
    if !terminated && !assigned {
        return Err(SemaError::PropertyNotInitialized(
            field_name.to_string(),
            types::display(field_ty),
        ));
    }
    Ok(())
}

/// Whether `stmts` definitely assigns `field_name` (as `this.<field_name>`)
/// by the time control falls off the end, and whether every path through
/// `stmts` terminates (via `return` or `throw`) before reaching that end.
/// An explicit `return` is itself a real exit point of construction and
/// requires the field already assigned *at that point* (checked eagerly,
/// below) — unlike `throw`, which discards the half-constructed object
/// entirely, so a path that only ever throws imposes no requirement.
fn field_assigned_after(
    stmts: &[Stmt],
    field_name: &str,
    field_ty: &Type,
    assigned: bool,
) -> Result<(bool, bool), SemaError> {
    let mut assigned = assigned;
    let mut terminated = false;
    for stmt in stmts {
        if terminated {
            break;
        }
        let (next_assigned, term) = field_assigned_stmt(stmt, field_name, field_ty, assigned)?;
        assigned = next_assigned;
        terminated = term;
    }
    Ok((assigned, terminated))
}

fn field_assigned_stmt(
    stmt: &Stmt,
    field_name: &str,
    field_ty: &Type,
    assigned: bool,
) -> Result<(bool, bool), SemaError> {
    match &stmt.kind {
        StmtKind::Return(_) => {
            if !assigned {
                return Err(SemaError::PropertyNotInitialized(
                    field_name.to_string(),
                    types::display(field_ty),
                ));
            }
            Ok((assigned, true))
        }
        StmtKind::Throw(_) => Ok((assigned, true)),
        StmtKind::Expr(Expr::Assign(LValue::Field(target, name), _))
            if matches!(**target, Expr::This) && name == field_name =>
        {
            Ok((true, false))
        }
        StmtKind::If {
            then_branch,
            else_branch,
            ..
        } => {
            let (then_assigned, then_term) =
                field_assigned_after(then_branch, field_name, field_ty, assigned)?;
            let (else_assigned, else_term) = match else_branch {
                Some(b) => field_assigned_after(b, field_name, field_ty, assigned)?,
                None => (assigned, false),
            };
            Ok(match (then_term, else_term) {
                (true, true) => (true, true),
                (true, false) => (else_assigned, false),
                (false, true) => (then_assigned, false),
                (false, false) => (then_assigned && else_assigned, false),
            })
        }
        // A loop body may execute zero times, so nothing it assigns is
        // guaranteed — but its own internal `return`s must still be
        // eagerly validated, hence the recursive call.
        StmtKind::While { body, .. } => {
            field_assigned_after(body, field_name, field_ty, assigned)?;
            Ok((assigned, false))
        }
        StmtKind::For { init, body, .. } => {
            let (after_init, _) = field_assigned_after(init, field_name, field_ty, assigned)?;
            field_assigned_after(body, field_name, field_ty, after_init)?;
            Ok((after_init, false))
        }
        StmtKind::ForEach { body, .. } => {
            field_assigned_after(body, field_name, field_ty, assigned)?;
            Ok((assigned, false))
        }
        StmtKind::Block(b) => field_assigned_after(b, field_name, field_ty, assigned),
        // Conservative, matching this codebase's existing documented gap for
        // `try` in the general E001 analysis (PLAN.md Phase 5): an exception
        // may strike at any point in `body`, so nothing it assigns is
        // guaranteed; only `finally` (which always runs) is.
        StmtKind::Try {
            body,
            catches,
            finally,
        } => {
            field_assigned_after(body, field_name, field_ty, assigned)?;
            for c in catches {
                field_assigned_after(&c.body, field_name, field_ty, assigned)?;
            }
            match finally {
                Some(f) => field_assigned_after(f, field_name, field_ty, assigned),
                None => Ok((assigned, false)),
            }
        }
        _ => Ok((assigned, false)),
    }
}

/// compiler.md § Const methods, "implements" clause — E044. A method that
/// implements an interface method declared `const` must itself be `const`
/// (matched by name + arity, same best-effort resolution as everywhere else
/// in this checker — see `check_duplicate_methods`).
fn check_const_interface_impl(
    class: &ClassDecl,
    classes: &ClassTable,
    this_fqcn: &str,
) -> Result<(), SemaError> {
    let Some(info) = classes.get(this_fqcn) else {
        return Ok(());
    };
    for iface_fqcn in &info.implements {
        let Some(iface_info) = classes.get(iface_fqcn) else {
            continue;
        };
        for iface_method in &iface_info.methods {
            if !iface_method.is_const {
                continue;
            }
            let Some(impl_method) = class.methods.iter().find(|m| {
                m.kind == MethodKind::Normal
                    && m.name == iface_method.name
                    && m.params.len() == iface_method.params.len()
            }) else {
                continue;
            };
            if !impl_method.is_const {
                return Err(SemaError::MethodMustBeConst(
                    impl_method.name.clone(),
                    iface_fqcn.clone(),
                ));
            }
        }
    }
    Ok(())
}

/// specs.md § Abstract classes and methods / § Final classes and methods —
/// E032 is checked at each `new` site (see `check_expr`'s `Expr::New` arm);
/// this covers the rest: E034 (abstract method with a body), E035 (extends
/// a final class), E036 (overrides a final method), E049 (conflicting
/// `abstract`/`final` on the same class or method), and E033 (a concrete
/// class that still has an unimplemented abstract method somewhere in its
/// `extends` chain, including one it declares directly itself).
fn check_abstract_final(
    class: &ClassDecl,
    classes: &ClassTable,
    imports: &HashMap<String, String>,
    this_fqcn: &str,
) -> Result<(), SemaError> {
    if class.is_abstract && class.is_final {
        return Err(SemaError::ConflictingModifiers(class.name.clone()));
    }
    for m in &class.methods {
        if m.is_abstract && m.is_final {
            return Err(SemaError::ConflictingModifiers(m.name.clone()));
        }
        if m.is_abstract && !m.body.is_empty() {
            return Err(SemaError::AbstractMethodHasBody(m.name.clone()));
        }
    }

    if let Some(parent_fqcn) = classes.get(this_fqcn).and_then(|c| c.extends.clone()) {
        if classes.get(&parent_fqcn).is_some_and(|p| p.is_final) {
            return Err(SemaError::ExtendFinalClass(parent_fqcn));
        }
        for m in &class.methods {
            if m.kind != MethodKind::Normal {
                continue;
            }
            let params: Vec<Type> = m
                .params
                .iter()
                .map(|p| class_table::resolve_type(&p.ty, imports))
                .collect();
            if class_table::find_method_exact(classes, &parent_fqcn, &m.name, &params)
                .is_some_and(|parent| parent.is_final)
            {
                return Err(SemaError::OverrideFinalMethod(m.name.clone()));
            }
        }
    }

    if !class.is_abstract {
        let mut current = this_fqcn.to_string();
        loop {
            let Some(info) = classes.get(&current) else {
                break;
            };
            for m in &info.methods {
                if m.is_abstract {
                    if let Some(nearest) =
                        class_table::find_method_exact(classes, this_fqcn, &m.name, &m.params)
                    {
                        if nearest.is_abstract {
                            return Err(SemaError::ClassMustBeAbstract(
                                class.name.clone(),
                                m.name.clone(),
                            ));
                        }
                    }
                }
            }
            match &info.extends {
                Some(parent) => current = parent.clone(),
                None => break,
            }
        }
    }
    Ok(())
}

/// compiler.md § Constructor delegation — `this(...)` must be the first
/// statement of a constructor (E045), and delegation chains must not be
/// cyclic (E046). Constructor overload resolution here is arity-only,
/// matching nl-codegen's best-effort resolution for this phase.
fn check_constructor_delegation(class: &ClassDecl) -> Result<(), SemaError> {
    let ctors: Vec<&MethodDecl> = class
        .methods
        .iter()
        .filter(|m| m.kind == MethodKind::Constructor)
        .collect();

    for ctor in &ctors {
        for (i, stmt) in ctor.body.iter().enumerate() {
            if matches!(&stmt.kind, StmtKind::ThisCall(_) | StmtKind::SuperCall(_)) && i != 0 {
                return Err(SemaError::ThisCallNotFirst);
            }
        }
    }

    for start in 0..ctors.len() {
        let mut current = start;
        let mut visited = HashSet::new();
        loop {
            if !visited.insert(current) {
                return Err(SemaError::DelegationCycle(class.name.clone()));
            }
            let Some(StmtKind::ThisCall(args)) = ctors[current].body.first().map(|s| &s.kind)
            else {
                break;
            };
            let argc = args.len();
            let Some(next) = ctors.iter().position(|c| {
                class_table::arity_in_range(
                    class_table::required_count(&c.params),
                    c.params.len(),
                    argc,
                )
            }) else {
                break;
            };
            current = next;
        }
    }
    Ok(())
}

fn check_method(
    method: &MethodDecl,
    sigs: &HashMap<String, MethodSig>,
    classes: &ClassTable,
    imports: &HashMap<String, String>,
    warnings: &mut Vec<(u32, SemaWarning)>,
    this_fqcn: &str,
) -> Result<(), Located> {
    let this_ty = if method.is_static {
        None
    } else {
        Some(Type::Named(this_fqcn.to_string()))
    };
    let super_ty = classes
        .get(this_fqcn)
        .and_then(|c| c.extends.clone())
        .map(Type::Named);
    let mut checker = MethodChecker {
        sigs,
        classes,
        imports,
        is_static: method.is_static,
        is_const_method: method.is_const,
        is_current_constructor: method.kind == MethodKind::Constructor,
        this_fqcn: this_fqcn.to_string(),
        this_ty,
        super_ty,
        scopes: Vec::new(),
        narrowed: HashMap::new(),
        next_id: 0,
        return_ty: class_table::resolve_type(&method.return_type, imports),
        skip_return_check: false,
        method_throws: resolve_throws(method, imports)
            .into_iter()
            .filter_map(|t| {
                if let Type::Named(fqcn) = t {
                    Some(fqcn)
                } else {
                    None
                }
            })
            .collect(),
        catch_stack: Vec::new(),
        const_vars: HashSet::new(),
        readonly_loop_vars: HashSet::new(),
        current_line: method.decl_line,
        warnings: Vec::new(),
    };
    checker.push_scope();
    let mut assigned = HashSet::new();
    for param in &method.params {
        let id = checker.declare(&param.name, class_table::resolve_type(&param.ty, imports));
        if param.is_const {
            checker.const_vars.insert(id);
        }
        // compiler.md § Named and optional parameter rules — E026.
        if let Some(default) = &param.default {
            if !is_const_expr(default) {
                return Err((method.decl_line, SemaError::DefaultNotConstant(param.name.clone())));
            }
        }
        // compiler.md § Ref parameter rules — E022: an optional parameter
        // can't also be `ref` (the caller must always supply a variable).
        if param.is_ref && param.default.is_some() {
            return Err((method.decl_line, SemaError::OptionalCannotBeRef));
        }
        assigned.insert(id);
    }
    checker
        .check_stmts(&method.body, assigned)
        .map_err(|e| (checker.current_line, e))?;
    checker.pop_scope();
    warnings.append(&mut checker.warnings);
    Ok(())
}

struct VarEntry {
    id: u32,
    ty: Type,
}

/// `assigned` is a flat set of variable ids "definitely assigned so far" on
/// the current control-flow path; ids are unique per declaration (never
/// reused), so it doesn't need to be pruned when a block's scope ends —
/// nothing can reference an out-of-scope name again anyway.
struct MethodChecker<'a> {
    sigs: &'a HashMap<String, MethodSig>,
    classes: &'a ClassTable,
    imports: &'a HashMap<String, String>,
    /// compiler.md § Static context restrictions — E040. `true` while
    /// checking a `static` method/constructor, where `this`/`super` have no
    /// meaning.
    is_static: bool,
    /// FQCN of the class currently being checked — the "accessor" context
    /// for compiler.md § Visibility enforcement (E018).
    this_fqcn: String,
    /// compiler.md § Const methods — E010/E011. `true` while checking a
    /// method declared `const`: `this.property = ...` and calls to
    /// non-`const` methods on `this` are both rejected.
    is_const_method: bool,
    /// compiler.md § Readonly classes and properties — E013/E014. `true`
    /// while checking a constructor (`construct`/`<construct>`), where
    /// `this.property = ...` is exempt from the readonly rule.
    is_current_constructor: bool,
    /// compiler.md § Const parameters/local variables — E012. Variable ids
    /// that cannot be reassigned/mutated, nor have a non-`const` method
    /// called on them (for object types). Uses the same never-reused id
    /// space as `assigned`.
    const_vars: HashSet<u32>,
    /// compiler.md § For-each loop in const context — E039. Same rules as
    /// `const_vars`, but a distinct set because the violation reports a
    /// different error code (the loop variable is *implicitly* const, not
    /// explicitly declared so).
    readonly_loop_vars: HashSet<u32>,
    /// `Some(Type::Named(fqcn))` inside an instance method/constructor,
    /// `None` in a static context (where `this` isn't valid — not yet
    /// enforced as a hard error, E040 lands with static-context checks).
    this_ty: Option<Type>,
    /// `Some(Type::Named(parent_fqcn))` inside an instance method/constructor
    /// of a class that `extends` another; used for `super.field`/
    /// `super.method(...)` expressions.
    super_ty: Option<Type>,
    scopes: Vec<HashMap<String, VarEntry>>,
    /// compiler.md § Type narrowing (smart casts) — the current *narrowed*
    /// type for a variable id, when it differs from its declared type
    /// (`VarEntry::ty`, unaffected). Overlaid on top of `scopes`/`resolve`,
    /// never on top of assignment-target validation (`check_assign` always
    /// checks against the declared type — narrowing only refines what a
    /// *read* of the variable sees). Absent from the map = "not currently
    /// narrowed, use the declared type." Entries are pushed/popped around a
    /// narrowed region (an `if` branch, one side of `&&`/`||`, a ternary
    /// branch) so narrowing never leaks past the region it was proven in,
    /// except for the one case compiler.md carves out explicitly: "early
    /// exit" narrowing, which is inserted without a matching pop so it
    /// survives into the code following the `if` (see `StmtKind::If`).
    narrowed: HashMap<u32, Type>,
    next_id: u32,
    return_ty: Type,
    /// While checking a closure body with no explicit return type
    /// (deduced — see `Expr::Closure` below), `return_ty` has nothing
    /// meaningful to hold, so `Stmt::Return`'s assignability check against
    /// it is skipped entirely rather than risk a false E004 against
    /// whatever `return_ty` happened to be left over (e.g. the *enclosing
    /// method's* return type, which is unrelated).
    skip_return_check: bool,
    /// Resolved (FQCN) `throws` clause of the method currently being
    /// checked — compiler.md § Checked exception propagation, E015.
    method_throws: Vec<String>,
    /// Resolved (FQCN) catch types of every `try` currently enclosing the
    /// code being checked, innermost last — pushed/popped in `check_try`.
    catch_stack: Vec<Vec<String>>,
    /// Line of the innermost statement currently being checked — set at the
    /// top of every `check_stmt` call (including nested/recursive ones, so
    /// it's always the most specific line active when an error is raised).
    /// `check_method` reads this after `check_stmts` fails to attach a
    /// location to the resulting `LocatedError`; initialized to the method's
    /// own `decl_line` for errors raised before any statement is checked
    /// (see the param-validation checks in `check_method`).
    current_line: u32,
    /// compiler.md § Warnings, W001 (specs.md § Nodiscard) — collected
    /// rather than raised as a `SemaError`: a nodiscard warning never fails
    /// compilation, so it can't go through the same `Result<_, Located>`
    /// path as everything else in this checker.
    warnings: Vec<(u32, SemaWarning)>,
}

impl<'a> MethodChecker<'a> {
    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn declare(&mut self, name: &str, ty: Type) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.scopes
            .last_mut()
            .expect("declare outside any scope")
            .insert(name.to_string(), VarEntry { id, ty });
        id
    }

    fn resolve(&self, name: &str) -> Option<(u32, Type)> {
        for scope in self.scopes.iter().rev() {
            if let Some(e) = scope.get(name) {
                return Some((e.id, e.ty.clone()));
            }
        }
        None
    }

    fn resolve_ty(&self, ty: &Type) -> Type {
        class_table::resolve_type(ty, self.imports)
    }

    /// `expr`, if it's a plain identifier resolving to a local
    /// variable/parameter — compiler.md § Type narrowing: "Narrowing
    /// applies to local variables and parameters only" (never
    /// `this.field`, an index expression, or a call result).
    fn narrowable_ident(&self, expr: &Expr) -> Option<(u32, Type)> {
        match expr {
            Expr::Ident(name) => self.resolve(name),
            _ => None,
        }
    }

    /// compiler.md § Type narrowing (smart casts) — the narrowing effects a
    /// boolean condition implies: `(then_narrow, else_narrow)`, each a list
    /// of variable-id -> refined-type pairs that hold when `cond` is `true`
    /// / `false` respectively. Only the forms in compiler.md's table are
    /// recognized (`!= null` / `== null`, `instanceof`, `&&`/`||` chains);
    /// anything else narrows nothing in either branch. Ternary conditions
    /// reuse this directly; `if` additionally uses it for "early exit"
    /// narrowing (see `StmtKind::If`).
    fn narrowing_from_cond(&self, cond: &Expr) -> (Vec<(u32, Type)>, Vec<(u32, Type)>) {
        match cond {
            Expr::Binary(BinOp::Ne, lhs, rhs) => {
                let target = if matches!(**rhs, Expr::NullLit) {
                    self.narrowable_ident(lhs)
                } else if matches!(**lhs, Expr::NullLit) {
                    self.narrowable_ident(rhs)
                } else {
                    None
                };
                match target {
                    Some((id, declared)) => {
                        (vec![(id, types::strip_null(&declared))], vec![(id, Type::NullT)])
                    }
                    None => (Vec::new(), Vec::new()),
                }
            }
            Expr::Binary(BinOp::Eq, lhs, rhs) => {
                let target = if matches!(**rhs, Expr::NullLit) {
                    self.narrowable_ident(lhs)
                } else if matches!(**lhs, Expr::NullLit) {
                    self.narrowable_ident(rhs)
                } else {
                    None
                };
                match target {
                    Some((id, declared)) => {
                        (vec![(id, Type::NullT)], vec![(id, types::strip_null(&declared))])
                    }
                    None => (Vec::new(), Vec::new()),
                }
            }
            // `null` always tests `false` for `instanceof` (specs.md §
            // Other operators), so the true branch also drops `null` from
            // the union — a plain `Type::Named(fqcn)`, not a union member.
            Expr::InstanceOf(target, type_name) => match self.narrowable_ident(target) {
                Some((id, _declared)) => (vec![(id, Type::Named(self.class_fqcn(type_name)))], Vec::new()),
                None => (Vec::new(), Vec::new()),
            },
            // `a && b` true => both `a` and `b`'s true-narrowing hold. Its
            // false-narrowing isn't a single fact (`a` false OR `b` false),
            // so left empty rather than guessing.
            Expr::Binary(BinOp::And, lhs, rhs) => {
                let (mut lt, _) = self.narrowing_from_cond(lhs);
                let (rt, _) = self.narrowing_from_cond(rhs);
                lt.extend(rt);
                (lt, Vec::new())
            }
            // Dual of `&&`: `a || b` false => both `a` and `b`'s
            // false-narrowing hold.
            Expr::Binary(BinOp::Or, lhs, rhs) => {
                let (_, mut le) = self.narrowing_from_cond(lhs);
                let (_, re) = self.narrowing_from_cond(rhs);
                le.extend(re);
                (Vec::new(), le)
            }
            _ => (Vec::new(), Vec::new()),
        }
    }

    /// Applies `narrow` as a temporary overlay on `self.narrowed`, checks
    /// `expr`, then restores the prior overlay state (even if `expr` fails
    /// to type-check) — narrowing from one side of `&&`/`||` or one branch
    /// of a ternary must not leak into the other.
    fn narrow_and_check_expr(
        &mut self,
        expr: &Expr,
        assigned: &mut HashSet<u32>,
        narrow: &[(u32, Type)],
    ) -> Result<Type, SemaError> {
        let saved: Vec<(u32, Option<Type>)> = narrow
            .iter()
            .map(|(id, ty)| (*id, self.narrowed.insert(*id, ty.clone())))
            .collect();
        let result = self.check_expr(expr, assigned);
        for (id, prev) in saved {
            match prev {
                Some(t) => {
                    self.narrowed.insert(id, t);
                }
                None => {
                    self.narrowed.remove(&id);
                }
            }
        }
        result
    }

    /// `narrow_and_check_expr`, for a statement block (an `if` branch)
    /// instead of a single expression.
    fn narrow_and_check_block(
        &mut self,
        block: &[Stmt],
        assigned: HashSet<u32>,
        narrow: &[(u32, Type)],
    ) -> Result<(HashSet<u32>, bool), SemaError> {
        let saved: Vec<(u32, Option<Type>)> = narrow
            .iter()
            .map(|(id, ty)| (*id, self.narrowed.insert(*id, ty.clone())))
            .collect();
        let result = self.check_block(block, assigned);
        for (id, prev) in saved {
            match prev {
                Some(t) => {
                    self.narrowed.insert(id, t);
                }
                None => {
                    self.narrowed.remove(&id);
                }
            }
        }
        result
    }

    fn class_fqcn(&self, name: &str) -> String {
        self.imports
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    /// specs.md § Nodiscard support — the name of the nodiscard method `expr`
    /// calls, if `expr` (an expression-statement's whole expression) is
    /// itself a call to one. Deliberately narrow: only a bare same-class call
    /// (`foo()`) or a direct instance call (`obj.method()`) is recognized,
    /// resolved through `simple_receiver_ty` below — a static/stdlib/native
    /// receiver never carries `is_nodiscard` (only concrete user classes do),
    /// so those are left alone rather than guessed at.
    fn nodiscard_call_name(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Call(name, args) => {
                let (_, method) =
                    class_table::find_method_owner(self.classes, &self.this_fqcn, name, args.len())?;
                method.is_nodiscard.then(|| name.clone())
            }
            Expr::MethodCall(target, name, args) => {
                let Type::Named(fqcn) = self.simple_receiver_ty(target)? else {
                    return None;
                };
                let (_, method) = class_table::find_method_owner(self.classes, &fqcn, name, args.len())?;
                method.is_nodiscard.then(|| name.clone())
            }
            _ => None,
        }
    }

    /// Best-effort static type of a call *receiver* expression, without
    /// `check_expr`'s full side-effecting type propagation (error checks,
    /// definite-assignment tracking) — only enough shapes to recognize the
    /// receiver of a realistic `obj.method()`/`this.field.method()`/chained
    /// `a.b().c()` nodiscard call. Anything else (indexing, casts, stdlib
    /// receivers, ...) gives up (`None`), same leniency as the rest of this
    /// checker for expression forms it doesn't fully model.
    fn simple_receiver_ty(&self, expr: &Expr) -> Option<Type> {
        match expr {
            Expr::This => self.this_ty.clone(),
            Expr::Ident(name) => {
                let (id, ty) = self.resolve(name)?;
                Some(self.narrowed.get(&id).cloned().unwrap_or(ty))
            }
            Expr::FieldAccess(target, name) => {
                let Type::Named(fqcn) = self.simple_receiver_ty(target)? else {
                    return None;
                };
                self.field_ty(&fqcn, name)
            }
            Expr::New(class_name, ..) => Some(Type::Named(self.class_fqcn(class_name))),
            Expr::MethodCall(target, name, args) => {
                let Type::Named(fqcn) = self.simple_receiver_ty(target)? else {
                    return None;
                };
                self.method_return_ty(&fqcn, name, args.len())
            }
            _ => None,
        }
    }

    /// Walks `fqcn`'s `extends` chain, so a field/method declared on an
    /// ancestor class resolves from a subclass reference too.
    fn field_ty(&self, fqcn: &str, name: &str) -> Option<Type> {
        let mut current = fqcn;
        loop {
            let info = self.classes.get(current)?;
            if let Some(f) = info.fields.iter().find(|f| f.name == name) {
                return Some(f.ty.clone());
            }
            current = info.extends.as_deref()?;
        }
    }

    /// compiler.md § Visibility enforcement — E018. Looks up `name` as a
    /// field of `fqcn` (walking `extends`, like `field_ty`) and checks it is
    /// accessible from the class currently being checked. A field absent
    /// from `self.classes` (native stdlib object, unresolved reference) is
    /// left to whatever leniency already applies at the call site.
    fn check_field_access(&self, fqcn: &str, name: &str) -> Result<(), SemaError> {
        let Some((owner, field)) = class_table::find_field_owner(self.classes, fqcn, name) else {
            return Ok(());
        };
        if !class_table::is_accessible(self.classes, field.visibility, &owner, &self.this_fqcn) {
            return Err(SemaError::MemberNotAccessible(
                name.to_string(),
                self.this_fqcn.clone(),
                visibility_str(field.visibility),
            ));
        }
        Ok(())
    }

    /// compiler.md § Ref parameter rules — E020/E021, checked for `arg` once
    /// it's known to bind to a parameter declared `ref` (`param_name` is
    /// only used in the error message). The call site must use the `ref`
    /// keyword (E021), and the argument must be a plain, non-const variable
    /// (E020) — not a literal, a field/index expression, or an expression
    /// result.
    fn check_ref_arg(&self, param_name: &str, arg: &Arg) -> Result<(), SemaError> {
        if !arg.is_ref {
            return Err(SemaError::MissingRefKeyword(param_name.to_string()));
        }
        let Expr::Ident(var_name) = &arg.value else {
            return Err(SemaError::RefArgNotVariable(param_name.to_string()));
        };
        if let Some((id, _)) = self.resolve(var_name) {
            if self.const_vars.contains(&id) || self.readonly_loop_vars.contains(&id) {
                return Err(SemaError::RefArgNotVariable(param_name.to_string()));
            }
        }
        Ok(())
    }

    /// `check_ref_arg`, applied to every `ref` parameter in a `bind_call_args`
    /// binding — shared by `Expr::New`/`Stmt::ThisCall`/`Stmt::SuperCall`,
    /// which all resolve against a `class_table::CtorInfo`.
    fn check_ref_args(
        &self,
        param_names: &[String],
        is_ref: &[bool],
        binding: &[Option<usize>],
        args: &[Arg],
    ) -> Result<(), SemaError> {
        for ((name, r), bound) in param_names.iter().zip(is_ref).zip(binding) {
            if *r {
                if let Some(arg_idx) = bound {
                    self.check_ref_arg(name, &args[*arg_idx])?;
                }
            }
        }
        Ok(())
    }

    /// Same as `check_field_access`, for an instance method call resolved by
    /// arity (matching the rest of this checker's best-effort overload
    /// resolution).
    fn check_method_access(&self, fqcn: &str, name: &str, argc: usize) -> Result<(), SemaError> {
        let Some((owner, method)) = class_table::find_method_owner(self.classes, fqcn, name, argc)
        else {
            return Ok(());
        };
        if !class_table::is_accessible(self.classes, method.visibility, &owner, &self.this_fqcn) {
            return Err(SemaError::MemberNotAccessible(
                name.to_string(),
                self.this_fqcn.clone(),
                visibility_str(method.visibility),
            ));
        }
        Ok(())
    }

    /// compiler.md § Readonly classes and properties — E013/E014. Assignment
    /// is allowed only inside the *declaring* class's own `construct`, and
    /// only via `this.property = ...` — a subclass constructor assigning an
    /// inherited readonly field directly (rather than through `super(...)`)
    /// is still rejected, matching specs.md § Readonly's note that
    /// delegation is the only path for that case.
    fn check_readonly(&self, target_expr: &Expr, fqcn: &str, name: &str) -> Result<(), SemaError> {
        let Some((owner, field)) = class_table::find_field_owner(self.classes, fqcn, name) else {
            return Ok(());
        };
        let class_is_readonly = self
            .classes
            .get(&owner)
            .is_some_and(|info| info.is_readonly);
        if !class_is_readonly && !field.readonly {
            return Ok(());
        }
        let exempt = self.is_current_constructor
            && self.this_fqcn == owner
            && matches!(target_expr, Expr::This);
        if exempt {
            return Ok(());
        }
        if class_is_readonly {
            Err(SemaError::ReadonlyClassModification(
                name.to_string(),
                owner,
            ))
        } else {
            Err(SemaError::ReadonlyPropertyModification(name.to_string()))
        }
    }

    /// compiler.md § Checked exception propagation — E015. `exc_fqcn` is
    /// exempt if it isn't a checked exception at all (not `Exception` or a
    /// non-`RuntimeException` subclass of it); otherwise it must be caught
    /// by an enclosing `try` (`catch_stack`) or declared in the current
    /// method's own `throws` clause.
    fn require_handled(&self, exc_fqcn: &str) -> Result<(), SemaError> {
        if !class_table::is_subclass_or_same(self.classes, exc_fqcn, "Exception")
            || class_table::is_subclass_or_same(self.classes, exc_fqcn, "RuntimeException")
        {
            return Ok(());
        }
        let covers =
            |declared: &str| class_table::is_subclass_or_same(self.classes, exc_fqcn, declared);
        if self
            .catch_stack
            .iter()
            .rev()
            .any(|catches| catches.iter().any(|c| covers(c)))
        {
            return Ok(());
        }
        if self.method_throws.iter().any(|t| covers(t)) {
            return Ok(());
        }
        Err(SemaError::UnhandledCheckedException(exc_fqcn.to_string()))
    }

    fn method_return_ty(&self, fqcn: &str, name: &str, argc: usize) -> Option<Type> {
        let mut current = fqcn;
        loop {
            let info = self.classes.get(current)?;
            if let Some(m) = info.methods.iter().find(|m| {
                m.name == name
                    && class_table::arity_in_range(m.required_count, m.params.len(), argc)
            }) {
                return Some(m.return_ty.clone());
            }
            current = info.extends.as_deref()?;
        }
    }

    fn method_throws(&self, fqcn: &str, name: &str, argc: usize) -> Vec<Type> {
        let mut current = fqcn;
        loop {
            let Some(info) = self.classes.get(current) else {
                return Vec::new();
            };
            if let Some(m) = info.methods.iter().find(|m| {
                m.name == name
                    && class_table::arity_in_range(m.required_count, m.params.len(), argc)
            }) {
                return m.throws.clone();
            }
            let Some(parent) = info.extends.as_deref() else {
                return Vec::new();
            };
            current = parent;
        }
    }

    /// Checks a block in its own scope. Returns the set of variables
    /// definitely assigned after it, and whether it unconditionally
    /// terminates the enclosing control-flow path (compiler.md § Definite
    /// assignment analysis, "Terminal statements").
    fn check_block(
        &mut self,
        block: &[Stmt],
        assigned: HashSet<u32>,
    ) -> Result<(HashSet<u32>, bool), SemaError> {
        self.push_scope();
        let result = self.check_stmts(block, assigned);
        self.pop_scope();
        result
    }

    fn check_stmts(
        &mut self,
        stmts: &[Stmt],
        mut assigned: HashSet<u32>,
    ) -> Result<(HashSet<u32>, bool), SemaError> {
        let mut terminated = false;
        for stmt in stmts {
            if terminated {
                break;
            }
            let (next_assigned, term) = self.check_stmt(stmt, assigned)?;
            assigned = next_assigned;
            terminated = term;
        }
        Ok((assigned, terminated))
    }

    fn check_stmt(
        &mut self,
        stmt: &Stmt,
        mut assigned: HashSet<u32>,
    ) -> Result<(HashSet<u32>, bool), SemaError> {
        self.current_line = stmt.line;
        match &stmt.kind {
            StmtKind::Return(Some(expr)) => {
                let ty = self.check_expr(expr, &mut assigned)?;
                if !self.skip_return_check {
                    self.check_assignable(&ty, &self.return_ty.clone())?;
                }
                Ok((assigned, true))
            }
            StmtKind::Return(None) => Ok((assigned, true)),
            StmtKind::Expr(expr) => {
                self.check_expr(expr, &mut assigned)?;
                // specs.md § Nodiscard — compiler.md W001: a bare call
                // statement (`foo();`/`obj.method();`) discards whatever it
                // returns. Only checked at this exact shape (the outermost
                // expression of an expression-statement is itself the call)
                // — same leniency this checker already applies elsewhere to
                // expression forms it doesn't fully model, rather than
                // reimplementing `check_expr`'s complete type propagation
                // just to chase a nodiscard call buried inside e.g. a binary
                // operand.
                if let Some(name) = self.nodiscard_call_name(expr) {
                    self.warnings
                        .push((stmt.line, SemaWarning::NodiscardDiscarded(name)));
                }
                // `system.ps.Process.exit(...)` (stdlib.md: "Terminal
                // statement: does not return") — treated as terminating the
                // current path exactly like `throw`/`return`, so e.g. an
                // `if`/`else` where one branch calls `exit(...)` and the
                // other assigns a variable still counts that variable as
                // definitely assigned afterwards (see `Stmt::If`'s merge
                // above). Detected structurally rather than through
                // `crate::stdlib::lookup`'s return value, since a plain
                // `Type::Void` there is indistinguishable from any other
                // void-returning stdlib call.
                let terminates = match expr {
                    Expr::MethodCall(target, name, _) if name == "exit" => {
                        dotted_path(target).as_deref() == Some("system.ps.Process")
                            && self.resolve("system").is_none()
                    }
                    _ => false,
                };
                Ok((assigned, terminates))
            }
            StmtKind::ThisCall(args) => {
                for a in args {
                    self.check_expr(&a.value, &mut assigned)?;
                }
                if let Some(ctor) =
                    class_table::find_ctor(self.classes, &self.this_fqcn, args.len())
                {
                    let binding = bind_call_args(&ctor.param_names, ctor.required_count, args)?;
                    self.check_ref_args(&ctor.param_names, &ctor.is_ref, &binding, args)?;
                }
                Ok((assigned, false))
            }
            StmtKind::SuperCall(args) => {
                for a in args {
                    self.check_expr(&a.value, &mut assigned)?;
                }
                if let Some(Type::Named(super_fqcn)) = self.super_ty.clone() {
                    if let Some(ctor) =
                        class_table::find_ctor(self.classes, &super_fqcn, args.len())
                    {
                        let binding = bind_call_args(&ctor.param_names, ctor.required_count, args)?;
                        self.check_ref_args(&ctor.param_names, &ctor.is_ref, &binding, args)?;
                    }
                }
                Ok((assigned, false))
            }
            StmtKind::Throw(expr) => {
                let ty = self.check_expr(expr, &mut assigned)?;
                if let Type::Named(fqcn) = &ty {
                    self.require_handled(fqcn)?;
                }
                Ok((assigned, true))
            }
            StmtKind::Try {
                body,
                catches,
                finally,
            } => self.check_try(body, catches, finally, assigned),
            StmtKind::VarDecl {
                ty,
                name,
                init,
                is_const,
            } => {
                let value_ty = match init {
                    Some(e) => Some(self.check_expr(e, &mut assigned)?),
                    None => None,
                };
                let declared_ty = match (ty, &value_ty) {
                    (Some(t), _) => self.resolve_ty(t),
                    (None, Some(v)) => v.clone(),
                    (None, None) => return Err(SemaError::AutoWithoutInitializer),
                };
                if let Some(v) = &value_ty {
                    self.check_assignable(v, &declared_ty)?;
                }
                let id = self.declare(name, declared_ty);
                if *is_const {
                    self.const_vars.insert(id);
                }
                if value_ty.is_some() {
                    assigned.insert(id);
                }
                Ok((assigned, false))
            }
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.check_expr(cond, &mut assigned)?;
                let (then_narrow, else_narrow) = self.narrowing_from_cond(cond);
                let (then_assigned, then_term) =
                    self.narrow_and_check_block(then_branch, assigned.clone(), &then_narrow)?;
                let (else_assigned, else_term) = match else_branch {
                    Some(b) => self.narrow_and_check_block(b, assigned.clone(), &else_narrow)?,
                    None => (assigned.clone(), false),
                };
                // compiler.md § Type narrowing, "early exit": when exactly
                // one branch unconditionally terminates the current path,
                // the other branch's narrowing is a fact for whatever
                // follows the `if` (no matching pop — see `narrowed`'s doc
                // comment on `MethodChecker`).
                match (then_term, else_term) {
                    (true, false) => {
                        for (id, ty) in else_narrow {
                            self.narrowed.insert(id, ty);
                        }
                    }
                    (false, true) => {
                        for (id, ty) in then_narrow {
                            self.narrowed.insert(id, ty);
                        }
                    }
                    _ => {}
                }
                Ok(match (then_term, else_term) {
                    (true, true) => (then_assigned.union(&else_assigned).cloned().collect(), true),
                    (true, false) => (else_assigned, false),
                    (false, true) => (then_assigned, false),
                    (false, false) => (
                        then_assigned
                            .intersection(&else_assigned)
                            .cloned()
                            .collect(),
                        false,
                    ),
                })
            }
            StmtKind::While { cond, body } => {
                self.check_expr(cond, &mut assigned)?;
                // The body may execute zero times: its assignments don't
                // make anything definitely assigned after the loop.
                self.check_block(body, assigned.clone())?;
                Ok((assigned, false))
            }
            StmtKind::ForEach {
                ty,
                var,
                iterable,
                body,
            } => {
                let iterable_ty = self.check_expr(iterable, &mut assigned)?;
                // Element type: `T` for a `T[]`, `T` for `system.List<T>`,
                // `MapEntry<K, V>` for `system.Map<K, V>` (iteration
                // desugars through `entries()` — vm.md § For-each loops).
                // Anything else is left lenient (`Void`) — nl-codegen
                // produces the precise "not iterable" error, same division
                // of labor as unknown classes/methods.
                let elem_ty = match &iterable_ty {
                    Type::Array(elem) => (**elem).clone(),
                    Type::Named(fqcn) => {
                        crate::native_generics::foreach_element_ty(fqcn).unwrap_or(Type::Void)
                    }
                    _ => Type::Void,
                };
                self.push_scope();
                let declared_ty = match ty {
                    Some(t) => {
                        let declared = self.resolve_ty(t);
                        self.check_assignable(&elem_ty, &declared)?;
                        declared
                    }
                    None => elem_ty,
                };
                // The loop variable is (re)assigned by the loop itself
                // before each iteration of the body.
                let id = self.declare(var, declared_ty);
                // compiler.md § For-each loop in const context — E039: the
                // loop variable is implicitly non-modifiable when iterating
                // `this.<field>` inside a `const` method, or a const/const
                // `ref` parameter.
                let is_readonly_collection = match iterable {
                    Expr::FieldAccess(target, _) => {
                        self.is_const_method && matches!(**target, Expr::This)
                    }
                    Expr::Ident(name) => self
                        .resolve(name)
                        .is_some_and(|(id, _)| self.const_vars.contains(&id)),
                    _ => false,
                };
                if is_readonly_collection {
                    self.readonly_loop_vars.insert(id);
                }
                let mut body_assigned = assigned.clone();
                body_assigned.insert(id);
                self.check_stmts(body, body_assigned)?;
                self.pop_scope();
                // Zero iterations possible — same rule as `while`.
                Ok((assigned, false))
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                self.push_scope();
                let mut inner = assigned.clone();
                for s in init {
                    let (next, _) = self.check_stmt(s, inner)?;
                    inner = next;
                }
                if let Some(cond) = cond {
                    self.check_expr(cond, &mut inner)?;
                }
                let (mut body_assigned, _) = self.check_block(body, inner)?;
                for e in step {
                    self.check_expr(e, &mut body_assigned)?;
                }
                self.pop_scope();
                // Same rule as `while`: nothing propagates past the loop.
                Ok((assigned, false))
            }
            // Not formally listed among compiler.md's terminal statements,
            // but code following `break`/`continue` in the same block is
            // equally unreachable on that path, so definite-assignment
            // merges (e.g. in an enclosing `if`) must treat them as such.
            StmtKind::Break | StmtKind::Continue => Ok((assigned, true)),
            StmtKind::Block(block) => self.check_block(block, assigned),
        }
    }

    fn check_assignable(&self, value_ty: &Type, target_ty: &Type) -> Result<(), SemaError> {
        // `Type::Void` here means "not actually modeled as a real type by
        // this checker" (an unresolved call, a closure literal — see
        // `Expr::Closure` above — or any of several other lenient
        // fallbacks throughout `check_expr`), not a genuine `void` value —
        // treated as a wildcard so those fallbacks don't produce a false
        // E004. A truly void-returning call assigned somewhere non-void
        // still fails, just at nl-codegen instead (`coerce_value`).
        if matches!(value_ty, Type::Void) {
            return Ok(());
        }
        if matches!(value_ty, Type::NullT) && !types::is_nullable(target_ty) {
            return Err(SemaError::NullToNonNullable(types::display(target_ty)));
        }
        if self.is_object_assignable(value_ty, target_ty) {
            return Ok(());
        }
        if !types::is_assignable(value_ty, target_ty) {
            return Err(SemaError::NotAssignable(
                types::display(value_ty),
                types::display(target_ty),
            ));
        }
        Ok(())
    }

    /// `types::is_assignable` only knows structural/primitive rules; it has
    /// no notion of interfaces. A class value is also assignable to any
    /// interface type it directly `implements` (compiler.md's subtyping for
    /// reference types) — checked separately here since it needs
    /// `self.classes`. Walks union members on both sides (compiler.md §
    /// Union type compatibility: a value is assignable to a union if it's a
    /// subtype of *some* constituent, e.g. `Animal|null pet = new Dog()`
    /// where `Dog implements Animal`). No transitivity through
    /// interface-`extends` or class inheritance (out of scope this phase).
    fn is_object_assignable(&self, value_ty: &Type, target_ty: &Type) -> bool {
        let target_members = types::members(target_ty);
        types::members(value_ty).iter().all(|vm| {
            let Type::Named(from) = vm else {
                return false;
            };
            target_members.iter().any(|tm| {
                let Type::Named(to) = tm else {
                    return false;
                };
                class_table::is_subclass_or_same(self.classes, from, to)
                    || self
                        .classes
                        .get(from)
                        .is_some_and(|info| info.implements.iter().any(|i| i == to))
            })
        })
    }

    /// `(T) expr` cast validation — compiler.md § Cast validation / E007.
    /// Numeric widening/narrowing between `int`/`float`/`byte` is always
    /// valid either way (the direction only matters for whether a cast is
    /// *required*, which is a parser/ergonomics concern, not a validity
    /// one); casting to `string` is restricted to primitives, same as `+`
    /// concatenation (E008) — this codebase doesn't implement `Stringable`
    /// dispatch (see `nl_vm::value::Value::to_display_string`), so a
    /// reference-type `(string)` cast can never actually succeed at
    /// runtime. Class casts (either direction — upcast is always valid,
    /// downcast is checked at runtime by `CHECKCAST`/`InvalidCastException`)
    /// are accepted between any two classes/interfaces related by `extends`
    /// or `implements`; unrelated classes are rejected at compile time.
    fn check_cast(&self, from: &Type, to: &Type) -> Result<(), SemaError> {
        // `Type::Void` = "not really modeled by this checker" wildcard, same
        // as `check_assignable` (an unresolved call, a closure literal, ...).
        if matches!(from, Type::Void) || matches!(to, Type::Void) {
            return Ok(());
        }
        // The bare `null` literal (not a `T|null` union) can be cast to any
        // reference type — vm.md's `CHECKCAST` explicitly lets `null`
        // through. Casting a genuinely nullable union to its non-null
        // member, just below, is the case compiler.md actually disallows.
        if matches!(from, Type::NullT) {
            return Ok(());
        }
        if types::is_nullable(from) && !types::is_nullable(to) {
            return Err(SemaError::BadCast(types::display(from), types::display(to)));
        }
        if types::is_numeric(from) && types::is_numeric(to) {
            return Ok(());
        }
        if matches!(to, Type::StringT) {
            return if is_concat_operand(from) {
                Ok(())
            } else {
                Err(SemaError::BadCast(types::display(from), types::display(to)))
            };
        }
        if let (Type::Named(f), Type::Named(t)) = (from, to) {
            if f == t
                || class_table::is_subclass_or_same(self.classes, f, t)
                || class_table::is_subclass_or_same(self.classes, t, f)
                || self
                    .classes
                    .get(f)
                    .is_some_and(|info| info.implements.iter().any(|i| i == t))
                || self
                    .classes
                    .get(t)
                    .is_some_and(|info| info.implements.iter().any(|i| i == f))
            {
                return Ok(());
            }
            return Err(SemaError::BadCast(types::display(from), types::display(to)));
        }
        if let (Type::Array(fe), Type::Array(te)) = (from, to) {
            return self.check_cast(fe, te);
        }
        if from == to {
            return Ok(());
        }
        Err(SemaError::BadCast(types::display(from), types::display(to)))
    }

    fn check_expr(&mut self, expr: &Expr, assigned: &mut HashSet<u32>) -> Result<Type, SemaError> {
        match expr {
            Expr::IntLit(_) => Ok(Type::Int),
            Expr::FloatLit(_) => Ok(Type::Float),
            Expr::BoolLit(_) => Ok(Type::Bool),
            Expr::StringLit(_) => Ok(Type::StringT),
            Expr::NullLit => Ok(Type::NullT),
            // compiler.md § Static context restrictions — E040: `this` has no
            // meaning in a static method (there is no instance).
            Expr::This => {
                if self.is_static {
                    return Err(SemaError::StaticContextMisuse("this".to_string()));
                }
                Ok(self.this_ty.clone().unwrap_or(Type::Void))
            }
            // Same restriction applies to `super`; a `super` used in a class
            // with no `extends` (but not static) is a separate, still-lenient
            // gap deferred to nl-codegen.
            Expr::Super => {
                if self.is_static {
                    return Err(SemaError::StaticContextMisuse("super".to_string()));
                }
                Ok(self.super_ty.clone().unwrap_or(Type::Void))
            }
            Expr::Ident(name) => {
                // Unresolved names have no dedicated E-code in compiler.md;
                // nl-codegen already rejects them, so just defer to it here.
                let Some((id, ty)) = self.resolve(name) else {
                    return Ok(Type::Void);
                };
                if !assigned.contains(&id) {
                    return Err(SemaError::NotDefinitelyAssigned(name.clone()));
                }
                // compiler.md § Type narrowing (smart casts) — a read sees
                // the current narrowed type, if any; assignment-target
                // validation (`check_assign`) deliberately doesn't go
                // through this and always uses the declared type instead.
                Ok(self.narrowed.get(&id).cloned().unwrap_or(ty))
            }
            Expr::Assign(target, value) => self.check_assign(target, value, assigned),
            Expr::Call(name, args) => {
                let mut arg_types = Vec::with_capacity(args.len());
                for a in args {
                    arg_types.push(self.check_expr(&a.value, assigned)?);
                }
                // Unresolved calls: no dedicated E-code, deferred to nl-codegen.
                let Some((params, return_ty, throws)) = self.sigs.get(name).cloned() else {
                    return Ok(Type::Void);
                };
                let names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
                let required = class_table::required_count(&params);
                let binding = bind_call_args(&names, required, args)?;
                for (p, bound) in params.iter().zip(&binding) {
                    if let Some(arg_idx) = bound {
                        if p.is_ref {
                            self.check_ref_arg(&p.name, &args[*arg_idx])?;
                        }
                        let expected = self.resolve_ty(&p.ty);
                        self.check_assignable(&arg_types[*arg_idx], &expected)?;
                    }
                }
                for t in &throws {
                    if let Type::Named(fqcn) = t {
                        self.require_handled(fqcn)?;
                    }
                }
                Ok(return_ty)
            }
            Expr::New(class_name, _type_args, args) => {
                for a in args {
                    self.check_expr(&a.value, assigned)?;
                }
                let fqcn = self.class_fqcn(class_name);
                // compiler.md § Abstract classes and methods — E032.
                if self.classes.get(&fqcn).is_some_and(|c| c.is_abstract) {
                    return Err(SemaError::InstantiateAbstractClass(fqcn));
                }
                if let Some(ctor) = class_table::find_ctor(self.classes, &fqcn, args.len()) {
                    // Constructors are never inherited (each class declares
                    // its own), so the declaring class is always `fqcn`
                    // itself — no `find_ctor`-with-owner needed here.
                    if !class_table::is_accessible(
                        self.classes,
                        ctor.visibility,
                        &fqcn,
                        &self.this_fqcn,
                    ) {
                        return Err(SemaError::MemberNotAccessible(
                            "<construct>".to_string(),
                            self.this_fqcn.clone(),
                            visibility_str(ctor.visibility),
                        ));
                    }
                    let binding = bind_call_args(&ctor.param_names, ctor.required_count, args)?;
                    self.check_ref_args(&ctor.param_names, &ctor.is_ref, &binding, args)?;
                    for t in ctor.throws.clone() {
                        if let Type::Named(exc_fqcn) = t {
                            self.require_handled(&exc_fqcn)?;
                        }
                    }
                }
                Ok(Type::Named(fqcn))
            }
            Expr::NewArray(elem_ty, dims) => {
                for size in dims.iter().flatten() {
                    let size_ty = self.check_expr(size, assigned)?;
                    if !types::is_numeric(&size_ty) {
                        // No dedicated E-code for a non-int array size yet;
                        // nl-codegen rejects it precisely.
                    }
                }
                // compiler.md § Multidimensional array creation — E038:
                // omitted sizes (`[]`) may only form a contiguous suffix
                // from the right. `m` = how many leading dimensions are
                // provided; anything from there on must stay omitted.
                let m = dims.iter().take_while(|d| d.is_some()).count();
                if dims[m..].iter().any(|d| d.is_some()) {
                    return Err(SemaError::NonContiguousArrayDimensionOmission(
                        new_array_source(elem_ty, dims),
                    ));
                }
                let resolved = self.resolve_ty(elem_ty);
                // compiler.md § Default values, "Array creation with fixed
                // size" — E031: a non-nullable class/interface element type
                // has no default value, so `new T[n]` is rejected. Only
                // applies when every dimension is actually provided — an
                // omitted trailing dimension makes the containing level's
                // element type an array (always nullable as an element), so
                // the missing-default problem never arises there. Scalars,
                // `string`, and nullable references all have a valid
                // default (see the table there) and are exempt regardless.
                if dims.len() == m && matches!(&resolved, Type::Named(_)) {
                    return Err(SemaError::NonNullableArrayFixedSize(types::display(
                        &resolved,
                    )));
                }
                Ok(build_new_array_type(&resolved, dims.len(), m))
            }
            Expr::NewArrayInit(elem_ty, elements) => {
                for e in elements {
                    self.check_expr(e, assigned)?;
                }
                Ok(Type::Array(Box::new(self.resolve_ty(elem_ty))))
            }
            Expr::FieldAccess(target, name) => {
                // `system.io.FileMode.Read` etc. — a dotted class-path
                // expression naming an enum-like stdlib constant, not a
                // value; same recognition-before-resolution shape as the
                // `Expr::MethodCall` arm's `system.Out.print(...)` check
                // below (see `crate::stdlib`'s module doc comment).
                if let Some(path) = dotted_path(target) {
                    let leading = path.split('.').next().expect("dotted_path is never empty");
                    if self.resolve(leading).is_none() {
                        if let Some(ty) = crate::stdlib::enum_const_ty(&path, name) {
                            return Ok(ty);
                        }
                        // `Status.OK` — a user-declared enum's case
                        // constant. Its *static* type is the enum itself
                        // (`Status`), not the case field's backing type
                        // (`int`/`string`) — same nominal-over-primitive
                        // shape as `enum_const_ty` above, just for a real
                        // `ClassInfo` instead of a hand-written stdlib
                        // table. Consulted before `check_expr(target)`
                        // below since `target` (e.g. `Ident("Status")`)
                        // isn't a value and would fail to resolve as one.
                        let fqcn = self.class_fqcn(&path);
                        if let Some(info) = self.classes.get(&fqcn) {
                            if info.is_enum && info.enum_cases.iter().any(|c| c == name) {
                                return Ok(Type::Named(fqcn));
                            }
                        }
                    }
                }
                let target_ty = self.check_expr(target, assigned)?;
                // A nullable native result type (e.g. `system.text.Regex
                // .matchFirst`'s `RegexMatch|null`) collapses to its named
                // member here, same as `nl_codegen::expr::expr_ty_of`'s
                // union-to-first-non-null-member rule — values are
                // dynamically tagged at runtime (vm.md § Value
                // representation), so this isn't narrowing, just recognizing
                // which class's field table to consult. A real `null` at
                // this point still throws `NullPointerException` at runtime
                // (`GET_FIELD` on a null receiver), same as ever.
                let named = match &target_ty {
                    Type::Named(fqcn) => Some(fqcn.as_str()),
                    Type::Union(members) => members.iter().find_map(|m| match m {
                        Type::Named(fqcn) => Some(fqcn.as_str()),
                        _ => None,
                    }),
                    _ => None,
                };
                let Some(fqcn) = named else {
                    return Ok(Type::Void);
                };
                // `status.value` — specs.md § Typed enums: reads back the
                // case's backing value. There is no real `value` field on
                // the generated class (case constants are named after the
                // case itself — vm.md § Enum representation), so this is
                // special-cased instead of falling into `field_ty` below,
                // which would report an unknown-field error. The backing
                // type is read off the first case field (case constants are
                // always emitted before any custom static field — see
                // `nl_syntax::parser::parse_enum_decl`).
                if name == "value" {
                    if let Some(info) = self.classes.get(fqcn) {
                        if info.is_enum {
                            let backing = info
                                .fields
                                .first()
                                .map(|f| f.ty.clone())
                                .unwrap_or(Type::Int);
                            return Ok(backing);
                        }
                    }
                }
                // `entry.key`/`entry.value` on a `system.MapEntry<K, V>` —
                // native result type, absent from `self.classes`.
                if let Some(ty) = crate::native_generics::field_ty(fqcn, name) {
                    return Ok(ty);
                }
                // `response.statusCode`/`.body`/`.headers` on a
                // `system.net.HttpResponse` — non-generic native result
                // type, same absence-from-`self.classes` situation.
                if let Some(ty) = crate::stdlib::result_field_ty(fqcn, name) {
                    return Ok(ty);
                }
                self.check_field_access(fqcn, name)?;
                Ok(self.field_ty(fqcn, name).unwrap_or(Type::Void))
            }
            Expr::MethodCall(target, name, args) => {
                let target_ty = self.check_expr(target, assigned)?;
                let mut arg_types = Vec::with_capacity(args.len());
                for a in args {
                    arg_types.push(self.check_expr(&a.value, assigned)?);
                }
                // `system.Out.print(...)` and friends: the receiver is a
                // dotted namespace/class path, not a value, so it never
                // resolves through `self.resolve`/`self.classes` above —
                // recognized here by name instead (see crate::stdlib).
                if let Some(path) = dotted_path(target) {
                    let leading = path.split('.').next().expect("dotted_path is never empty");
                    if self.resolve(leading).is_none() {
                        if let Some((param_types, return_ty)) =
                            crate::stdlib::lookup(&path, name, args.len())
                        {
                            for (actual, expected) in arg_types.iter().zip(&param_types) {
                                self.check_assignable(actual, expected)?;
                            }
                            for exc in crate::stdlib::throws(&path, name) {
                                self.require_handled(exc)?;
                            }
                            return Ok(return_ty);
                        }
                        // `Utils.max(a, b)` / `Status.tryFrom(...)` — a
                        // dotted path resolving to a *user* class's static
                        // method, not a value (mirrors nl-codegen's
                        // `compile_static_user_call` recognition). Without
                        // this, the call's return type falls through to the
                        // lenient `Expr::Ident` "unresolved -> Void"
                        // default below — harmless when the result is
                        // discarded or assigned to an explicitly-typed
                        // local, but wrong for anything that then
                        // type-checks the result further (`== null`, a
                        // `match` subject, an `auto`-inferred local) —
                        // exactly what enum `from`/`tryFrom` need.
                        let fqcn = self.class_fqcn(&path);
                        if let Some((owner, method)) =
                            class_table::find_method_owner(self.classes, &fqcn, name, args.len())
                        {
                            if method.is_static {
                                if !class_table::is_accessible(
                                    self.classes,
                                    method.visibility,
                                    &owner,
                                    &self.this_fqcn,
                                ) {
                                    return Err(SemaError::MemberNotAccessible(
                                        name.to_string(),
                                        self.this_fqcn.clone(),
                                        visibility_str(method.visibility),
                                    ));
                                }
                                let binding = bind_call_args(
                                    &method.param_names,
                                    method.required_count,
                                    args,
                                )?;
                                self.check_ref_args(
                                    &method.param_names,
                                    &method.is_ref,
                                    &binding,
                                    args,
                                )?;
                                for t in method.throws.clone() {
                                    if let Type::Named(exc_fqcn) = t {
                                        self.require_handled(&exc_fqcn)?;
                                    }
                                }
                                return Ok(method.return_ty.clone());
                            }
                        }
                    }
                }
                match &target_ty {
                    // specs.md § Arrays, Built-in methods — `length` is a
                    // dedicated opcode (`ARRAY_LENGTH`) rather than a native
                    // dispatch, but is still checked here like the rest.
                    // `map`/`filter`/`forEach`/`sort`/`find` take a closure
                    // argument, which always checks as `Type::Void` (a
                    // closure literal's own inferred type is still never
                    // deduced as a real `Type::Function` here — see
                    // `Expr::Closure` above), so nothing more to validate
                    // about `arg_types` for those; `map`'s actual element
                    // type `U` isn't
                    // statically known either, hence the `Type::Void`
                    // wildcard return (same joker `check_assignable` already
                    // gives every other not-yet-modeled expression form) —
                    // `nl-codegen` recovers the real element type from the
                    // closure's own deduced return type at emission time.
                    Type::Array(elem) => match (name.as_str(), args.len()) {
                        ("length", 0) => Ok(Type::Int),
                        ("slice", 2) => {
                            for a in &arg_types {
                                self.check_assignable(a, &Type::Int)?;
                            }
                            Ok(Type::Array(elem.clone()))
                        }
                        ("map", 1) => Ok(Type::Void),
                        ("filter", 1) => Ok(Type::Array(elem.clone())),
                        ("forEach", 1) => Ok(Type::Void),
                        ("sort", 1) => Ok(Type::Void),
                        ("find", 1) => Ok(Type::Union(vec![(**elem).clone(), Type::NullT])),
                        _ => Ok(Type::Void),
                    },
                    // `text.trim()` etc. — see `crate::stdlib::lookup`'s
                    // doc comment: instance calls are looked up under the
                    // same table as the static `system.String.trim(text)`
                    // form, keyed by the *full* argument count (receiver
                    // included). Unknown methods fall through leniently to
                    // `Type::Void`, same as the `Type::Named` arm below —
                    // nl-codegen produces the real diagnostic.
                    Type::StringT => {
                        let full_argc = args.len() + 1;
                        match crate::stdlib::lookup("system.String", name, full_argc) {
                            Some((param_types, return_ty)) => {
                                for (actual, expected) in arg_types.iter().zip(&param_types[1..]) {
                                    self.check_assignable(actual, expected)?;
                                }
                                Ok(return_ty)
                            }
                            None => Ok(Type::Void),
                        }
                    }
                    // `list.size()`/`map.get(k)` etc. — `fqcn` is already a
                    // monomorphized instantiation FQCN like
                    // `"system.List<int>"` (nl_syntax::monomorphize mangles
                    // `new system.List<int>(...)`/`system.List<int>`-typed
                    // locals before nl-sema ever runs), so the element
                    // type(s) are recovered straight from it — see
                    // `crate::native_generics`'s doc comment.
                    // `handle.read(...)` etc. — instance methods of a native
                    // stdlib object class (`system.io.FileHandle`), which has
                    // no entry in `self.classes`; its checked exceptions
                    // still feed E015 like a user class's `throws` would.
                    Type::Named(fqcn) if crate::stdlib::is_native_instance(fqcn) => {
                        match crate::stdlib::instance_lookup(fqcn, name, args.len()) {
                            Some((param_types, return_ty)) => {
                                for (actual, expected) in arg_types.iter().zip(&param_types) {
                                    self.check_assignable(actual, expected)?;
                                }
                                for exc in crate::stdlib::throws(fqcn, name) {
                                    self.require_handled(exc)?;
                                }
                                Ok(return_ty)
                            }
                            None => Ok(Type::Void),
                        }
                    }
                    Type::Named(fqcn) if crate::native_generics::is_instance(fqcn) => {
                        match crate::native_generics::method_signature(fqcn, name, args.len()) {
                            Some((param_types, return_ty)) => {
                                for (actual, expected) in arg_types.iter().zip(&param_types) {
                                    self.check_assignable(actual, expected)?;
                                }
                                Ok(return_ty)
                            }
                            None => Ok(Type::Void),
                        }
                    }
                    Type::Named(fqcn) => {
                        self.check_method_access(fqcn, name, args.len())?;
                        // compiler.md § Named and optional parameter rules —
                        // E023-E025. Unresolved (unknown method) is left
                        // lenient, like the rest of this branch.
                        if let Some((_, method)) =
                            class_table::find_method_owner(self.classes, fqcn, name, args.len())
                        {
                            let binding =
                                bind_call_args(&method.param_names, method.required_count, args)?;
                            self.check_ref_args(
                                &method.param_names,
                                &method.is_ref,
                                &binding,
                                args,
                            )?;
                        }
                        // compiler.md § Const methods — E011: a non-`const`
                        // method cannot be called on `this` from inside a
                        // `const` method.
                        if self.is_const_method && matches!(**target, Expr::This) {
                            if let Some((_, method)) =
                                class_table::find_method_owner(self.classes, fqcn, name, args.len())
                            {
                                if !method.is_const {
                                    return Err(SemaError::ConstMethodNonConstCall(name.clone()));
                                }
                            }
                        }
                        // compiler.md § Const parameters/local variables —
                        // E012: "for object types, only const methods may be
                        // called on it".
                        if let Expr::Ident(var_name) = &**target {
                            if let Some((id, _)) = self.resolve(var_name) {
                                let is_readonly_loop_var = self.readonly_loop_vars.contains(&id);
                                let is_const_var = self.const_vars.contains(&id);
                                if is_readonly_loop_var || is_const_var {
                                    if let Some((_, method)) = class_table::find_method_owner(
                                        self.classes,
                                        fqcn,
                                        name,
                                        args.len(),
                                    ) {
                                        if !method.is_const {
                                            return Err(if is_readonly_loop_var {
                                                SemaError::ConstLoopVariableModification(
                                                    var_name.clone(),
                                                )
                                            } else {
                                                SemaError::ConstModification(var_name.clone())
                                            });
                                        }
                                    }
                                }
                            }
                        }
                        for t in self.method_throws(fqcn, name, args.len()) {
                            if let Type::Named(exc_fqcn) = t {
                                self.require_handled(&exc_fqcn)?;
                            }
                        }
                        Ok(self
                            .method_return_ty(fqcn, name, args.len())
                            .unwrap_or(Type::Void))
                    }
                    _ => Ok(Type::Void),
                }
            }
            Expr::Index(target, index) => {
                let target_ty = self.check_expr(target, assigned)?;
                let index_ty = self.check_expr(index, assigned)?;
                let _ = index_ty;
                match target_ty {
                    Type::Array(elem) => Ok(*elem),
                    _ => Ok(Type::Void),
                }
            }
            Expr::InstanceOf(target, _type_name) => {
                self.check_expr(target, assigned)?;
                Ok(Type::Bool)
            }
            Expr::Cast(target_ty, inner) => {
                let value_ty = self.check_expr(inner, assigned)?;
                let target_ty = self.resolve_ty(target_ty);
                self.check_cast(&value_ty, &target_ty)?;
                Ok(target_ty)
            }
            Expr::PostIncr(name) | Expr::PostDecr(name) => {
                let Some((id, ty)) = self.resolve(name) else {
                    return Ok(Type::Int);
                };
                if !assigned.contains(&id) {
                    return Err(SemaError::NotDefinitelyAssigned(name.clone()));
                }
                if self.readonly_loop_vars.contains(&id) {
                    return Err(SemaError::ConstLoopVariableModification(name.clone()));
                }
                if self.const_vars.contains(&id) {
                    return Err(SemaError::ConstModification(name.clone()));
                }
                Ok(ty)
            }
            Expr::Unary(op, inner) => {
                let ty = self.check_expr(inner, assigned)?;
                match op {
                    UnOp::Neg if types::is_numeric(&ty) => Ok(ty),
                    UnOp::Neg => Err(SemaError::BadUnaryOperator(
                        "-".to_string(),
                        types::display(&ty),
                    )),
                    UnOp::Not if matches!(ty, Type::Bool) => Ok(Type::Bool),
                    UnOp::Not => Err(SemaError::BadUnaryOperator(
                        "!".to_string(),
                        types::display(&ty),
                    )),
                }
            }
            Expr::Binary(op, lhs, rhs) => self.check_binary(*op, lhs, rhs, assigned),
            Expr::Match(subject, arms) => self.check_match(subject, arms, assigned),
            Expr::Ternary(cond, then_e, else_e) => {
                let cond_ty = self.check_expr(cond, assigned)?;
                if !matches!(cond_ty, Type::Bool) {
                    return Err(SemaError::BadUnaryOperator(
                        "?:".to_string(),
                        types::display(&cond_ty),
                    ));
                }
                // compiler.md § Type narrowing: "Ternary condition — same
                // as if/else: each branch sees the narrowing implied by the
                // condition."
                let (then_narrow, else_narrow) = self.narrowing_from_cond(cond);
                let then_ty = self.narrow_and_check_expr(then_e, assigned, &then_narrow)?;
                // Lenient about mismatched branch types, same as `match`
                // arms above — nl-codegen enforces coercibility at emission
                // time, where it also has `ExprTy` to work with.
                self.narrow_and_check_expr(else_e, assigned, &else_narrow)?;
                Ok(then_ty)
            }
            // specs.md § Nullish coalescing operator / § Elvis operator.
            // Leniency mirrors `Ternary` above: the result type is
            // approximated as the left operand's type with `null` stripped
            // (real coercion of the right operand happens in nl-codegen,
            // which has `ExprTy` to work with). Not enforcing that the left
            // operand's type actually includes `null` is deliberate — no
            // E-code covers this (Phase 7's 49 codes are closed), so this
            // stays as permissive as `Ternary`/`match` rather than adding a
            // new one for it.
            Expr::Coalesce(lhs, rhs) | Expr::Elvis(lhs, rhs) => {
                let lty = self.check_expr(lhs, assigned)?;
                self.check_expr(rhs, assigned)?;
                Ok(types::strip_null(&lty))
            }
            // vm.md § Closures — checked like a nested block with its own
            // additional param declarations, so definite assignment on a
            // *captured* variable still applies (it must be assigned by the
            // time the closure literal is created — capture is by value,
            // see nl-codegen's `ExprTy::Closure`). No dedicated static type
            // to report: `Type::Function` exists (specs.md § Function type
            // assignment, for *explicit* declarations — see `resolve_ty`),
            // but a closure *literal*'s own inferred type is still never
            // deduced into one here — deliberately still `Type::Void`, same
            // leniency `check_assignable`
            // already gives every other not-yet-modeled expression form.
            Expr::Closure {
                params,
                body,
                return_type,
                ..
            } => {
                self.push_scope();
                let mut inner_assigned = assigned.clone();
                for p in params {
                    let ty = self.resolve_ty(&p.ty);
                    let id = self.declare(&p.name, ty);
                    if p.is_const {
                        self.const_vars.insert(id);
                    }
                    if let Some(default) = &p.default {
                        if !is_const_expr(default) {
                            return Err(SemaError::DefaultNotConstant(p.name.clone()));
                        }
                    }
                    inner_assigned.insert(id);
                }
                // A closure's `return` statements must be checked against
                // *its own* declared/deduced return type, not whatever
                // `self.return_ty` holds for the enclosing method.
                let saved_return_ty = std::mem::replace(&mut self.return_ty, Type::Void);
                let saved_skip = self.skip_return_check;
                match return_type {
                    Some(t) => {
                        self.return_ty = self.resolve_ty(t);
                        self.skip_return_check = false;
                    }
                    None => self.skip_return_check = true,
                }
                let body_result = match body {
                    nl_syntax::ast::ClosureBody::Block(block) => {
                        self.check_stmts(block, inner_assigned).map(|_| ())
                    }
                    nl_syntax::ast::ClosureBody::Expr(e) => {
                        self.check_expr(e, &mut inner_assigned).map(|_| ())
                    }
                };
                self.return_ty = saved_return_ty;
                self.skip_return_check = saved_skip;
                self.pop_scope();
                body_result?;
                Ok(Type::Void)
            }
        }
    }

    /// compiler.md § Match exhaustiveness — E047. Exhaustive without a
    /// `default` arm for `bool` (both `true`/`false` present) and for an
    /// enum subject (every case name has an arm — specs.md § Enums);
    /// everything else requires `default`. Two arms with the same constant
    /// literal are also E047 (the second would be unreachable).
    fn check_match(
        &mut self,
        subject: &Expr,
        arms: &[MatchArm],
        assigned: &mut HashSet<u32>,
    ) -> Result<Type, SemaError> {
        let subject_ty = self.check_expr(subject, assigned)?;
        // An enum-typed subject's arms are `EnumName.CaseName` patterns —
        // `Expr::FieldAccess` nodes, not constant literals, so they need
        // their own coverage tracking (by case name) alongside `literal_eq`'s
        // duplicate check below (which still applies, e.g. two arms both
        // matching `Status.OK`).
        let enum_case_names: Option<Vec<String>> = match &subject_ty {
            Type::Named(fqcn) => self
                .classes
                .get(fqcn)
                .filter(|info| info.is_enum)
                .map(|info| info.enum_cases.clone()),
            _ => None,
        };
        let mut seen: Vec<&Expr> = Vec::new();
        let mut has_default = false;
        let mut has_true = false;
        let mut has_false = false;
        let mut covered_cases: HashSet<String> = HashSet::new();
        let mut result_ty: Option<Type> = None;
        for arm in arms {
            match &arm.pattern {
                None => has_default = true,
                Some(pat) => {
                    if seen.iter().any(|s| literal_eq(s, pat)) {
                        return Err(SemaError::MatchNotExhaustive(
                            "unreachable duplicate arm".to_string(),
                        ));
                    }
                    seen.push(pat);
                    match pat {
                        Expr::BoolLit(true) => has_true = true,
                        Expr::BoolLit(false) => has_false = true,
                        Expr::FieldAccess(_, case_name) => {
                            covered_cases.insert(case_name.clone());
                        }
                        _ => {}
                    }
                    self.check_expr(pat, assigned)?;
                }
            }
            let value_ty = self.check_expr(&arm.value, assigned)?;
            if result_ty.is_none() {
                result_ty = Some(value_ty);
            }
        }
        let exhaustive = has_default
            || (matches!(subject_ty, Type::Bool) && has_true && has_false)
            || enum_case_names
                .as_ref()
                .is_some_and(|cases| cases.iter().all(|c| covered_cases.contains(c)));
        if !exhaustive {
            return Err(SemaError::MatchNotExhaustive("default".to_string()));
        }
        Ok(result_ty.unwrap_or(Type::Void))
    }

    /// compiler.md § Unreachable catch clauses — E048.
    fn check_try(
        &mut self,
        body: &Block,
        catches: &[CatchClause],
        finally: &Option<Block>,
        assigned: HashSet<u32>,
    ) -> Result<(HashSet<u32>, bool), SemaError> {
        for j in 0..catches.len() {
            let ty_j = self.class_fqcn(&catches[j].ty);
            for earlier in &catches[..j] {
                let ty_i = self.class_fqcn(&earlier.ty);
                if class_table::is_subclass_or_same(self.classes, &ty_j, &ty_i) {
                    return Err(SemaError::UnreachableCatch(
                        catches[j].ty.clone(),
                        earlier.ty.clone(),
                    ));
                }
            }
        }

        let catch_types: Vec<String> = catches.iter().map(|c| self.class_fqcn(&c.ty)).collect();
        self.catch_stack.push(catch_types);
        let body_result = self.check_block(body, assigned.clone());
        self.catch_stack.pop();
        body_result?;
        for catch in catches {
            self.push_scope();
            let ty = self.resolve_ty(&Type::Named(catch.ty.clone()));
            let id = self.declare(&catch.var, ty);
            let mut catch_assigned = assigned.clone();
            catch_assigned.insert(id);
            self.check_stmts(&catch.body, catch_assigned)?;
            self.pop_scope();
        }

        // A `try` statement's own definite-assignment contribution is
        // deliberately conservative: since an exception may occur at any
        // point inside `body`, nothing it or a `catch` block assigns is
        // guaranteed afterward except what `finally` (which always runs)
        // itself assigns. See PLAN.md Phase 5 for the documented gap versus
        // compiler.md's full flow-sensitive rule.
        match finally {
            Some(finally_body) => {
                let (finally_assigned, finally_term) = self.check_block(finally_body, assigned)?;
                Ok((finally_assigned, finally_term))
            }
            None => Ok((assigned, false)),
        }
    }

    fn check_assign(
        &mut self,
        target: &LValue,
        value: &Expr,
        assigned: &mut HashSet<u32>,
    ) -> Result<Type, SemaError> {
        match target {
            LValue::Local(name) => {
                let value_ty = self.check_expr(value, assigned)?;
                let Some((id, declared_ty)) = self.resolve(name) else {
                    return Ok(value_ty);
                };
                // compiler.md § Const parameters/local variables — E012. The
                // *initial* assignment of a `const T x = expr;` local goes
                // through `Stmt::VarDecl` directly, never through here — any
                // `Expr::Assign` reaching this arm for a const-tracked id is
                // therefore necessarily a reassignment.
                if self.readonly_loop_vars.contains(&id) {
                    return Err(SemaError::ConstLoopVariableModification(name.clone()));
                }
                if self.const_vars.contains(&id) {
                    return Err(SemaError::ConstModification(name.clone()));
                }
                self.check_assignable(&value_ty, &declared_ty)?;
                assigned.insert(id);
                // compiler.md § Type narrowing, invalidation rules: "An
                // assignment to the variable inside the narrowed region
                // resets its type to the declared type (then re-narrows
                // from subsequent tests)".
                self.narrowed.remove(&id);
                Ok(declared_ty)
            }
            LValue::Field(target_expr, name) => {
                // compiler.md § Const methods — E010: `this.property = ...`
                // is rejected unconditionally inside a `const` method, even
                // before the target/field itself resolves to anything.
                if self.is_const_method && matches!(**target_expr, Expr::This) {
                    return Err(SemaError::ConstMethodPropertyModification(name.clone()));
                }
                let target_ty = self.check_expr(target_expr, assigned)?;
                let value_ty = self.check_expr(value, assigned)?;
                let Type::Named(fqcn) = &target_ty else {
                    return Ok(value_ty);
                };
                let Some(field_ty) = self.field_ty(fqcn, name) else {
                    return Ok(value_ty);
                };
                self.check_field_access(fqcn, name)?;
                self.check_readonly(target_expr, fqcn, name)?;
                self.check_assignable(&value_ty, &field_ty)?;
                Ok(field_ty)
            }
            LValue::Index(target_expr, index_expr) => {
                let target_ty = self.check_expr(target_expr, assigned)?;
                self.check_expr(index_expr, assigned)?;
                let value_ty = self.check_expr(value, assigned)?;
                let Type::Array(elem) = target_ty else {
                    return Ok(value_ty);
                };
                self.check_assignable(&value_ty, &elem)?;
                Ok(*elem)
            }
        }
    }

    fn check_binary(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        assigned: &mut HashSet<u32>,
    ) -> Result<Type, SemaError> {
        if matches!(op, BinOp::And | BinOp::Or) {
            let lty = self.check_expr(lhs, assigned)?;
            if !matches!(lty, Type::Bool) {
                return Err(SemaError::BadUnaryOperator(
                    op_symbol(op),
                    types::display(&lty),
                ));
            }
            // compiler.md § Type narrowing: "`&&` chains: the narrowing
            // from the left operand applies within the right operand" —
            // and dually, `||`'s right operand sees the *negation* of the
            // left operand's narrowing (`x == null || x.length() == 0`).
            let (lt_then, lt_else) = self.narrowing_from_cond(lhs);
            let narrow_for_rhs = if op == BinOp::And { &lt_then } else { &lt_else };
            let rty = self.narrow_and_check_expr(rhs, assigned, narrow_for_rhs)?;
            if !matches!(rty, Type::Bool) {
                return Err(SemaError::BadUnaryOperator(
                    op_symbol(op),
                    types::display(&rty),
                ));
            }
            return Ok(Type::Bool);
        }

        let lty = self.check_expr(lhs, assigned)?;
        let rty = self.check_expr(rhs, assigned)?;

        // String concatenation: '+' where either static type is `string`.
        if op == BinOp::Add && (matches!(lty, Type::StringT) || matches!(rty, Type::StringT)) {
            if !matches!(lty, Type::StringT) && !is_concat_operand(&lty) {
                return Err(SemaError::BadConcatenation(types::display(&lty)));
            }
            if !matches!(rty, Type::StringT) && !is_concat_operand(&rty) {
                return Err(SemaError::BadConcatenation(types::display(&rty)));
            }
            return Ok(Type::StringT);
        }

        self.check_numeric_or_eq(op, &lty, &rty)
    }

    fn check_numeric_or_eq(&self, op: BinOp, lty: &Type, rty: &Type) -> Result<Type, SemaError> {
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                types::widen_numeric(lty, rty)
                    // vm.md § Integer arithmetic: there is no byte-typed
                    // arithmetic opcode — `byte` operands are always widened to
                    // `int` before IADD/ISUB/etc, even when both sides are
                    // `byte` (widen_numeric's identical-type passthrough would
                    // otherwise keep the result as `byte`, which the VM never
                    // actually produces).
                    .map(|widened| {
                        if matches!(widened, Type::Byte) {
                            Type::Int
                        } else {
                            widened
                        }
                    })
                    .ok_or_else(|| {
                        SemaError::BadBinaryOperator(
                            op_symbol(op),
                            types::display(lty),
                            types::display(rty),
                        )
                    })
            }
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => types::widen_numeric(lty, rty)
                .map(|_| Type::Bool)
                .ok_or_else(|| {
                    SemaError::BadBinaryOperator(
                        op_symbol(op),
                        types::display(lty),
                        types::display(rty),
                    )
                }),
            // specs.md § Comparison operators — `<=>` always yields `int`
            // (-1/0/1), never the widened operand type itself.
            BinOp::Cmp3 => types::widen_numeric(lty, rty)
                .map(|_| Type::Int)
                .ok_or_else(|| {
                    SemaError::BadBinaryOperator(
                        op_symbol(op),
                        types::display(lty),
                        types::display(rty),
                    )
                }),
            BinOp::Eq | BinOp::Ne => {
                if matches!(lty, Type::NullT) || matches!(rty, Type::NullT) {
                    let other = if matches!(lty, Type::NullT) { rty } else { lty };
                    if matches!(other, Type::NullT)
                        || types::is_nullable(other)
                        || matches!(other, Type::Named(_) | Type::Array(_))
                    {
                        return Ok(Type::Bool);
                    }
                    return Err(SemaError::BadBinaryOperator(
                        op_symbol(op),
                        types::display(lty),
                        types::display(rty),
                    ));
                }
                if types::widen_numeric(lty, rty).is_some()
                    || types::is_assignable(lty, rty)
                    || types::is_assignable(rty, lty)
                {
                    return Ok(Type::Bool);
                }
                Err(SemaError::BadBinaryOperator(
                    op_symbol(op),
                    types::display(lty),
                    types::display(rty),
                ))
            }
            BinOp::And | BinOp::Or => unreachable!("handled in check_binary"),
        }
    }
}

/// compiler.md § Named and optional parameter rules — E026: a default
/// value must be a compile-time constant. This codebase has no general
/// constant-folding evaluator (out of scope), so "compile-time constant"
/// is recognized structurally: a literal, or a unary minus directly on a
/// numeric literal (`-1`, `-2.5`) — covers every default value shown in
/// specs.md's own examples.
fn is_const_expr(expr: &Expr) -> bool {
    match expr {
        Expr::IntLit(_)
        | Expr::FloatLit(_)
        | Expr::BoolLit(_)
        | Expr::StringLit(_)
        | Expr::NullLit => true,
        Expr::Unary(UnOp::Neg, inner) => matches!(**inner, Expr::IntLit(_) | Expr::FloatLit(_)),
        _ => false,
    }
}

/// compiler.md § Named and optional parameter rules (E023-E026). Binds
/// call-site `args` (source order) against a callee's parameters —
/// `names[i]` is parameter `i`'s name, `required` is how many leading
/// parameters have no default. Returns one `Option<usize>` per parameter:
/// the index into `args` supplying it, or `None` if its default is used.
/// Positional arguments must precede all named ones (E024); a parameter
/// can't be supplied twice (E025, whether positional+named or named
/// twice); every required parameter must end up bound (E023). An unknown
/// named argument or a positional argument beyond the last parameter has
/// no dedicated E-code — deferred to nl-codegen, like every other
/// unresolved reference in this checker.
fn bind_call_args(
    names: &[String],
    required: usize,
    args: &[Arg],
) -> Result<Vec<Option<usize>>, SemaError> {
    let mut binding: Vec<Option<usize>> = vec![None; names.len()];
    let mut seen_named = false;
    for (i, arg) in args.iter().enumerate() {
        match &arg.name {
            None => {
                if seen_named {
                    return Err(SemaError::PositionalArgAfterNamed);
                }
                if i < binding.len() {
                    binding[i] = Some(i);
                }
            }
            Some(name) => {
                seen_named = true;
                let Some(p_idx) = names.iter().position(|n| n == name) else {
                    continue;
                };
                if binding[p_idx].is_some() {
                    return Err(SemaError::ParamProvidedTwice(name.clone()));
                }
                binding[p_idx] = Some(i);
            }
        }
    }
    for (p_idx, name) in names.iter().enumerate().take(required) {
        if binding[p_idx].is_none() {
            return Err(SemaError::RequiredParamNotProvided(name.clone()));
        }
    }
    Ok(binding)
}

/// The static type of `new T[n1]...[nk]`, given the resolved base type `T`,
/// the total dimension count `k`, and `m` (how many leading dimensions are
/// actually provided) — compiler.md § Multidimensional array creation.
/// When every dimension is provided (`m == k`) this is just `T` wrapped in
/// `k` plain arrays. Otherwise the first omitted level's element type
/// becomes nullable exactly once (not at every level below it — vm.md's
/// `NEW_ARRAY` already defaults unallocated reference-typed elements to
/// `null` on its own), then that nullable type is wrapped in the `m`
/// allocated array layers. `m == 0` means nothing is ever allocated, so the
/// whole expression is itself nullable.
fn build_new_array_type(resolved_elem: &Type, k: usize, m: usize) -> Type {
    fn plain_array(base: &Type, depth: usize) -> Type {
        let mut ty = base.clone();
        for _ in 0..depth {
            ty = Type::Array(Box::new(ty));
        }
        ty
    }
    if m == k {
        return plain_array(resolved_elem, k);
    }
    let unallocated = plain_array(resolved_elem, k - m);
    let nullable = Type::Union(vec![unallocated, Type::NullT]);
    plain_array(&nullable, m)
}

/// Reconstructs `new T[n1][n2]...` (or `[]` for an omitted size) as source
/// text for the E038 error message.
fn new_array_source(elem_ty: &Type, dims: &[Option<Expr>]) -> String {
    let mut s = format!("new {}", types::display(elem_ty));
    for d in dims {
        match d {
            Some(_) => s.push_str("[n]"),
            None => s.push_str("[]"),
        }
    }
    s
}

/// Reconstructs a dotted path (`"system.Out"`) from a chain of
/// `Ident`/`FieldAccess` nodes, or `None` if `expr` isn't such a chain (e.g.
/// it's a call or a literal) — used to recognize a `system.*` stdlib class
/// reference, which never resolves as a value the normal way (see
/// `crate::stdlib`).
fn dotted_path(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(name) => Some(name.clone()),
        Expr::FieldAccess(base, name) => Some(format!("{}.{name}", dotted_path(base)?)),
        _ => None,
    }
}

/// Structural equality for match-arm patterns (E047 duplicate-arm check) —
/// only literals are comparable this phase (no enum constants yet).
fn literal_eq(a: &Expr, b: &Expr) -> bool {
    match (a, b) {
        (Expr::IntLit(x), Expr::IntLit(y)) => x == y,
        (Expr::StringLit(x), Expr::StringLit(y)) => x == y,
        (Expr::BoolLit(x), Expr::BoolLit(y)) => x == y,
        _ => false,
    }
}

fn is_concat_operand(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Int | Type::Float | Type::Bool | Type::Byte | Type::StringT
    )
}

fn visibility_str(v: nl_syntax::ast::Visibility) -> String {
    match v {
        nl_syntax::ast::Visibility::Public => "public".to_string(),
        nl_syntax::ast::Visibility::Protected => "protected".to_string(),
        nl_syntax::ast::Visibility::Private => "private".to_string(),
    }
}

fn op_symbol(op: BinOp) -> String {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Gt => ">",
        BinOp::Le => "<=",
        BinOp::Ge => ">=",
        BinOp::Cmp3 => "<=>",
        BinOp::And => "&&",
        BinOp::Or => "||",
    }
    .to_string()
}
