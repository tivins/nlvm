use std::collections::HashMap;

use nl_bytecode::{ConstantPool, Opcode};
use nl_syntax::ast::{BinOp, Expr, LValue, Type, UnOp};

use crate::class_table::{find_ctor, find_field, find_method, resolve_type, ClassInfo};
use crate::error::CodegenError;
use crate::type_desc::{method_descriptor, type_descriptor};

#[derive(Debug, Clone, PartialEq)]
pub enum ExprTy {
    Int,
    Float,
    Bool,
    Byte,
    StringT,
    Null,
    Void,
    /// Declared/static class type, holding its FQCN.
    Object(String),
    /// Array element type.
    Array(Box<ExprTy>),
    /// A closure value — vm.md § Closures and anonymous functions. `fqcn`
    /// is the synthetic closure class generated for this specific literal
    /// (see `crate::closure`); a closure's *static* type is therefore
    /// really "the type of this one literal", not a structural function
    /// type shared across literals with the same shape — there is no
    /// `typedef`'d function-type syntax to unify them against yet (out of
    /// scope this phase, see PLAN.md).
    Closure {
        params: Vec<ExprTy>,
        return_ty: Box<ExprTy>,
        fqcn: String,
    },
}

/// Inverse of `expr_ty_of`, needed to build field/method descriptors for
/// synthesized closure classes, which only ever deal in `ExprTy` (computed
/// from already-compiled expressions) rather than the source `Type`s
/// `nl-sema`/the rest of `nl-codegen` resolve ahead of time. Closures
/// themselves have no `Type` representation (see `ExprTy::Closure`'s doc
/// comment) — reachable only if a closure captures another closure, which
/// isn't exercised; falls back to `Type::Void` rather than panicking.
fn expr_ty_to_type(ty: &ExprTy) -> Type {
    match ty {
        ExprTy::Int => Type::Int,
        ExprTy::Float => Type::Float,
        ExprTy::Bool => Type::Bool,
        ExprTy::Byte => Type::Byte,
        ExprTy::StringT => Type::StringT,
        ExprTy::Null => Type::NullT,
        ExprTy::Void => Type::Void,
        ExprTy::Object(fqcn) => Type::Named(fqcn.clone()),
        ExprTy::Array(inner) => Type::Array(Box::new(expr_ty_to_type(inner))),
        ExprTy::Closure { .. } => Type::Void,
    }
}

pub(crate) enum IdentRef {
    Local(LocalSlot),
    CapturedField(ExprTy),
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
        Type::Array(inner) => ExprTy::Array(Box::new(expr_ty_of(inner))),
        // Callers are expected to have already resolved `Named` to an FQCN
        // via `class_table::resolve_type` before reaching here.
        Type::Named(name) => ExprTy::Object(name.clone()),
        // Values are dynamically tagged at runtime (vm.md § Value
        // representation), so a union collapses to the `ExprTy` of its first
        // non-null member for codegen purposes — nullability itself is
        // already enforced earlier by nl-sema, not re-checked here.
        Type::Union(members) => members
            .iter()
            .find(|m| !matches!(m, Type::NullT))
            .map(expr_ty_of)
            .unwrap_or(ExprTy::Null),
        // nl_syntax::monomorphize resolves every `Type::Generic` to a plain
        // `Type::Named` before nl-codegen ever runs — see its module doc.
        Type::Generic(name, args) => unreachable!("unresolved generic type '{name}<...>' ({} args) reached codegen", args.len()),
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

#[derive(Debug, Clone)]
pub(crate) struct LocalSlot {
    pub index: u16,
    pub ty: ExprTy,
}

pub(crate) struct LoopCtx {
    pub break_patches: Vec<(usize, usize)>,
    pub continue_patches: Vec<(usize, usize)>,
    /// `Emitter.finally_stack.len()` when this loop was entered — `break`/
    /// `continue` only replay `finally` blocks pushed *after* this point
    /// (i.e. a `try`/`finally` nested inside the loop body), not ones
    /// wrapping the loop itself (compiler.md's `finally` duplication rule
    /// only fires for exits that actually leave the protected region).
    pub finally_depth: usize,
}

pub struct Emitter<'a> {
    pub code: Vec<u8>,
    pub cp: &'a mut ConstantPool,
    pub(crate) static_sigs: &'a HashMap<String, MethodSig>,
    pub(crate) classes: &'a HashMap<String, ClassInfo>,
    pub(crate) imports: &'a HashMap<String, String>,
    pub(crate) this_class: u16,
    pub(crate) this_fqcn: String,
    depth: i32,
    max_depth: i32,
    pub(crate) scopes: Vec<HashMap<String, LocalSlot>>,
    next_local: u16,
    max_locals: u16,
    pub(crate) loops: Vec<LoopCtx>,
    /// Accumulated across every `try` statement in this method — vm.md §
    /// Exception table.
    pub exception_table: Vec<nl_bytecode::ExceptionTableEntry>,
    /// Enclosing `finally` blocks currently protecting the code being
    /// compiled, innermost last — compiler.md's `finally` duplication rule:
    /// `return`/`break`/`continue` must run every `finally` block they exit
    /// through. Cloned (not borrowed) to sidestep threading an AST lifetime
    /// through `Emitter`; these blocks are small and this is compile-time
    /// only. See `Stmt::Return`/`Break`/`Continue` in `stmt.rs`.
    pub(crate) finally_stack: Vec<nl_syntax::ast::Block>,
    /// Non-empty only inside a closure's synthesized `invoke` method — name
    /// -> type of each captured variable, backed by a field of the same
    /// name on `this`. Consulted as a fallback *after* `self.scopes` (so an
    /// inner declaration that shadows a capture's name still wins — see
    /// `resolve_ident`). See `crate::closure`.
    pub(crate) captured_fields: HashMap<String, ExprTy>,
    /// Synthetic closure classes generated while compiling this method,
    /// collected here and threaded back up to `compile_program` so they're
    /// included in the linked program's module set (vm.md § Closures: "The
    /// compiler generates a synthetic class for each closure").
    pub(crate) closures: Vec<nl_bytecode::Module>,
    /// Disambiguates synthetic closure class names within this method
    /// (`{this_fqcn}${method_name}$closure{N}`) — see `crate::closure`.
    pub(crate) closure_counter: u32,
    /// Set by `Stmt::Return(Some(_))` as it compiles — the only way this
    /// crate has to learn an expression's type without a separate
    /// inference pass. Ignored by ordinary methods; consulted by a
    /// block-bodied closure with no explicit return type to deduce one
    /// (specs.md's `() => { return 42; }` — deduced `int`).
    pub(crate) inferred_return_ty: Option<ExprTy>,
    /// `"{this_fqcn}$m{method_index}"` — base for this method's synthetic
    /// closure class names (`$closure0`, `$closure1`, ...). Keyed by
    /// position in `class.methods` rather than just the method's name so
    /// overloads (same name, different params) don't collide.
    pub(crate) closure_name_prefix: String,
}

impl<'a> Emitter<'a> {
    pub fn new(
        cp: &'a mut ConstantPool,
        static_sigs: &'a HashMap<String, MethodSig>,
        classes: &'a HashMap<String, ClassInfo>,
        imports: &'a HashMap<String, String>,
        this_class: u16,
        this_fqcn: String,
    ) -> Self {
        let closure_name_prefix = this_fqcn.clone();
        Self {
            code: Vec::new(),
            cp,
            static_sigs,
            classes,
            imports,
            this_class,
            this_fqcn,
            depth: 0,
            max_depth: 0,
            scopes: Vec::new(),
            next_local: 0,
            max_locals: 0,
            loops: Vec::new(),
            exception_table: Vec::new(),
            finally_stack: Vec::new(),
            captured_fields: HashMap::new(),
            closures: Vec::new(),
            closure_counter: 0,
            inferred_return_ty: None,
            closure_name_prefix,
        }
    }

