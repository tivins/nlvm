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
    BinOp, Block, CatchClause, ClassDecl, Expr, LValue, MatchArm, MethodDecl, MethodKind, SourceFile, SourceItem,
    Stmt, Type, UnOp,
};

use crate::class_table::{self, ClassTable};
use crate::error::SemaError;
use crate::types;

/// A method's signature, as seen from call sites within the same class
/// (bare, unqualified calls — always static, as before this phase).
type MethodSig = (Vec<Type>, Type, Vec<Type>);

pub fn check_source_file(file: &SourceFile, all_files: &[SourceFile], classes: &ClassTable) -> Result<(), SemaError> {
    let SourceItem::Class(class) = &file.item else {
        // Interfaces declare signatures only — nothing to flow-check yet.
        return Ok(());
    };

    check_duplicate_methods(class)?;
    check_constructor_delegation(class)?;

    let imports = class_table::import_map(file, all_files);
    let this_fqcn = class_table::fqcn_of(file);

    let mut sigs: HashMap<String, MethodSig> = HashMap::new();
    for m in &class.methods {
        if m.is_static && m.kind == MethodKind::Normal {
            let param_types: Vec<Type> = m.params.iter().map(|p| class_table::resolve_type(&p.ty, &imports)).collect();
            let return_ty = class_table::resolve_type(&m.return_type, &imports);
            let throws = resolve_throws(m, &imports);
            sigs.insert(m.name.clone(), (param_types, return_ty, throws));
        }
    }

    for method in &class.methods {
        check_method(method, &sigs, classes, &imports, &this_fqcn)?;
        // compiler.md § Exception inheritance rules — E016/E017. Only
        // meaningful for instance methods (static methods hide, not
        // override) that actually override an ancestor's method.
        if !method.is_static && method.kind == MethodKind::Normal {
            check_exception_override(method, classes, &imports, &this_fqcn)?;
        }
    }
    Ok(())
}

