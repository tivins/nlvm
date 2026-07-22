use std::collections::{HashMap, HashSet};

use nl_bytecode::{ConstantPool, Opcode};
use nl_syntax::ast::{Arg, BinOp, Expr, LValue, Type, UnOp};

use crate::class_table::{
    find_ctor, find_field, find_method, find_operator_method, resolve_type, ClassInfo, MethodInfo,
};
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
    /// is either the synthetic closure class generated for one specific
    /// literal (see `crate::closure`), when this `ExprTy` comes straight
    /// from compiling a closure expression, or `FUNCTION_TYPE_FQCN` — a
    /// placeholder — when it comes from `expr_ty_of` resolving an explicit
    /// `Type::Function` (specs.md § Function type assignment: a local,
    /// field, parameter, or return type written as `(int) => bool`, as
    /// opposed to a closure literal's own inferred type). The placeholder
    /// is safe: `Opcode::InvokeClosure` dispatches purely on the receiver's
    /// *runtime* class plus the method name/descriptor (see
    /// `nl_vm::interpreter`'s handler — the constant-pool method_ref's
    /// static class is decoded and discarded), so any string here compiles
    /// to a working call as long as `params`/`return_ty` match the actual
    /// value's `invoke` descriptor — enforced structurally (ignoring
    /// `fqcn`) by `Emitter::coerce_value`'s dedicated `Closure`/`Closure`
    /// branch. What this does *not* give us is target typing (specs.md's
    /// return-type-deduction rule 5): a closure literal directly assigned
    /// to an explicitly *wider* function type (e.g. `(int) => float k =
    /// (int n) => n;`) still compiles its own `invoke` descriptor from its
    /// own deduced type, so `coerce_value` rejects the mismatch instead of
    /// inserting the widening — there's no adapter/thunk mechanism to make
    /// that safe without also fixing the pre-existing gap that closure
    /// bodies never coerce their `return` values to a declared/expected
    /// type (see `compile_closure`). Assumed limitation, not attempted this
    /// phase: every spec example other than that one numeric-widening case
    /// already has matching descriptors on both sides.
    Closure {
        params: Vec<ExprTy>,
        return_ty: Box<ExprTy>,
        fqcn: String,
    },
}

/// Placeholder `fqcn` for an `ExprTy::Closure` built from an explicit
/// `Type::Function` rather than a specific closure literal — see that
/// variant's doc comment.
pub const FUNCTION_TYPE_FQCN: &str = "$FunctionType";

/// Inverse of `expr_ty_of`, needed to build field/method descriptors for
/// synthesized closure classes, which only ever deal in `ExprTy` (computed
/// from already-compiled expressions) rather than the source `Type`s
/// `nl-sema`/the rest of `nl-codegen` resolve ahead of time. A genuine
/// closure *literal*'s `ExprTy` has no `Type` representation (see
/// `ExprTy::Closure`'s doc comment) and falls back to `Type::Void` rather
/// than panicking — reachable only if a closure captures another closure,
/// which isn't exercised. One built from an explicit `Type::Function` (the
/// placeholder-`fqcn` case, same doc comment) round-trips exactly, since
/// there's a real source `Type` behind it.
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
        ExprTy::Closure {
            params,
            return_ty,
            fqcn,
        } if fqcn == FUNCTION_TYPE_FQCN => Type::Function {
            params: params.iter().map(expr_ty_to_type).collect(),
            return_type: Box::new(expr_ty_to_type(return_ty)),
            throws: Vec::new(),
        },
        ExprTy::Closure { .. } => Type::Void,
    }
}

/// Whether two `ExprTy::Closure` values have the same function-type shape —
/// same arity, each param and the return type structurally equal — ignoring
/// `fqcn` (see `ExprTy::Closure`'s doc comment: a placeholder `fqcn` never
/// carries meaning, and even between two real closure literals,
/// `InvokeClosure` dispatch never looks at it). Mirrors
/// `nl_sema::types::atom_eq`'s `Type::Function` case, one layer down (over
/// `ExprTy` instead of `Type`, post-resolution).
fn closure_shape_eq(a: &ExprTy, b: &ExprTy) -> bool {
    match (a, b) {
        (
            ExprTy::Closure {
                params: pa,
                return_ty: ra,
                ..
            },
            ExprTy::Closure {
                params: pb,
                return_ty: rb,
                ..
            },
        ) => {
            pa.len() == pb.len()
                && pa.iter().zip(pb).all(|(x, y)| closure_shape_eq(x, y))
                && closure_shape_eq(ra, rb)
        }
        (ExprTy::Array(ea), ExprTy::Array(eb)) => closure_shape_eq(ea, eb),
        _ => a == b,
    }
}

/// `base` wrapped in `depth` array layers — e.g. `plain_array_of(int, 2)` is
/// `int[][]`. Used to build the element-type descriptor for each allocated
/// layer of a multidimensional `new T[...]` (compiler.md § Multidimensional
/// array creation); nullability from omitted deeper dimensions is not
/// represented here (`ExprTy` erases it — see `nl-codegen`'s doc note in
/// PLAN.md, values are dynamically tagged at runtime instead).
fn plain_array_of(base: &Type, depth: usize) -> Type {
    let mut ty = base.clone();
    for _ in 0..depth {
        ty = Type::Array(Box::new(ty));
    }
    ty
}

/// Closure invocations don't support named/optional arguments yet (only
/// user-class methods/constructors do, via `crate::class_table::
/// resolve_positional_args` — see PLAN.md) — rejects a named argument with
/// a clear diagnostic instead of silently misbinding it, and otherwise
/// just unwraps each `Arg` to its plain expression.
fn require_positional_args(args: &[Arg]) -> Result<Vec<Expr>, CodegenError> {
    args.iter()
        .map(|a| match &a.name {
            Some(name) => Err(CodegenError::Unsupported(format!(
                "named argument '{name}' is not supported when calling a closure"
            ))),
            None => Ok(a.value.clone()),
        })
        .collect()
}

pub(crate) enum IdentRef {
    Local(LocalSlot),
    CapturedField(CapturedField),
}

/// A closure-captured variable's field on `this` — vm.md § Closures and
/// anonymous functions / § Variable capture and boxing. `ty` is the
/// logical/declared type as source code sees it; `boxed` mirrors
/// `LocalSlot::boxed`'s meaning one layer down: when `true`, the physical
/// field holds a `Box<ty>` shared with the enclosing scope (mutations in
/// either direction are visible to the other), and every read/write must go
/// through `Box<ty>.value` instead of the field directly.
#[derive(Debug, Clone)]
pub(crate) struct CapturedField {
    pub ty: ExprTy,
    pub boxed: bool,
}

/// Where a closure literal's capture gets its *current value* from at the
/// creation site (`compile_closure`'s copy loop) — either an ordinary local
/// slot in the enclosing scope, or (2+ levels of closure nesting) a capture
/// already sitting on the enclosing closure's own `this`.
enum CaptureSource {
    Local(u16),
    Recaptured,
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
        Type::Generic(name, args) => unreachable!(
            "unresolved generic type '{name}<...>' ({} args) reached codegen",
            args.len()
        ),
        // See `ExprTy::Closure`'s doc comment for the placeholder `fqcn`.
        Type::Function {
            params,
            return_type,
            ..
        } => ExprTy::Closure {
            params: params.iter().map(expr_ty_of).collect(),
            return_ty: Box::new(expr_ty_of(return_type)),
            fqcn: FUNCTION_TYPE_FQCN.to_string(),
        },
    }
}

/// Static signature of a method in the class currently being compiled —
/// enough to type-check call sites and resolve them to a constant-pool
/// `MethodRef`, built in a first pass so calls (including recursive/forward
/// calls) can resolve regardless of declaration order.
#[derive(Debug, Clone)]
pub struct MethodSig {
    pub param_types: Vec<ExprTy>,
    /// Parallel to `param_types` — see `crate::class_table::CtorInfo`'s
    /// fields of the same name/shape (this is the same information, kept
    /// separately because bare calls resolve through this same-class cache
    /// rather than `crate::class_table`).
    pub param_names: Vec<String>,
    pub defaults: Vec<Option<Expr>>,
    /// See `crate::class_table::CtorInfo::is_ref`.
    pub is_ref: Vec<bool>,
    pub return_ty: ExprTy,
    pub method_ref_index: u16,
}

#[derive(Debug, Clone)]
pub(crate) struct LocalSlot {
    pub index: u16,
    /// The parameter/local's own declared/logical type — for a `ref`
    /// parameter this is `T` (what the *source* sees), even though the
    /// slot physically holds a `Box<T>` reference (see `boxed`).
    pub ty: ExprTy,
    /// `Some(T)` when this slot is a `ref` parameter bound to a caller's
    /// box (vm.md § Ref parameters (boxing)) rather than a plain local —
    /// every read/write of it must go through `Box<T>.value` instead of a
    /// direct `LOAD`/`STORE`/`IINC` on the slot. `None` for every ordinary
    /// local (locals/other params can never be boxed — only `ref`
    /// parameters are).
    pub boxed: Option<ExprTy>,
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
    /// vm.md § Method descriptor (line-number table) — one entry per source
    /// line change, recorded at each statement's first emitted byte. Used at
    /// runtime to resolve a frame's current `pc` back to a source line for
    /// `Exception.stackTrace`.
    pub line_table: Vec<nl_bytecode::LineTableEntry>,
    last_line: u32,
    /// Enclosing `finally` blocks currently protecting the code being
    /// compiled, innermost last — compiler.md's `finally` duplication rule:
    /// `return`/`break`/`continue` must run every `finally` block they exit
    /// through. Cloned (not borrowed) to sidestep threading an AST lifetime
    /// through `Emitter`; these blocks are small and this is compile-time
    /// only. See `Stmt::Return`/`Break`/`Continue` in `stmt.rs`.
    pub(crate) finally_stack: Vec<nl_syntax::ast::Block>,
    /// Non-empty only inside a closure's synthesized `invoke` method — name
    /// -> type/boxed-ness of each captured variable, backed by a field of
    /// the same name on `this`. Consulted as a fallback *after*
    /// `self.scopes` (so an inner declaration that shadows a capture's name
    /// still wins — see `resolve_ident`). See `crate::closure`.
    pub(crate) captured_fields: HashMap<String, CapturedField>,
    /// Names in the method/closure body currently being compiled that need
    /// a `Box<T>` slot (vm.md § Variable capture and boxing) — computed once
    /// up front by `crate::closure::boxed_captures_in_block`/
    /// `boxed_captures` and consulted by `stmt::compile_stmt`'s `VarDecl`
    /// arms and `compile_method`'s parameter loop to decide, at each
    /// declaration, whether to box it.
    pub(crate) boxed_captures: HashSet<String>,
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
            line_table: Vec::new(),
            last_line: 0,
            finally_stack: Vec::new(),
            captured_fields: HashMap::new(),
            boxed_captures: HashSet::new(),
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
        let blocks: Vec<nl_syntax::ast::Block> =
            self.finally_stack[from..].iter().rev().cloned().collect();
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

