//! Free-variable collection for closure literals — vm.md § Closures and
//! anonymous functions. `referenced_names` walks a closure body's AST and
//! returns every bare name it references (`Expr::Ident`, assignment
//! targets, `++`/`--` operands); `Emitter::compile_closure` then checks
//! each candidate against the *enclosing* method's locals to decide which
//! ones are actual captures (a name that isn't an outer local is something
//! else entirely — a class reference, or a name declared inside the
//! closure body itself — and is simply left alone).

use std::collections::HashSet;

use nl_syntax::ast::{Block, ClosureBody, Expr, LValue, Stmt, StmtKind};

pub(crate) fn referenced_names(body: &ClosureBody) -> HashSet<String> {
    let mut names = HashSet::new();
    match body {
        ClosureBody::Block(block) => collect_block(block, &mut names),
        ClosureBody::Expr(e) => collect_expr(e, &mut names),
    }
    names
}

fn collect_block(block: &Block, names: &mut HashSet<String>) {
    for stmt in block {
        collect_stmt(stmt, names);
    }
}

fn collect_stmt(stmt: &Stmt, names: &mut HashSet<String>) {
    match &stmt.kind {
        StmtKind::Return(Some(e)) | StmtKind::Throw(e) => collect_expr(e, names),
        StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue => {}
        StmtKind::Expr(e) => collect_expr(e, names),
        StmtKind::VarDecl { init, .. } => {
            if let Some(e) = init {
                collect_expr(e, names);
            }
        }
        StmtKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_expr(cond, names);
            collect_block(then_branch, names);
            if let Some(b) = else_branch {
                collect_block(b, names);
            }
        }
        StmtKind::While { cond, body } => {
            collect_expr(cond, names);
            collect_block(body, names);
        }
        StmtKind::ForEach { iterable, body, .. } => {
            collect_expr(iterable, names);
            collect_block(body, names);
        }
        StmtKind::For {
            init,
            cond,
            step,
            body,
        } => {
            for s in init {
                collect_stmt(s, names);
            }
            if let Some(c) = cond {
                collect_expr(c, names);
            }
            for e in step {
                collect_expr(e, names);
            }
            collect_block(body, names);
        }
        StmtKind::Block(b) => collect_block(b, names),
        StmtKind::ThisCall(args) | StmtKind::SuperCall(args) => {
            for a in args {
                collect_expr(&a.value, names);
            }
        }
        StmtKind::Try {
            body,
            catches,
            finally,
        } => {
            collect_block(body, names);
            for c in catches {
                collect_block(&c.body, names);
            }
            if let Some(f) = finally {
                collect_block(f, names);
            }
        }
        StmtKind::Switch { subject, cases } => {
            collect_expr(subject, names);
            for case in cases {
                if let Some(v) = &case.value {
                    collect_expr(v, names);
                }
                collect_block(&case.body, names);
            }
        }
    }
}

fn collect_expr(expr: &Expr, names: &mut HashSet<String>) {
    match expr {
        Expr::IntLit(_)
        | Expr::FloatLit(_)
        | Expr::BoolLit(_)
        | Expr::StringLit(_)
        | Expr::NullLit
        | Expr::This
        | Expr::Super => {}
        Expr::Ident(name) | Expr::PostIncr(name) | Expr::PostDecr(name) => {
            names.insert(name.clone());
        }
        Expr::Assign(target, value) => {
            collect_lvalue(target, names);
            collect_expr(value, names);
        }
        Expr::Call(_, args) | Expr::New(_, _, args) => {
            for a in args {
                collect_expr(&a.value, names);
            }
        }
        Expr::NewArray(_, dims) => {
            for size in dims.iter().flatten() {
                collect_expr(size, names);
            }
        }
        Expr::NewArrayInit(_, elements) => {
            for e in elements {
                collect_expr(e, names);
            }
        }
        Expr::FieldAccess(target, _) | Expr::InstanceOf(target, _) => collect_expr(target, names),
        Expr::Cast(_, inner) => collect_expr(inner, names),
        Expr::MethodCall(target, _, args) => {
            collect_expr(target, names);
            for a in args {
                collect_expr(&a.value, names);
            }
        }
        Expr::Index(target, index) => {
            collect_expr(target, names);
            collect_expr(index, names);
        }
        Expr::Unary(_, inner) => collect_expr(inner, names),
        Expr::Binary(_, lhs, rhs) => {
            collect_expr(lhs, names);
            collect_expr(rhs, names);
        }
        Expr::Match(subject, arms) => {
            collect_expr(subject, names);
            for arm in arms {
                if let Some(p) = &arm.pattern {
                    collect_expr(p, names);
                }
                collect_expr(&arm.value, names);
            }
        }
        Expr::Ternary(cond, then_e, else_e) => {
            collect_expr(cond, names);
            collect_expr(then_e, names);
            collect_expr(else_e, names);
        }
        Expr::Coalesce(lhs, rhs) | Expr::Elvis(lhs, rhs) => {
            collect_expr(lhs, names);
            collect_expr(rhs, names);
        }
        Expr::Closure { params, body, .. } => {
            // A nested closure may itself reference a variable from this
            // (outer) closure's enclosing scope — recurse, but drop its own
            // parameter names first so they aren't mistaken for captures.
            let mut inner = HashSet::new();
            match body {
                ClosureBody::Block(b) => collect_block(b, &mut inner),
                ClosureBody::Expr(e) => collect_expr(e, &mut inner),
            }
            for p in params {
                inner.remove(&p.name);
            }
            names.extend(inner);
        }
    }
}

