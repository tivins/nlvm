use std::collections::HashMap;

use nl_bytecode::{ConstantPool, Opcode};
use nl_syntax::ast::{BinOp, Expr, Type, UnOp};

use crate::error::CodegenError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExprTy {
    Int,
    Float,
    Bool,
    Byte,
    StringT,
    Null,
    Void,
    /// Array/class types — not yet representable (milestone 5); values of
    /// this type flow through identifiers/calls but reject further use.
    Other,
}

pub fn expr_ty_of(ty: &Type) -> ExprTy {
    match ty {
        Type::Int => ExprTy::Int,
        Type::Float => ExprTy::Float,
        Type::Bool => ExprTy::Bool,
        Type::Byte => ExprTy::Byte,
        Type::StringT => ExprTy::StringT,
        Type::Void => ExprTy::Void,
        Type::NullT => ExprTy::Null,
        Type::Array(_) | Type::Named(_) => ExprTy::Other,
        // Values are dynamically tagged at runtime (vm.md § Value
        // representation), so a union collapses to the `ExprTy` of its first
        // non-null member for codegen purposes — nullability itself is
        // already enforced earlier by nl-sema, not re-checked here.
        Type::Union(members) => members
            .iter()
            .find(|m| !matches!(m, Type::NullT))
            .map(expr_ty_of)
            .unwrap_or(ExprTy::Null),
    }
}

/// Static signature of a method in the class currently being compiled —
/// enough to type-check call sites and resolve them to a constant-pool
/// `MethodRef`, built in a first pass so calls (including recursive/forward
/// calls) can resolve regardless of declaration order.
#[derive(Debug, Clone)]
pub struct MethodSig {
    pub param_types: Vec<ExprTy>,
    pub return_ty: ExprTy,
    pub method_ref_index: u16,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LocalSlot {
    pub index: u16,
    pub ty: ExprTy,
}

pub(crate) struct LoopCtx {
    pub break_patches: Vec<(usize, usize)>,
    pub continue_patches: Vec<(usize, usize)>,
}

pub struct Emitter<'a> {
    pub code: Vec<u8>,
    pub cp: &'a mut ConstantPool,
    pub(crate) methods: &'a HashMap<String, MethodSig>,
    depth: i32,
    max_depth: i32,
    pub(crate) scopes: Vec<HashMap<String, LocalSlot>>,
    next_local: u16,
    max_locals: u16,
    pub(crate) loops: Vec<LoopCtx>,
}

impl<'a> Emitter<'a> {
    pub fn new(cp: &'a mut ConstantPool, methods: &'a HashMap<String, MethodSig>) -> Self {
        Self {
            code: Vec::new(),
            cp,
            methods,
            depth: 0,
            max_depth: 0,
            scopes: Vec::new(),
            next_local: 0,
            max_locals: 0,
            loops: Vec::new(),
        }
    }

    pub fn max_stack(&self) -> u16 {
        self.max_depth.max(0) as u16
    }

    pub fn max_locals(&self) -> u16 {
        self.max_locals
    }

    fn track(&mut self, delta: i32) {
        self.depth += delta;
        if self.depth > self.max_depth {
            self.max_depth = self.depth;
        }
    }

    fn op(&mut self, op: Opcode, stack_delta: i32) {
        self.code.push(op as u8);
        self.track(stack_delta);
    }

    fn op_u16(&mut self, op: Opcode, operand: u16, stack_delta: i32) {
        self.code.push(op as u8);
        self.code.extend_from_slice(&operand.to_be_bytes());
        self.track(stack_delta);
    }

    fn op_i8(&mut self, op: Opcode, operand: i8, stack_delta: i32) {
        self.code.push(op as u8);
        self.code.push(operand as u8);
        self.track(stack_delta);
    }

    fn op_i16(&mut self, op: Opcode, operand: i16, stack_delta: i32) {
        self.code.push(op as u8);
        self.code.extend_from_slice(&operand.to_be_bytes());
        self.track(stack_delta);
    }

