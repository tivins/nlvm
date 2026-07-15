use nl_bytecode::{ConstantPool, Opcode};
use nl_syntax::ast::{BinOp, Expr, UnOp};

use crate::error::CodegenError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExprTy {
    Int,
    Float,
    Bool,
    StringT,
    Null,
}

pub struct Emitter<'a> {
    pub code: Vec<u8>,
    pub cp: &'a mut ConstantPool,
    depth: i32,
    max_depth: i32,
}

impl<'a> Emitter<'a> {
    pub fn new(cp: &'a mut ConstantPool) -> Self {
        Self {
            code: Vec::new(),
            cp,
            depth: 0,
            max_depth: 0,
        }
    }

    pub fn max_stack(&self) -> u16 {
        self.max_depth.max(0) as u16
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

    /// Emits a branch opcode with a placeholder offset; pops `stack_delta`
    /// (0 for GOTO, -1 for IF_TRUE/IF_FALSE). Returns (opcode_pc, operand_pos)
    /// for later patching with `patch_branch`.
    fn branch(&mut self, op: Opcode, stack_delta: i32) -> (usize, usize) {
        let opcode_pc = self.code.len();
        self.code.push(op as u8);
        let operand_pos = self.code.len();
        self.code.extend_from_slice(&0i16.to_be_bytes());
        self.track(stack_delta);
        (opcode_pc, operand_pos)
    }

    fn patch_branch(&mut self, opcode_pc: usize, operand_pos: usize) {
        let target = self.code.len() as i32;
        let offset = (target - opcode_pc as i32) as i16;
        self.code[operand_pos..operand_pos + 2].copy_from_slice(&offset.to_be_bytes());
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
            Expr::Unary(op, inner) => self.compile_unary(*op, inner),
            Expr::Binary(op, lhs, rhs) => self.compile_binary(*op, lhs, rhs),
        }
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
