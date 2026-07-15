use nl_bytecode::Opcode;
use nl_syntax::ast::{Block, Stmt};

use crate::error::CodegenError;
use crate::expr::{expr_ty_of, Emitter, ExprTy, LoopCtx};

impl<'a> Emitter<'a> {
    pub fn compile_block(&mut self, block: &[Stmt]) -> Result<(), CodegenError> {
        self.push_scope();
        for stmt in block {
            self.compile_stmt(stmt)?;
        }
        self.pop_scope();
        Ok(())
    }

    pub fn compile_stmt(&mut self, stmt: &Stmt) -> Result<(), CodegenError> {
        match stmt {
            Stmt::Return(Some(expr)) => {
                self.compile_expr(expr)?;
                self.code.push(Opcode::ReturnValue as u8);
            }
            Stmt::Return(None) => {
                self.code.push(Opcode::Return as u8);
            }
            Stmt::Expr(expr) => {
                self.compile_expr_stmt(expr)?;
            }
            Stmt::VarDecl { ty, name, init } => {
                let init_ty = self.compile_expr(init)?;
                let declared_ty = match ty {
                    Some(t) => expr_ty_of(t),
                    None => init_ty,
                };
                if declared_ty == ExprTy::Float && init_ty == ExprTy::Int {
                    self.emit_i2f();
                } else if declared_ty != init_ty {
                    return Err(CodegenError::Unsupported(format!(
                        "cannot initialize variable '{name}' of type {declared_ty:?} with {init_ty:?}"
                    )));
                }
                let index = self.declare_local(name.clone(), declared_ty);
                self.emit_store(index);
            }
            Stmt::If {
                cond,
                then_branch,
                else_branch,
            } => self.compile_if(cond, then_branch, else_branch.as_deref())?,
            Stmt::While { cond, body } => self.compile_while(cond, body)?,
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => self.compile_for(init, cond.as_ref(), step, body)?,
            Stmt::Break => {
                if self.loops.is_empty() {
                    return Err(CodegenError::Unsupported("'break' outside a loop".to_string()));
                }
                let patch = self.branch(Opcode::Goto, 0);
                self.loops.last_mut().unwrap().break_patches.push(patch);
            }
            Stmt::Continue => {
                if self.loops.is_empty() {
                    return Err(CodegenError::Unsupported("'continue' outside a loop".to_string()));
                }
                let patch = self.branch(Opcode::Goto, 0);
                self.loops.last_mut().unwrap().continue_patches.push(patch);
            }
            Stmt::Block(block) => self.compile_block(block)?,
        }
        Ok(())
    }

    fn compile_if(&mut self, cond: &nl_syntax::ast::Expr, then_branch: &Block, else_branch: Option<&[Stmt]>) -> Result<(), CodegenError> {
        self.compile_expr_bool(cond)?;
        let false_patch = self.branch(Opcode::IfFalse, -1);
        self.compile_block(then_branch)?;
        match else_branch {
            Some(else_branch) => {
                let end_patch = self.branch(Opcode::Goto, 0);
                let else_pc = self.code.len();
                self.patch_branch_to(false_patch.0, false_patch.1, else_pc);
                self.compile_block(else_branch)?;
                let end_pc = self.code.len();
                self.patch_branch_to(end_patch.0, end_patch.1, end_pc);
            }
            None => {
                let end_pc = self.code.len();
                self.patch_branch_to(false_patch.0, false_patch.1, end_pc);
            }
        }
        Ok(())
    }

    fn compile_while(&mut self, cond: &nl_syntax::ast::Expr, body: &Block) -> Result<(), CodegenError> {
        let cond_pc = self.code.len();
        self.compile_expr_bool(cond)?;
        let exit_patch = self.branch(Opcode::IfFalse, -1);

        self.loops.push(LoopCtx {
            break_patches: Vec::new(),
            continue_patches: Vec::new(),
        });
        self.compile_block(body)?;
        let ctx = self.loops.pop().unwrap();
        for (pc, operand) in ctx.continue_patches {
            self.patch_branch_to(pc, operand, cond_pc);
        }

        self.emit_goto_to(cond_pc);
        let end_pc = self.code.len();
        self.patch_branch_to(exit_patch.0, exit_patch.1, end_pc);
        for (pc, operand) in ctx.break_patches {
            self.patch_branch_to(pc, operand, end_pc);
        }
        Ok(())
    }

    fn compile_for(
        &mut self,
        init: &[Stmt],
        cond: Option<&nl_syntax::ast::Expr>,
        step: &[nl_syntax::ast::Expr],
        body: &Block,
    ) -> Result<(), CodegenError> {
        self.push_scope();
        for stmt in init {
            self.compile_stmt(stmt)?;
        }

        let cond_pc = self.code.len();
        let exit_patch = match cond {
            Some(cond) => {
                self.compile_expr_bool(cond)?;
                Some(self.branch(Opcode::IfFalse, -1))
            }
            None => None,
        };

        self.loops.push(LoopCtx {
            break_patches: Vec::new(),
            continue_patches: Vec::new(),
        });
        self.compile_block(body)?;
        let ctx = self.loops.pop().unwrap();

        let step_pc = self.code.len();
        for (pc, operand) in ctx.continue_patches {
            self.patch_branch_to(pc, operand, step_pc);
        }
        for expr in step {
            self.compile_expr_stmt(expr)?;
        }

        self.emit_goto_to(cond_pc);
        let end_pc = self.code.len();
        if let Some((pc, operand)) = exit_patch {
            self.patch_branch_to(pc, operand, end_pc);
        }
        for (pc, operand) in ctx.break_patches {
            self.patch_branch_to(pc, operand, end_pc);
        }
        self.pop_scope();
        Ok(())
    }
}