    fn op_iinc(&mut self, local_index: u16, delta: i16) {
        self.code.push(Opcode::IInc as u8);
        self.code.extend_from_slice(&local_index.to_be_bytes());
        self.code.extend_from_slice(&delta.to_be_bytes());
    }

    /// Emits a branch opcode with a placeholder offset; pops `stack_delta`
    /// (0 for GOTO, -1 for IF_TRUE/IF_FALSE). Returns (opcode_pc, operand_pos)
    /// for later patching with `patch_branch`/`patch_branch_to`.
    pub(crate) fn branch(&mut self, op: Opcode, stack_delta: i32) -> (usize, usize) {
        let opcode_pc = self.code.len();
        self.code.push(op as u8);
        let operand_pos = self.code.len();
        self.code.extend_from_slice(&0i16.to_be_bytes());
        self.track(stack_delta);
        (opcode_pc, operand_pos)
    }

    fn patch_branch(&mut self, opcode_pc: usize, operand_pos: usize) {
        self.patch_branch_to(opcode_pc, operand_pos, self.code.len());
    }

    pub(crate) fn patch_branch_to(&mut self, opcode_pc: usize, operand_pos: usize, target: usize) {
        let offset = (target as i32 - opcode_pc as i32) as i16;
        self.code[operand_pos..operand_pos + 2].copy_from_slice(&offset.to_be_bytes());
    }

    pub(crate) fn emit_store(&mut self, local_index: u16) {
        self.op_u16(Opcode::Store, local_index, -1);
    }

    pub(crate) fn emit_i2f(&mut self) {
        self.op(Opcode::I2F, 0);
    }

    pub(crate) fn emit_goto_to(&mut self, target: usize) {
        let opcode_pc = self.code.len();
        self.code.push(Opcode::Goto as u8);
        let offset = (target as i32 - opcode_pc as i32) as i16;
        self.code.extend_from_slice(&offset.to_be_bytes());
        self.track(0);
    }

    pub(crate) fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    pub(crate) fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    pub(crate) fn declare_local(&mut self, name: String, ty: ExprTy) -> u16 {
        let index = self.next_local;
        self.next_local += 1;
        if self.next_local > self.max_locals {
            self.max_locals = self.next_local;
        }
        self.scopes
            .last_mut()
            .expect("declare_local outside any scope")
            .insert(name, LocalSlot { index, ty });
        index
    }

    fn lookup_local(&self, name: &str) -> Result<LocalSlot, CodegenError> {
        for scope in self.scopes.iter().rev() {
            if let Some(slot) = scope.get(name) {
                return Ok(*slot);
            }
        }
        Err(CodegenError::Unsupported(format!("undefined variable '{name}'")))
    }

    /// Compiles `expr` as a statement: evaluates it and discards any value
    /// it leaves on the stack. Used for expression statements and for-loop
    /// step expressions.
    pub fn compile_expr_stmt(&mut self, expr: &Expr) -> Result<(), CodegenError> {
        let ty = self.compile_expr(expr)?;
        if ty != ExprTy::Void {
            self.op(Opcode::Pop, -1);
        }
        Ok(())
    }

    pub(crate) fn compile_expr_bool(&mut self, expr: &Expr) -> Result<(), CodegenError> {
        let ty = self.compile_expr(expr)?;
        if ty != ExprTy::Bool {
            return Err(CodegenError::Unsupported(format!("expected bool condition, got {ty:?}")));
        }
        Ok(())
    }