    pub(crate) fn op_u16_u16(
        &mut self,
        op: Opcode,
        operand1: u16,
        operand2: u16,
        stack_delta: i32,
    ) {
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

    /// Records a line-number table entry at the current `pc` if `line`
    /// differs from the last one recorded (vm.md § Method descriptor —
    /// entries are sorted by ascending `start_pc`, each covering offsets up
    /// to the next entry's `start_pc`, so a run of statements on the same
    /// line only needs one entry). Called once per statement, at its first
    /// emitted byte; `line == 0` (no source position, e.g. `auto`-desugared
    /// synthetic statements) is skipped rather than recorded as a fake `0`
    /// entry that would shadow whatever real line preceded it.
    pub(crate) fn record_line(&mut self, line: u32) {
        if line == 0 || line == self.last_line {
            return;
        }
        self.last_line = line;
        self.line_table.push(nl_bytecode::LineTableEntry {
            start_pc: self.code.len() as u16,
            line,
        });
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

    /// compiler.md § Type narrowing (smart casts) — `nl-sema` already
    /// enforces narrowing when *type-checking* `a.bark()` inside
    /// `if (a instanceof Dog)`; `nl-codegen` has its own separate (and
    /// coarser) `ExprTy` inference, which must independently see `a` as
    /// `Dog` there too, or method/field resolution picks the wrong
    /// declaring class (or fails outright, e.g. a method that only exists
    /// on the narrowed subtype). Unlike `nl-sema`'s narrowing (keyed by a
    /// never-reused variable id, layered over `resolve`), this overlays the
    /// *actual* `LocalSlot.ty` in whichever scope currently holds `name` —
    /// simpler because codegen only ever needs this for `instanceof`
    /// (nullable unions already erase to their non-null `ExprTy` member
    /// regardless of narrowing — see `expr_ty_of`'s `Type::Union` arm — so
    /// null-check narrowing needs no codegen-side counterpart). Returns the
    /// previous type so the caller can restore it once the narrowed region
    /// ends; `None` if `name` isn't a plain local (e.g. a closure capture)
    /// — left un-narrowed rather than erroring, same leniency as the rest
    /// of this crate's best-effort resolution.
    fn set_local_ty(&mut self, name: &str, ty: ExprTy) -> Option<ExprTy> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(slot) = scope.get_mut(name) {
                return Some(std::mem::replace(&mut slot.ty, ty));
            }
        }
        None
    }

    /// If `cond` is `<ident> instanceof C`, narrows that local's `ExprTy` to
    /// `C` for the duration of the caller-supplied closure (the `if`'s
    /// then-branch — compiler.md's table only narrows the `true` branch for
    /// `instanceof`), then restores it. A no-op passthrough for any other
    /// condition shape.
    pub(crate) fn with_instanceof_narrowing<T>(
        &mut self,
        cond: &Expr,
        f: impl FnOnce(&mut Self) -> Result<T, CodegenError>,
    ) -> Result<T, CodegenError> {
        let restore = match cond {
            Expr::InstanceOf(target, type_name) => match target.as_ref() {
                Expr::Ident(name) => {
                    let fqcn = self.resolve_class_name(type_name);
                    self.set_local_ty(name, ExprTy::Object(fqcn))
                        .map(|old| (name.clone(), old))
                }
                _ => None,
            },
            _ => None,
        };
        let result = f(self);
        if let Some((name, old_ty)) = restore {
            self.set_local_ty(&name, old_ty);
        }
        result
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
            .insert(
                name,
                LocalSlot {
                    index,
                    ty,
                    boxed: None,
                },
            );
        index
    }

    /// Like `declare_local`, for a `ref` parameter — vm.md § Ref parameters
    /// (boxing): the slot physically holds the caller's `Box<T>`, not a `T`
    /// directly, so every later read/write of `name` needs to go through
    /// the box (see `LocalSlot::boxed`).
    pub(crate) fn declare_ref_param(&mut self, name: String, inner_ty: ExprTy) -> u16 {
        let index = self.next_local;
        self.next_local += 1;
        if self.next_local > self.max_locals {
            self.max_locals = self.next_local;
        }
        self.scopes
            .last_mut()
            .expect("declare_ref_param outside any scope")
            .insert(
                name,
                LocalSlot {
                    index,
                    ty: inner_ty.clone(),
                    boxed: Some(inner_ty),
                },
            );
        index
    }

    /// Re-declares an already-declared parameter as boxed — vm.md § Variable
    /// capture and boxing, applied to a parameter that some closure
    /// captures-and-mutates (see `compile_boxed_var_decl`/
    /// `declare_boxed_var_uninit` for the analogous `T name = init;` case).
    /// Must run *after* every parameter has claimed its ordinary positional
    /// slot — boxing here, not inside the declaration loop itself, keeps
    /// every parameter's slot index matching the method descriptor's
    /// calling convention, which reserves exactly one slot per parameter
    /// regardless of what this method does with it afterwards. Allocates a
    /// fresh slot for the box and re-points `name` at it (`declare_ref_param`
    /// inserting the same key again simply overwrites the scope's previous
    /// entry) — the original raw slot is left in place but never read again.
    pub(crate) fn rebox_local(&mut self, name: &str, ty: ExprTy) {
        let raw = self
            .lookup_local(name)
            .expect("rebox_local is only ever called right after declaring this local/param");
        let box_fqcn = crate::class_table::box_fqcn(&expr_ty_to_type(&ty));
        let class_index = self.cp.add_class(&box_fqcn);
        self.op_u16(Opcode::New, class_index, 1);
        self.op(Opcode::Dup, 1);
        self.op_u16(Opcode::Load, raw.index, 1);
        let field_ref = self.box_value_field_ref(&ty);
        self.op_u16(Opcode::SetField, field_ref, -2);
        let boxed_index = self.declare_ref_param(name.to_string(), ty);
        self.emit_store(boxed_index);
    }

    /// `T name = init;` for a local that some closure captures-and-mutates
    /// (vm.md § Variable capture and boxing) — same physical shape as a
    /// `ref` parameter (`LocalSlot::boxed`), except the box is allocated
    /// here at declaration instead of by a caller. `declared_ty` must come
    /// from an explicit type annotation: `nl_syntax::monomorphize`'s
    /// box-request collection can't syntactically infer a type for `auto`,
    /// so `stmt::compile_stmt` never calls this for an `auto` declaration
    /// (see `Emitter::boxed_captures`'s doc comment).
    pub(crate) fn compile_boxed_var_decl(
        &mut self,
        declared_ty: ExprTy,
        name: &str,
        init: &Expr,
    ) -> Result<(), CodegenError> {
        let box_fqcn = crate::class_table::box_fqcn(&expr_ty_to_type(&declared_ty));
        let class_index = self.cp.add_class(&box_fqcn);
        self.op_u16(Opcode::New, class_index, 1);
        self.op(Opcode::Dup, 1);
        let init_ty = self.compile_expr(init)?;
        self.coerce_value(&init_ty, &declared_ty, name)?;
        let field_ref = self.box_value_field_ref(&declared_ty);
        self.op_u16(Opcode::SetField, field_ref, -2);
        let index = self.declare_ref_param(name.to_string(), declared_ty);
        self.emit_store(index);
        Ok(())
    }

    /// `T name;` (no initializer) for a boxed capture — see
    /// `compile_boxed_var_decl`. `NEW` zero-initializes the box's `value`
    /// field like any other object field (vm.md § Object layout); nl-sema's
    /// definite-assignment check (E001) guarantees a real write reaches it
    /// before any read, so this placeholder is never observed.
    pub(crate) fn declare_boxed_var_uninit(&mut self, declared_ty: ExprTy, name: &str) {
        let box_fqcn = crate::class_table::box_fqcn(&expr_ty_to_type(&declared_ty));
        let class_index = self.cp.add_class(&box_fqcn);
        self.op_u16(Opcode::New, class_index, 1);
        let index = self.declare_ref_param(name.to_string(), declared_ty);
        self.emit_store(index);
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
        Err(CodegenError::Unsupported(format!(
            "undefined variable '{name}'"
        )))
    }

    /// `name` resolves either to an ordinary local (`self.scopes`, checked
    /// first so a shadowing declaration wins) or, inside a closure's
    /// `invoke` method, to a captured variable's field on `this`
    /// (`self.captured_fields`).
    pub(crate) fn resolve_ident(&self, name: &str) -> Result<IdentRef, CodegenError> {
        if let Ok(slot) = self.lookup_local(name) {
            return Ok(IdentRef::Local(slot));
        }
        if let Some(field) = self.captured_fields.get(name) {
            return Ok(IdentRef::CapturedField(field.clone()));
        }
        Err(CodegenError::Unsupported(format!(
            "undefined variable '{name}'"
        )))
    }

    /// The constant-pool field-ref for `this.name` — the closure's
    /// synthetic class always has a field of the same name as the capture
    /// (see `crate::closure`). When `boxed`, the field's *physical* type is
    /// `Box<ty>`, not `ty` (vm.md § Variable capture and boxing), matching
    /// what `compile_closure` actually declares/populates for a boxed
    /// capture.
    fn captured_field_ref(&mut self, name: &str, ty: &ExprTy, boxed: bool) -> u16 {
        let physical_ty = if boxed {
            ExprTy::Object(crate::class_table::box_fqcn(&expr_ty_to_type(ty)))
        } else {
            ty.clone()
        };
        let class_index = self.cp.add_class(&self.this_fqcn.clone());
        let name_index = self.cp.add_utf8(name.to_string());
        let type_index = self
            .cp
            .add_type_desc(&type_descriptor(&expr_ty_to_type(&physical_ty)));
        self.cp.add_field_ref(class_index, name_index, type_index)
    }

    /// Emits `this.name` (`GET_FIELD` off local 0) for a captured variable,
    /// unwrapping through `Box<ty>.value` when `boxed` (vm.md § Variable
    /// capture and boxing) — same shape as a boxed local's read (see the
    /// `Expr::Ident` arm of `compile_expr`).
    fn emit_get_captured_field(&mut self, name: &str, ty: &ExprTy, boxed: bool) {
        self.op_u16(Opcode::Load, 0, 1);
        let field_ref = self.captured_field_ref(name, ty, boxed);
        self.op_u16(Opcode::GetField, field_ref, 0);
        if boxed {
            let value_field_ref = self.box_value_field_ref(ty);
            self.op_u16(Opcode::GetField, value_field_ref, 0);
        }
    }

    /// Emits `this.name` for a captured variable *without* unwrapping
    /// `Box<ty>.value` when `boxed` — unlike `emit_get_captured_field`, this
    /// is for copying the raw source (box reference or plain value) into
    /// another closure's own field at its creation site
    /// (`CaptureSource::Recaptured` in `compile_closure`), not for reading
    /// the value in an expression position.
    fn emit_get_captured_field_raw(&mut self, name: &str, ty: &ExprTy, boxed: bool) {
        self.op_u16(Opcode::Load, 0, 1);
        let field_ref = self.captured_field_ref(name, ty, boxed);
        self.op_u16(Opcode::GetField, field_ref, 0);
    }

    /// `Box<T>.value`'s field-ref constant-pool index, for the box wrapping
    /// `inner_ty` — vm.md § Ref parameters (boxing). Shared by every
    /// read/write of a `ref` parameter (`LocalSlot::boxed`) and by the
    /// caller-side boxing/unboxing sequence around a `ref` call argument.
    fn box_value_field_ref(&mut self, inner_ty: &ExprTy) -> u16 {
        let box_fqcn = crate::class_table::box_fqcn(&expr_ty_to_type(inner_ty));
        let class_index = self.cp.add_class(&box_fqcn);
        let name_index = self.cp.add_utf8("value".to_string());
        let type_index = self
            .cp
            .add_type_desc(&type_descriptor(&expr_ty_to_type(inner_ty)));
        self.cp.add_field_ref(class_index, name_index, type_index)
    }

