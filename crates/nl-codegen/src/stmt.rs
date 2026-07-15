use nl_bytecode::{ExceptionTableEntry, Opcode};
use nl_syntax::ast::{Block, CatchClause, Stmt, Type};

use crate::class_table::resolve_type;
use crate::error::CodegenError;
use crate::expr::{expr_ty_of, Emitter, LoopCtx};

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
            Stmt::VarDecl { ty, name, init: Some(init) } => {
                let init_ty = self.compile_expr(init)?;
                let declared_ty = match ty {
                    Some(t) => expr_ty_of(&resolve_type(t, self.imports)),
                    None => init_ty.clone(),
                };
                self.coerce_value(&init_ty, &declared_ty, name)?;
                let index = self.declare_local(name.clone(), declared_ty);
                self.emit_store(index);
            }
            Stmt::VarDecl { ty, name, init: None } => {
                // `auto` without an initializer is rejected by nl-sema
                // (E005); reaching this point implies `ty` is present. The
                // slot is reserved but left unwritten — nl-sema's definite
                // assignment check (E001) guarantees no read reaches it
                // before an explicit assignment.
                let declared_ty = expr_ty_of(&resolve_type(
                    ty.as_ref().expect("nl-sema guarantees a type here"),
                    self.imports,
                ));
                self.declare_local(name.clone(), declared_ty);
            }
            Stmt::ThisCall(args) => {
                self.compile_this_call(args)?;
            }
            Stmt::SuperCall(args) => {
                self.compile_super_call(args)?;
            }
            Stmt::Throw(expr) => {
                self.compile_expr(expr)?;
                self.op(Opcode::Throw, -1);
            }
            Stmt::Try { body, catches, finally } => self.compile_try(body, catches, finally)?,
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

    /// `try { ... } catch (T1 a) { ... } catch (T2 b) { ... } finally { ... }`
    /// — vm.md § Exception handling. Per-catch exception-table entries cover
    /// the `try` body only (declaration order = table order = specificity
    /// order, matching E048's reachability rule already enforced by
    /// nl-sema); a `finally`, if present, gets a catch-all (`catch_type =
    /// 0`) entry covering the body *and* every catch handler, plus a second
    /// copy of its code inline on the normal-completion path.
    ///
    /// Known gap versus the spec (documented, not implemented this phase):
    /// `return`/`break`/`continue` inside a `try`/`catch` jump straight out
    /// without running an enclosing `finally` — the spec requires
    /// duplicating `finally` at every such exit point, which is deferred.
    fn compile_try(&mut self, body: &Block, catches: &[CatchClause], finally: &Option<Block>) -> Result<(), CodegenError> {
        let try_start = self.code.len();
        self.compile_block(body)?;
        let try_end = self.code.len();
        let mut end_patches = vec![self.branch(Opcode::Goto, 0)];

        let mut catch_entries = Vec::with_capacity(catches.len());
        for catch in catches {
            let handler_pc = self.code.len();
            let fqcn = self.resolve_class_name(&catch.ty);
            let catch_type = self.cp.add_class(&fqcn);
            self.push_scope();
            let local = self.declare_local(catch.var.clone(), expr_ty_of(&Type::Named(fqcn)));
            // The VM clears the operand stack and pushes the caught
            // exception before jumping here (vm.md § Throw and stack
            // unwinding) — store it into the catch parameter.
            self.emit_store(local);
            for stmt in &catch.body {
                self.compile_stmt(stmt)?;
            }
            self.pop_scope();
            end_patches.push(self.branch(Opcode::Goto, 0));
            catch_entries.push((handler_pc, catch_type));
        }
        let catches_end = self.code.len();

        let finally_handler_pc = if let Some(finally_body) = finally {
            let handler_pc = self.code.len();
            let exc_local = self.declare_scratch_local(expr_ty_of(&Type::Named("Exception".to_string())));
            self.emit_store(exc_local);
            self.compile_block(finally_body)?;
            self.op_u16(Opcode::Load, exc_local, 1);
            self.op(Opcode::Throw, -1);
            Some(handler_pc)
        } else {
            None
        };

        // Normal-completion path: falls through here from the try body or
        // any catch handler, runs `finally` (a second copy) if present, then
        // continues after the whole statement.
        let normal_finally_pc = self.code.len();
        if let Some(finally_body) = finally {
            self.compile_block(finally_body)?;
        }
        for (pc, operand) in end_patches {
            self.patch_branch_to(pc, operand, normal_finally_pc);
        }

        for (handler_pc, catch_type) in catch_entries {
            self.exception_table.push(ExceptionTableEntry {
                start_pc: try_start as u16,
                end_pc: try_end as u16,
                handler_pc: handler_pc as u16,
                catch_type,
            });
        }
        if let Some(handler_pc) = finally_handler_pc {
            self.exception_table.push(ExceptionTableEntry {
                start_pc: try_start as u16,
                end_pc: catches_end as u16,
                handler_pc: handler_pc as u16,
                catch_type: 0,
            });
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