fn collect_lvalue(lvalue: &LValue, names: &mut HashSet<String>) {
    match lvalue {
        LValue::Local(name) => {
            names.insert(name.clone());
        }
        LValue::Field(target, _) => collect_expr(target, names),
        LValue::Index(target, index) => {
            collect_expr(target, names);
            collect_expr(index, names);
        }
    }
}

/// vm.md § Variable capture and boxing — every name in `block` that both (a)
/// is referenced inside at least one closure literal reachable from `block`
/// and (b) is a mutation target (`=`, `++`, `--`) somewhere in `block`,
/// whether that mutation happens inside the capturing closure or in the
/// surrounding code. These are exactly the captures that need a shared
/// `Box<T>` between the closure and its enclosing scope rather than a
/// snapshot copy (vm.md: "captured **read-only**... may be copied directly
/// ... without boxing").
///
/// Deliberately whole-block rather than scoped to each name's own declaring
/// statement: a same-named local in an unrelated sibling scope may end up
/// boxed unnecessarily, but boxing is always semantically safe (see
/// `crate::expr::LocalSlot::boxed`) — it only ever adds an indirection, so
/// this over-approximation costs nothing but a little precision.
pub(crate) fn boxed_captures_in_block(block: &[Stmt]) -> HashSet<String> {
    let mut captured = HashSet::new();
    let mut mutated = HashSet::new();
    for stmt in block {
        scan_stmt(stmt, &mut captured, &mut mutated);
    }
    captured.intersection(&mutated).cloned().collect()
}

/// Same as `boxed_captures_in_block`, for a closure literal's own body
/// (`ClosureBody` rather than a plain statement list) — used when compiling
/// a closure that itself contains nested closures capturing its locals.
pub(crate) fn boxed_captures(body: &ClosureBody) -> HashSet<String> {
    let mut captured = HashSet::new();
    let mut mutated = HashSet::new();
    match body {
        ClosureBody::Block(block) => {
            for stmt in block {
                scan_stmt(stmt, &mut captured, &mut mutated);
            }
        }
        ClosureBody::Expr(e) => scan_expr(e, &mut captured, &mut mutated),
    }
    captured.intersection(&mutated).cloned().collect()
}

/// Walks `stmt`, harvesting into `captured` (every closure literal's free
/// names, via `referenced_names`) and `mutated` (every assignment/`++`/`--`
/// target, including inside closure bodies) — see `boxed_captures_in_block`.
fn scan_stmt(stmt: &Stmt, captured: &mut HashSet<String>, mutated: &mut HashSet<String>) {
    match &stmt.kind {
        StmtKind::Return(Some(e)) | StmtKind::Throw(e) => scan_expr(e, captured, mutated),
        StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue => {}
        StmtKind::Expr(e) => scan_expr(e, captured, mutated),
        StmtKind::VarDecl { init, .. } => {
            if let Some(e) = init {
                scan_expr(e, captured, mutated);
            }
        }
        StmtKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            scan_expr(cond, captured, mutated);
            for s in then_branch {
                scan_stmt(s, captured, mutated);
            }
            if let Some(b) = else_branch {
                for s in b {
                    scan_stmt(s, captured, mutated);
                }
            }
        }
        StmtKind::While { cond, body } => {
            scan_expr(cond, captured, mutated);
            for s in body {
                scan_stmt(s, captured, mutated);
            }
        }
        StmtKind::ForEach { iterable, body, .. } => {
            scan_expr(iterable, captured, mutated);
            for s in body {
                scan_stmt(s, captured, mutated);
            }
        }
        StmtKind::For {
            init,
            cond,
            step,
            body,
        } => {
            for s in init {
                scan_stmt(s, captured, mutated);
            }
            if let Some(c) = cond {
                scan_expr(c, captured, mutated);
            }
            for e in step {
                scan_expr(e, captured, mutated);
            }
            for s in body {
                scan_stmt(s, captured, mutated);
            }
        }
        StmtKind::Block(b) => {
            for s in b {
                scan_stmt(s, captured, mutated);
            }
        }
        StmtKind::ThisCall(args) | StmtKind::SuperCall(args) => {
            for a in args {
                scan_expr(&a.value, captured, mutated);
            }
        }
        StmtKind::Try {
            body,
            catches,
            finally,
        } => {
            for s in body {
                scan_stmt(s, captured, mutated);
            }
            for c in catches {
                for s in &c.body {
                    scan_stmt(s, captured, mutated);
                }
            }
            if let Some(f) = finally {
                for s in f {
                    scan_stmt(s, captured, mutated);
                }
            }
        }
        StmtKind::Switch { subject, cases } => {
            scan_expr(subject, captured, mutated);
            for case in cases {
                if let Some(v) = &case.value {
                    scan_expr(v, captured, mutated);
                }
                for s in &case.body {
                    scan_stmt(s, captured, mutated);
                }
            }
        }
    }
}

