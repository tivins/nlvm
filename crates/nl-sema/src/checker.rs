//! Per-file semantic checker — name resolution, definite assignment (E001),
//! null safety (E003/E004), `auto` deduction (E005), string concatenation
//! (E008), operator compatibility (E009), and duplicate methods (E041).
//! See nlvm-specs/docs/compiler.md.
//!
//! Scoped to what nl-codegen already compiles: a single class per file,
//! static methods only, calls resolved within the same class. Checks that
//! require features not yet in the AST (interfaces, instance methods,
//! `match`, `try`/`catch`, `instanceof`, casts, templates, ...) land with
//! those features in later phases.

use std::collections::{HashMap, HashSet};

use nl_syntax::ast::{BinOp, ClassDecl, Expr, MethodDecl, SourceFile, Stmt, Type, UnOp};

use crate::error::SemaError;
use crate::types;

/// A method's signature, as seen from call sites within the same class.
type MethodSig = (Vec<Type>, Type);

pub fn check_source_file(file: &SourceFile) -> Result<(), SemaError> {
    check_duplicate_methods(&file.class)?;

    let mut sigs: HashMap<String, MethodSig> = HashMap::new();
    for m in &file.class.methods {
        let param_types: Vec<Type> = m.params.iter().map(|p| p.ty.clone()).collect();
        sigs.insert(m.name.clone(), (param_types, m.return_type.clone()));
    }

    for method in &file.class.methods {
        check_method(method, &sigs)?;
    }
    Ok(())
}

/// compiler.md § Duplicate definitions — E041. Signature = name + parameter
/// types only; return type does not distinguish methods.
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

fn check_method(method: &MethodDecl, sigs: &HashMap<String, MethodSig>) -> Result<(), SemaError> {
    let mut checker = MethodChecker {
        sigs,
        scopes: Vec::new(),
        next_id: 0,
        return_ty: method.return_type.clone(),
    };
    checker.push_scope();
    let mut assigned = HashSet::new();
    for param in &method.params {
        let id = checker.declare(&param.name, param.ty.clone());
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
    scopes: Vec<HashMap<String, VarEntry>>,
    next_id: u32,
    return_ty: Type,
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
                self.check_assignable(&ty, &self.return_ty.clone())?;
                Ok((assigned, true))
            }
            Stmt::Return(None) => Ok((assigned, true)),
            Stmt::Expr(expr) => {
                self.check_expr(expr, &mut assigned)?;
                Ok((assigned, false))
            }
            Stmt::VarDecl { ty, name, init } => {
                let value_ty = match init {
                    Some(e) => Some(self.check_expr(e, &mut assigned)?),
                    None => None,
                };
                let declared_ty = match (ty, &value_ty) {
                    (Some(t), _) => t.clone(),
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
        if matches!(value_ty, Type::NullT) && !types::is_nullable(target_ty) {
            return Err(SemaError::NullToNonNullable(types::display(target_ty)));
        }
        if !types::is_assignable(value_ty, target_ty) {
            return Err(SemaError::NotAssignable(types::display(value_ty), types::display(target_ty)));
        }
        Ok(())
    }

    fn check_expr(&mut self, expr: &Expr, assigned: &mut HashSet<u32>) -> Result<Type, SemaError> {
        match expr {
            Expr::IntLit(_) => Ok(Type::Int),
            Expr::FloatLit(_) => Ok(Type::Float),
            Expr::BoolLit(_) => Ok(Type::Bool),
            Expr::StringLit(_) => Ok(Type::StringT),
            Expr::NullLit => Ok(Type::NullT),
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
            Expr::Assign(name, value) => {
                let value_ty = self.check_expr(value, assigned)?;
                let Some((id, declared_ty)) = self.resolve(name) else {
                    return Ok(value_ty);
                };
                self.check_assignable(&value_ty, &declared_ty)?;
                assigned.insert(id);
                Ok(declared_ty)
            }
            Expr::Call(name, args) => {
                let mut arg_types = Vec::with_capacity(args.len());
                for a in args {
                    arg_types.push(self.check_expr(a, assigned)?);
                }
                // Unresolved calls: no dedicated E-code, deferred to nl-codegen.
                let Some((param_types, return_ty)) = self.sigs.get(name) else {
                    return Ok(Type::Void);
                };
                if arg_types.len() == param_types.len() {
                    for (actual, expected) in arg_types.iter().zip(param_types) {
                        self.check_assignable(actual, expected)?;
                    }
                }
                Ok(return_ty.clone())
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
                    if matches!(other, Type::NullT) || types::is_nullable(other) {
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
