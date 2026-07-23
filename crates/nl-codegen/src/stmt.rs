use nl_bytecode::{ExceptionTableEntry, Opcode};
use nl_syntax::ast::{Block, CatchClause, Stmt, StmtKind, Type};

use crate::class_table::resolve_type;
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
        self.record_line(stmt.line);
        match &stmt.kind {
            StmtKind::Return(Some(expr)) => {
                let ty = self.compile_expr(expr)?;
                self.inferred_return_ty = Some(ty);
                self.replay_finally_blocks(0)?;
                self.code.push(Opcode::ReturnValue as u8);
            }
            StmtKind::Return(None) => {
                self.replay_finally_blocks(0)?;
                self.code.push(Opcode::Return as u8);
            }
            StmtKind::Expr(expr) => {
                self.compile_expr_stmt(expr)?;
            }
            StmtKind::VarDecl {
                ty,
                name,
                init: Some(init),
                is_const: _,
            } => {
                // vm.md § Variable capture and boxing — a variable that some
                // closure captures-and-mutates needs a shared `Box<T>`
                // rather than a plain slot (`Emitter::boxed_captures`,
                // computed once per method/closure body). Only ever true
                // for an explicitly-typed declaration (see
                // `compile_boxed_var_decl`'s doc comment), so the `auto`
                // path below is unaffected.
                if self.boxed_captures.contains(name) {
                    let declared_ty = expr_ty_of(&resolve_type(
                        ty.as_ref()
                            .expect("boxed captures are always explicitly typed — see nl_syntax::monomorphize::collect_closure_box_requests"),
                        self.imports,
                    ));
                    self.compile_boxed_var_decl(declared_ty, name, init)?;
                } else {
                    let init_ty = self.compile_expr(init)?;
                    let declared_ty = match ty {
                        Some(t) => expr_ty_of(&resolve_type(t, self.imports)),
                        None => init_ty.clone(),
                    };
                    self.coerce_value(&init_ty, &declared_ty, name)?;
                    let index = self.declare_local(name.clone(), declared_ty);
                    self.emit_store(index);
                }
            }
            StmtKind::VarDecl {
                ty,
                name,
                init: None,
                is_const: _,
            } => {
                // `auto` without an initializer is rejected by nl-sema
                // (E005); reaching this point implies `ty` is present. The
                // slot is reserved but left unwritten — nl-sema's definite
                // assignment check (E001) guarantees no read reaches it
                // before an explicit assignment.
                let declared_ty = expr_ty_of(&resolve_type(
                    ty.as_ref().expect("nl-sema guarantees a type here"),
                    self.imports,
                ));
                if self.boxed_captures.contains(name) {
                    self.declare_boxed_var_uninit(declared_ty, name);
                } else {
                    self.declare_local(name.clone(), declared_ty);
                }
            }
            StmtKind::ThisCall(args) => {
                self.compile_this_call(args)?;
            }
            StmtKind::SuperCall(args) => {
                self.compile_super_call(args)?;
            }
            StmtKind::Throw(expr) => {
                self.compile_expr(expr)?;
                self.op(Opcode::Throw, -1);
            }
            StmtKind::Try {
                body,
                catches,
                finally,
            } => self.compile_try(body, catches, finally)?,
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => self.compile_if(cond, then_branch, else_branch.as_deref())?,
            StmtKind::While { cond, body } => self.compile_while(cond, body)?,
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => self.compile_for(init, cond.as_ref(), step, body)?,
            StmtKind::ForEach {
                ty,
                var,
                iterable,
                body,
                ..
            } => self.compile_foreach(ty.as_ref(), var, iterable, body)?,
            StmtKind::Break => {
                // Targets the nearest enclosing construct of either kind —
                // a `switch` or a real loop (specs.md § Switch/Match).
                if self.loops.is_empty() {
                    return Err(CodegenError::Unsupported(
                        "'break' outside a loop or switch".to_string(),
                    ));
                }
                self.replay_finally_blocks(self.loops.last().unwrap().finally_depth)?;
                let patch = self.branch(Opcode::Goto, 0);
                self.loops.last_mut().unwrap().break_patches.push(patch);
            }
            StmtKind::Continue => {
                // Skips past any enclosing `switch` frame(s) to the nearest
                // real loop — see `LoopCtx::is_switch`'s doc comment.
                let Some(finally_depth) = self
                    .loops
                    .iter()
                    .rev()
                    .find(|l| !l.is_switch)
                    .map(|l| l.finally_depth)
                else {
                    return Err(CodegenError::Unsupported(
                        "'continue' outside a loop".to_string(),
                    ));
                };
                self.replay_finally_blocks(finally_depth)?;
                let patch = self.branch(Opcode::Goto, 0);
                self.loops
                    .iter_mut()
                    .rev()
                    .find(|l| !l.is_switch)
                    .unwrap()
                    .continue_patches
                    .push(patch);
            }
            StmtKind::Block(block) => self.compile_block(block)?,
            StmtKind::Switch { subject, cases } => self.compile_switch(subject, cases)?,
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
    /// `return`/`break`/`continue` inside the `try` body or a `catch`
    /// handler run a clone of `finally` first (`Emitter::finally_stack`,
    /// pushed here and popped before `finally`'s own code is emitted, so a
    /// `finally` block's own exits don't re-trigger it but still trigger
    /// any *outer* enclosing `finally`).
    fn compile_try(
        &mut self,
        body: &Block,
        catches: &[CatchClause],
        finally: &Option<Block>,
    ) -> Result<(), CodegenError> {
        if let Some(finally_body) = finally {
            self.finally_stack.push(finally_body.clone());
        }

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

        if finally.is_some() {
            self.finally_stack.pop();
        }

        let finally_handler_pc = if let Some(finally_body) = finally {
            let handler_pc = self.code.len();
            let exc_local =
                self.declare_scratch_local(expr_ty_of(&Type::Named("Exception".to_string())));
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

    fn compile_if(
        &mut self,
        cond: &nl_syntax::ast::Expr,
        then_branch: &Block,
        else_branch: Option<&[Stmt]>,
    ) -> Result<(), CodegenError> {
        self.compile_expr_bool(cond)?;
        let false_patch = self.branch(Opcode::IfFalse, -1);
        self.with_instanceof_narrowing(cond, |this| this.compile_block(then_branch))?;
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

    fn compile_while(
        &mut self,
        cond: &nl_syntax::ast::Expr,
        body: &Block,
    ) -> Result<(), CodegenError> {
        let cond_pc = self.code.len();
        self.compile_expr_bool(cond)?;
        let exit_patch = self.branch(Opcode::IfFalse, -1);

        self.loops.push(LoopCtx {
            break_patches: Vec::new(),
            continue_patches: Vec::new(),
            finally_depth: self.finally_stack.len(),
            is_switch: false,
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
            finally_depth: self.finally_stack.len(),
            is_switch: false,
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

    /// `for ([const] item : collection)` — vm.md § For-each loops:
    /// desugared into an index-based loop over hidden scratch locals (the
    /// collection reference and the index), exactly the pseudo-bytecode
    /// pattern the spec gives. Arrays use `ARRAY_LENGTH`/`ARRAY_LOAD`;
    /// `system.List<T>` uses `size()`/`get(i)` via `INVOKE_INSTANCE`.
    /// `system.Map<K,V>` iteration needs `entries()`/`MapEntry<K,V>`
    /// (neither exists — PLAN.md Phase 6 gap) and is rejected explicitly.
    fn compile_foreach(
        &mut self,
        ty: Option<&Type>,
        var: &str,
        iterable: &nl_syntax::ast::Expr,
        body: &Block,
    ) -> Result<(), CodegenError> {
        self.push_scope();
        let mut iterable_ty = self.compile_expr(iterable)?;
        // A map is iterated through its `entries()` array (vm.md § For-each
        // loops): call it right away and loop over the result exactly like
        // a plain array of `MapEntry<K, V>`.
        if let ExprTy::Object(fqcn) = &iterable_ty {
            if fqcn.starts_with("system.Map<") {
                let (_, ret) = crate::native_generics::method_signature(fqcn, "entries", 0)
                    .expect("system.Map instantiation always has entries()");
                self.emit_native_instance_call(&fqcn.clone(), "entries", 0)?;
                iterable_ty = expr_ty_of(&ret);
            }
        }
        let (list_fqcn, elem_ty) = match &iterable_ty {
            ExprTy::Array(elem) => (None, (**elem).clone()),
            ExprTy::Object(fqcn) if fqcn.starts_with("system.List<") => {
                let (_, ret) = crate::native_generics::method_signature(fqcn, "get", 1)
                    .expect("system.List instantiation always has get(int)");
                (Some(fqcn.clone()), expr_ty_of(&ret))
            }
            other => {
                return Err(CodegenError::Unsupported(format!(
                    "for-each over non-iterable type {other:?} (arrays, system.List and system.Map only)"
                )))
            }
        };
        let coll_local = self.declare_scratch_local(iterable_ty.clone());
        self.emit_store(coll_local);
        let idx_local = self.declare_scratch_local(ExprTy::Int);
        self.emit_int_const(0);
        self.emit_store(idx_local);
        // Declared once, re-stored before each iteration of the body.
        let item_ty = match ty {
            Some(t) => expr_ty_of(&resolve_type(t, self.imports)),
            None => elem_ty,
        };
        let item_local = self.declare_local(var.to_string(), item_ty);

        let cond_pc = self.code.len();
        self.op_u16(Opcode::Load, idx_local, 1);
        self.op_u16(Opcode::Load, coll_local, 1);
        match &list_fqcn {
            None => self.op(Opcode::ArrayLength, 0),
            Some(fqcn) => self.emit_native_instance_call(fqcn, "size", 0)?,
        }
        self.op(Opcode::CmpLt, -1);
        let exit_patch = self.branch(Opcode::IfFalse, -1);

        self.op_u16(Opcode::Load, coll_local, 1);
        self.op_u16(Opcode::Load, idx_local, 1);
        match &list_fqcn {
            None => self.op(Opcode::ArrayLoad, -1),
            Some(fqcn) => self.emit_native_instance_call(fqcn, "get", 1)?,
        }
        self.emit_store(item_local);

        self.loops.push(LoopCtx {
            break_patches: Vec::new(),
            continue_patches: Vec::new(),
            finally_depth: self.finally_stack.len(),
            is_switch: false,
        });
        self.compile_block(body)?;
        let ctx = self.loops.pop().unwrap();

        // `continue` proceeds to the next element: jump to the increment.
        let step_pc = self.code.len();
        for (pc, operand) in ctx.continue_patches {
            self.patch_branch_to(pc, operand, step_pc);
        }
        self.op_iinc(idx_local, 1);
        self.emit_goto_to(cond_pc);
        let end_pc = self.code.len();
        self.patch_branch_to(exit_patch.0, exit_patch.1, end_pc);
        for (pc, operand) in ctx.break_patches {
            self.patch_branch_to(pc, operand, end_pc);
        }
        self.pop_scope();
        Ok(())
    }

    /// `switch (subject) { case v1: ... case v2: ... default: ... }` —
    /// specs.md § Switch/Match, fall-through semantics. Compiled as a single-
    /// pass dispatch (one `CmpEq`+branch per `case`, testing the subject —
    /// stashed in a scratch local so its value survives across the whole
    /// statement, unlike `compile_match`'s stack-resident `Dup`/`Pop`, which
    /// wouldn't survive arbitrary statement bodies between comparisons)
    /// followed by every case's body laid out as one flat instruction
    /// sequence in source order: a matched `case`'s jump target is the start
    /// of its own body, and falling off the end of a body (no `break`) runs
    /// straight into the next one — that's fall-through, for free, from the
    /// physical layout. `default`, if present, is where "no case matched"
    /// falls through to, regardless of its position among `cases` (matching
    /// C's `switch`: `default` is the wildcard for unmatched values, not
    /// necessarily the last arm — see `StmtKind::Switch`'s doc comment).
    /// `break` inside any case body is handled by `compile_stmt`'s
    /// `StmtKind::Break` arm through the same `self.loops` stack real loops
    /// use (`LoopCtx::is_switch = true` here so `continue` knows to skip
    /// past this frame to an enclosing real loop instead).
    fn compile_switch(
        &mut self,
        subject: &nl_syntax::ast::Expr,
        cases: &[nl_syntax::ast::SwitchCase],
    ) -> Result<(), CodegenError> {
        let subject_ty = self.compile_expr(subject)?;
        let subject_local = self.declare_scratch_local(subject_ty.clone());
        self.emit_store(subject_local);

        // Dispatch: one comparison + conditional jump per value-`case`,
        // targeting a body-start patch resolved once every body has been
        // laid out below. `default_index`, if set, is where the
        // "nothing matched" fallthrough goes instead of past the whole
        // statement.
        let mut value_patches: Vec<(usize, (usize, usize))> = Vec::new();
        let mut default_index: Option<usize> = None;
        for (i, case) in cases.iter().enumerate() {
            match &case.value {
                Some(value) => {
                    self.op_u16(Opcode::Load, subject_local, 1);
                    let value_ty = self.compile_expr(value)?;
                    self.coerce_value(&value_ty, &subject_ty, "switch case")?;
                    self.op(Opcode::CmpEq, -1);
                    value_patches.push((i, self.branch(Opcode::IfTrue, -1)));
                }
                None => default_index = Some(i),
            }
        }
        let no_match_patch = self.branch(Opcode::Goto, 0);

        self.loops.push(LoopCtx {
            break_patches: Vec::new(),
            continue_patches: Vec::new(),
            finally_depth: self.finally_stack.len(),
            is_switch: true,
        });
        let mut body_starts = vec![0usize; cases.len()];
        for (i, case) in cases.iter().enumerate() {
            body_starts[i] = self.code.len();
            self.compile_block(&case.body)?;
        }
        let end_pc = self.code.len();
        let ctx = self.loops.pop().unwrap();

        for (i, (pc, operand)) in value_patches {
            self.patch_branch_to(pc, operand, body_starts[i]);
        }
        let (pc, operand) = no_match_patch;
        self.patch_branch_to(pc, operand, default_index.map_or(end_pc, |i| body_starts[i]));
        for (pc, operand) in ctx.break_patches {
            self.patch_branch_to(pc, operand, end_pc);
        }
        debug_assert!(
            ctx.continue_patches.is_empty(),
            "'continue' always attaches to the nearest non-switch LoopCtx — see StmtKind::Continue"
        );
        Ok(())
    }

    /// One `INVOKE_INSTANCE` against a native generic class (the arguments
    /// and receiver must already be on the stack) — same method-ref shape
    /// `compile_method_call` emits for an explicit `list.get(i)` call.
    fn emit_native_instance_call(
        &mut self,
        fqcn: &str,
        name: &str,
        argc: usize,
    ) -> Result<(), CodegenError> {
        let (params, ret) =
            crate::native_generics::method_signature(fqcn, name, argc).ok_or_else(|| {
                CodegenError::Unsupported(format!(
                    "unknown method '{name}' on '{fqcn}' with {argc} argument(s)"
                ))
            })?;
        let descriptor = crate::type_desc::method_descriptor(&params, &ret);
        let name_index = self.cp.add_utf8(name.to_string());
        let descriptor_index = self.cp.add_type_desc(&descriptor);
        let class_index = self.cp.add_class(fqcn);
        let method_ref = self
            .cp
            .add_method_ref(class_index, name_index, descriptor_index);
        let result_delta = if expr_ty_of(&ret) == ExprTy::Void {
            0
        } else {
            1
        };
        self.op_u16(
            Opcode::InvokeInstance,
            method_ref,
            result_delta - argc as i32 - 1,
        );
        Ok(())
    }
}