    /// Emits a clone of every currently-active `finally` block, innermost
    /// first — used by `return`/`break`/`continue` before they jump out of
    /// the region those blocks protect (see `finally_stack`).
    pub(crate) fn replay_finally_blocks(&mut self, from: usize) -> Result<(), CodegenError> {
        let blocks: Vec<nl_syntax::ast::Block> = self.finally_stack[from..].iter().rev().cloned().collect();
        for block in &blocks {
            for stmt in block {
                self.compile_stmt(stmt)?;
            }
        }
        Ok(())
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

    pub(crate) fn op(&mut self, op: Opcode, stack_delta: i32) {
        self.code.push(op as u8);
        self.track(stack_delta);
    }

    pub(crate) fn op_u16(&mut self, op: Opcode, operand: u16, stack_delta: i32) {
        self.code.push(op as u8);
        self.code.extend_from_slice(&operand.to_be_bytes());
        self.track(stack_delta);
    }

    pub(crate) fn op_u16_u16(&mut self, op: Opcode, operand1: u16, operand2: u16, stack_delta: i32) {
        self.code.push(op as u8);
        self.code.extend_from_slice(&operand1.to_be_bytes());
        self.code.extend_from_slice(&operand2.to_be_bytes());
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

    pub(crate) fn op_iinc(&mut self, local_index: u16, delta: i16) {
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

    /// A compiler-internal scratch local (name can never collide with a
    /// user identifier — `$` doesn't lex as an identifier character) used to
    /// hold an intermediate value while emitting field/array-element
    /// assignment (which must leave the assigned value on the stack as the
    /// expression's own result, after popping the receiver/index/value for
    /// SET_FIELD/ARRAY_STORE).
    pub(crate) fn declare_scratch_local(&mut self, ty: ExprTy) -> u16 {
        let name = format!("$tmp{}", self.next_local);
        self.declare_local(name, ty)
    }

    pub(crate) fn lookup_local(&self, name: &str) -> Result<LocalSlot, CodegenError> {
        for scope in self.scopes.iter().rev() {
            if let Some(slot) = scope.get(name) {
                return Ok(slot.clone());
            }
        }
        Err(CodegenError::Unsupported(format!("undefined variable '{name}'")))
    }

    /// `name` resolves either to an ordinary local (`self.scopes`, checked
    /// first so a shadowing declaration wins) or, inside a closure's
    /// `invoke` method, to a captured variable's field on `this`
    /// (`self.captured_fields`).
    pub(crate) fn resolve_ident(&self, name: &str) -> Result<IdentRef, CodegenError> {
        if let Ok(slot) = self.lookup_local(name) {
            return Ok(IdentRef::Local(slot));
        }
        if let Some(ty) = self.captured_fields.get(name) {
            return Ok(IdentRef::CapturedField(ty.clone()));
        }
        Err(CodegenError::Unsupported(format!("undefined variable '{name}'")))
    }

    /// Emits `this.name` (`GET_FIELD` off local 0) for a captured variable
    /// — the closure's synthetic class always has a field of the same name
    /// as the capture (see `crate::closure`).
    fn emit_get_captured_field(&mut self, name: &str, ty: &ExprTy) {
        self.op_u16(Opcode::Load, 0, 1);
        let class_index = self.cp.add_class(&self.this_fqcn.clone());
        let name_index = self.cp.add_utf8(name.to_string());
        let type_index = self.cp.add_type_desc(&type_descriptor(&expr_ty_to_type(ty)));
        let field_ref = self.cp.add_field_ref(class_index, name_index, type_index);
        self.op_u16(Opcode::GetField, field_ref, 0);
    }

    pub(crate) fn resolve_class_name(&self, name: &str) -> String {
        self.imports.get(name).cloned().unwrap_or_else(|| name.to_string())
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
            Expr::This => {
                self.op_u16(Opcode::Load, 0, 1);
                Ok(ExprTy::Object(self.this_fqcn.clone()))
            }
            // Same receiver value as `this` at runtime — only the static
            // type (and, for method calls, the dispatch mode) differs; see
            // `compile_method_call`'s special-case for non-virtual dispatch.
            Expr::Super => {
                let super_fqcn = self.superclass_fqcn()?;
                self.op_u16(Opcode::Load, 0, 1);
                Ok(ExprTy::Object(super_fqcn))
            }
            Expr::Ident(name) => match self.resolve_ident(name)? {
                IdentRef::Local(slot) => {
                    self.op_u16(Opcode::Load, slot.index, 1);
                    Ok(slot.ty)
                }
                IdentRef::CapturedField(ty) => {
                    self.emit_get_captured_field(name, &ty);
                    Ok(ty)
                }
            },
            Expr::Assign(target, value) => self.compile_assign(target, value),
            Expr::Call(name, args) => self.compile_call(name, args),
            Expr::New(class_name, _type_args, args) => self.compile_new(class_name, args),
            Expr::NewArray(elem_ty, size) => self.compile_new_array(elem_ty, size),
            Expr::NewArrayInit(elem_ty, elements) => self.compile_new_array_init(elem_ty, elements),
            Expr::FieldAccess(target, name) => self.compile_field_access(target, name),
            Expr::MethodCall(target, name, args) => self.compile_method_call(target, name, args),
            Expr::Index(target, index) => self.compile_index(target, index),
            Expr::InstanceOf(target, type_name) => self.compile_instanceof(target, type_name),
            Expr::PostIncr(name) => self.compile_incr(name, 1),
            Expr::PostDecr(name) => self.compile_incr(name, -1),
            Expr::Unary(op, inner) => self.compile_unary(*op, inner),
            Expr::Binary(op, lhs, rhs) => self.compile_binary(*op, lhs, rhs),
            Expr::Match(subject, arms) => self.compile_match(subject, arms),
            Expr::Ternary(cond, then_e, else_e) => self.compile_ternary(cond, then_e, else_e),
            Expr::Closure { params, return_type, throws, body } => {
                let _ = throws; // parsed only — see PLAN.md's closures gap (checked-exception verification not extended into closure bodies).
                self.compile_closure(params, return_type, body)
            }
        }
    }

    /// `(params) => body` — vm.md § Closures and anonymous functions.
    /// Generates a synthetic closure class (one field per captured
    /// variable, one `invoke` method compiled from `body`) and emits the
    /// creation site: `NEW` + one `SET_FIELD` per capture, copying each
    /// captured variable's *current* value (by-value capture — see
    /// `ExprTy::Closure`'s doc comment for the boxing gap versus the spec).
    fn compile_closure(
        &mut self,
        params: &[nl_syntax::ast::Param],
        return_type: &Option<Type>,
        body: &nl_syntax::ast::ClosureBody,
    ) -> Result<ExprTy, CodegenError> {
        let param_names: std::collections::HashSet<&str> = params.iter().map(|p| p.name.as_str()).collect();
        let mut candidates: Vec<String> = crate::closure::referenced_names(body).into_iter().collect();
        candidates.retain(|n| !param_names.contains(n.as_str()));
        candidates.sort();

        // Only names that actually resolve as a local in *this* (enclosing)
        // scope are real captures; anything else (a class reference, or a
        // name declared inside the closure body itself) is left for the
        // inner emitter to resolve normally.
        let captures: Vec<(String, ExprTy, u16)> = candidates
            .into_iter()
            .filter_map(|name| self.lookup_local(&name).ok().map(|slot| (name, slot.ty, slot.index)))
            .collect();

        let synth_fqcn = format!("{}$closure{}", self.closure_name_prefix, self.closure_counter);
        self.closure_counter += 1;

        let resolved_params: Vec<Type> = params.iter().map(|p| resolve_type(&p.ty, self.imports)).collect();
        let param_expr_tys: Vec<ExprTy> = resolved_params.iter().map(expr_ty_of).collect();

        let mut synth_cp = ConstantPool::new();
        let synth_this_class = synth_cp.add_class(&synth_fqcn);
        let captured_fields: HashMap<String, ExprTy> = captures.iter().map(|(n, ty, _)| (n.clone(), ty.clone())).collect();

        let deduced_return_ty;
        let invoke_method;
        let mut nested_closures;
        {
            let mut inner = Emitter::new(&mut synth_cp, self.static_sigs, self.classes, self.imports, synth_this_class, synth_fqcn.clone());
            inner.captured_fields = captured_fields;
            inner.push_scope();
            inner.declare_local("this".to_string(), ExprTy::Object(synth_fqcn.clone()));
            for (param, resolved_ty) in params.iter().zip(&resolved_params) {
                inner.declare_local(param.name.clone(), expr_ty_of(resolved_ty));
            }
            deduced_return_ty = match body {
                nl_syntax::ast::ClosureBody::Block(block) => {
                    for stmt in block {
                        inner.compile_stmt(stmt)?;
                    }
                    let ret = match return_type {
                        Some(t) => expr_ty_of(&resolve_type(t, self.imports)),
                        None => inner.inferred_return_ty.clone().unwrap_or(ExprTy::Void),
                    };
                    if ret == ExprTy::Void {
                        inner.op(Opcode::Return, 0);
                    }
                    ret
                }
                nl_syntax::ast::ClosureBody::Expr(e) => {
                    let value_ty = inner.compile_expr(e)?;
                    inner.op(Opcode::ReturnValue, 0);
                    match return_type {
                        Some(t) => expr_ty_of(&resolve_type(t, self.imports)),
                        None => value_ty,
                    }
                }
            };
            inner.pop_scope();

            let descriptor = method_descriptor(&resolved_params, &expr_ty_to_type(&deduced_return_ty));
            let name_index = inner.cp.add_utf8("invoke".to_string());
            let descriptor_index = inner.cp.add_type_desc(&descriptor);
            invoke_method = nl_bytecode::MethodDescriptor {
                flags: nl_bytecode::method_flags::PUBLIC,
                name_index,
                descriptor_index,
                throws_types: Vec::new(),
                max_locals: inner.max_locals(),
                max_stack: inner.max_stack(),
                code: inner.code,
                exception_table: inner.exception_table,
                line_table: Vec::new(),
            };
            nested_closures = inner.closures;
        }

        let fields: Vec<nl_bytecode::FieldDescriptor> = captures
            .iter()
            .map(|(name, ty, _)| {
                let name_index = synth_cp.add_utf8(name.clone());
                let type_index = synth_cp.add_type_desc(&type_descriptor(&expr_ty_to_type(ty)));
                nl_bytecode::FieldDescriptor { flags: nl_bytecode::field_flags::PUBLIC, name_index, type_index }
            })
            .collect();

        self.closures.push(nl_bytecode::Module {
            version: nl_bytecode::module::VERSION,
            constant_pool: synth_cp,
            this_class: synth_this_class,
            class_flags: 0,
            super_class: 0,
            interfaces: Vec::new(),
            fields,
            methods: vec![invoke_method],
            hash_algo: nl_bytecode::HashAlgo::Sha256,
        });
        self.closures.append(&mut nested_closures);

        // Creation site: allocate, then copy each capture's current value
        // into the new object's field of the same name.
        let class_index = self.cp.add_class(&synth_fqcn);
        self.op_u16(Opcode::New, class_index, 1);
        for (name, ty, outer_index) in &captures {
            self.op(Opcode::Dup, 1);
            self.op_u16(Opcode::Load, *outer_index, 1);
            let field_class_index = self.cp.add_class(&synth_fqcn);
            let name_index = self.cp.add_utf8(name.clone());
            let type_index = self.cp.add_type_desc(&type_descriptor(&expr_ty_to_type(ty)));
            let field_ref = self.cp.add_field_ref(field_class_index, name_index, type_index);
            self.op_u16(Opcode::SetField, field_ref, -2);
        }

        Ok(ExprTy::Closure { params: param_expr_tys, return_ty: Box::new(deduced_return_ty), fqcn: synth_fqcn })
    }

    /// Invokes a closure whose receiver has already been pushed onto the
    /// stack (by `compile_call`, from either a local or a captured field).
    fn compile_closure_invoke(
        &mut self,
        params: &[ExprTy],
        return_ty: &ExprTy,
        fqcn: &str,
        args: &[Expr],
    ) -> Result<ExprTy, CodegenError> {
        self.compile_call_args(args, params, "closure call")?;
        let class_index = self.cp.add_class(fqcn);
        let name_index = self.cp.add_utf8("invoke".to_string());
        let param_types: Vec<Type> = params.iter().map(expr_ty_to_type).collect();
        let descriptor = method_descriptor(&param_types, &expr_ty_to_type(return_ty));
        let descriptor_index = self.cp.add_type_desc(&descriptor);
        let method_ref = self.cp.add_method_ref(class_index, name_index, descriptor_index);
        let result_delta = if *return_ty == ExprTy::Void { 0 } else { 1 };
        self.op_u16(Opcode::InvokeClosure, method_ref, result_delta - args.len() as i32 - 1);
        Ok(return_ty.clone())
    }

    /// `cond ? then : else` — a conditional branch, mirroring
    /// `compile_short_circuit`'s pattern of tracking stack depth linearly
    /// through both (mutually exclusive at runtime) branches.
    fn compile_ternary(&mut self, cond: &Expr, then_e: &Expr, else_e: &Expr) -> Result<ExprTy, CodegenError> {
        self.compile_expr_bool(cond)?;
        let (else_pc, else_operand) = self.branch(Opcode::IfFalse, -1);
        let then_ty = self.compile_expr(then_e)?;
        let (end_pc, end_operand) = self.branch(Opcode::Goto, 0);
        self.patch_branch_to(else_pc, else_operand, self.code.len());
        let else_ty = self.compile_expr(else_e)?;
        self.coerce_value(&else_ty, &then_ty, "ternary branch")?;
        self.patch_branch_to(end_pc, end_operand, self.code.len());
        Ok(then_ty)
    }

    /// `match(subject) { pattern: value, ... }` — vm.md § Match expressions:
    /// a chain of `DUP`+compare+branch, one per non-`default` arm. Sema
    /// (E047) guarantees exhaustiveness, so a missing `default` arm can only
    /// happen for an exhaustively-covered `bool` subject — in that case the
    /// last arm doubles as the fallback (no comparison emitted for it).
    fn compile_match(&mut self, subject: &Expr, arms: &[nl_syntax::ast::MatchArm]) -> Result<ExprTy, CodegenError> {
        let subject_ty = self.compile_expr(subject)?;
        let mut end_patches = Vec::new();
        let mut result_ty: Option<ExprTy> = None;
        let last = arms.len().saturating_sub(1);
        for (i, arm) in arms.iter().enumerate() {
            let is_fallback = arm.pattern.is_none() || i == last;
            let next_patch = if is_fallback {
                None
            } else {
                self.op(Opcode::Dup, 1);
                let pattern = arm.pattern.as_ref().expect("non-fallback arm always has a pattern");
                let pattern_ty = self.compile_expr(pattern)?;
                self.coerce_value(&pattern_ty, &subject_ty, "match pattern")?;
                self.op(Opcode::CmpEq, -1);
                Some(self.branch(Opcode::IfFalse, -1))
            };
            self.op(Opcode::Pop, -1);
            let value_ty = self.compile_expr(&arm.value)?;
            if let Some(expected) = &result_ty {
                self.coerce_value(&value_ty, expected, "match arm")?;
            } else {
                result_ty = Some(value_ty);
            }
            end_patches.push(self.branch(Opcode::Goto, 0));
            if let Some((pc, operand)) = next_patch {
                self.patch_branch_to(pc, operand, self.code.len());
            }
        }
        let end_pc = self.code.len();
        for (pc, operand) in end_patches {
            self.patch_branch_to(pc, operand, end_pc);
        }
        Ok(result_ty.unwrap_or(ExprTy::Void))
    }

    fn compile_assign(&mut self, target: &LValue, value: &Expr) -> Result<ExprTy, CodegenError> {
        match target {
            LValue::Local(name) => match self.resolve_ident(name)? {
                IdentRef::Local(slot) => {
                    let value_ty = self.compile_expr(value)?;
                    self.coerce_value(&value_ty, &slot.ty, name)?;
                    // Leave a copy as the expression's own value (assignment
                    // is an expression, e.g. usable as `a = b = 1;`).
                    self.op(Opcode::Dup, 1);
                    self.op_u16(Opcode::Store, slot.index, -1);
                    Ok(slot.ty)
                }
                IdentRef::CapturedField(field_ty) => {
                    let value_ty = self.compile_expr(value)?;
                    self.coerce_value(&value_ty, &field_ty, name)?;
                    self.op(Opcode::Dup, 1);
                    let tmp = self.declare_scratch_local(field_ty.clone());
                    self.emit_store(tmp);
                    self.op_u16(Opcode::Load, 0, 1);
                    self.op(Opcode::Swap, 0);
                    let class_index = self.cp.add_class(&self.this_fqcn.clone());
                    let name_index = self.cp.add_utf8(name.clone());
                    let type_index = self.cp.add_type_desc(&type_descriptor(&expr_ty_to_type(&field_ty)));
                    let field_ref = self.cp.add_field_ref(class_index, name_index, type_index);
                    self.op_u16(Opcode::SetField, field_ref, -2);
                    self.op_u16(Opcode::Load, tmp, 1);
                    Ok(field_ty)
                }
            },
            LValue::Field(target_expr, field_name) => {
                let target_ty = self.compile_expr(target_expr)?;
                let ExprTy::Object(fqcn) = &target_ty else {
                    return Err(CodegenError::Unsupported(format!(
                        "field access on non-object type {target_ty:?}"
                    )));
                };
                let fqcn = fqcn.clone();
                let field = self.lookup_field(&fqcn, field_name)?;
                let field_ty = expr_ty_of(&field);
                let value_ty = self.compile_expr(value)?;
                self.coerce_value(&value_ty, &field_ty, field_name)?;
                self.op(Opcode::Dup, 1);
                let tmp = self.declare_scratch_local(field_ty.clone());
                self.emit_store(tmp);
                let class_index = self.cp.add_class(&fqcn);
                let name_index = self.cp.add_utf8(field_name.clone());
                let type_index = self.cp.add_type_desc(&type_descriptor(&field));
                let field_ref = self.cp.add_field_ref(class_index, name_index, type_index);
                self.op_u16(Opcode::SetField, field_ref, -2);
                self.op_u16(Opcode::Load, tmp, 1);
                Ok(field_ty)
            }
            LValue::Index(target_expr, index_expr) => {
                let target_ty = self.compile_expr(target_expr)?;
                let ExprTy::Array(elem) = target_ty else {
                    return Err(CodegenError::Unsupported(format!(
                        "indexed assignment on non-array type {target_ty:?}"
                    )));
                };
                let elem_ty = *elem;
                let index_ty = self.compile_expr(index_expr)?;
                if index_ty != ExprTy::Int {
                    return Err(CodegenError::Unsupported("array index must be int".to_string()));
                }
                let value_ty = self.compile_expr(value)?;
                self.coerce_value(&value_ty, &elem_ty, "array element")?;
                self.op(Opcode::Dup, 1);
                let tmp = self.declare_scratch_local(elem_ty.clone());
                self.emit_store(tmp);
                self.op(Opcode::ArrayStore, -3);
                self.op_u16(Opcode::Load, tmp, 1);
                Ok(elem_ty)
            }
        }
    }

    fn lookup_field(&self, fqcn: &str, name: &str) -> Result<Type, CodegenError> {
        find_field(self.classes, fqcn, name)
            .map(|f| f.ty.clone())
            .ok_or_else(|| CodegenError::Unsupported(format!("unknown field '{name}' on '{fqcn}'")))
    }

    /// The direct superclass's FQCN, for `super.field`/`super.method(...)`
    /// and `super(...)` constructor delegation.
    pub(crate) fn superclass_fqcn(&self) -> Result<String, CodegenError> {
        self.classes
            .get(&self.this_fqcn)
            .and_then(|c| c.extends.clone())
            .ok_or_else(|| CodegenError::Unsupported(format!("'super' used in class '{}' with no superclass", self.this_fqcn)))
    }

    fn compile_incr(&mut self, name: &str, delta: i16) -> Result<ExprTy, CodegenError> {
        let slot = match self.resolve_ident(name)? {
            IdentRef::Local(slot) => slot,
            // `IINC` operates on a local-variable slot by index; a captured
            // variable is a field on `this` instead, which would need a
            // separate load/add/store sequence — not implemented (rare
            // enough in practice, and by-value capture already means the
            // mutation wouldn't be observable outside the closure anyway;
            // see `ExprTy::Closure`'s doc comment).
            IdentRef::CapturedField(_) => {
                return Err(CodegenError::Unsupported(format!(
                    "'++'/'--' on captured closure variable '{name}' is not supported"
                )))
            }
        };
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
        // `add(5, 3)` where `add` is a closure-typed local/capture, not a
        // same-class static method — vm.md § Closures: "the compiler
        // determines the closure's type signature at compile time".
        match self.resolve_ident(name) {
            Ok(IdentRef::Local(slot)) => {
                if let ExprTy::Closure { params, return_ty, fqcn } = slot.ty {
                    self.op_u16(Opcode::Load, slot.index, 1);
                    return self.compile_closure_invoke(&params, &return_ty, &fqcn, args);
                }
            }
            Ok(IdentRef::CapturedField(ty)) => {
                if let ExprTy::Closure { params, return_ty, fqcn } = ty.clone() {
                    self.emit_get_captured_field(name, &ty);
                    return self.compile_closure_invoke(&params, &return_ty, &fqcn, args);
                }
            }
            Err(_) => {}
        }
        let sig = self
            .static_sigs
            .get(name)
            .cloned()
            .ok_or_else(|| CodegenError::Unsupported(format!("call to unknown method '{name}'")))?;
        self.compile_call_args(args, &sig.param_types, name)?;
        let result_delta = if sig.return_ty == ExprTy::Void { 0 } else { 1 };
        self.op_u16(
            Opcode::InvokeStatic,
            sig.method_ref_index,
            result_delta - args.len() as i32,
        );
        Ok(sig.return_ty)
    }

    fn compile_new(&mut self, class_name: &str, args: &[Expr]) -> Result<ExprTy, CodegenError> {
        let fqcn = self.resolve_class_name(class_name);
        let class_index = self.cp.add_class(&fqcn);
        self.op_u16(Opcode::New, class_index, 1);
        self.op(Opcode::Dup, 1);

        // `new system.List<int>(...)`/`new system.Map<K,V>(...)` — `fqcn`
        // is already the monomorphized instantiation name by this point
        // (nl_syntax::monomorphize), same as any user template. No
        // `ClassInfo` is registered for it (native, no `.nl` source), so
        // `find_ctor` below would always fail; `crate::native_generics`
        // recovers the constructor's parameter types straight from the
        // mangled name instead — see its doc comment. The emitted bytecode
        // shape (`NEW`/`DUP`/args/`INVOKE_SPECIAL <construct>`) is
        // identical either way; only the parameter-type source differs, and
        // `nl_vm::interpreter` intercepts `INVOKE_SPECIAL` against a native
        // generic class before ever consulting `Program`'s module map (like
        // `nl_vm::native::is_native_class` does for `INVOKE_STATIC`).
        let params: Vec<Type> = if let Some(param_types) = crate::native_generics::ctor_param_types(&fqcn, args.len()) {
            param_types
        } else if let Some(param_types) = crate::stdlib::ctor_param_types(&fqcn, args.len()) {
            // `new system.Random()`/`new system.Random(int seed)` — the
            // other native instance class besides FileHandle, but
            // constructible directly (see `crate::stdlib::ctor_param_types`'s
            // doc comment).
            param_types
        } else {
            find_ctor(self.classes, &fqcn, args.len())
                .cloned()
                .ok_or_else(|| {
                    CodegenError::Unsupported(format!(
                        "no constructor of '{fqcn}' with {} argument(s)",
                        args.len()
                    ))
                })?
                .params
        };
        let param_tys: Vec<ExprTy> = params.iter().map(expr_ty_of).collect();
        self.compile_call_args(args, &param_tys, &fqcn)?;

        let descriptor = method_descriptor(&params, &Type::Void);
        let name_index = self.cp.add_utf8("<construct>");
        let descriptor_index = self.cp.add_type_desc(&descriptor);
        let method_ref = self.cp.add_method_ref(class_index, name_index, descriptor_index);
        self.op_u16(Opcode::InvokeSpecial, method_ref, -(1 + args.len() as i32));
        Ok(ExprTy::Object(fqcn))
    }

    fn compile_super_method_call(&mut self, name: &str, args: &[Expr]) -> Result<ExprTy, CodegenError> {
        let super_fqcn = self.superclass_fqcn()?;
        self.op_u16(Opcode::Load, 0, 1);
        let method = find_method(self.classes, &super_fqcn, name, args.len())
            .cloned()
            .ok_or_else(|| {
                CodegenError::Unsupported(format!(
                    "unknown method '{name}' on superclass '{super_fqcn}' with {} argument(s)",
                    args.len()
                ))
            })?;
        let param_tys: Vec<ExprTy> = method.params.iter().map(expr_ty_of).collect();
        self.compile_call_args(args, &param_tys, name)?;

        let descriptor = method_descriptor(&method.params, &method.return_ty);
        let name_index = self.cp.add_utf8(name.to_string());
        let descriptor_index = self.cp.add_type_desc(&descriptor);
        let class_index = self.cp.add_class(&super_fqcn);
        let method_ref = self.cp.add_method_ref(class_index, name_index, descriptor_index);
        let return_ty = expr_ty_of(&method.return_ty);
        let result_delta = if return_ty == ExprTy::Void { 0 } else { 1 };
        self.op_u16(Opcode::InvokeSpecial, method_ref, result_delta - args.len() as i32 - 1);
        Ok(return_ty)
    }

    /// `super(args);` constructor delegation — like `this(...)` but invokes
    /// the direct superclass's constructor instead of an overload in the
    /// same class.
    pub(crate) fn compile_super_call(&mut self, args: &[Expr]) -> Result<(), CodegenError> {
        let super_fqcn = self.superclass_fqcn()?;
        self.op_u16(Opcode::Load, 0, 1);
        let ctor = find_ctor(self.classes, &super_fqcn, args.len())
            .cloned()
            .ok_or_else(|| {
                CodegenError::Unsupported(format!(
                    "no constructor of '{super_fqcn}' with {} argument(s) for super(...)",
                    args.len()
                ))
            })?;
        let param_tys: Vec<ExprTy> = ctor.params.iter().map(expr_ty_of).collect();
        self.compile_call_args(args, &param_tys, "super(...)")?;

        let descriptor = method_descriptor(&ctor.params, &Type::Void);
        let name_index = self.cp.add_utf8("<construct>");
        let descriptor_index = self.cp.add_type_desc(&descriptor);
        let class_index = self.cp.add_class(&super_fqcn);
        let method_ref = self.cp.add_method_ref(class_index, name_index, descriptor_index);
        self.op_u16(Opcode::InvokeSpecial, method_ref, -(1 + args.len() as i32));
        Ok(())
    }

    pub(crate) fn compile_this_call(&mut self, args: &[Expr]) -> Result<(), CodegenError> {
        self.op_u16(Opcode::Load, 0, 1);
        let ctor = find_ctor(self.classes, &self.this_fqcn, args.len())
            .cloned()
            .ok_or_else(|| {
                CodegenError::Unsupported(format!(
                    "no constructor of '{}' with {} argument(s) for this(...)",
                    self.this_fqcn,
                    args.len()
                ))
            })?;
        let param_tys: Vec<ExprTy> = ctor.params.iter().map(expr_ty_of).collect();
        self.compile_call_args(args, &param_tys, "this(...)")?;

        let descriptor = method_descriptor(&ctor.params, &Type::Void);
        let name_index = self.cp.add_utf8("<construct>");
        let descriptor_index = self.cp.add_type_desc(&descriptor);
        let method_ref = self.cp.add_method_ref(self.this_class, name_index, descriptor_index);
        self.op_u16(Opcode::InvokeSpecial, method_ref, -(1 + args.len() as i32));
        Ok(())
    }

    fn compile_new_array(&mut self, elem_ty: &Type, size: &Expr) -> Result<ExprTy, CodegenError> {
        let size_ty = self.compile_expr(size)?;
        if size_ty != ExprTy::Int {
            return Err(CodegenError::Unsupported(format!(
                "array size must be int, found {size_ty:?}"
            )));
        }
        let resolved_elem = resolve_type(elem_ty, self.imports);
        let type_index = self.cp.add_type_desc(&type_descriptor(&resolved_elem));
        self.op_u16(Opcode::NewArray, type_index, 0);
        Ok(ExprTy::Array(Box::new(expr_ty_of(&resolved_elem))))
    }

    fn compile_new_array_init(&mut self, elem_ty: &Type, elements: &[Expr]) -> Result<ExprTy, CodegenError> {
        let resolved_elem = resolve_type(elem_ty, self.imports);
        let elem_expr_ty = expr_ty_of(&resolved_elem);
        for e in elements {
            let actual = self.compile_expr(e)?;
            self.coerce_value(&actual, &elem_expr_ty, "array element")?;
        }
        let count: u16 = elements.len().try_into().map_err(|_| {
            CodegenError::Unsupported(format!(
                "array initializer list has {} elements, max supported is {}",
                elements.len(),
                u16::MAX
            ))
        })?;
        let type_index = self.cp.add_type_desc(&type_descriptor(&resolved_elem));
        self.op_u16_u16(Opcode::NewArrayInit, type_index, count, 1 - count as i32);
        Ok(ExprTy::Array(Box::new(elem_expr_ty)))
    }

    fn compile_field_access(&mut self, target: &Expr, name: &str) -> Result<ExprTy, CodegenError> {
        // `system.io.FileMode.Read` etc. — a dotted class-path expression
        // naming an enum-like stdlib int constant, not a value; compiling
        // `target` normally would try (and fail) to load a local variable
        // named `system`. Same shape as `compile_method_call`'s
        // `system.Out.print(...)` check below.
        if let Some(path) = dotted_path(target) {
            let leading = path.split('.').next().expect("dotted_path is never empty");
            if self.lookup_local(leading).is_err() {
                if let Some(value) = crate::stdlib::enum_const_value(&path, name) {
                    self.emit_int_const(value);
                    return Ok(ExprTy::Object(path));
                }
            }
        }
        let target_ty = self.compile_expr(target)?;
        let ExprTy::Object(fqcn) = &target_ty else {
            return Err(CodegenError::Unsupported(format!(
                "field access on non-object type {target_ty:?}"
            )));
        };
        let fqcn = fqcn.clone();
        // `entry.key`/`entry.value` on a `system.MapEntry<K, V>` — native
        // result type with no `ClassInfo`, field types come from the
        // mangled name (see `crate::native_generics::field_ty`).
        let field = match crate::native_generics::field_ty(&fqcn, name) {
            Some(ty) => ty,
            None => match crate::stdlib::result_field_ty(&fqcn, name) {
                Some(ty) => ty,
                None => self.lookup_field(&fqcn, name)?,
            },
        };
        let field_ty = expr_ty_of(&field);
        let class_index = self.cp.add_class(&fqcn);
        let name_index = self.cp.add_utf8(name.to_string());
        let type_index = self.cp.add_type_desc(&type_descriptor(&field));
        let field_ref = self.cp.add_field_ref(class_index, name_index, type_index);
        self.op_u16(Opcode::GetField, field_ref, 0);
        Ok(field_ty)
    }

    fn compile_method_call(&mut self, target: &Expr, name: &str, args: &[Expr]) -> Result<ExprTy, CodegenError> {
        // `super.method(...)` — non-virtual dispatch straight to the
        // superclass's implementation (vm.md § Super calls), unlike every
        // other receiver which goes through virtual INVOKE_INSTANCE below.
        if matches!(target, Expr::Super) {
            return self.compile_super_method_call(name, args);
        }
        // `system.Out.print(...)` and friends: the receiver is a dotted
        // class-path expression (nested `FieldAccess`/`Ident`), not a value
        // — compiling it normally would try (and fail) to load a local
        // variable named `system`. Detected before falling into the normal
        // instance-call path below.
        if let Some(path) = dotted_path(target) {
            let leading = path.split('.').next().expect("dotted_path is never empty");
            if self.lookup_local(leading).is_err() && crate::stdlib::is_stdlib_class(&path) {
                return self.compile_stdlib_call(&path, name, args);
            }
        }
        let target_ty = self.compile_expr(target)?;
        match &target_ty {
            ExprTy::Array(_) if name == "length" && args.is_empty() => {
                self.op(Opcode::ArrayLength, 0);
                Ok(ExprTy::Int)
            }
            // `numbers.slice/map/filter/forEach/sort/find(...)` — specs.md §
            // Arrays, Built-in methods. `length` above is the one dedicated
            // opcode (`ARRAY_LENGTH`, performance-critical per vm.md); the
            // rest go through `compile_array_method_call`'s `INVOKE_INSTANCE`.
            ExprTy::Array(elem) => {
                let elem_ty = (**elem).clone();
                self.compile_array_method_call(elem_ty, name, args)
            }
            // `text.trim()` etc. — stdlib.md § system.String instance
            // methods. The receiver is already compiled and sitting on the
            // stack; look up the *full* signature (receiver included) in
            // `crate::stdlib::signature`, the same table the static
            // `system.String.trim(text)` form uses, then compile the
            // remaining args and emit `INVOKE_STATIC system.String.<name>`.
            ExprTy::StringT => {
                let full_argc = args.len() + 1;
                let (param_types, return_ty) =
                    crate::stdlib::signature("system.String", name, full_argc).ok_or_else(|| {
                        CodegenError::Unsupported(format!(
                            "unknown method '{name}' on string with {} argument(s)",
                            args.len()
                        ))
                    })?;
                let extra_param_tys: Vec<ExprTy> = param_types[1..].iter().map(expr_ty_of).collect();
                self.compile_call_args(args, &extra_param_tys, name)?;
                self.emit_native_static("system.String", name, &param_types, &return_ty)
            }
            ExprTy::Object(fqcn) => {
                let fqcn = fqcn.clone();
                // `list.size()`/`map.get(k)` etc. — see `compile_new`'s
                // matching comment and `crate::native_generics`'s doc
                // comment; `handle.read(...)` etc. likewise resolve from
                // `crate::stdlib::instance_signature` (`system.io.FileHandle`
                // has no bytecode `Module` either). Falls through to the
                // ordinary user-class path below for everything else.
                let (params, return_ty) = if let Some(sig) = crate::stdlib::instance_signature(&fqcn, name, args.len()) {
                    sig
                } else if let Some(sig) = crate::native_generics::method_signature(&fqcn, name, args.len()) {
                    sig
                } else {
                    let method = find_method(self.classes, &fqcn, name, args.len())
                        .cloned()
                        .ok_or_else(|| {
                            CodegenError::Unsupported(format!(
                                "unknown method '{name}' on '{fqcn}' with {} argument(s)",
                                args.len()
                            ))
                        })?;
                    (method.params, method.return_ty)
                };
                let param_tys: Vec<ExprTy> = params.iter().map(expr_ty_of).collect();
                self.compile_call_args(args, &param_tys, name)?;

                let descriptor = method_descriptor(&params, &return_ty);
                let name_index = self.cp.add_utf8(name.to_string());
                let descriptor_index = self.cp.add_type_desc(&descriptor);
                // The static type's class is enough here: the VM re-resolves
                // the receiver's *runtime* class for INVOKE_INSTANCE, so this
                // also works when `fqcn` is an interface with no bytecode of
                // its own (interface dispatch — vm.md § Interface dispatch).
                let class_index = self.cp.add_class(&fqcn);
                let method_ref = self.cp.add_method_ref(class_index, name_index, descriptor_index);
                let return_expr_ty = expr_ty_of(&return_ty);
                let result_delta = if return_expr_ty == ExprTy::Void { 0 } else { 1 };
                self.op_u16(Opcode::InvokeInstance, method_ref, result_delta - args.len() as i32 - 1);
                Ok(return_expr_ty)
            }
            other => Err(CodegenError::Unsupported(format!(
                "method call on unsupported type {other:?}"
            ))),
        }
    }

    /// The six native array methods with callbacks (specs.md § Arrays,
    /// Built-in methods; `length` is handled by the caller via the
    /// dedicated `ARRAY_LENGTH` opcode before reaching here). `slice` takes
    /// two plain ints; the rest take a single closure argument, invoked by
    /// `nl_vm::native::dispatch_array` once per element (twice per pair for
    /// `Map.forEach`, a separate call site — see that function's doc
    /// comment).
    ///
    /// `map`'s result element type `U` has no static representation (no
    /// `Type::Function` this phase — see `ExprTy::Closure`'s doc comment),
    /// so unlike `filter`/`find` (which keep the receiver's own element
    /// type, since their callback can't change it) it is recovered directly
    /// from the closure literal's own *deduced* return type
    /// (`ExprTy::Closure`'s `return_ty`) rather than guessed — more precise
    /// than falling back to the `Type::Void` wildcard nl-sema uses (see
    /// `checker.rs`'s matching arm), and needed so a subsequent
    /// `U[] result = numbers.map(...)` assignment sees the real `U`.
    fn compile_array_method_call(&mut self, elem_ty: ExprTy, name: &str, args: &[Expr]) -> Result<ExprTy, CodegenError> {
        match (name, args.len()) {
            ("slice", 2) => {
                self.compile_call_args(args, &[ExprTy::Int, ExprTy::Int], name)?;
                self.emit_array_call(name, &[ExprTy::Int, ExprTy::Int], ExprTy::Array(Box::new(elem_ty)))
            }
            ("map", 1) => {
                let closure_ty = self.compile_expr(&args[0])?;
                let result_elem = match &closure_ty {
                    ExprTy::Closure { return_ty, .. } => (**return_ty).clone(),
                    _ => return Err(CodegenError::Unsupported(format!("'{name}' expects a closure argument"))),
                };
                self.coerce_value(&closure_ty, &ExprTy::Void, name)?;
                self.emit_array_call(name, &[ExprTy::Void], ExprTy::Array(Box::new(result_elem)))
            }
            ("filter", 1) => {
                let closure_ty = self.compile_expr(&args[0])?;
                self.coerce_value(&closure_ty, &ExprTy::Void, name)?;
                self.emit_array_call(name, &[ExprTy::Void], ExprTy::Array(Box::new(elem_ty)))
            }
            ("forEach", 1) | ("sort", 1) => {
                let closure_ty = self.compile_expr(&args[0])?;
                self.coerce_value(&closure_ty, &ExprTy::Void, name)?;
                self.emit_array_call(name, &[ExprTy::Void], ExprTy::Void)
            }
            ("find", 1) => {
                let closure_ty = self.compile_expr(&args[0])?;
                self.coerce_value(&closure_ty, &ExprTy::Void, name)?;
                // `T|null` — `ExprTy` has no union representation (values
                // are dynamically tagged at runtime); collapses to `T`,
                // same as every other nullable native result this codebase
                // returns (e.g. `Map.get`).
                self.emit_array_call(name, &[ExprTy::Void], elem_ty)
            }
            _ => Err(CodegenError::Unsupported(format!(
                "unknown array method '{name}' with {} argument(s)",
                args.len()
            ))),
        }
    }

    /// One `INVOKE_INSTANCE` against an array receiver (already on the
    /// stack, args already compiled/coerced on top of it) for one of the
    /// six native callback methods. Arrays have no class of their own to
    /// key a method ref by — the constant-pool class name here is a
    /// placeholder (`"system.Array"`, never a real class) since
    /// `nl_vm::interpreter` dispatches on the receiver's `Value::Array`
    /// variant rather than by class name (see `nl_vm::native::dispatch_array`).
    fn emit_array_call(&mut self, name: &str, param_types: &[ExprTy], return_ty: ExprTy) -> Result<ExprTy, CodegenError> {
        let param_ast_types: Vec<Type> = param_types.iter().map(expr_ty_to_type).collect();
        let descriptor = method_descriptor(&param_ast_types, &expr_ty_to_type(&return_ty));
        let name_index = self.cp.add_utf8(name.to_string());
        let descriptor_index = self.cp.add_type_desc(&descriptor);
        let class_index = self.cp.add_class("system.Array");
        let method_ref = self.cp.add_method_ref(class_index, name_index, descriptor_index);
        let result_delta = if return_ty == ExprTy::Void { 0 } else { 1 };
        self.op_u16(Opcode::InvokeInstance, method_ref, result_delta - param_types.len() as i32 - 1);
        Ok(return_ty)
    }

    /// Emits an `INVOKE_STATIC` against a native `system.*` class (no
    /// backing bytecode `Module` — see `nl_vm::native`). `print`/`println`
    /// are normalized to their single `(string) -> void` overload first
    /// (`crate::stdlib::is_printlike`); everything else uses its declared
    /// signature from `crate::stdlib::signature`.
    fn compile_stdlib_call(&mut self, fqcn: &str, name: &str, args: &[Expr]) -> Result<ExprTy, CodegenError> {
        if crate::stdlib::is_printlike(fqcn, name) {
            if args.len() != 1 {
                return Err(CodegenError::Unsupported(format!(
                    "'{name}' expects 1 argument, got {}",
                    args.len()
                )));
            }
            let ty = self.compile_expr(&args[0])?;
            match ty {
                ExprTy::StringT => {}
                ExprTy::Int | ExprTy::Float | ExprTy::Bool => self.op(Opcode::ToString, 0),
                other => {
                    return Err(CodegenError::Unsupported(format!(
                        "'{name}' expects a string/int/float/bool argument, got {other:?}"
                    )))
                }
            }
            return self.emit_native_static(fqcn, name, &[Type::StringT], &Type::Void);
        }

        // `system.ps.Process.run` — two overloads at the same arity
        // (`string[] args` vs `string command`, stdlib.md), which
        // `crate::stdlib::signature` can't disambiguate the way `print`
        // does (that table only keys on arity). The two shapes need
        // genuinely different bytecode, not a shared normalization, so the
        // concrete descriptor is picked from the compiled argument's actual
        // `ExprTy` — the VM's native dispatch (`nl_vm::native`) likewise
        // switches on the argument's runtime value variant, not the
        // descriptor.
        if fqcn == "system.ps.Process" && name == "run" {
            if args.len() != 1 {
                return Err(CodegenError::Unsupported(format!("'run' expects 1 argument, got {}", args.len())));
            }
            let ty = self.compile_expr(&args[0])?;
            let param_ty = match &ty {
                ExprTy::StringT => Type::StringT,
                ExprTy::Array(elem) if **elem == ExprTy::StringT => Type::Array(Box::new(Type::StringT)),
                other => {
                    return Err(CodegenError::Unsupported(format!(
                        "'run' expects a string or string[] argument, got {other:?}"
                    )))
                }
            };
            return self.emit_native_static(fqcn, name, &[param_ty], &crate::stdlib::process_result());
        }

        let (param_types, return_ty) = crate::stdlib::signature(fqcn, name, args.len()).ok_or_else(|| {
            CodegenError::Unsupported(format!(
                "unknown stdlib method '{fqcn}.{name}' with {} argument(s)",
                args.len()
            ))
        })?;
        let param_expr_tys: Vec<ExprTy> = param_types.iter().map(expr_ty_of).collect();
        self.compile_call_args(args, &param_expr_tys, name)?;
        self.emit_native_static(fqcn, name, &param_types, &return_ty)
    }

    /// `params`/`return_ty` describe both the operand-stack effect (the
    /// caller must already have pushed exactly `params.len()` values) and
    /// the constant-pool `MethodRef` descriptor the VM's native dispatcher
    /// matches on.
    fn emit_native_static(&mut self, fqcn: &str, name: &str, params: &[Type], return_ty: &Type) -> Result<ExprTy, CodegenError> {
        let class_index = self.cp.add_class(fqcn);
        let name_index = self.cp.add_utf8(name.to_string());
        let descriptor = method_descriptor(params, return_ty);
        let descriptor_index = self.cp.add_type_desc(&descriptor);
        let method_ref = self.cp.add_method_ref(class_index, name_index, descriptor_index);
        let ret = expr_ty_of(return_ty);
        let result_delta = if ret == ExprTy::Void { 0 } else { 1 };
        self.op_u16(Opcode::InvokeStatic, method_ref, result_delta - params.len() as i32);
        Ok(ret)
    }

    fn compile_index(&mut self, target: &Expr, index: &Expr) -> Result<ExprTy, CodegenError> {
        let target_ty = self.compile_expr(target)?;
        let ExprTy::Array(elem) = target_ty else {
            return Err(CodegenError::Unsupported(format!(
                "indexing on non-array type {target_ty:?}"
            )));
        };
        let index_ty = self.compile_expr(index)?;
        if index_ty != ExprTy::Int {
            return Err(CodegenError::Unsupported("array index must be int".to_string()));
        }
        self.op(Opcode::ArrayLoad, -1);
        Ok(*elem)
    }

    fn compile_instanceof(&mut self, target: &Expr, type_name: &str) -> Result<ExprTy, CodegenError> {
        self.compile_expr(target)?;
        let fqcn = self.resolve_class_name(type_name);
        let class_index = self.cp.add_class(&fqcn);
        self.op_u16(Opcode::InstanceOf, class_index, 0);
        Ok(ExprTy::Bool)
    }

    /// Coerces a single already-compiled value on top of the stack from
    /// `actual` to `expected` (int -> float widening; `null` is accepted for
    /// any type here since nullability itself is nl-sema's job). Used for
    /// plain-assignment/initializer sites; call-argument lists use
    /// `compile_call_args`, which applies the same rule per argument.
    pub(crate) fn coerce_value(&mut self, actual: &ExprTy, expected: &ExprTy, what: &str) -> Result<(), CodegenError> {
        if *expected == ExprTy::Float && *actual == ExprTy::Int {
            self.op(Opcode::I2F, 0);
        } else if *actual == ExprTy::Null {
            // Nullability was already validated by nl-sema.
        } else if matches!((actual, expected), (ExprTy::Object(_), ExprTy::Object(_))) {
            // Interface/subtype assignability between two object types is
            // nl-sema's job (it has the class table's `implements` lists);
            // a `Value::Object` doesn't carry its static type at runtime, so
            // there's nothing for codegen to enforce here either way.
        } else if matches!(actual, ExprTy::Closure { .. }) && *expected == ExprTy::Void {
            // A closure literal's own static type is just "this one
            // literal" (see `ExprTy::Closure`'s doc comment) — there's no
            // function-type syntax yet to check a callback parameter's
            // shape against, so any closure is accepted wherever a
            // callback param is declared (`Type::Void` used as the same
            // joker nl-sema uses for a closure's own inferred type). First
            // exercised by `system.thread.Thread(() => void task)`.
        } else if actual != expected {
            return Err(CodegenError::Unsupported(format!(
                "cannot assign {actual:?} to '{what}' of type {expected:?}"
            )));
        }
        Ok(())
    }

    fn compile_call_args(&mut self, args: &[Expr], param_types: &[ExprTy], ctx: &str) -> Result<(), CodegenError> {
        if args.len() != param_types.len() {
            return Err(CodegenError::Unsupported(format!(
                "'{ctx}' expects {} argument(s), got {}",
                param_types.len(),
                args.len()
            )));
        }
        for (arg, expected_ty) in args.iter().zip(param_types) {
            let actual = self.compile_expr(arg)?;
            self.coerce_value(&actual, expected_ty, ctx)?;
        }
        Ok(())
    }

    pub(crate) fn emit_int_const(&mut self, v: i64) {
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

        // Non-numeric equality (string/bool/null/references/...): the VM
        // compares tagged values directly (vm.md § Value representation) —
        // no numeric widening applies, and nl-sema already validated that
        // the comparison is legal.
        if matches!(op, BinOp::Eq | BinOp::Ne) && !(is_numeric_ty(&ty_l) && is_numeric_ty(&ty_r)) {
            let opcode = if op == BinOp::Eq { Opcode::CmpEq } else { Opcode::CmpNe };
            self.op(opcode, -1);
            return Ok(ExprTy::Bool);
        }

        let numeric_ty = self.promote_numeric(ty_l, ty_r)?;

        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                let opcode = arithmetic_opcode(op, &numeric_ty);
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
            BinOp::Cmp3 => {
                self.op(Opcode::CmpThreeWay, -1);
                Ok(ExprTy::Int)
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
            // `byte` has no dedicated arithmetic opcode and `CMP_*` only
            // accepts matching-tag pairs — widen to `int` (vm.md § Integer
            // arithmetic: "byte values are widened to int before
            // arithmetic"), then reuse the int/float rules above.
            (ExprTy::Byte, ExprTy::Byte) => {
                // stack: [..., lhs_byte, rhs_byte] -> widen both to int.
                self.op(Opcode::B2I, 0);
                self.op(Opcode::Swap, 0);
                self.op(Opcode::B2I, 0);
                self.op(Opcode::Swap, 0);
                Ok(ExprTy::Int)
            }
            (ExprTy::Byte, ExprTy::Int) => {
                // stack: [..., lhs_byte, rhs_int] -> widen lhs.
                self.op(Opcode::Swap, 0);
                self.op(Opcode::B2I, 0);
                self.op(Opcode::Swap, 0);
                Ok(ExprTy::Int)
            }
            (ExprTy::Int, ExprTy::Byte) => {
                // stack: [..., lhs_int, rhs_byte] -> widen rhs.
                self.op(Opcode::B2I, 0);
                Ok(ExprTy::Int)
            }
            (ExprTy::Byte, ExprTy::Float) => {
                // stack: [..., lhs_byte, rhs_float] -> widen lhs to float.
                self.op(Opcode::Swap, 0);
                self.op(Opcode::B2I, 0);
                self.op(Opcode::I2F, 0);
                self.op(Opcode::Swap, 0);
                Ok(ExprTy::Float)
            }
            (ExprTy::Float, ExprTy::Byte) => {
                // stack: [..., lhs_float, rhs_byte] -> widen rhs to float.
                self.op(Opcode::B2I, 0);
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

/// Reconstructs a dotted path (`"system.Out"`) from a chain of
/// `Ident`/`FieldAccess` nodes, or `None` if `expr` isn't such a chain —
/// used to recognize a `system.*` stdlib class reference before it's
/// (incorrectly) compiled as a value. Mirrors `nl_sema::checker`'s copy.
fn dotted_path(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(name) => Some(name.clone()),
        Expr::FieldAccess(base, name) => Some(format!("{}.{name}", dotted_path(base)?)),
        _ => None,
    }
}

fn is_numeric_ty(ty: &ExprTy) -> bool {
    matches!(ty, ExprTy::Int | ExprTy::Float | ExprTy::Byte)
}

fn arithmetic_opcode(op: BinOp, ty: &ExprTy) -> Opcode {
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