    pub(crate) fn resolve_class_name(&self, name: &str) -> String {
        self.imports
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
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
            return Err(CodegenError::Unsupported(format!(
                "expected bool condition, got {ty:?}"
            )));
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
                self.op(
                    if *v {
                        Opcode::ConstTrue
                    } else {
                        Opcode::ConstFalse
                    },
                    1,
                );
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
                    // vm.md § Ref parameters (boxing) — a `ref` parameter's
                    // slot holds the caller's `Box<T>`; every read unwraps
                    // it through `Box<T>.value`.
                    if let Some(inner_ty) = &slot.boxed {
                        let field_ref = self.box_value_field_ref(inner_ty);
                        self.op_u16(Opcode::GetField, field_ref, 0);
                    }
                    Ok(slot.ty)
                }
                IdentRef::CapturedField(field) => {
                    self.emit_get_captured_field(name, &field.ty, field.boxed);
                    Ok(field.ty)
                }
            },
            Expr::Assign(target, value) => self.compile_assign(target, value),
            Expr::Call(name, args) => self.compile_call(name, args),
            Expr::New(class_name, _type_args, args) => self.compile_new(class_name, args),
            Expr::NewArray(elem_ty, dims) => self.compile_new_array(elem_ty, dims),
            Expr::NewArrayInit(elem_ty, elements) => self.compile_new_array_init(elem_ty, elements),
            Expr::FieldAccess(target, name) => self.compile_field_access(target, name),
            Expr::MethodCall(target, name, args) => self.compile_method_call(target, name, args),
            Expr::Index(target, index) => self.compile_index(target, index),
            Expr::InstanceOf(target, type_name) => self.compile_instanceof(target, type_name),
            Expr::Cast(ty, inner) => self.compile_cast(ty, inner),
            Expr::PostIncr(name) => self.compile_incr(name, 1),
            Expr::PostDecr(name) => self.compile_incr(name, -1),
            Expr::Unary(op, inner) => self.compile_unary(*op, inner),
            Expr::Binary(op, lhs, rhs) => self.compile_binary(*op, lhs, rhs),
            Expr::Match(subject, arms) => self.compile_match(subject, arms),
            Expr::Ternary(cond, then_e, else_e) => self.compile_ternary(cond, then_e, else_e),
            Expr::Coalesce(lhs, rhs) => self.compile_coalesce(lhs, rhs),
            Expr::Elvis(lhs, rhs) => self.compile_elvis(lhs, rhs),
            Expr::Closure {
                params,
                return_type,
                throws,
                body,
            } => {
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
        let param_names: std::collections::HashSet<&str> =
            params.iter().map(|p| p.name.as_str()).collect();
        let mut candidates: Vec<String> =
            crate::closure::referenced_names(body).into_iter().collect();
        candidates.retain(|n| !param_names.contains(n.as_str()));
        candidates.sort();

        // A candidate is a real capture if it resolves either as a local in
        // *this* (enclosing) scope, or — when `self` is itself a closure's
        // `invoke` emitter — as one of *its own* captured fields (a closure
        // nested 2+ levels deep referencing a variable captured by an
        // intermediate closure, not its own params/locals). Anything else (a
        // class reference, or a name declared inside the closure body
        // itself) is left for the inner emitter to resolve normally.
        // `boxed` (already decided at the original declaration, by
        // `stmt::compile_stmt`'s `VarDecl` arms/`compile_method`'s parameter
        // loop consulting `Emitter::boxed_captures`, and carried unchanged
        // through every re-capture since `captured_fields` copies it as-is)
        // means the source already physically holds the shared `Box<T>`
        // reference, so simply copying it below (the creation-site copy
        // loop) already copies the box, not a snapshot of its contents.
        let captures: Vec<(String, ExprTy, CaptureSource, bool)> = candidates
            .into_iter()
            .filter_map(|name| {
                if let Ok(slot) = self.lookup_local(&name) {
                    return Some((name, slot.ty, CaptureSource::Local(slot.index), slot.boxed.is_some()));
                }
                self.captured_fields.get(&name).map(|field| {
                    (name.clone(), field.ty.clone(), CaptureSource::Recaptured, field.boxed)
                })
            })
            .collect();

        let synth_fqcn = format!(
            "{}$closure{}",
            self.closure_name_prefix, self.closure_counter
        );
        self.closure_counter += 1;

        let resolved_params: Vec<Type> = params
            .iter()
            .map(|p| resolve_type(&p.ty, self.imports))
            .collect();
        let param_expr_tys: Vec<ExprTy> = resolved_params.iter().map(expr_ty_of).collect();

        let mut synth_cp = ConstantPool::new();
        let synth_this_class = synth_cp.add_class(&synth_fqcn);
        let captured_fields: HashMap<String, CapturedField> = captures
            .iter()
            .map(|(n, ty, _, boxed)| {
                (
                    n.clone(),
                    CapturedField {
                        ty: ty.clone(),
                        boxed: *boxed,
                    },
                )
            })
            .collect();

        let deduced_return_ty;
        let invoke_method;
        let mut nested_closures;
        {
            let mut inner = Emitter::new(
                &mut synth_cp,
                self.static_sigs,
                self.classes,
                self.imports,
                synth_this_class,
                synth_fqcn.clone(),
            );
            inner.captured_fields = captured_fields;
            // A closure nested inside this one may itself capture-and-mutate
            // one of *this* closure's own locals/params — same analysis as
            // `compile_method`'s, scoped to this closure's own body.
            inner.boxed_captures = crate::closure::boxed_captures(body);
            inner.push_scope();
            inner.declare_local("this".to_string(), ExprTy::Object(synth_fqcn.clone()));
            for (param, resolved_ty) in params.iter().zip(&resolved_params) {
                inner.declare_local(param.name.clone(), expr_ty_of(resolved_ty));
            }
            // Box this closure's own parameters that a *nested* closure
            // captures-and-mutates — must run after every parameter has
            // claimed its ordinary positional slot (see `rebox_local`).
            for (param, resolved_ty) in params.iter().zip(&resolved_params) {
                if inner.boxed_captures.contains(&param.name) {
                    inner.rebox_local(&param.name, expr_ty_of(resolved_ty));
                }
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

            // The `invoke` method's *descriptor* must be built through the
            // same `ExprTy` round-trip every call site uses to build its
            // own expected descriptor (`compile_closure_invoke`,
            // `coerce_value`'s closure-shape branch, `expr_ty_of` on an
            // explicit `Type::Function`) — not straight from
            // `resolved_params`. The two disagree exactly when a param's
            // declared type is a union (e.g. `string|null`): `ExprTy` always
            // collapses it to its non-null member (vm.md, values are
            // dynamically tagged at runtime — nullability isn't part of the
            // physical descriptor), but `resolved_params` still carries the
            // full `Type::Union`, which `type_descriptor` renders as
            // `"string|null"`. Using that directly here would give this
            // literal's own `invoke` a descriptor no caller ever builds,
            // failing `Module::find_method_by_descriptor` at dispatch time.
            let descriptor_params: Vec<Type> = param_expr_tys.iter().map(expr_ty_to_type).collect();
            let descriptor =
                method_descriptor(&descriptor_params, &expr_ty_to_type(&deduced_return_ty));
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
                line_table: inner.line_table,
            };
            nested_closures = inner.closures;
        }

        // A boxed capture's field physically holds `Box<ty>`, not `ty` (vm.md
        // § Variable capture and boxing) — must match what the creation-site
        // copy loop below and every in-body read/write
        // (`emit_get_captured_field`/`compile_assign`/`compile_incr`)
        // already assume.
        let fields: Vec<nl_bytecode::FieldDescriptor> = captures
            .iter()
            .map(|(name, ty, _, boxed)| {
                let physical_ty = if *boxed {
                    ExprTy::Object(crate::class_table::box_fqcn(&expr_ty_to_type(ty)))
                } else {
                    ty.clone()
                };
                let name_index = synth_cp.add_utf8(name.clone());
                let type_index =
                    synth_cp.add_type_desc(&type_descriptor(&expr_ty_to_type(&physical_ty)));
                nl_bytecode::FieldDescriptor {
                    flags: nl_bytecode::field_flags::PUBLIC,
                    name_index,
                    type_index,
                }
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
        // into the new object's field of the same name. For a boxed capture
        // (vm.md § Variable capture and boxing), the source already
        // physically holds the shared `Box<ty>` reference (see
        // `LocalSlot::boxed`/`CapturedField::boxed`), so this copies the box
        // itself, not a snapshot of its contents — the closure and its
        // source end up referencing the exact same box. `CaptureSource::
        // Recaptured` re-reads `this.name` (this closure's own captured
        // field, set up the same way when *this* closure was itself
        // created) instead of a local slot — the 2+ levels of nesting case.
        let class_index = self.cp.add_class(&synth_fqcn);
        self.op_u16(Opcode::New, class_index, 1);
        for (name, ty, source, boxed) in &captures {
            self.op(Opcode::Dup, 1);
            match source {
                CaptureSource::Local(outer_index) => {
                    self.op_u16(Opcode::Load, *outer_index, 1);
                }
                CaptureSource::Recaptured => {
                    self.emit_get_captured_field_raw(name, ty, *boxed);
                }
            }
            let field_ref = self.captured_field_ref(name, ty, *boxed);
            self.op_u16(Opcode::SetField, field_ref, -2);
        }

        Ok(ExprTy::Closure {
            params: param_expr_tys,
            return_ty: Box::new(deduced_return_ty),
            fqcn: synth_fqcn,
        })
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
        self.compile_call_args(args, params, &vec![false; args.len()], "closure call")?;
        let class_index = self.cp.add_class(fqcn);
        let name_index = self.cp.add_utf8("invoke".to_string());
        let param_types: Vec<Type> = params.iter().map(expr_ty_to_type).collect();
        let descriptor = method_descriptor(&param_types, &expr_ty_to_type(return_ty));
        let descriptor_index = self.cp.add_type_desc(&descriptor);
        let method_ref = self
            .cp
            .add_method_ref(class_index, name_index, descriptor_index);
        let result_delta = if *return_ty == ExprTy::Void { 0 } else { 1 };
        self.op_u16(
            Opcode::InvokeClosure,
            method_ref,
            result_delta - args.len() as i32 - 1,
        );
        Ok(return_ty.clone())
    }

    /// `cond ? then : else` — a conditional branch, mirroring
    /// `compile_short_circuit`'s pattern of tracking stack depth linearly
    /// through both (mutually exclusive at runtime) branches.
    fn compile_ternary(
        &mut self,
        cond: &Expr,
        then_e: &Expr,
        else_e: &Expr,
    ) -> Result<ExprTy, CodegenError> {
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

    /// `a ?? b` — specs.md § Nullish coalescing operator; vm.md § Nullish
    /// coalescing and elvis operators (`DUP`+`IS_NONNULL`+`IF_TRUE`, `POP`
    /// otherwise then evaluate `b`). `IsNonNull` already consumes the `DUP`d
    /// copy and leaves the original value on the stack for the non-null
    /// path, exactly matching that pseudocode. Result type is the left
    /// operand's `ExprTy` (unions already collapse to their non-null
    /// member's `ExprTy` — see `expr_ty_of`); lenient about the right
    /// operand's type like `compile_ternary`, via the same `coerce_value`.
    fn compile_coalesce(&mut self, lhs: &Expr, rhs: &Expr) -> Result<ExprTy, CodegenError> {
        let lhs_ty = self.compile_expr(lhs)?;
        self.op(Opcode::Dup, 1);
        self.op(Opcode::IsNonNull, 0);
        let (skip_pc, skip_operand) = self.branch(Opcode::IfTrue, -1);
        self.op(Opcode::Pop, -1);
        let rhs_ty = self.compile_expr(rhs)?;
        self.coerce_value(&rhs_ty, &lhs_ty, "?? right operand")?;
        self.patch_branch(skip_pc, skip_operand);
        Ok(lhs_ty)
    }

    /// `a ?: b` — specs.md § Elvis operator; vm.md § Nullish coalescing and
    /// elvis operators. Same shape as `compile_coalesce`, but the "use `b`"
    /// branch also triggers on `false`/`0`, not just `null`. vm.md notes the
    /// compiler may simplify the falsy check based on `a`'s static type —
    /// done here via `lhs_ty` (only `null` applies to `StringT`/`Object`/
    /// `Array`/`Closure`, since a value of one of those static types can
    /// never actually be `false` or a numeric zero at runtime).
    fn compile_elvis(&mut self, lhs: &Expr, rhs: &Expr) -> Result<ExprTy, CodegenError> {
        let lhs_ty = self.compile_expr(lhs)?;
        let mut falsy_branches = Vec::new();

        self.op(Opcode::Dup, 1);
        self.op(Opcode::IsNull, 0);
        falsy_branches.push(self.branch(Opcode::IfTrue, -1));

        match lhs_ty {
            ExprTy::Bool => {
                self.op(Opcode::Dup, 1);
                self.op(Opcode::ConstFalse, 1);
                self.op(Opcode::CmpEq, -1);
                falsy_branches.push(self.branch(Opcode::IfTrue, -1));
            }
            ExprTy::Int => {
                self.op(Opcode::Dup, 1);
                self.op(Opcode::ConstIZero, 1);
                self.op(Opcode::CmpEq, -1);
                falsy_branches.push(self.branch(Opcode::IfTrue, -1));
            }
            ExprTy::Float => {
                self.op(Opcode::Dup, 1);
                self.op(Opcode::ConstFZero, 1);
                self.op(Opcode::CmpEq, -1);
                falsy_branches.push(self.branch(Opcode::IfTrue, -1));
            }
            ExprTy::Byte => {
                self.op(Opcode::Dup, 1);
                self.op(Opcode::B2I, 0);
                self.op(Opcode::ConstIZero, 1);
                self.op(Opcode::CmpEq, -1);
                falsy_branches.push(self.branch(Opcode::IfTrue, -1));
            }
            _ => {}
        }

        let (skip_pc, skip_operand) = self.branch(Opcode::Goto, 0);
        for (pc, operand) in falsy_branches {
            self.patch_branch(pc, operand);
        }
        self.op(Opcode::Pop, -1);
        let rhs_ty = self.compile_expr(rhs)?;
        self.coerce_value(&rhs_ty, &lhs_ty, "?: right operand")?;
        self.patch_branch(skip_pc, skip_operand);
        Ok(lhs_ty)
    }

    /// `match(subject) { pattern: value, ... }` — vm.md § Match expressions:
    /// a chain of `DUP`+compare+branch, one per non-`default` arm. Sema
    /// (E047) guarantees exhaustiveness, so a missing `default` arm can only
    /// happen for an exhaustively-covered `bool` subject — in that case the
    /// last arm doubles as the fallback (no comparison emitted for it).
    fn compile_match(
        &mut self,
        subject: &Expr,
        arms: &[nl_syntax::ast::MatchArm],
    ) -> Result<ExprTy, CodegenError> {
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
                let pattern = arm
                    .pattern
                    .as_ref()
                    .expect("non-fallback arm always has a pattern");
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
                    // Compound assignment operator overloading — specs.md §
                    // Overloadable operators: `+=`/`-=`/`*=`/`/=`/`%=`
                    // desugar at parse time to `Assign(Local(name),
                    // Binary(op, Ident(name), rhs))` (see
                    // `nl_syntax::parser::parse_assignment`); when `name`'s
                    // declared type is a user class defining the matching
                    // `operator<op>=`, dispatch to it directly (mutates
                    // `this` in place and returns `Self`) instead of
                    // falling into the generic path below, which would
                    // otherwise compile the same `Binary` and land on
                    // `operator<op>` (create-new) via `compile_binary`
                    // instead — mirrors `nl_sema::checker::check_assign`'s
                    // identical preference. Skipped for a boxed (`ref`
                    // parameter) slot — not exercised, falls through to the
                    // generic path below like any other unsupported shape.
                    if slot.boxed.is_none() {
                        if let Expr::Binary(op, inner, rhs) = value {
                            if matches!(&**inner, Expr::Ident(inner_name) if inner_name == name) {
                                if let ExprTy::Object(fqcn) = &slot.ty {
                                    if let Some(method_name) = compound_operator_method_name(*op) {
                                        if let Some(rhs_ty) = self.peek_type(rhs) {
                                            let rhs_ast_ty = expr_ty_to_type(&rhs_ty);
                                            if let Some(method) = find_operator_method(
                                                self.classes,
                                                fqcn,
                                                method_name,
                                                std::slice::from_ref(&rhs_ast_ty),
                                            ) {
                                                let return_ty = method.return_ty.clone();
                                                let fqcn = fqcn.clone();
                                                self.op_u16(Opcode::Load, slot.index, 1);
                                                self.compile_expr(rhs)?;
                                                let result_ty = self.emit_operator_call(
                                                    &fqcn,
                                                    method_name,
                                                    std::slice::from_ref(&rhs_ast_ty),
                                                    &return_ty,
                                                );
                                                self.op(Opcode::Dup, 1);
                                                self.op_u16(Opcode::Store, slot.index, -1);
                                                return Ok(result_ty);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    let value_ty = self.compile_expr(value)?;
                    // vm.md § Ref parameters (boxing) — writing to a `ref`
                    // parameter writes through `Box<T>.value`, not the
                    // slot directly (which holds the box reference itself).
                    if let Some(inner_ty) = slot.boxed.clone() {
                        self.coerce_value(&value_ty, &inner_ty, name)?;
                        self.op(Opcode::Dup, 1);
                        let tmp = self.declare_scratch_local(inner_ty.clone());
                        self.emit_store(tmp);
                        self.op_u16(Opcode::Load, slot.index, 1);
                        self.op(Opcode::Swap, 0);
                        let field_ref = self.box_value_field_ref(&inner_ty);
                        self.op_u16(Opcode::SetField, field_ref, -2);
                        self.op_u16(Opcode::Load, tmp, 1);
                        return Ok(inner_ty);
                    }
                    self.coerce_value(&value_ty, &slot.ty, name)?;
                    // Leave a copy as the expression's own value (assignment
                    // is an expression, e.g. usable as `a = b = 1;`).
                    self.op(Opcode::Dup, 1);
                    self.op_u16(Opcode::Store, slot.index, -1);
                    Ok(slot.ty)
                }
                IdentRef::CapturedField(field) => {
                    let field_ty = field.ty;
                    let value_ty = self.compile_expr(value)?;
                    self.coerce_value(&value_ty, &field_ty, name)?;
                    self.op(Opcode::Dup, 1);
                    let tmp = self.declare_scratch_local(field_ty.clone());
                    self.emit_store(tmp);
                    self.op_u16(Opcode::Load, 0, 1);
                    if field.boxed {
                        // vm.md § Variable capture and boxing — `this.name`
                        // is a `Box<field_ty>` shared with the enclosing
                        // scope; write through `.value`, not the field
                        // itself (mirrors the boxed-local case above).
                        let box_field_ref = self.captured_field_ref(name, &field_ty, true);
                        self.op_u16(Opcode::GetField, box_field_ref, 0);
                        self.op(Opcode::Swap, 0);
                        let value_field_ref = self.box_value_field_ref(&field_ty);
                        self.op_u16(Opcode::SetField, value_field_ref, -2);
                    } else {
                        self.op(Opcode::Swap, 0);
                        let field_ref = self.captured_field_ref(name, &field_ty, false);
                        self.op_u16(Opcode::SetField, field_ref, -2);
                    }
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
                    return Err(CodegenError::Unsupported(
                        "array index must be int".to_string(),
                    ));
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
            .ok_or_else(|| {
                CodegenError::Unsupported(format!(
                    "'super' used in class '{}' with no superclass",
                    self.this_fqcn
                ))
            })
    }

    fn compile_incr(&mut self, name: &str, delta: i16) -> Result<ExprTy, CodegenError> {
        let slot = match self.resolve_ident(name)? {
            IdentRef::Local(slot) => slot,
            // `IINC` operates on a local-variable slot by index; a captured
            // variable is a field on `this` instead. Only reachable here
            // when it's boxed: `nl_codegen::closure::boxed_captures` always
            // classifies a `++`/`--` target inside a closure as a mutation,
            // so `stmt::compile_stmt`'s `VarDecl` arms/`compile_method`'s
            // parameter loop will already have boxed the enclosing
            // declaration by the time this closure was compiled — an
            // unboxed captured field reaching this point would mean that
            // analysis missed something, so this fails loudly rather than
            // silently mutating a copy nobody else observes.
            IdentRef::CapturedField(field) if field.boxed => {
                if field.ty != ExprTy::Int {
                    return Err(CodegenError::Unsupported(format!(
                        "'++'/'--' only supported on int, found {:?}",
                        field.ty
                    )));
                }
                self.op_u16(Opcode::Load, 0, 1);
                let box_field_ref = self.captured_field_ref(name, &field.ty, true);
                self.op_u16(Opcode::GetField, box_field_ref, 0);
                self.op(Opcode::Dup, 1);
                let value_field_ref = self.box_value_field_ref(&field.ty);
                self.op_u16(Opcode::GetField, value_field_ref, 0);
                self.emit_int_const(delta as i64);
                self.op(Opcode::IAdd, -1);
                self.op_u16(Opcode::SetField, value_field_ref, -2);
                return Ok(ExprTy::Void);
            }
            IdentRef::CapturedField(_) => {
                return Err(CodegenError::Unsupported(format!(
                    "'++'/'--' on captured closure variable '{name}' is not supported (not boxed)"
                )))
            }
        };
        // Overloaded `++`/`--` — specs.md § Overloadable operators: the
        // prefix and postfix forms invoke the *same* `operator++`/
        // `operator--` method (no separate prefix form exists in this
        // grammar — only postfix, `Expr::PostIncr`/`PostDecr`), which
        // mutates `this` and returns `Self`. Matches vm.md § Object
        // operations, "Overloaded `++`/`--`": "the compiler emits
        // `INVOKE_INSTANCE`". Like the plain-`int` case just below, this
        // returns `ExprTy::Void` (no expression value) rather than the
        // spec's "postfix evaluates to the mutated reference" — consistent
        // with this codebase's existing postfix support, which is
        // statement-only in the same way (no `LOAD` before the mutation to
        // preserve an original value either). Skipped for a boxed (`ref`
        // parameter) slot, like the plain-`int` case — not exercised.
        if let ExprTy::Object(fqcn) = &slot.ty {
            let method_name = if delta > 0 { "operator++" } else { "operator--" };
            if slot.boxed.is_none() {
                if let Some(method) = find_operator_method(self.classes, fqcn, method_name, &[]) {
                    let return_ty = method.return_ty.clone();
                    let fqcn = fqcn.clone();
                    self.op_u16(Opcode::Load, slot.index, 1);
                    let _ = self.emit_operator_call(&fqcn, method_name, &[], &return_ty);
                    self.op_u16(Opcode::Store, slot.index, -1);
                    return Ok(ExprTy::Void);
                }
            }
            return Err(CodegenError::Unsupported(format!(
                "'++'/'--' on '{fqcn}' requires an '{method_name}' overload"
            )));
        }
        if slot.ty != ExprTy::Int {
            return Err(CodegenError::Unsupported(format!(
                "'++'/'--' only supported on int, found {:?}",
                slot.ty
            )));
        }
        // vm.md § Ref parameters (boxing) — `IINC` operates on a plain local
        // slot; a `ref` parameter's slot holds a `Box<int>` reference
        // instead, so this desugars to an explicit read/add/write through
        // `Box<int>.value`.
        if let Some(inner_ty) = slot.boxed.clone() {
            self.op_u16(Opcode::Load, slot.index, 1);
            self.op(Opcode::Dup, 1);
            let field_ref = self.box_value_field_ref(&inner_ty);
            self.op_u16(Opcode::GetField, field_ref, 0);
            self.emit_int_const(delta as i64);
            self.op(Opcode::IAdd, -1);
            self.op_u16(Opcode::SetField, field_ref, -2);
            return Ok(ExprTy::Void);
        }
        self.op_iinc(slot.index, delta);
        Ok(ExprTy::Void)
    }

    fn compile_call(&mut self, name: &str, args: &[Arg]) -> Result<ExprTy, CodegenError> {
        // `add(5, 3)` where `add` is a closure-typed local/capture, not a
        // same-class static method — vm.md § Closures: "the compiler
        // determines the closure's type signature at compile time".
        match self.resolve_ident(name) {
            Ok(IdentRef::Local(slot)) => {
                if let ExprTy::Closure {
                    params,
                    return_ty,
                    fqcn,
                } = slot.ty
                {
                    self.op_u16(Opcode::Load, slot.index, 1);
                    let positional = require_positional_args(args)?;
                    return self.compile_closure_invoke(&params, &return_ty, &fqcn, &positional);
                }
            }
            Ok(IdentRef::CapturedField(field)) => {
                if let ExprTy::Closure {
                    params,
                    return_ty,
                    fqcn,
                } = field.ty.clone()
                {
                    self.emit_get_captured_field(name, &field.ty, field.boxed);
                    let positional = require_positional_args(args)?;
                    return self.compile_closure_invoke(&params, &return_ty, &fqcn, &positional);
                }
            }
            Err(_) => {}
        }
        let sig =
            self.static_sigs.get(name).cloned().ok_or_else(|| {
                CodegenError::Unsupported(format!("call to unknown method '{name}'"))
            })?;
        let positional =
            crate::class_table::resolve_positional_args(&sig.param_names, &sig.defaults, args);
        let boxes = self.compile_call_args(&positional, &sig.param_types, &sig.is_ref, name)?;
        let result_delta = if sig.return_ty == ExprTy::Void { 0 } else { 1 };
        self.op_u16(
            Opcode::InvokeStatic,
            sig.method_ref_index,
            result_delta - positional.len() as i32,
        );
        self.emit_unbox_ref_args(&boxes);
        Ok(sig.return_ty)
    }

    fn compile_new(&mut self, class_name: &str, args: &[Arg]) -> Result<ExprTy, CodegenError> {
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
        // Native constructors have no `Param` metadata (no `.nl` source),
        // so they don't support named/optional arguments — `args` must be
        // fully positional there.
        let (params, is_ref, positional): (Vec<Type>, Vec<bool>, Vec<Expr>) = if let Some(
            param_types,
        ) =
            crate::native_generics::ctor_param_types(&fqcn, args.len())
        {
            let n = param_types.len();
            (param_types, vec![false; n], require_positional_args(args)?)
        } else if let Some(param_types) = crate::stdlib::ctor_param_types(&fqcn, args.len()) {
            // `new system.Random()`/`new system.Random(int seed)` — the
            // other native instance class besides FileHandle, but
            // constructible directly (see
            // `crate::stdlib::ctor_param_types`'s doc comment).
            let n = param_types.len();
            (param_types, vec![false; n], require_positional_args(args)?)
        } else {
            let ctor = find_ctor(self.classes, &fqcn, args.len())
                .cloned()
                .ok_or_else(|| {
                    CodegenError::Unsupported(format!(
                        "no constructor of '{fqcn}' with {} argument(s)",
                        args.len()
                    ))
                })?;
            let positional = crate::class_table::resolve_positional_args(
                &ctor.param_names,
                &ctor.defaults,
                args,
            );
            (ctor.params, ctor.is_ref, positional)
        };
        let param_tys: Vec<ExprTy> = params.iter().map(expr_ty_of).collect();
        let boxes = self.compile_call_args(&positional, &param_tys, &is_ref, &fqcn)?;

        let cc_params = crate::class_table::calling_convention_params(&params, &is_ref);
        let descriptor = method_descriptor(&cc_params, &Type::Void);
        let name_index = self.cp.add_utf8("<construct>");
        let descriptor_index = self.cp.add_type_desc(&descriptor);
        let method_ref = self
            .cp
            .add_method_ref(class_index, name_index, descriptor_index);
        self.op_u16(
            Opcode::InvokeSpecial,
            method_ref,
            -(1 + positional.len() as i32),
        );
        self.emit_unbox_ref_args(&boxes);
        Ok(ExprTy::Object(fqcn))
    }

    /// `Foo.method(...)` where `fqcn` is a user (non-`system.*`) class
    /// resolved from a dotted receiver path — see `compile_method_call`'s
    /// matching comment. Unlike the unqualified `add(5, 3)` form
    /// (`compile_call`, which only ever targets the *current* class's own
    /// `static_sigs`), this reaches a static method declared on *any*
    /// class in the program, so the method ref is built fresh here instead
    /// of looked up from a precomputed table.
    fn compile_static_user_call(
        &mut self,
        fqcn: &str,
        name: &str,
        args: &[Arg],
        method: MethodInfo,
    ) -> Result<ExprTy, CodegenError> {
        let positional = crate::class_table::resolve_positional_args(
            &method.param_names,
            &method.defaults,
            args,
        );
        let param_tys: Vec<ExprTy> = method.params.iter().map(expr_ty_of).collect();
        let boxes = self.compile_call_args(&positional, &param_tys, &method.is_ref, name)?;

        let cc_params =
            crate::class_table::calling_convention_params(&method.params, &method.is_ref);
        let descriptor = method_descriptor(&cc_params, &method.return_ty);
        let name_index = self.cp.add_utf8(name.to_string());
        let descriptor_index = self.cp.add_type_desc(&descriptor);
        let class_index = self.cp.add_class(fqcn);
        let method_ref = self
            .cp
            .add_method_ref(class_index, name_index, descriptor_index);
        let return_ty = expr_ty_of(&method.return_ty);
        let result_delta = if return_ty == ExprTy::Void { 0 } else { 1 };
        self.op_u16(
            Opcode::InvokeStatic,
            method_ref,
            result_delta - positional.len() as i32,
        );
        self.emit_unbox_ref_args(&boxes);
        Ok(return_ty)
    }

    fn compile_super_method_call(
        &mut self,
        name: &str,
        args: &[Arg],
    ) -> Result<ExprTy, CodegenError> {
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
        let positional = crate::class_table::resolve_positional_args(
            &method.param_names,
            &method.defaults,
            args,
        );
        let param_tys: Vec<ExprTy> = method.params.iter().map(expr_ty_of).collect();
        let boxes = self.compile_call_args(&positional, &param_tys, &method.is_ref, name)?;

        let cc_params =
            crate::class_table::calling_convention_params(&method.params, &method.is_ref);
        let descriptor = method_descriptor(&cc_params, &method.return_ty);
        let name_index = self.cp.add_utf8(name.to_string());
        let descriptor_index = self.cp.add_type_desc(&descriptor);
        let class_index = self.cp.add_class(&super_fqcn);
        let method_ref = self
            .cp
            .add_method_ref(class_index, name_index, descriptor_index);
        let return_ty = expr_ty_of(&method.return_ty);
        let result_delta = if return_ty == ExprTy::Void { 0 } else { 1 };
        self.op_u16(
            Opcode::InvokeSpecial,
            method_ref,
            result_delta - positional.len() as i32 - 1,
        );
        self.emit_unbox_ref_args(&boxes);
        Ok(return_ty)
    }

    /// `super(args);` constructor delegation — like `this(...)` but invokes
    /// the direct superclass's constructor instead of an overload in the
    /// same class.
    pub(crate) fn compile_super_call(&mut self, args: &[Arg]) -> Result<(), CodegenError> {
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
        let positional =
            crate::class_table::resolve_positional_args(&ctor.param_names, &ctor.defaults, args);
        let param_tys: Vec<ExprTy> = ctor.params.iter().map(expr_ty_of).collect();
        let boxes = self.compile_call_args(&positional, &param_tys, &ctor.is_ref, "super(...)")?;

        let cc_params = crate::class_table::calling_convention_params(&ctor.params, &ctor.is_ref);
        let descriptor = method_descriptor(&cc_params, &Type::Void);
        let name_index = self.cp.add_utf8("<construct>");
        let descriptor_index = self.cp.add_type_desc(&descriptor);
        let class_index = self.cp.add_class(&super_fqcn);
        let method_ref = self
            .cp
            .add_method_ref(class_index, name_index, descriptor_index);
        self.op_u16(
            Opcode::InvokeSpecial,
            method_ref,
            -(1 + positional.len() as i32),
        );
        self.emit_unbox_ref_args(&boxes);
        Ok(())
    }

    pub(crate) fn compile_this_call(&mut self, args: &[Arg]) -> Result<(), CodegenError> {
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
        let positional =
            crate::class_table::resolve_positional_args(&ctor.param_names, &ctor.defaults, args);
        let param_tys: Vec<ExprTy> = ctor.params.iter().map(expr_ty_of).collect();
        let boxes = self.compile_call_args(&positional, &param_tys, &ctor.is_ref, "this(...)")?;

        let cc_params = crate::class_table::calling_convention_params(&ctor.params, &ctor.is_ref);
        let descriptor = method_descriptor(&cc_params, &Type::Void);
        let name_index = self.cp.add_utf8("<construct>");
        let descriptor_index = self.cp.add_type_desc(&descriptor);
        let method_ref = self
            .cp
            .add_method_ref(self.this_class, name_index, descriptor_index);
        self.op_u16(
            Opcode::InvokeSpecial,
            method_ref,
            -(1 + positional.len() as i32),
        );
        self.emit_unbox_ref_args(&boxes);
        Ok(())
    }

    /// `new T[n1][n2]...` — compiler.md § Multidimensional array creation.
    /// `m` (the count of leading provided sizes) determines how many array
    /// layers are actually allocated; nl-sema (E038) already guarantees the
    /// rest are a contiguous omitted suffix. `m == 0` means nothing is ever
    /// allocated — the whole expression is `null`.
    fn compile_new_array(
        &mut self,
        elem_ty: &Type,
        dims: &[Option<Expr>],
    ) -> Result<ExprTy, CodegenError> {
        let resolved_elem = resolve_type(elem_ty, self.imports);
        let k = dims.len();
        let m = dims.iter().take_while(|d| d.is_some()).count();
        let result_ty = expr_ty_of(&plain_array_of(&resolved_elem, k));
        if m == 0 {
            self.op(Opcode::ConstNull, 1);
            return Ok(result_ty);
        }
        self.emit_new_array_level(0, m, k, &resolved_elem, dims)?;
        Ok(result_ty)
    }

    /// Allocates dimension `level` (0-indexed, always `< m`) of a
    /// multidimensional `new T[...]`, leaving the array reference on the
    /// stack. Levels below `m - 1` recursively populate every element with
    /// the next nested array (desugaring steps 2/3); level `m - 1` just
    /// lets `NEW_ARRAY` default-initialize its elements (`null` for
    /// reference-typed elements — which covers every omitted deeper level,
    /// vm.md § `NEW_ARRAY`), matching E031's "non-nullable element has no
    /// default" rule only ever applying when every dimension is provided.
    fn emit_new_array_level(
        &mut self,
        level: usize,
        m: usize,
        k: usize,
        resolved_elem: &Type,
        dims: &[Option<Expr>],
    ) -> Result<(), CodegenError> {
        let size_expr = dims[level]
            .as_ref()
            .expect("level < m always has a provided size");
        let size_ty = self.compile_expr(size_expr)?;
        if size_ty != ExprTy::Int {
            return Err(CodegenError::Unsupported(format!(
                "array size must be int, found {size_ty:?}"
            )));
        }
        let size_local = self.declare_scratch_local(ExprTy::Int);
        self.emit_store(size_local);

        let elem_at_level = plain_array_of(resolved_elem, k - level - 1);
        let type_index = self.cp.add_type_desc(&type_descriptor(&elem_at_level));
        self.op_u16(Opcode::Load, size_local, 1);
        self.op_u16(Opcode::NewArray, type_index, 0);

        if level + 1 >= m {
            return Ok(());
        }

        let arr_ty = ExprTy::Array(Box::new(expr_ty_of(&elem_at_level)));
        let arr_local = self.declare_scratch_local(arr_ty);
        self.emit_store(arr_local);
        let idx_local = self.declare_scratch_local(ExprTy::Int);
        self.emit_int_const(0);
        self.emit_store(idx_local);

        let cond_pc = self.code.len();
        self.op_u16(Opcode::Load, idx_local, 1);
        self.op_u16(Opcode::Load, size_local, 1);
        self.op(Opcode::CmpLt, -1);
        let exit_patch = self.branch(Opcode::IfFalse, -1);

        self.op_u16(Opcode::Load, arr_local, 1);
        self.op_u16(Opcode::Load, idx_local, 1);
        self.emit_new_array_level(level + 1, m, k, resolved_elem, dims)?;
        self.op(Opcode::ArrayStore, -3);

        self.op_iinc(idx_local, 1);
        self.emit_goto_to(cond_pc);
        let end_pc = self.code.len();
        self.patch_branch_to(exit_patch.0, exit_patch.1, end_pc);

        self.op_u16(Opcode::Load, arr_local, 1);
        Ok(())
    }

    fn compile_new_array_init(
        &mut self,
        elem_ty: &Type,
        elements: &[Expr],
    ) -> Result<ExprTy, CodegenError> {
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
                // `Status.OK` — a user-declared enum's case constant. Case
                // values are always compile-time constants (the field's own
                // `init` expression — see `nl_syntax::parser::
                // parse_enum_decl`), so this re-compiles that expression at
                // the reference site instead of reading real static storage
                // (there is no `GET_STATIC`/class-static-storage mechanism
                // in this implementation — see `crate::class_table::
                // FieldInfo::init`'s doc comment). The expression's own
                // static type is the enum itself, not its backing type
                // (matches `nl_sema::checker`'s `Expr::FieldAccess` arm).
                let fqcn = self.resolve_class_name(leading);
                if let Some(info) = self.classes.get(&fqcn) {
                    if info.is_enum {
                        if let Some(field) = info.fields.iter().find(|f| &f.name == name) {
                            let init = field.init.clone().expect(
                                "enum case fields always carry an init expression",
                            );
                            self.compile_expr(&init)?;
                            return Ok(ExprTy::Object(fqcn));
                        }
                    }
                }
            }
        }
        let target_ty = self.compile_expr(target)?;
        // `status.value` — specs.md § Typed enums. The case constant *is*
        // the backing value at runtime (no wrapper object — vm.md § Enum
        // representation), so this is identity: no `GET_FIELD` to emit,
        // just leave whatever `target` already pushed on the stack. The
        // backing type comes off the enum's first field, same convention as
        // `nl_sema::checker`'s matching special case.
        if name == "value" {
            if let ExprTy::Object(fqcn) = &target_ty {
                if let Some(info) = self.classes.get(fqcn) {
                    if info.is_enum {
                        let backing = info
                            .fields
                            .first()
                            .map(|f| expr_ty_of(&f.ty))
                            .unwrap_or(ExprTy::Int);
                        return Ok(backing);
                    }
                }
            }
        }
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

    fn compile_method_call(
        &mut self,
        target: &Expr,
        name: &str,
        args: &[Arg],
    ) -> Result<ExprTy, CodegenError> {
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
                let positional = require_positional_args(args)?;
                return self.compile_stdlib_call(&path, name, &positional);
            }
            // `Utils.max(a, b)` — a dotted path resolving to a *user* class
            // (as opposed to the `system.*` case above), not a value: same
            // recognize-before-falling-into-`compile_expr` shape, but
            // resolved via `resolve_class_name`/`self.classes` instead of
            // the stdlib table (specs.md's `Utils.swap(ref x, ref y)` etc.).
            if self.lookup_local(leading).is_err() && self.captured_fields.get(leading).is_none() {
                let fqcn = self.resolve_class_name(&path);
                if let Some(method) = find_method(self.classes, &fqcn, name, args.len()) {
                    if method.is_static {
                        return self.compile_static_user_call(&fqcn, name, args, method.clone());
                    }
                }
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
            // Native array built-ins have no `Param` metadata, so (like
            // every other native call below) they don't support
            // named/optional arguments.
            ExprTy::Array(elem) => {
                let elem_ty = (**elem).clone();
                let positional = require_positional_args(args)?;
                self.compile_array_method_call(elem_ty, name, &positional)
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
                    crate::stdlib::signature("system.String", name, full_argc).ok_or_else(
                        || {
                            CodegenError::Unsupported(format!(
                                "unknown method '{name}' on string with {} argument(s)",
                                args.len()
                            ))
                        },
                    )?;
                let extra_param_tys: Vec<ExprTy> =
                    param_types[1..].iter().map(expr_ty_of).collect();
                let positional = require_positional_args(args)?;
                let n = positional.len();
                self.compile_call_args(&positional, &extra_param_tys, &vec![false; n], name)?;
                self.emit_native_static("system.String", name, &param_types, &return_ty)
            }
            ExprTy::Object(fqcn) => {
                let fqcn = fqcn.clone();
                // `list.size()`/`map.get(k)` etc. — see `compile_new`'s
                // matching comment and `crate::native_generics`'s doc
                // comment; `handle.read(...)` etc. likewise resolve from
                // `crate::stdlib::instance_signature` (`system.io.FileHandle`
                // has no bytecode `Module` either). Falls through to the
                // ordinary user-class path below for everything else. Only
                // that last path has `Param` metadata (a real `.nl`
                // declaration), so it's the only one that resolves
                // named/optional arguments rather than requiring positional.
                let (params, return_ty, is_ref, positional) = if let Some((p, r)) =
                    crate::stdlib::instance_signature(&fqcn, name, args.len())
                {
                    let n = p.len();
                    (p, r, vec![false; n], require_positional_args(args)?)
                } else if let Some((p, r)) =
                    crate::native_generics::method_signature(&fqcn, name, args.len())
                {
                    let n = p.len();
                    (p, r, vec![false; n], require_positional_args(args)?)
                } else {
                    let method = find_method(self.classes, &fqcn, name, args.len())
                        .cloned()
                        .ok_or_else(|| {
                            CodegenError::Unsupported(format!(
                                "unknown method '{name}' on '{fqcn}' with {} argument(s)",
                                args.len()
                            ))
                        })?;
                    let positional = crate::class_table::resolve_positional_args(
                        &method.param_names,
                        &method.defaults,
                        args,
                    );
                    (method.params, method.return_ty, method.is_ref, positional)
                };
                let param_tys: Vec<ExprTy> = params.iter().map(expr_ty_of).collect();
                let boxes = self.compile_call_args(&positional, &param_tys, &is_ref, name)?;

                let cc_params = crate::class_table::calling_convention_params(&params, &is_ref);
                let descriptor = method_descriptor(&cc_params, &return_ty);
                let name_index = self.cp.add_utf8(name.to_string());
                let descriptor_index = self.cp.add_type_desc(&descriptor);
                // The static type's class is enough here: the VM re-resolves
                // the receiver's *runtime* class for INVOKE_INSTANCE, so this
                // also works when `fqcn` is an interface with no bytecode of
                // its own (interface dispatch — vm.md § Interface dispatch).
                let class_index = self.cp.add_class(&fqcn);
                let method_ref = self
                    .cp
                    .add_method_ref(class_index, name_index, descriptor_index);
                let return_expr_ty = expr_ty_of(&return_ty);
                let result_delta = if return_expr_ty == ExprTy::Void { 0 } else { 1 };
                // specs.md § Enums, "Custom methods and properties": an
                // enum's instance methods run on a receiver that is a raw
                // primitive (int/string — vm.md § Enum representation), not
                // a heap object with a vtable, so `INVOKE_INSTANCE`'s
                // virtual dispatch (which requires `Value::Object` — see
                // `nl_vm::interpreter`) can't apply. Enums can't be
                // subclassed either, so there is no dispatch to virtualize
                // in the first place — same non-virtual `INVOKE_SPECIAL`
                // `super.method(...)` already uses, which works with any
                // receiver `Value` (it just binds it to local 0).
                let is_enum_receiver = self.classes.get(&fqcn).is_some_and(|i| i.is_enum);
                let opcode = if is_enum_receiver {
                    Opcode::InvokeSpecial
                } else {
                    Opcode::InvokeInstance
                };
                self.op_u16(
                    opcode,
                    method_ref,
                    result_delta - positional.len() as i32 - 1,
                );
                self.emit_unbox_ref_args(&boxes);
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
    /// `map`'s result element type `U` has no static representation from
    /// nl-sema (a closure literal's own inferred type is still `Type::Void`
    /// there — see `checker.rs`'s `Expr::Closure` arm; `Type::Function`
    /// only models *explicit* function-type declarations, not a literal's
    /// own deduced shape — see `ExprTy::Closure`'s doc comment), so unlike
    /// `filter`/`find` (which keep the receiver's own element
    /// type, since their callback can't change it) it is recovered directly
    /// from the closure literal's own *deduced* return type
    /// (`ExprTy::Closure`'s `return_ty`) rather than guessed — more precise
    /// than falling back to the `Type::Void` wildcard nl-sema uses (see
    /// `checker.rs`'s matching arm), and needed so a subsequent
    /// `U[] result = numbers.map(...)` assignment sees the real `U`.
    fn compile_array_method_call(
        &mut self,
        elem_ty: ExprTy,
        name: &str,
        args: &[Expr],
    ) -> Result<ExprTy, CodegenError> {
        match (name, args.len()) {
            ("slice", 2) => {
                self.compile_call_args(args, &[ExprTy::Int, ExprTy::Int], &[false, false], name)?;
                self.emit_array_call(
                    name,
                    &[ExprTy::Int, ExprTy::Int],
                    ExprTy::Array(Box::new(elem_ty)),
                )
            }
            ("map", 1) => {
                let closure_ty = self.compile_expr(&args[0])?;
                let result_elem = match &closure_ty {
                    ExprTy::Closure { return_ty, .. } => (**return_ty).clone(),
                    _ => {
                        return Err(CodegenError::Unsupported(format!(
                            "'{name}' expects a closure argument"
                        )))
                    }
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
    fn emit_array_call(
        &mut self,
        name: &str,
        param_types: &[ExprTy],
        return_ty: ExprTy,
    ) -> Result<ExprTy, CodegenError> {
        let param_ast_types: Vec<Type> = param_types.iter().map(expr_ty_to_type).collect();
        let descriptor = method_descriptor(&param_ast_types, &expr_ty_to_type(&return_ty));
        let name_index = self.cp.add_utf8(name.to_string());
        let descriptor_index = self.cp.add_type_desc(&descriptor);
        let class_index = self.cp.add_class("system.Array");
        let method_ref = self
            .cp
            .add_method_ref(class_index, name_index, descriptor_index);
        let result_delta = if return_ty == ExprTy::Void { 0 } else { 1 };
        self.op_u16(
            Opcode::InvokeInstance,
            method_ref,
            result_delta - param_types.len() as i32 - 1,
        );
        Ok(return_ty)
    }

    /// Emits an `INVOKE_STATIC` against a native `system.*` class (no
    /// backing bytecode `Module` — see `nl_vm::native`). `print`/`println`
    /// are normalized to their single `(string) -> void` overload first
    /// (`crate::stdlib::is_printlike`); everything else uses its declared
    /// signature from `crate::stdlib::signature`.
    fn compile_stdlib_call(
        &mut self,
        fqcn: &str,
        name: &str,
        args: &[Expr],
    ) -> Result<ExprTy, CodegenError> {
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
                return Err(CodegenError::Unsupported(format!(
                    "'run' expects 1 argument, got {}",
                    args.len()
                )));
            }
            let ty = self.compile_expr(&args[0])?;
            let param_ty = match &ty {
                ExprTy::StringT => Type::StringT,
                ExprTy::Array(elem) if **elem == ExprTy::StringT => {
                    Type::Array(Box::new(Type::StringT))
                }
                other => {
                    return Err(CodegenError::Unsupported(format!(
                        "'run' expects a string or string[] argument, got {other:?}"
                    )))
                }
            };
            return self.emit_native_static(
                fqcn,
                name,
                &[param_ty],
                &crate::stdlib::process_result(),
            );
        }

        let (param_types, return_ty) = crate::stdlib::signature(fqcn, name, args.len())
            .ok_or_else(|| {
                CodegenError::Unsupported(format!(
                    "unknown stdlib method '{fqcn}.{name}' with {} argument(s)",
                    args.len()
                ))
            })?;
        let param_expr_tys: Vec<ExprTy> = param_types.iter().map(expr_ty_of).collect();
        self.compile_call_args(args, &param_expr_tys, &vec![false; args.len()], name)?;
        self.emit_native_static(fqcn, name, &param_types, &return_ty)
    }

    /// `params`/`return_ty` describe both the operand-stack effect (the
    /// caller must already have pushed exactly `params.len()` values) and
    /// the constant-pool `MethodRef` descriptor the VM's native dispatcher
    /// matches on.
    fn emit_native_static(
        &mut self,
        fqcn: &str,
        name: &str,
        params: &[Type],
        return_ty: &Type,
    ) -> Result<ExprTy, CodegenError> {
        let class_index = self.cp.add_class(fqcn);
        let name_index = self.cp.add_utf8(name.to_string());
        let descriptor = method_descriptor(params, return_ty);
        let descriptor_index = self.cp.add_type_desc(&descriptor);
        let method_ref = self
            .cp
            .add_method_ref(class_index, name_index, descriptor_index);
        let ret = expr_ty_of(return_ty);
        let result_delta = if ret == ExprTy::Void { 0 } else { 1 };
        self.op_u16(
            Opcode::InvokeStatic,
            method_ref,
            result_delta - params.len() as i32,
        );
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
            return Err(CodegenError::Unsupported(
                "array index must be int".to_string(),
            ));
        }
        self.op(Opcode::ArrayLoad, -1);
        Ok(*elem)
    }

    fn compile_instanceof(
        &mut self,
        target: &Expr,
        type_name: &str,
    ) -> Result<ExprTy, CodegenError> {
        self.compile_expr(target)?;
        let fqcn = self.resolve_class_name(type_name);
        let class_index = self.cp.add_class(&fqcn);
        self.op_u16(Opcode::InstanceOf, class_index, 0);
        Ok(ExprTy::Bool)
    }

    /// `(T) expr` — specs.md § Type conversions and casting. Validity is
    /// nl-sema's job (E007); by the time this runs the cast is known-valid,
    /// so this only has to pick which (if any) conversion opcode makes the
    /// value's runtime representation match `T`.
    fn compile_cast(&mut self, ty: &Type, inner: &Expr) -> Result<ExprTy, CodegenError> {
        let actual = self.compile_expr(inner)?;
        let target = expr_ty_of(&resolve_type(ty, self.imports));
        match (&actual, &target) {
            (ExprTy::Int, ExprTy::Float) => self.op(Opcode::I2F, 0),
            (ExprTy::Float, ExprTy::Int) => self.op(Opcode::F2I, 0),
            (ExprTy::Int, ExprTy::Byte) => self.op(Opcode::I2B, 0),
            (ExprTy::Byte, ExprTy::Int) => self.op(Opcode::B2I, 0),
            // No direct byte<->float opcode — vm.md's numeric conversions
            // only define I2F/F2I/I2B/B2I — so route through `int`.
            (ExprTy::Byte, ExprTy::Float) => {
                self.op(Opcode::B2I, 0);
                self.op(Opcode::I2F, 0);
            }
            (ExprTy::Float, ExprTy::Byte) => {
                self.op(Opcode::F2I, 0);
                self.op(Opcode::I2B, 0);
            }
            (actual, ExprTy::StringT) if *actual != ExprTy::StringT => self.op(Opcode::ToString, 0),
            // Class up/downcast: `CHECKCAST` throws `InvalidCastException`
            // at runtime on a failed downcast (specs.md § Type conversions
            // and casting); a harmless no-op check for an upcast, which
            // nl-sema already knows always succeeds.
            (ExprTy::Object(_), ExprTy::Object(target_fqcn)) => {
                let class_index = self.cp.add_class(target_fqcn);
                self.op_u16(Opcode::CheckCast, class_index, 0);
            }
            // Identical type, or no runtime representation change needed.
            _ => {}
        }
        Ok(target)
    }

    /// Coerces a single already-compiled value on top of the stack from
    /// `actual` to `expected` (int -> float widening; `null` is accepted for
    /// any type here since nullability itself is nl-sema's job). Used for
    /// plain-assignment/initializer sites; call-argument lists use
    /// `compile_call_args`, which applies the same rule per argument.
    pub(crate) fn coerce_value(
        &mut self,
        actual: &ExprTy,
        expected: &ExprTy,
        what: &str,
    ) -> Result<(), CodegenError> {
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
            // A closure literal assigned where only an untyped callback
            // param exists (`Type::Void` used as the same joker nl-sema
            // uses for a closure's own inferred type — no explicit
            // `Type::Function` written at that declaration). First
            // exercised by `system.thread.Thread(() => void task)`.
        } else if matches!((actual, expected), (ExprTy::Closure { .. }, ExprTy::Closure { .. }))
            && closure_shape_eq(actual, expected)
        {
            // Assigning to an explicit `Type::Function` (specs.md §
            // Function type assignment) — see `ExprTy::Closure`'s doc
            // comment. Structural match on params/return type, `fqcn`
            // ignored: no bytecode needed, `actual`'s already-compiled
            // `invoke` descriptor is exactly what any `InvokeClosure` built
            // from `expected`'s placeholder `fqcn` will look for at
            // dispatch time (which resolves purely by the receiver's
            // runtime class + name/descriptor, never `expected`'s `fqcn`).
        } else if actual != expected {
            return Err(CodegenError::Unsupported(format!(
                "cannot assign {actual:?} to '{what}' of type {expected:?}"
            )));
        }
        Ok(())
    }

    /// Compiles each of `args`, in order, onto the stack, ready for an
    /// `INVOKE_*`/`NEW`+`INVOKE_SPECIAL` sequence right after. `is_ref[i]`
    /// marks a `ref` parameter (compiler.md § Ref parameter rules; vm.md §
    /// Ref parameters (boxing)) — nl-sema has already validated (E020) that
    /// its argument is a plain variable, so it's boxed into a scratch local
    /// *before* any argument is pushed (matching vm.md's own worked
    /// example), then the box is pushed in the argument's place. Native
    /// calls (array/string/stdlib built-ins) have no `Param` metadata and
    /// so are always all-`false` here — same fast path as before this
    /// existed. Returns one `(var_local, box_local, inner_ty)` triple per
    /// *freshly* boxed `ref` argument, to be unboxed with
    /// `emit_unbox_ref_args` right after the call is emitted — forwarding
    /// an already-boxed `ref` parameter (`ref` passed straight through to
    /// another `ref` parameter) needs no fresh box and no unboxing here,
    /// since its original owner already does that once its own call
    /// returns.
    fn compile_call_args(
        &mut self,
        args: &[Expr],
        param_types: &[ExprTy],
        is_ref: &[bool],
        ctx: &str,
    ) -> Result<Vec<(u16, u16, ExprTy)>, CodegenError> {
        if args.len() != param_types.len() {
            return Err(CodegenError::Unsupported(format!(
                "'{ctx}' expects {} argument(s), got {}",
                param_types.len(),
                args.len()
            )));
        }
        enum RefPlan {
            None,
            NewBox(u16, u16, ExprTy),
            Forward(u16),
        }
        let mut plans = Vec::with_capacity(args.len());
        for ((arg, ty), r) in args.iter().zip(param_types).zip(is_ref) {
            if *r {
                let Expr::Ident(var_name) = arg else {
                    return Err(CodegenError::Unsupported(format!(
                        "'{ctx}' ref argument must be a variable"
                    )));
                };
                let var_slot = self.lookup_local(var_name)?;
                if var_slot.boxed.is_some() {
                    plans.push(RefPlan::Forward(var_slot.index));
                } else {
                    let box_local = self.emit_new_box(ty, var_slot.index);
                    plans.push(RefPlan::NewBox(box_local, var_slot.index, ty.clone()));
                }
            } else {
                plans.push(RefPlan::None);
            }
        }
        for (i, plan) in plans.iter().enumerate() {
            match plan {
                RefPlan::NewBox(box_local, ..) | RefPlan::Forward(box_local) => {
                    self.op_u16(Opcode::Load, *box_local, 1);
                }
                RefPlan::None => {
                    let actual = self.compile_expr(&args[i])?;
                    self.coerce_value(&actual, &param_types[i], ctx)?;
                }
            }
        }
        Ok(plans
            .into_iter()
            .filter_map(|p| match p {
                RefPlan::NewBox(box_local, var_local, ty) => Some((var_local, box_local, ty)),
                _ => None,
            })
            .collect())
    }

    /// `NEW Box<T>; DUP; LOAD var; SET_FIELD Box.value; STORE box_local` —
    /// vm.md § Ref parameters (boxing), caller side, first half. Returns
    /// the scratch local now holding the box reference.
    fn emit_new_box(&mut self, inner_ty: &ExprTy, var_local: u16) -> u16 {
        let box_fqcn = crate::class_table::box_fqcn(&expr_ty_to_type(inner_ty));
        let class_index = self.cp.add_class(&box_fqcn);
        self.op_u16(Opcode::New, class_index, 1);
        self.op(Opcode::Dup, 1);
        self.op_u16(Opcode::Load, var_local, 1);
        let field_ref = self.box_value_field_ref(inner_ty);
        self.op_u16(Opcode::SetField, field_ref, -2);
        let box_local = self.declare_scratch_local(ExprTy::Object(box_fqcn));
        self.emit_store(box_local);
        box_local
    }

    /// `LOAD box_local; GET_FIELD Box.value; STORE var_local`, once per
    /// entry — vm.md § Ref parameters (boxing), caller side, second half,
    /// emitted right after the call instruction returns.
    fn emit_unbox_ref_args(&mut self, boxes: &[(u16, u16, ExprTy)]) {
        for (var_local, box_local, inner_ty) in boxes {
            self.op_u16(Opcode::Load, *box_local, 1);
            let field_ref = self.box_value_field_ref(inner_ty);
            self.op_u16(Opcode::GetField, field_ref, 0);
            self.emit_store(*var_local);
        }
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

    /// One `INVOKE_INSTANCE` against a user-class operator-overload receiver
    /// already on the stack (specs.md § Operator Overloading), with
    /// `params.len()` argument(s) already compiled/coerced on top of it (0
    /// for unary/`operator++`/`operator--`, 1 for binary/compound
    /// assignment) — same bytecode shape as `compile_method_call`'s
    /// user-class branch, trimmed to what a resolved operator call already
    /// knows statically (no named/optional/`ref` arguments, no enum
    /// receiver — operators aren't overloadable on those).
    fn emit_operator_call(
        &mut self,
        fqcn: &str,
        method_name: &str,
        params: &[Type],
        return_ty: &Type,
    ) -> ExprTy {
        let descriptor = method_descriptor(params, return_ty);
        let name_index = self.cp.add_utf8(method_name.to_string());
        let descriptor_index = self.cp.add_type_desc(&descriptor);
        let class_index = self.cp.add_class(fqcn);
        let method_ref = self
            .cp
            .add_method_ref(class_index, name_index, descriptor_index);
        let return_expr_ty = expr_ty_of(return_ty);
        let result_delta = if return_expr_ty == ExprTy::Void { 0 } else { 1 };
        self.op_u16(
            Opcode::InvokeInstance,
            method_ref,
            result_delta - params.len() as i32 - 1,
        );
        return_expr_ty
    }

    fn compile_unary(&mut self, op: UnOp, inner: &Expr) -> Result<ExprTy, CodegenError> {
        let ty = self.compile_expr(inner)?;
        if let ExprTy::Object(fqcn) = &ty {
            let method_name = match op {
                UnOp::Neg => "operator-",
                UnOp::Not => "operator!",
            };
            if let Some(method) = find_operator_method(self.classes, fqcn, method_name, &[]) {
                let return_ty = method.return_ty.clone();
                let fqcn = fqcn.clone();
                return Ok(self.emit_operator_call(&fqcn, method_name, &[], &return_ty));
            }
        }
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
                other => Err(CodegenError::Unsupported(format!("unary '-' on {other:?}"))),
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

    fn compile_binary(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
    ) -> Result<ExprTy, CodegenError> {
        match op {
            BinOp::And => return self.compile_short_circuit(true, lhs, rhs),
            BinOp::Or => return self.compile_short_circuit(false, lhs, rhs),
            _ => {}
        }

        // Operator overloading — specs.md § Operator Overloading. Peeked
        // (rather than compiled first) so this can be tried before
        // compiling either side; `peek_type` is best-effort (see its doc
        // comment) so a receiver shape it doesn't cover (e.g. `new
        // Vector2(...) + p2`) falls through to the ordinary numeric path
        // below and its usual "unsupported" error — a known limitation,
        // consistent with the rest of this best-effort resolver. nl-sema
        // has already validated the overload exists when this reaches
        // codegen, but codegen keeps its own independent lookup (same
        // division of labor as every other call site here).
        if let Some(op_method) = operator_method_name(op) {
            if let (Some(ExprTy::Object(fqcn)), Some(rhs_ty)) =
                (self.peek_type(lhs), self.peek_type(rhs))
            {
                let rhs_ast_ty = expr_ty_to_type(&rhs_ty);
                if let Some(method) = find_operator_method(
                    self.classes,
                    &fqcn,
                    op_method,
                    std::slice::from_ref(&rhs_ast_ty),
                ) {
                    let return_ty = method.return_ty.clone();
                    self.compile_expr(lhs)?;
                    self.compile_expr(rhs)?;
                    return Ok(self.emit_operator_call(
                        &fqcn,
                        op_method,
                        std::slice::from_ref(&rhs_ast_ty),
                        &return_ty,
                    ));
                }
            }
        }

        // String concatenation: '+' where either side is a string.
        if op == BinOp::Add {
            let (peek_l, peek_r) = (self.peek_type(lhs), self.peek_type(rhs));
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
            let opcode = if op == BinOp::Eq {
                Opcode::CmpEq
            } else {
                Opcode::CmpNe
            };
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
        let branch_op = if is_and {
            Opcode::IfFalse
        } else {
            Opcode::IfTrue
        };
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

    /// Best-effort static type of an expression without emitting code — used
    /// only to decide whether `+` means string concatenation before
    /// committing to bytecode order. Takes `&self` (unlike a plain free
    /// function) so an `Expr::Ident` leaf can be resolved to its declared
    /// type — needed for a left-associative concatenation chain like
    /// `s + toString(x) + ","`, where the nested `Binary(Add, s, toString(x))`
    /// has no literal leaf and would otherwise peek as `None`.
    fn peek_type(&self, expr: &Expr) -> Option<ExprTy> {
        match expr {
            Expr::StringLit(_) => Some(ExprTy::StringT),
            Expr::IntLit(_) => Some(ExprTy::Int),
            Expr::FloatLit(_) => Some(ExprTy::Float),
            Expr::BoolLit(_) => Some(ExprTy::Bool),
            Expr::NullLit => Some(ExprTy::Null),
            Expr::Ident(name) => match self.resolve_ident(name).ok()? {
                IdentRef::Local(slot) => Some(slot.ty),
                IdentRef::CapturedField(field) => Some(field.ty),
            },
            // Operator overloading — needed so a chained/nested overloaded
            // expression (`p1 + p2 + 1`, parsed as `Binary(Add,
            // Binary(Add, p1, p2), 1)`) still peeks as `Object` at the
            // outer level: `compile_binary`'s own operator-overload check
            // peeks `lhs` *before* compiling anything, so without this the
            // inner `p1 + p2` (itself dispatched to `operator+` only once
            // actually compiled) would be invisible here and the outer `+
            // 1` would wrongly fall through to the built-in numeric path.
            Expr::Binary(op, l, r) => {
                if let Some(op_method) = operator_method_name(*op) {
                    if let (Some(ExprTy::Object(fqcn)), Some(rhs_ty)) =
                        (self.peek_type(l), self.peek_type(r))
                    {
                        let rhs_ast_ty = expr_ty_to_type(&rhs_ty);
                        if let Some(method) = find_operator_method(
                            self.classes,
                            &fqcn,
                            op_method,
                            std::slice::from_ref(&rhs_ast_ty),
                        ) {
                            return Some(expr_ty_of(&method.return_ty));
                        }
                    }
                }
                if *op == BinOp::Add {
                    match (self.peek_type(l), self.peek_type(r)) {
                        (Some(ExprTy::StringT), _) | (_, Some(ExprTy::StringT)) => {
                            return Some(ExprTy::StringT)
                        }
                        _ => {}
                    }
                }
                None
            }
            // `new ClassName(...)` — needed so operator overloading (see
            // `compile_binary`'s doc comment) recognizes a fresh instance
            // as an operand without compiling it, e.g. `p3 += new
            // Vector2(2, 3)` or `p1 + new Vector2(1, 1)`.
            Expr::New(class_name, _type_args, _args) => {
                Some(ExprTy::Object(self.resolve_class_name(class_name)))
            }
            // `obj.field` — resolved the same way `compile_field_access`
            // resolves it, but purely (no bytecode emitted): peek the
            // receiver's static class, then reuse the same three lookup
            // tables in the same order (native-generics result types,
            // stdlib `Result<T>` fields, then user-declared fields).
            Expr::FieldAccess(target, name) => {
                let ExprTy::Object(fqcn) = self.peek_type(target)? else {
                    return None;
                };
                let field = crate::native_generics::field_ty(&fqcn, name)
                    .or_else(|| crate::stdlib::result_field_ty(&fqcn, name))
                    .or_else(|| find_field(self.classes, &fqcn, name).map(|f| f.ty.clone()))?;
                Some(expr_ty_of(&field))
            }
            // `obj.method(...)` — same idea, via `find_method`'s declared
            // return type instead of compiling the call.
            Expr::MethodCall(target, name, args) => {
                let ExprTy::Object(fqcn) = self.peek_type(target)? else {
                    return None;
                };
                let method = find_method(self.classes, &fqcn, name, args.len())?;
                Some(expr_ty_of(&method.return_ty))
            }
            _ => None,
        }
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

/// Canonical `operator<sym>` method name for `op` — specs.md § Overloadable
/// operators. `None` for the non-overloadable ops (`==`/`!=`/`&&`/`||`) —
/// mirrors `nl_sema::checker`'s copy.
fn operator_method_name(op: BinOp) -> Option<&'static str> {
    match op {
        BinOp::Add => Some("operator+"),
        BinOp::Sub => Some("operator-"),
        BinOp::Mul => Some("operator*"),
        BinOp::Div => Some("operator/"),
        BinOp::Mod => Some("operator%"),
        BinOp::Lt => Some("operator<"),
        BinOp::Gt => Some("operator>"),
        BinOp::Le => Some("operator<="),
        BinOp::Ge => Some("operator>="),
        BinOp::Cmp3 => Some("operator<=>"),
        BinOp::Eq | BinOp::Ne | BinOp::And | BinOp::Or => None,
    }
}

/// Canonical `operator<sym>=` compound-assignment method name for `op` —
/// specs.md § Overloadable operators (`+= -= *= /= %=`) — mirrors
/// `nl_sema::checker`'s copy.
fn compound_operator_method_name(op: BinOp) -> Option<&'static str> {
    match op {
        BinOp::Add => Some("operator+="),
        BinOp::Sub => Some("operator-="),
        BinOp::Mul => Some("operator*="),
        BinOp::Div => Some("operator/="),
        BinOp::Mod => Some("operator%="),
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