fn resolve_throws(m: &MethodDecl, imports: &HashMap<String, String>) -> Vec<Type> {
    m.throws.iter().map(|n| Type::Named(imports.get(n).cloned().unwrap_or_else(|| n.clone()))).collect()
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
    let params: Vec<Type> = method.params.iter().map(|p| class_table::resolve_type(&p.ty, imports)).collect();
    let Some(parent) = class_table::find_method_exact(classes, &super_fqcn, &method.name, &params) else {
        return Ok(());
    };
    if parent.is_static {
        return Ok(());
    }
    let child_throws = resolve_throws(method, imports);
    let is_checked = |t: &Type| {
        let Type::Named(fqcn) = t else { return false };
        class_table::is_subclass_or_same(classes, fqcn, "Exception") && !class_table::is_subclass_or_same(classes, fqcn, "RuntimeException")
    };
    // "child throws type C covers parent throws type P" iff C is P or a
    // subclass of P — the single relation both E016 and E017 check, just
    // iterated in opposite directions.
    let covers = |c: &str, p: &str| class_table::is_subclass_or_same(classes, c, p);

    // E016: every checked exception the parent declares must be covered by
    // some type the child declares.
    for parent_exc in parent.throws.iter().filter(|t| is_checked(t)) {
        let Type::Named(parent_fqcn) = parent_exc else { continue };
        let handled = child_throws.iter().any(|c| matches!(c, Type::Named(child_fqcn) if covers(child_fqcn, parent_fqcn)));
        if !handled {
            return Err(SemaError::MissingThrowsInOverride(method.name.clone(), parent_fqcn.clone()));
        }
    }
    // E017: every checked exception the child declares must itself be
    // covered by something the parent already declares.
    for child_exc in child_throws.iter().filter(|t| is_checked(t)) {
        let Type::Named(child_fqcn) = child_exc else { continue };
        let handled = parent.throws.iter().any(|p| matches!(p, Type::Named(parent_fqcn) if covers(child_fqcn, parent_fqcn)));
        if !handled {
            return Err(SemaError::ExtraThrowsInOverride(method.name.clone(), child_fqcn.clone()));
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
                return Err(SemaError::DuplicateMethod(a.name.clone(), class.name.clone()));
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
            if matches!(stmt, Stmt::ThisCall(_) | Stmt::SuperCall(_)) && i != 0 {
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
            let Some(Stmt::ThisCall(args)) = ctors[current].body.first() else {
                break;
            };
            let argc = args.len();
            let Some(next) = ctors.iter().position(|c| c.params.len() == argc) else {
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
    this_fqcn: &str,
) -> Result<(), SemaError> {
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
        this_ty,
        super_ty,
        scopes: Vec::new(),
        next_id: 0,
        return_ty: class_table::resolve_type(&method.return_type, imports),
        skip_return_check: false,
        method_throws: resolve_throws(method, imports)
            .into_iter()
            .filter_map(|t| if let Type::Named(fqcn) = t { Some(fqcn) } else { None })
            .collect(),
        catch_stack: Vec::new(),
    };
    checker.push_scope();
    let mut assigned = HashSet::new();
    for param in &method.params {
        let id = checker.declare(&param.name, class_table::resolve_type(&param.ty, imports));
        assigned.insert(id);
    }
    checker.check_stmts(&method.body, assigned)?;
    checker.pop_scope();
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
    /// `Some(Type::Named(fqcn))` inside an instance method/constructor,
    /// `None` in a static context (where `this` isn't valid — not yet
    /// enforced as a hard error, E040 lands with static-context checks).
    this_ty: Option<Type>,
    /// `Some(Type::Named(parent_fqcn))` inside an instance method/constructor
    /// of a class that `extends` another; used for `super.field`/
    /// `super.method(...)` expressions.
    super_ty: Option<Type>,
    scopes: Vec<HashMap<String, VarEntry>>,
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

    fn class_fqcn(&self, name: &str) -> String {
        self.imports.get(name).cloned().unwrap_or_else(|| name.to_string())
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
        let covers = |declared: &str| class_table::is_subclass_or_same(self.classes, exc_fqcn, declared);
        if self.catch_stack.iter().rev().any(|catches| catches.iter().any(|c| covers(c))) {
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
            if let Some(m) = info.methods.iter().find(|m| m.name == name && m.params.len() == argc) {
                return Some(m.return_ty.clone());
            }
            current = info.extends.as_deref()?;
        }
    }

    fn method_throws(&self, fqcn: &str, name: &str, argc: usize) -> Vec<Type> {
        let mut current = fqcn;
        loop {
            let Some(info) = self.classes.get(current) else { return Vec::new() };
            if let Some(m) = info.methods.iter().find(|m| m.name == name && m.params.len() == argc) {
                return m.throws.clone();
            }
            let Some(parent) = info.extends.as_deref() else { return Vec::new() };
            current = parent;
        }
    }

    /// Checks a block in its own scope. Returns the set of variables
    /// definitely assigned after it, and whether it unconditionally
    /// terminates the enclosing control-flow path (compiler.md § Definite
    /// assignment analysis, "Terminal statements").
    fn check_block(&mut self, block: &[Stmt], assigned: HashSet<u32>) -> Result<(HashSet<u32>, bool), SemaError> {
        self.push_scope();
        let result = self.check_stmts(block, assigned);
        self.pop_scope();
        result
    }

    fn check_stmts(&mut self, stmts: &[Stmt], mut assigned: HashSet<u32>) -> Result<(HashSet<u32>, bool), SemaError> {
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

    fn check_stmt(&mut self, stmt: &Stmt, mut assigned: HashSet<u32>) -> Result<(HashSet<u32>, bool), SemaError> {
        match stmt {
            Stmt::Return(Some(expr)) => {
                let ty = self.check_expr(expr, &mut assigned)?;
                if !self.skip_return_check {
                    self.check_assignable(&ty, &self.return_ty.clone())?;
                }
                Ok((assigned, true))
            }
            Stmt::Return(None) => Ok((assigned, true)),
            Stmt::Expr(expr) => {
                self.check_expr(expr, &mut assigned)?;
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
                        dotted_path(target).as_deref() == Some("system.ps.Process") && self.resolve("system").is_none()
                    }
                    _ => false,
                };
                Ok((assigned, terminates))
            }
            Stmt::ThisCall(args) | Stmt::SuperCall(args) => {
                for a in args {
                    self.check_expr(a, &mut assigned)?;
                }
                Ok((assigned, false))
            }
            Stmt::Throw(expr) => {
                let ty = self.check_expr(expr, &mut assigned)?;
                if let Type::Named(fqcn) = &ty {
                    self.require_handled(fqcn)?;
                }
                Ok((assigned, true))
            }
            Stmt::Try { body, catches, finally } => self.check_try(body, catches, finally, assigned),
            Stmt::VarDecl { ty, name, init } => {
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
                if value_ty.is_some() {
                    assigned.insert(id);
                }
                Ok((assigned, false))
            }
            Stmt::If { cond, then_branch, else_branch } => {
                self.check_expr(cond, &mut assigned)?;
                let (then_assigned, then_term) = self.check_block(then_branch, assigned.clone())?;
                let (else_assigned, else_term) = match else_branch {
                    Some(b) => self.check_block(b, assigned.clone())?,
                    None => (assigned.clone(), false),
                };
                Ok(match (then_term, else_term) {
                    (true, true) => (then_assigned.union(&else_assigned).cloned().collect(), true),
                    (true, false) => (else_assigned, false),
                    (false, true) => (then_assigned, false),
                    (false, false) => (then_assigned.intersection(&else_assigned).cloned().collect(), false),
                })
            }
            Stmt::While { cond, body } => {
                self.check_expr(cond, &mut assigned)?;
                // The body may execute zero times: its assignments don't
                // make anything definitely assigned after the loop.
                self.check_block(body, assigned.clone())?;
                Ok((assigned, false))
            }
            Stmt::ForEach { ty, var, iterable, body } => {
                let iterable_ty = self.check_expr(iterable, &mut assigned)?;
                // Element type: `T` for a `T[]`, `T` for `system.List<T>`,
                // `MapEntry<K, V>` for `system.Map<K, V>` (iteration
                // desugars through `entries()` — vm.md § For-each loops).
                // Anything else is left lenient (`Void`) — nl-codegen
                // produces the precise "not iterable" error, same division
                // of labor as unknown classes/methods.
                let elem_ty = match &iterable_ty {
                    Type::Array(elem) => (**elem).clone(),
                    Type::Named(fqcn) => crate::native_generics::foreach_element_ty(fqcn).unwrap_or(Type::Void),
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
                let mut body_assigned = assigned.clone();
                body_assigned.insert(id);
                self.check_stmts(body, body_assigned)?;
                self.pop_scope();
                // Zero iterations possible — same rule as `while`.
                Ok((assigned, false))
            }
            Stmt::For { init, cond, step, body } => {
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
            Stmt::Break | Stmt::Continue => Ok((assigned, true)),
            Stmt::Block(block) => self.check_block(block, assigned),
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
            return Err(SemaError::NotAssignable(types::display(value_ty), types::display(target_ty)));
        }
        Ok(())
    }

    /// `types::is_assignable` only knows structural/primitive rules; it has
    /// no notion of interfaces. A class value is also assignable to any
    /// interface type it directly `implements` (compiler.md's subtyping for
    /// reference types) — checked separately here since it needs
    /// `self.classes`. No transitivity through interface-`extends` or class
    /// inheritance (out of scope this phase).
    fn is_object_assignable(&self, value_ty: &Type, target_ty: &Type) -> bool {
        let (Type::Named(from), Type::Named(to)) = (value_ty, target_ty) else {
            return false;
        };
        class_table::is_subclass_or_same(self.classes, from, to)
            || self.classes.get(from).is_some_and(|info| info.implements.iter().any(|i| i == to))
    }

    fn check_expr(&mut self, expr: &Expr, assigned: &mut HashSet<u32>) -> Result<Type, SemaError> {
        match expr {
            Expr::IntLit(_) => Ok(Type::Int),
            Expr::FloatLit(_) => Ok(Type::Float),
            Expr::BoolLit(_) => Ok(Type::Bool),
            Expr::StringLit(_) => Ok(Type::StringT),
            Expr::NullLit => Ok(Type::NullT),
            // Unresolved (`this` outside an instance method): no dedicated
            // E-code yet, deferred to nl-codegen (E040 lands with static
            // context checks).
            Expr::This => Ok(self.this_ty.clone().unwrap_or(Type::Void)),
            // Unresolved (`super` in a class with no `extends`): deferred to
            // nl-codegen, same leniency as `this` outside an instance method.
            Expr::Super => Ok(self.super_ty.clone().unwrap_or(Type::Void)),
            Expr::Ident(name) => {
                // Unresolved names have no dedicated E-code in compiler.md;
                // nl-codegen already rejects them, so just defer to it here.
                let Some((id, ty)) = self.resolve(name) else {
                    return Ok(Type::Void);
                };
                if !assigned.contains(&id) {
                    return Err(SemaError::NotDefinitelyAssigned(name.clone()));
                }
                Ok(ty)
            }
            Expr::Assign(target, value) => self.check_assign(target, value, assigned),
            Expr::Call(name, args) => {
                let mut arg_types = Vec::with_capacity(args.len());
                for a in args {
                    arg_types.push(self.check_expr(a, assigned)?);
                }
                // Unresolved calls: no dedicated E-code, deferred to nl-codegen.
                let Some((param_types, return_ty, throws)) = self.sigs.get(name).cloned() else {
                    return Ok(Type::Void);
                };
                if arg_types.len() == param_types.len() {
                    for (actual, expected) in arg_types.iter().zip(&param_types) {
                        self.check_assignable(actual, expected)?;
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
                    self.check_expr(a, assigned)?;
                }
                let fqcn = self.class_fqcn(class_name);
                if let Some(ctor) = class_table::find_ctor(self.classes, &fqcn, args.len()) {
                    for t in ctor.throws.clone() {
                        if let Type::Named(exc_fqcn) = t {
                            self.require_handled(&exc_fqcn)?;
                        }
                    }
                }
                Ok(Type::Named(fqcn))
            }
            Expr::NewArray(elem_ty, size) => {
                let size_ty = self.check_expr(size, assigned)?;
                if !types::is_numeric(&size_ty) {
                    // No dedicated E-code for a non-int array size yet;
                    // nl-codegen rejects it precisely.
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
                Ok(self.field_ty(fqcn, name).unwrap_or(Type::Void))
            }
            Expr::MethodCall(target, name, args) => {
                let target_ty = self.check_expr(target, assigned)?;
                let mut arg_types = Vec::with_capacity(args.len());
                for a in args {
                    arg_types.push(self.check_expr(a, assigned)?);
                }
                // `system.Out.print(...)` and friends: the receiver is a
                // dotted namespace/class path, not a value, so it never
                // resolves through `self.resolve`/`self.classes` above —
                // recognized here by name instead (see crate::stdlib).
                if let Some(path) = dotted_path(target) {
                    let leading = path.split('.').next().expect("dotted_path is never empty");
                    if self.resolve(leading).is_none() {
                        if let Some((param_types, return_ty)) = crate::stdlib::lookup(&path, name, args.len()) {
                            for (actual, expected) in arg_types.iter().zip(&param_types) {
                                self.check_assignable(actual, expected)?;
                            }
                            for exc in crate::stdlib::throws(&path, name) {
                                self.require_handled(exc)?;
                            }
                            return Ok(return_ty);
                        }
                    }
                }
                match &target_ty {
                    Type::Array(_) if name == "length" && args.is_empty() => Ok(Type::Int),
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
                        for t in self.method_throws(fqcn, name, args.len()) {
                            if let Type::Named(exc_fqcn) = t {
                                self.require_handled(&exc_fqcn)?;
                            }
                        }
                        Ok(self.method_return_ty(fqcn, name, args.len()).unwrap_or(Type::Void))
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
            Expr::PostIncr(name) | Expr::PostDecr(name) => {
                let Some((id, ty)) = self.resolve(name) else {
                    return Ok(Type::Int);
                };
                if !assigned.contains(&id) {
                    return Err(SemaError::NotDefinitelyAssigned(name.clone()));
                }
                Ok(ty)
            }
            Expr::Unary(op, inner) => {
                let ty = self.check_expr(inner, assigned)?;
                match op {
                    UnOp::Neg if types::is_numeric(&ty) => Ok(ty),
                    UnOp::Neg => Err(SemaError::BadUnaryOperator("-".to_string(), types::display(&ty))),
                    UnOp::Not if matches!(ty, Type::Bool) => Ok(Type::Bool),
                    UnOp::Not => Err(SemaError::BadUnaryOperator("!".to_string(), types::display(&ty))),
                }
            }
            Expr::Binary(op, lhs, rhs) => self.check_binary(*op, lhs, rhs, assigned),
            Expr::Match(subject, arms) => self.check_match(subject, arms, assigned),
            Expr::Ternary(cond, then_e, else_e) => {
                let cond_ty = self.check_expr(cond, assigned)?;
                if !matches!(cond_ty, Type::Bool) {
                    return Err(SemaError::BadUnaryOperator("?:".to_string(), types::display(&cond_ty)));
                }
                let then_ty = self.check_expr(then_e, assigned)?;
                // Lenient about mismatched branch types, same as `match`
                // arms above — nl-codegen enforces coercibility at emission
                // time, where it also has `ExprTy` to work with.
                self.check_expr(else_e, assigned)?;
                Ok(then_ty)
            }
            // vm.md § Closures — checked like a nested block with its own
            // additional param declarations, so definite assignment on a
            // *captured* variable still applies (it must be assigned by the
            // time the closure literal is created — capture is by value,
            // see nl-codegen's `ExprTy::Closure`). No dedicated static type
            // to report (no `Type::Function` this phase — see PLAN.md's
            // closures gap), so `Type::Void`, same leniency `check_assignable`
            // already gives every other not-yet-modeled expression form.
            Expr::Closure { params, body, return_type, .. } => {
                self.push_scope();
                let mut inner_assigned = assigned.clone();
                for p in params {
                    let ty = self.resolve_ty(&p.ty);
                    let id = self.declare(&p.name, ty);
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
                    nl_syntax::ast::ClosureBody::Block(block) => self.check_stmts(block, inner_assigned).map(|_| ()),
                    nl_syntax::ast::ClosureBody::Expr(e) => self.check_expr(e, &mut inner_assigned).map(|_| ()),
                };
                self.return_ty = saved_return_ty;
                self.skip_return_check = saved_skip;
                self.pop_scope();
                body_result?;
                Ok(Type::Void)
            }
        }
    }

    /// compiler.md § Match exhaustiveness — E047. No enums yet, so the only
    /// type that can be exhaustive without a `default` arm is `bool` (both
    /// `true` and `false` present); everything else requires `default`.
    /// Two arms with the same constant literal are also E047 (the second
    /// would be unreachable).
    fn check_match(&mut self, subject: &Expr, arms: &[MatchArm], assigned: &mut HashSet<u32>) -> Result<Type, SemaError> {
        let subject_ty = self.check_expr(subject, assigned)?;
        let mut seen: Vec<&Expr> = Vec::new();
        let mut has_default = false;
        let mut has_true = false;
        let mut has_false = false;
        let mut result_ty: Option<Type> = None;
        for arm in arms {
            match &arm.pattern {
                None => has_default = true,
                Some(pat) => {
                    if seen.iter().any(|s| literal_eq(s, pat)) {
                        return Err(SemaError::MatchNotExhaustive("unreachable duplicate arm".to_string()));
                    }
                    seen.push(pat);
                    match pat {
                        Expr::BoolLit(true) => has_true = true,
                        Expr::BoolLit(false) => has_false = true,
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
        let exhaustive = has_default || (matches!(subject_ty, Type::Bool) && has_true && has_false);
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
                    return Err(SemaError::UnreachableCatch(catches[j].ty.clone(), earlier.ty.clone()));
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

    fn check_assign(&mut self, target: &LValue, value: &Expr, assigned: &mut HashSet<u32>) -> Result<Type, SemaError> {
        match target {
            LValue::Local(name) => {
                let value_ty = self.check_expr(value, assigned)?;
                let Some((id, declared_ty)) = self.resolve(name) else {
                    return Ok(value_ty);
                };
                self.check_assignable(&value_ty, &declared_ty)?;
                assigned.insert(id);
                Ok(declared_ty)
            }
            LValue::Field(target_expr, name) => {
                let target_ty = self.check_expr(target_expr, assigned)?;
                let value_ty = self.check_expr(value, assigned)?;
                let Type::Named(fqcn) = &target_ty else {
                    return Ok(value_ty);
                };
                let Some(field_ty) = self.field_ty(fqcn, name) else {
                    return Ok(value_ty);
                };
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

    fn check_binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, assigned: &mut HashSet<u32>) -> Result<Type, SemaError> {
        if matches!(op, BinOp::And | BinOp::Or) {
            let lty = self.check_expr(lhs, assigned)?;
            if !matches!(lty, Type::Bool) {
                return Err(SemaError::BadUnaryOperator(op_symbol(op), types::display(&lty)));
            }
            let rty = self.check_expr(rhs, assigned)?;
            if !matches!(rty, Type::Bool) {
                return Err(SemaError::BadUnaryOperator(op_symbol(op), types::display(&rty)));
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
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => types::widen_numeric(lty, rty)
                .ok_or_else(|| SemaError::BadBinaryOperator(op_symbol(op), types::display(lty), types::display(rty))),
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => types::widen_numeric(lty, rty)
                .map(|_| Type::Bool)
                .ok_or_else(|| SemaError::BadBinaryOperator(op_symbol(op), types::display(lty), types::display(rty))),
            BinOp::Eq | BinOp::Ne => {
                if matches!(lty, Type::NullT) || matches!(rty, Type::NullT) {
                    let other = if matches!(lty, Type::NullT) { rty } else { lty };
                    if matches!(other, Type::NullT) || types::is_nullable(other) || matches!(other, Type::Named(_) | Type::Array(_)) {
                        return Ok(Type::Bool);
                    }
                    return Err(SemaError::BadBinaryOperator(op_symbol(op), types::display(lty), types::display(rty)));
                }
                if types::widen_numeric(lty, rty).is_some()
                    || types::is_assignable(lty, rty)
                    || types::is_assignable(rty, lty)
                {
                    return Ok(Type::Bool);
                }
                Err(SemaError::BadBinaryOperator(op_symbol(op), types::display(lty), types::display(rty)))
            }
            BinOp::And | BinOp::Or => unreachable!("handled in check_binary"),
        }
    }
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
    matches!(ty, Type::Int | Type::Float | Type::Bool | Type::Byte | Type::StringT)
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
        BinOp::And => "&&",
        BinOp::Or => "||",
    }
    .to_string()
}