fn scan_expr(expr: &Expr, captured: &mut HashSet<String>, mutated: &mut HashSet<String>) {
    match expr {
        Expr::Closure { params, body, .. } => {
            captured.extend(referenced_names(body));
            // Recurse into the closure's own body for *mutations* too
            // (including of a doubly-nested closure inside it) — its own
            // parameters are excluded, mirroring `referenced_names`'
            // shadow handling: a closure reassigning its own parameter
            // isn't mutating anything captured from outside it.
            let mut inner_mutated = HashSet::new();
            match body {
                ClosureBody::Block(b) => {
                    for s in b {
                        scan_stmt(s, captured, &mut inner_mutated);
                    }
                }
                ClosureBody::Expr(e) => scan_expr(e, captured, &mut inner_mutated),
            }
            let param_names: HashSet<&str> = params.iter().map(|p| p.name.as_str()).collect();
            mutated.extend(
                inner_mutated
                    .into_iter()
                    .filter(|n| !param_names.contains(n.as_str())),
            );
        }
        Expr::PostIncr(name) | Expr::PostDecr(name) => {
            mutated.insert(name.clone());
        }
        Expr::IntLit(_)
        | Expr::FloatLit(_)
        | Expr::BoolLit(_)
        | Expr::StringLit(_)
        | Expr::NullLit
        | Expr::This
        | Expr::Super
        | Expr::Ident(_) => {}
        Expr::Assign(target, value) => {
            scan_lvalue(target, captured, mutated);
            scan_expr(value, captured, mutated);
        }
        Expr::Call(_, args) | Expr::New(_, _, args) => {
            for a in args {
                scan_expr(&a.value, captured, mutated);
            }
        }
        Expr::NewArray(_, dims) => {
            for size in dims.iter().flatten() {
                scan_expr(size, captured, mutated);
            }
        }
        Expr::NewArrayInit(_, elements) => {
            for e in elements {
                scan_expr(e, captured, mutated);
            }
        }
        Expr::FieldAccess(target, _) | Expr::InstanceOf(target, _) => {
            scan_expr(target, captured, mutated)
        }
        Expr::Cast(_, inner) => scan_expr(inner, captured, mutated),
        Expr::MethodCall(target, _, args) => {
            scan_expr(target, captured, mutated);
            for a in args {
                scan_expr(&a.value, captured, mutated);
            }
        }
        Expr::Index(target, index) => {
            scan_expr(target, captured, mutated);
            scan_expr(index, captured, mutated);
        }
        Expr::Unary(_, inner) => scan_expr(inner, captured, mutated),
        Expr::Binary(_, lhs, rhs) => {
            scan_expr(lhs, captured, mutated);
            scan_expr(rhs, captured, mutated);
        }
        Expr::Match(subject, arms) => {
            scan_expr(subject, captured, mutated);
            for arm in arms {
                if let Some(p) = &arm.pattern {
                    scan_expr(p, captured, mutated);
                }
                scan_expr(&arm.value, captured, mutated);
            }
        }
        Expr::Ternary(cond, then_e, else_e) => {
            scan_expr(cond, captured, mutated);
            scan_expr(then_e, captured, mutated);
            scan_expr(else_e, captured, mutated);
        }
        Expr::Coalesce(lhs, rhs) | Expr::Elvis(lhs, rhs) => {
            scan_expr(lhs, captured, mutated);
            scan_expr(rhs, captured, mutated);
        }
    }
}

fn scan_lvalue(lvalue: &LValue, captured: &mut HashSet<String>, mutated: &mut HashSet<String>) {
    match lvalue {
        LValue::Local(name) => {
            mutated.insert(name.clone());
        }
        LValue::Field(target, _) => scan_expr(target, captured, mutated),
        LValue::Index(target, index) => {
            scan_expr(target, captured, mutated);
            scan_expr(index, captured, mutated);
        }
    }
}
