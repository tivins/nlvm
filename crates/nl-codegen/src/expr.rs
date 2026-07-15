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

    pub(crate) fn op(&mut self, op: Opcode, stack_delta: i32) {
        self.code.push(op as u8);
        self.track(stack_delta);
    }

    pub(crate) fn op_u16(&mut self, op: Opcode, operand: u16, stack_delta: i32) {
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

    fn lookup_local(&self, name: &str) -> Result<LocalSlot, CodegenError> {
        for scope in self.scopes.iter().rev() {
            if let Some(slot) = scope.get(name) {
                return Ok(slot.clone());
            }
        }
        Err(CodegenError::Unsupported(format!("undefined variable '{name}'")))
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
            Expr::Ident(name) => {
                let slot = self.lookup_local(name)?;
                self.op_u16(Opcode::Load, slot.index, 1);
                Ok(slot.ty)
            }
            Expr::Assign(target, value) => self.compile_assign(target, value),
            Expr::Call(name, args) => self.compile_call(name, args),
            Expr::New(class_name, args) => self.compile_new(class_name, args),
            Expr::NewArray(elem_ty, size) => self.compile_new_array(elem_ty, size),
            Expr::FieldAccess(target, name) => self.compile_field_access(target, name),
            Expr::MethodCall(target, name, args) => self.compile_method_call(target, name, args),
            Expr::Index(target, index) => self.compile_index(target, index),
            Expr::InstanceOf(target, type_name) => self.compile_instanceof(target, type_name),
            Expr::PostIncr(name) => self.compile_incr(name, 1),
            Expr::PostDecr(name) => self.compile_incr(name, -1),
            Expr::Unary(op, inner) => self.compile_unary(*op, inner),
            Expr::Binary(op, lhs, rhs) => self.compile_binary(*op, lhs, rhs),
            Expr::Match(subject, arms) => self.compile_match(subject, arms),
        }
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
            LValue::Local(name) => {
                let slot = self.lookup_local(name)?;
                let value_ty = self.compile_expr(value)?;
                self.coerce_value(&value_ty, &slot.ty, name)?;
                // Leave a copy as the expression's own value (assignment is
                // an expression, e.g. usable as `a = b = 1;`).
                self.op(Opcode::Dup, 1);
                self.op_u16(Opcode::Store, slot.index, -1);
                Ok(slot.ty)
            }
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

        let ctor = find_ctor(self.classes, &fqcn, args.len())
            .cloned()
            .ok_or_else(|| {
                CodegenError::Unsupported(format!(
                    "no constructor of '{fqcn}' with {} argument(s)",
                    args.len()
                ))
            })?;
        let param_tys: Vec<ExprTy> = ctor.params.iter().map(expr_ty_of).collect();
        self.compile_call_args(args, &param_tys, &fqcn)?;

        let descriptor = method_descriptor(&ctor.params, &Type::Void);
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

    fn compile_field_access(&mut self, target: &Expr, name: &str) -> Result<ExprTy, CodegenError> {
        let target_ty = self.compile_expr(target)?;
        let ExprTy::Object(fqcn) = &target_ty else {
            return Err(CodegenError::Unsupported(format!(
                "field access on non-object type {target_ty:?}"
            )));
        };
        let fqcn = fqcn.clone();
        let field = self.lookup_field(&fqcn, name)?;
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
        let target_ty = self.compile_expr(target)?;
        match &target_ty {
            ExprTy::Array(_) if name == "length" && args.is_empty() => {
                self.op(Opcode::ArrayLength, 0);
                Ok(ExprTy::Int)
            }
            ExprTy::Object(fqcn) => {
                let fqcn = fqcn.clone();
                let method = find_method(self.classes, &fqcn, name, args.len())
                    .cloned()
                    .ok_or_else(|| {
                        CodegenError::Unsupported(format!(
                            "unknown method '{name}' on '{fqcn}' with {} argument(s)",
                            args.len()
                        ))
                    })?;
                let param_tys: Vec<ExprTy> = method.params.iter().map(expr_ty_of).collect();
                self.compile_call_args(args, &param_tys, name)?;

                let descriptor = method_descriptor(&method.params, &method.return_ty);
                let name_index = self.cp.add_utf8(name.to_string());
                let descriptor_index = self.cp.add_type_desc(&descriptor);
                // The static type's class is enough here: the VM re-resolves
                // the receiver's *runtime* class for INVOKE_INSTANCE, so this
                // also works when `fqcn` is an interface with no bytecode of
                // its own (interface dispatch — vm.md § Interface dispatch).
                let class_index = self.cp.add_class(&fqcn);
                let method_ref = self.cp.add_method_ref(class_index, name_index, descriptor_index);
                let return_ty = expr_ty_of(&method.return_ty);
                let result_delta = if return_ty == ExprTy::Void { 0 } else { 1 };
                self.op_u16(Opcode::InvokeInstance, method_ref, result_delta - args.len() as i32 - 1);
                Ok(return_ty)
            }
            other => Err(CodegenError::Unsupported(format!(
                "method call on unsupported type {other:?}"
            ))),
        }
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