    pub fn compile_expr(&mut self, expr: &Expr) -> Result<ExprTy, CodegenError> {
        match expr {
            Expr::IntLit(v) => {
                self.emit_int_const(*v);
                Ok(ExprTy::Int)
            }
            Expr::FloatLit(v) => {
                self.emit_float_const(*v);
                Ok(ExprTy::Float)
            }
            Expr::BoolLit(v) => {
                self.op(if *v { Opcode::ConstTrue } else { Opcode::ConstFalse }, 1);
                Ok(ExprTy::Bool)
            }
            Expr::StringLit(s) => {
                let idx = self.cp.add_utf8(s.clone());
                self.op_u16(Opcode::Ldc, idx, 1);
                Ok(ExprTy::StringT)
            }
            Expr::NullLit => {
                self.op(Opcode::ConstNull, 1);
                Ok(ExprTy::Null)
            }
            Expr::Ident(name) => {
                let slot = self.lookup_local(name)?;
                self.op_u16(Opcode::Load, slot.index, 1);
                Ok(slot.ty)
            }
            Expr::Assign(name, value) => self.compile_assign(name, value),
            Expr::Call(name, args) => self.compile_call(name, args),
            Expr::PostIncr(name) => self.compile_incr(name, 1),
            Expr::PostDecr(name) => self.compile_incr(name, -1),
            Expr::Unary(op, inner) => self.compile_unary(*op, inner),
            Expr::Binary(op, lhs, rhs) => self.compile_binary(*op, lhs, rhs),
        }
    }

    fn compile_assign(&mut self, name: &str, value: &Expr) -> Result<ExprTy, CodegenError> {
        let slot = self.lookup_local(name)?;
        let value_ty = self.compile_expr(value)?;
        if slot.ty == ExprTy::Float && value_ty == ExprTy::Int {
            self.op(Opcode::I2F, 0);
        } else if value_ty == ExprTy::Null {
            // Nullability was already validated by nl-sema; a `Value::Null`
            // fits any slot regardless of its static (non-null) `ExprTy`.
        } else if slot.ty != value_ty {
            return Err(CodegenError::Unsupported(format!(
                "cannot assign {value_ty:?} to variable '{name}' of type {:?}",
                slot.ty
            )));
        }
        // Leave a copy as the expression's own value (assignment is an
        // expression, e.g. usable as `a = b = 1;`).
        self.op(Opcode::Dup, 1);
        self.op_u16(Opcode::Store, slot.index, -1);
        Ok(slot.ty)
    }

    fn compile_incr(&mut self, name: &str, delta: i16) -> Result<ExprTy, CodegenError> {
        let slot = self.lookup_local(name)?;
        if slot.ty != ExprTy::Int {
            return Err(CodegenError::Unsupported(format!(
                "'++'/'--' only supported on int, found {:?}",
                slot.ty
            )));
        }
        self.op_iinc(slot.index, delta);
        Ok(ExprTy::Void)
    }

    fn compile_call(&mut self, name: &str, args: &[Expr]) -> Result<ExprTy, CodegenError> {
        let sig = self
            .methods
            .get(name)
            .cloned()
            .ok_or_else(|| CodegenError::Unsupported(format!("call to unknown method '{name}'")))?;
        if args.len() != sig.param_types.len() {
            return Err(CodegenError::Unsupported(format!(
                "'{name}' expects {} argument(s), got {}",
                sig.param_types.len(),
                args.len()
            )));
        }
        for (arg, expected_ty) in args.iter().zip(&sig.param_types) {
            let actual = self.compile_expr(arg)?;
            if *expected_ty == ExprTy::Float && actual == ExprTy::Int {
                self.op(Opcode::I2F, 0);
            } else if actual == ExprTy::Null {
                // Nullability was already validated by nl-sema.
            } else if actual != *expected_ty {
                return Err(CodegenError::Unsupported(format!(
                    "argument to '{name}' has type {actual:?}, expected {expected_ty:?}"
                )));
            }
        }
        let result_delta = if sig.return_ty == ExprTy::Void { 0 } else { 1 };
        self.op_u16(
            Opcode::InvokeStatic,
            sig.method_ref_index,
            result_delta - args.len() as i32,
        );
        Ok(sig.return_ty)
    }

    fn emit_int_const(&mut self, v: i64) {
        match v {
            0 => self.op(Opcode::ConstIZero, 1),
            1 => self.op(Opcode::ConstIOne, 1),
            v if i8::try_from(v).is_ok() => self.op_i8(Opcode::BiPush, v as i8, 1),
            v if i16::try_from(v).is_ok() => self.op_i16(Opcode::SiPush, v as i16, 1),
            v => {
                let idx = self.cp.add_int(v);
                self.op_u16(Opcode::Ldc, idx, 1);
            }
        }
    }

    fn emit_float_const(&mut self, v: f64) {
        if v == 0.0 {
            self.op(Opcode::ConstFZero, 1);
        } else if v == 1.0 {
            self.op(Opcode::ConstFOne, 1);
        } else {
            let idx = self.cp.add_float(v);
            self.op_u16(Opcode::Ldc, idx, 1);
        }
    }

    fn compile_unary(&mut self, op: UnOp, inner: &Expr) -> Result<ExprTy, CodegenError> {
        let ty = self.compile_expr(inner)?;
        match op {
            UnOp::Neg => match ty {
                ExprTy::Int => {
                    self.op(Opcode::INeg, 0);
                    Ok(ExprTy::Int)
                }
                ExprTy::Float => {
                    self.op(Opcode::FNeg, 0);
                    Ok(ExprTy::Float)
                }
                other => Err(CodegenError::Unsupported(format!(
                    "unary '-' on {other:?}"
                ))),
            },
            UnOp::Not => match ty {
                ExprTy::Bool => {
                    self.op(Opcode::Not, 0);
                    Ok(ExprTy::Bool)
                }
                other => Err(CodegenError::Unsupported(format!("unary '!' on {other:?}"))),
            },
        }
    }

    fn compile_binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr) -> Result<ExprTy, CodegenError> {
        match op {
            BinOp::And => return self.compile_short_circuit(true, lhs, rhs),
            BinOp::Or => return self.compile_short_circuit(false, lhs, rhs),
            _ => {}
        }

        // String concatenation: '+' where either side is a string.
        if op == BinOp::Add {
            let (peek_l, peek_r) = (peek_type(lhs), peek_type(rhs));
            if peek_l == Some(ExprTy::StringT) || peek_r == Some(ExprTy::StringT) {
                let ty_l = self.compile_expr(lhs)?;
                if ty_l != ExprTy::StringT {
                    self.op(Opcode::ToString, 0);
                }
                let ty_r = self.compile_expr(rhs)?;
                if ty_r != ExprTy::StringT {
                    self.op(Opcode::ToString, 0);
                }
                self.op(Opcode::StrConcat, -1);
                return Ok(ExprTy::StringT);
            }
        }

        let ty_l = self.compile_expr(lhs)?;
        let ty_r = self.compile_expr(rhs)?;

        // Non-numeric equality (string/bool/null/...): the VM compares
        // tagged values directly (vm.md § Value representation) — no
        // numeric widening applies, and nl-sema already validated that the
        // comparison is legal.
        if matches!(op, BinOp::Eq | BinOp::Ne) && !(is_numeric_ty(ty_l) && is_numeric_ty(ty_r)) {
            let opcode = if op == BinOp::Eq { Opcode::CmpEq } else { Opcode::CmpNe };
            self.op(opcode, -1);
            return Ok(ExprTy::Bool);
        }

        let numeric_ty = self.promote_numeric(ty_l, ty_r)?;

        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                let opcode = arithmetic_opcode(op, numeric_ty);
                self.op(opcode, -1);
                Ok(numeric_ty)
            }
            BinOp::Eq => {
                self.op(Opcode::CmpEq, -1);
                Ok(ExprTy::Bool)
            }
            BinOp::Ne => {
                self.op(Opcode::CmpNe, -1);
                Ok(ExprTy::Bool)
            }
            BinOp::Lt => {
                self.op(Opcode::CmpLt, -1);
                Ok(ExprTy::Bool)
            }
            BinOp::Gt => {
                self.op(Opcode::CmpGt, -1);
                Ok(ExprTy::Bool)
            }
            BinOp::Le => {
                self.op(Opcode::CmpLe, -1);
                Ok(ExprTy::Bool)
            }
            BinOp::Ge => {
                self.op(Opcode::CmpGe, -1);
                Ok(ExprTy::Bool)
            }
            BinOp::And | BinOp::Or => unreachable!("handled above"),
        }
    }

    /// If operand types differ (one int, one float), converts the `int` side
    /// in place on the stack via `I2F` and returns `Float`; otherwise both
    /// sides must already match.
    fn promote_numeric(&mut self, ty_l: ExprTy, ty_r: ExprTy) -> Result<ExprTy, CodegenError> {
        match (ty_l, ty_r) {
            (ExprTy::Int, ExprTy::Int) => Ok(ExprTy::Int),
            (ExprTy::Float, ExprTy::Float) => Ok(ExprTy::Float),
            (ExprTy::Int, ExprTy::Float) => {
                // stack: [..., lhs_int, rhs_float] -> convert lhs in place.
                self.op(Opcode::Swap, 0);
                self.op(Opcode::I2F, 0);
                self.op(Opcode::Swap, 0);
                Ok(ExprTy::Float)
            }
            (ExprTy::Float, ExprTy::Int) => {
                // stack: [..., lhs_float, rhs_int] -> convert top (rhs).
                self.op(Opcode::I2F, 0);
                Ok(ExprTy::Float)
            }
            (a, b) => Err(CodegenError::Unsupported(format!(
                "arithmetic/comparison between {a:?} and {b:?}"
            ))),
        }
    }

    fn compile_short_circuit(
        &mut self,
        is_and: bool,
        lhs: &Expr,
        rhs: &Expr,
    ) -> Result<ExprTy, CodegenError> {
        let ty_l = self.compile_expr(lhs)?;
        if ty_l != ExprTy::Bool {
            return Err(CodegenError::Unsupported(format!(
                "logical operator on non-bool {ty_l:?}"
            )));
        }
        self.op(Opcode::Dup, 1);
        let branch_op = if is_and { Opcode::IfFalse } else { Opcode::IfTrue };
        let (branch_pc, branch_operand) = self.branch(branch_op, -1);
        self.op(Opcode::Pop, -1);
        let ty_r = self.compile_expr(rhs)?;
        if ty_r != ExprTy::Bool {
            return Err(CodegenError::Unsupported(format!(
                "logical operator on non-bool {ty_r:?}"
            )));
        }
        let (end_pc, end_operand) = self.branch(Opcode::Goto, 0);
        self.patch_branch(branch_pc, branch_operand);
        self.patch_branch(end_pc, end_operand);
        Ok(ExprTy::Bool)
    }
}

/// Best-effort static type of an expression without emitting code — used
/// only to decide whether `+` means string concatenation before committing
/// to bytecode order.
fn peek_type(expr: &Expr) -> Option<ExprTy> {
    match expr {
        Expr::StringLit(_) => Some(ExprTy::StringT),
        Expr::IntLit(_) => Some(ExprTy::Int),
        Expr::FloatLit(_) => Some(ExprTy::Float),
        Expr::BoolLit(_) => Some(ExprTy::Bool),
        Expr::NullLit => Some(ExprTy::Null),
        Expr::Binary(BinOp::Add, l, r) => match (peek_type(l), peek_type(r)) {
            (Some(ExprTy::StringT), _) | (_, Some(ExprTy::StringT)) => Some(ExprTy::StringT),
            _ => None,
        },
        _ => None,
    }
}

fn is_numeric_ty(ty: ExprTy) -> bool {
    matches!(ty, ExprTy::Int | ExprTy::Float | ExprTy::Byte)
}

fn arithmetic_opcode(op: BinOp, ty: ExprTy) -> Opcode {
    match (op, ty) {
        (BinOp::Add, ExprTy::Int) => Opcode::IAdd,
        (BinOp::Sub, ExprTy::Int) => Opcode::ISub,
        (BinOp::Mul, ExprTy::Int) => Opcode::IMul,
        (BinOp::Div, ExprTy::Int) => Opcode::IDiv,
        (BinOp::Mod, ExprTy::Int) => Opcode::IMod,
        (BinOp::Add, ExprTy::Float) => Opcode::FAdd,
        (BinOp::Sub, ExprTy::Float) => Opcode::FSub,
        (BinOp::Mul, ExprTy::Float) => Opcode::FMul,
        (BinOp::Div, ExprTy::Float) => Opcode::FDiv,
        (BinOp::Mod, ExprTy::Float) => Opcode::FMod,
        _ => unreachable!("non-numeric type reached arithmetic_opcode"),
    }
}
