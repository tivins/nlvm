mod class_table;
mod closure;
pub mod error;
mod expr;
mod native_generics;
mod stdlib;
mod stmt;
mod type_desc;

use std::collections::HashMap;

use nl_bytecode::{
    class_flags, field_flags, method_flags, ConstantPool, HashAlgo, MethodDescriptor, Module,
};
use nl_syntax::ast::{
    ClassDecl, Expr, LValue, MethodDecl, MethodKind, SourceFile, SourceItem, Stmt, StmtKind, Type,
    Visibility,
};

use class_table::{build_class_table, fqcn_of, import_map, resolve_type, ClassInfo};
pub use error::CodegenError;
use expr::{expr_ty_of, Emitter, MethodSig};
use type_desc::method_descriptor;

/// Compiles a whole program (every file that will be linked together) in one
/// pass: a shared class table is built first so `new`/field access/instance
/// method calls that cross file boundaries resolve to real constant-pool
/// entries. See `nl_vm::Program` for how these modules are linked at load
/// time.
pub fn compile_program(files: &[SourceFile]) -> Result<Vec<Module>, CodegenError> {
    // Built-in exception classes (nl_syntax::prelude) are implicitly part of
    // every program — see class_table::import_map, which seeds their simple
    // names so user code can reference them without a `use`. Prepended
    // *before* expansion (not after): the prelude's `Box<T>` (vm.md § Ref
    // parameters (boxing)) is itself a template, and `nl_syntax::monomorphize
    // ::expand` only ever monomorphizes templates it can see in its own
    // input. nl-sema expands the exact same combined input the same way
    // (see its `check_compile`), so both crates always agree on the
    // expanded program.
    let mut unexpanded = nl_syntax::prelude::files();
    unexpanded.extend(files.to_vec());

    // specs.md § Typedef — alias expansion runs first, same ordering
    // (and same reasoning) as `nl_sema::check_compile_with_warnings`'s
    // identical two-step expansion, so both crates always agree on the
    // expanded program.
    let unexpanded = nl_syntax::typedef::expand(unexpanded);

    // Template classes (specs.md § Template class) are expanded into
    // ordinary monomorphized classes before anything else sees them — see
    // nl_syntax::monomorphize.
    let all_files = nl_syntax::monomorphize::expand(unexpanded);

    let classes = build_class_table(&all_files);
    let mut modules = Vec::new();
    for file in &all_files {
        modules.extend(compile_file(file, &all_files, &classes)?);
    }
    Ok(modules)
}

/// Single-file convenience wrapper — still valid for programs that don't
/// reference any other class (e.g. the milestone 1-4 fixtures). `compile_program`
/// always also returns the built-in prelude's modules, so the caller's own
/// module is found by name rather than assumed to be at a fixed index.
pub fn compile_source_file(file: &SourceFile) -> Result<Module, CodegenError> {
    let fqcn = fqcn_of(file);
    let modules = compile_program(std::slice::from_ref(file))?;
    Ok(modules
        .into_iter()
        .find(|m| m.this_class_name() == Some(fqcn.as_str()))
        .expect("compile_program always compiles the input file's own module"))
}

/// Returns the file's own module first, followed by any synthetic closure
/// classes generated while compiling its methods (vm.md § Closures — "the
/// compiler generates a synthetic class for each closure").
fn compile_file(
    file: &SourceFile,
    all_files: &[SourceFile],
    classes: &HashMap<String, ClassInfo>,
) -> Result<Vec<Module>, CodegenError> {
    let imports = import_map(file, all_files);
    let fqcn = fqcn_of(file);
    let mut cp = ConstantPool::new();
    let this_class = cp.add_class(&fqcn);

    match &file.item {
        // vm.md § Method descriptor, "Abstract method representation":
        // "Interface method declarations are encoded the same way
        // (implicitly abstract)" — each signature becomes a code-less
        // `ABSTRACT` stub, not an omitted method, so `nl_bytecode::Module::
        // validate`'s invariants and virtual dispatch's
        // `find_method_by_descriptor` see it like any other abstract method.
        SourceItem::Interface(iface) => {
            let methods = iface
                .methods
                .iter()
                .map(|m| interface_method_descriptor(m, &mut cp, &imports))
                .collect();
            Ok(vec![Module {
                version: nl_bytecode::module::VERSION,
                constant_pool: cp,
                this_class,
                class_flags: class_flags::INTERFACE,
                super_class: 0,
                interfaces: Vec::new(),
                fields: Vec::new(),
                methods,
                hash_algo: HashAlgo::Sha256,
            }])
        }
        SourceItem::Class(class) => {
            let super_class = match &class.extends {
                Some(name) => {
                    let super_fqcn = imports.get(name).cloned().unwrap_or_else(|| name.clone());
                    cp.add_class(&super_fqcn)
                }
                None => 0,
            };
            // compiler.md § Interface inheritance — flattened to include
            // every interface each directly-`implements`-ed one transitively
            // `extends`, not just the names written after `implements`
            // itself (see `class_table::interface_closure`).
            let direct_interface_fqcns: Vec<String> = class
                .implements
                .iter()
                .map(|name| imports.get(name).cloned().unwrap_or_else(|| name.clone()))
                .collect();
            let interfaces = class_table::interface_closure(classes, &direct_interface_fqcns)
                .into_iter()
                .map(|iface_fqcn| cp.add_class(&iface_fqcn))
                .collect();

            let fields = class
                .fields
                .iter()
                .map(|f| {
                    let name_index = cp.add_utf8(f.name.clone());
                    let resolved_ty = resolve_type(&f.ty, &imports);
                    let type_index = cp.add_type_desc(&type_desc::type_descriptor(&resolved_ty));
                    let mut flags = visibility_field_flag(f.visibility);
                    if f.is_static {
                        flags |= field_flags::STATIC;
                    }
                    if f.readonly {
                        flags |= field_flags::READONLY;
                    }
                    nl_bytecode::FieldDescriptor {
                        flags,
                        name_index,
                        type_index,
                    }
                })
                .collect();

            // First pass: register every static method's signature so bare
            // (unqualified) calls resolve regardless of declaration order —
            // instance methods/constructors are only reachable via `expr.m(...)`
            // /`new`/`this(...)`, resolved directly at their call site instead.
            // Every same-name static overload is kept (declaration order) —
            // see issue #7. Previously this was `HashMap<String, MethodSig>`
            // where the last-declared overload silently overwrote earlier
            // ones, and `Emitter::compile_call` would then always emit an
            // `INVOKE_STATIC` against that single overload regardless of
            // actual argument types. Must stay in lockstep with nl-sema's
            // own per-name overload set (see `nl_sema::checker`'s `sigs`).
            let mut static_sigs: HashMap<String, Vec<MethodSig>> = HashMap::new();
            for m in &class.methods {
                if m.is_static && m.kind == MethodKind::Normal {
                    let name_index = cp.add_utf8(m.name.clone());
                    let params: Vec<Type> = m
                        .params
                        .iter()
                        .map(|p| resolve_type(&p.ty, &imports))
                        .collect();
                    let is_ref: Vec<bool> = m.params.iter().map(|p| p.is_ref).collect();
                    let return_ty = resolve_type(&m.return_type, &imports);
                    // vm.md § Ref parameters (boxing) — a `ref` parameter's
                    // *physical* type in the descriptor is `Box<T>`, not `T`.
                    let cc_params = class_table::calling_convention_params(&params, &is_ref);
                    let descriptor = method_descriptor(&cc_params, &return_ty);
                    let descriptor_index = cp.add_type_desc(&descriptor);
                    let method_ref_index =
                        cp.add_method_ref(this_class, name_index, descriptor_index);
                    static_sigs.entry(m.name.clone()).or_default().push(MethodSig {
                        param_types: params.iter().map(expr_ty_of).collect(),
                        param_names: m.params.iter().map(|p| p.name.clone()).collect(),
                        defaults: m.params.iter().map(|p| p.default.clone()).collect(),
                        is_ref,
                        return_ty: expr_ty_of(&return_ty),
                        method_ref_index,
                    });
                }
            }

            // Field initializers — specs.md § Default values: "A class
            // property ... must be initialized either at the declaration
            // site or inside every `construct` path". The declaration-site
            // form has no dedicated bytecode representation; it's desugared
            // here into ordinary assignment statements spliced into each
            // constructor (instance fields — `this.field = init`) or into a
            // single synthetic `<clinit>` (static fields — `ClassName.field
            // = init`, see `compile_static_init`). Enums are left alone
            // entirely: their fields (case constants, plus any hand-written
            // extra static field) are already handled by
            // `Emitter::compile_field_access`'s recompile-at-use-site enum
            // branch, which predates and doesn't need real static storage.
            let instance_field_inits: Vec<Stmt> = if class.is_enum {
                Vec::new()
            } else {
                class
                    .fields
                    .iter()
                    .filter(|f| !f.is_static)
                    .filter_map(field_init_stmt(Expr::This))
                    .collect()
            };

            let mut methods = Vec::with_capacity(class.methods.len());
            let mut closure_modules = Vec::new();
            // specs.md § Abstract classes and methods — an abstract method
            // has no body and is never itself instantiable/directly callable
            // (E032 rejects `new` on its class; E033 guarantees every
            // concrete subclass provides a real override, which virtual
            // dispatch always resolves to first), but vm.md still requires a
            // code-less stub descriptor (`ABSTRACT` flag, `code_length = 0`)
            // rather than omitting the method from the module entirely.
            for (method_index, m) in class.methods.iter().enumerate() {
                if m.is_abstract {
                    methods.push(abstract_method_descriptor(m, &mut cp, &imports));
                    continue;
                }
                let patched;
                let m = if m.kind == MethodKind::Constructor && !instance_field_inits.is_empty() {
                    patched = prepend_field_inits(m, &instance_field_inits);
                    &patched
                } else {
                    m
                };
                let (descriptor, closures) = compile_method(
                    m.name.as_str(),
                    method_index,
                    m,
                    class,
                    &mut cp,
                    this_class,
                    &fqcn,
                    &imports,
                    classes,
                    &static_sigs,
                )?;
                methods.push(descriptor);
                closure_modules.extend(closures);
            }

            // Static field initializers — see the comment above. Compiled
            // last (after every declared method, so `class.methods.len()`
            // is a free `method_index` for the closure-naming prefix) into
            // one synthetic `<clinit>`, run once per class by
            // `nl_vm::program::run_static_initializers` before `main`.
            if !class.is_enum {
                let static_init_stmts: Vec<Stmt> = class
                    .fields
                    .iter()
                    .filter(|f| f.is_static)
                    .filter_map(field_init_stmt(Expr::Ident(class.name.clone())))
                    .collect();
                if !static_init_stmts.is_empty() {
                    let clinit = MethodDecl {
                        name: "<clinit>".to_string(),
                        kind: MethodKind::Normal,
                        visibility: Visibility::Public,
                        visibility_explicit: true,
                        is_static: true,
                        is_const: false,
                        is_abstract: false,
                        is_final: false,
                        is_nodiscard: false,
                        return_type: Type::Void,
                        params: Vec::new(),
                        throws: Vec::new(),
                        body: static_init_stmts,
                        decl_line: class.decl_line,
                    };
                    let (descriptor, closures) = compile_method(
                        "<clinit>",
                        class.methods.len(),
                        &clinit,
                        class,
                        &mut cp,
                        this_class,
                        &fqcn,
                        &imports,
                        classes,
                        &static_sigs,
                    )?;
                    methods.push(descriptor);
                    closure_modules.extend(closures);
                }
            }

            let mut flags = 0u16;
            if class.is_readonly {
                flags |= class_flags::READONLY;
            }
            if class.is_enum {
                flags |= class_flags::ENUM;
            }
            if class.is_abstract {
                flags |= class_flags::ABSTRACT;
            }
            if class.is_final {
                flags |= class_flags::FINAL;
            }
            let mut modules = vec![Module {
                version: nl_bytecode::module::VERSION,
                constant_pool: cp,
                this_class,
                class_flags: flags,
                super_class,
                interfaces,
                fields,
                methods,
                hash_algo: HashAlgo::Sha256,
            }];
            modules.extend(closure_modules);
            Ok(modules)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn compile_method(
    name: &str,
    method_index: usize,
    method: &nl_syntax::ast::MethodDecl,
    class: &ClassDecl,
    cp: &mut ConstantPool,
    this_class: u16,
    this_fqcn: &str,
    imports: &HashMap<String, String>,
    classes: &HashMap<String, ClassInfo>,
    static_sigs: &HashMap<String, Vec<MethodSig>>,
) -> Result<(MethodDescriptor, Vec<Module>), CodegenError> {
    let _ = class;
    let name_index = cp.add_utf8(name.to_string());
    let resolved_params: Vec<Type> = method
        .params
        .iter()
        .map(|p| resolve_type(&p.ty, imports))
        .collect();
    let is_ref: Vec<bool> = method.params.iter().map(|p| p.is_ref).collect();
    let resolved_return = resolve_type(&method.return_type, imports);
    // vm.md § Ref parameters (boxing) — a `ref` parameter's *physical* type
    // in this method's own descriptor is `Box<T>`, not `T` (must match
    // what every call site builds its `method_ref`/`INVOKE_*` against).
    let cc_params = class_table::calling_convention_params(&resolved_params, &is_ref);
    let descriptor = method_descriptor(&cc_params, &resolved_return);
    let descriptor_index = cp.add_type_desc(&descriptor);

    let mut emitter = Emitter::new(
        cp,
        static_sigs,
        classes,
        imports,
        this_class,
        this_fqcn.to_string(),
    );
    emitter.closure_name_prefix = format!("{this_fqcn}$m{method_index}");
    emitter.boxed_captures = closure::boxed_captures_in_block(&method.body);
    emitter.push_scope();
    if !method.is_static {
        emitter.declare_local(
            "this".to_string(),
            expr::ExprTy::Object(this_fqcn.to_string()),
        );
    }
    for ((param, resolved_ty), r) in method.params.iter().zip(&resolved_params).zip(&is_ref) {
        if *r {
            emitter.declare_ref_param(param.name.clone(), expr_ty_of(resolved_ty));
        } else {
            emitter.declare_local(param.name.clone(), expr_ty_of(resolved_ty));
        }
    }
    // Box a non-`ref` parameter that some closure captures-and-mutates
    // (vm.md § Variable capture and boxing) — must run after every
    // parameter has claimed its ordinary positional slot above (see
    // `Emitter::rebox_local`).
    for ((param, resolved_ty), r) in method.params.iter().zip(&resolved_params).zip(&is_ref) {
        if !*r && emitter.boxed_captures.contains(&param.name) {
            emitter.rebox_local(&param.name, expr_ty_of(resolved_ty));
        }
    }
    for stmt in &method.body {
        emitter.compile_stmt(stmt)?;
    }
    if resolved_return == Type::Void {
        emitter.code.push(nl_bytecode::Opcode::Return as u8);
    }
    emitter.pop_scope();

    // Metadata only at this layer — checked-exception propagation (E015)
    // and override compatibility (E016/E017) are enforced by nl-sema
    // (crate::checker), not re-derived from this bytecode-level list.
    let throws_types: Vec<u16> = method
        .throws
        .iter()
        .map(|name| {
            let fqcn = imports.get(name).cloned().unwrap_or_else(|| name.clone());
            emitter.cp.add_class(&fqcn)
        })
        .collect();

    let mut flags = visibility_method_flag(method.visibility);
    if method.is_static {
        flags |= method_flags::STATIC;
    }
    if method.is_final {
        flags |= method_flags::FINAL;
    }
    match method.kind {
        MethodKind::Constructor => flags |= method_flags::CONSTRUCTOR,
        MethodKind::Destructor => flags |= method_flags::DESTRUCTOR,
        MethodKind::Normal => {}
    }

    let descriptor = MethodDescriptor {
        flags,
        name_index,
        descriptor_index,
        throws_types,
        max_locals: emitter.max_locals(),
        max_stack: emitter.max_stack(),
        code: emitter.code,
        exception_table: emitter.exception_table,
        line_table: emitter.line_table,
    };
    Ok((descriptor, emitter.closures))
}

/// vm.md § Method descriptor, "Abstract method representation" — an
/// `abstract` method (`MethodDecl.is_abstract`, always an empty `body`) is
/// encoded as a stub descriptor rather than compiled: `max_locals = 0`,
/// `max_stack = 0`, `code_length = 0`, no exception/line table. There is no
/// `Emitter` to run, so this only needs the name/descriptor computation
/// `compile_method` also does at its top.
fn abstract_method_descriptor(
    method: &MethodDecl,
    cp: &mut ConstantPool,
    imports: &HashMap<String, String>,
) -> MethodDescriptor {
    let name_index = cp.add_utf8(method.name.clone());
    let resolved_params: Vec<Type> = method
        .params
        .iter()
        .map(|p| resolve_type(&p.ty, imports))
        .collect();
    let is_ref: Vec<bool> = method.params.iter().map(|p| p.is_ref).collect();
    let resolved_return = resolve_type(&method.return_type, imports);
    let cc_params = class_table::calling_convention_params(&resolved_params, &is_ref);
    let descriptor = method_descriptor(&cc_params, &resolved_return);
    let descriptor_index = cp.add_type_desc(&descriptor);
    let throws_types: Vec<u16> = method
        .throws
        .iter()
        .map(|name| {
            let fqcn = imports.get(name).cloned().unwrap_or_else(|| name.clone());
            cp.add_class(&fqcn)
        })
        .collect();

    let mut flags = visibility_method_flag(method.visibility) | method_flags::ABSTRACT;
    if method.is_static {
        flags |= method_flags::STATIC;
    }

    MethodDescriptor {
        flags,
        name_index,
        descriptor_index,
        throws_types,
        max_locals: 0,
        max_stack: 0,
        code: Vec::new(),
        exception_table: Vec::new(),
        line_table: Vec::new(),
    }
}

/// Same stub shape as `abstract_method_descriptor`, from an interface's
/// `MethodSig` instead of a class's `MethodDecl` — specs.md § Interface
/// inheritance: "Interface methods are implicitly abstract". An interface
/// method has no `is_static`/`throws` to carry (the grammar doesn't allow
/// either on a signature-only declaration) and is always `public`.
fn interface_method_descriptor(
    sig: &nl_syntax::ast::MethodSig,
    cp: &mut ConstantPool,
    imports: &HashMap<String, String>,
) -> MethodDescriptor {
    let name_index = cp.add_utf8(sig.name.clone());
    let resolved_params: Vec<Type> = sig
        .params
        .iter()
        .map(|p| resolve_type(&p.ty, imports))
        .collect();
    let is_ref: Vec<bool> = sig.params.iter().map(|p| p.is_ref).collect();
    let resolved_return = resolve_type(&sig.return_type, imports);
    let cc_params = class_table::calling_convention_params(&resolved_params, &is_ref);
    let descriptor = method_descriptor(&cc_params, &resolved_return);
    let descriptor_index = cp.add_type_desc(&descriptor);

    MethodDescriptor {
        flags: method_flags::PUBLIC | method_flags::ABSTRACT,
        name_index,
        descriptor_index,
        throws_types: Vec::new(),
        max_locals: 0,
        max_stack: 0,
        code: Vec::new(),
        exception_table: Vec::new(),
        line_table: Vec::new(),
    }
}

/// Builds `<receiver>.<field.name> = <field.init>;` for a field declared
/// with an initializer — `receiver` is `Expr::This` for an instance field
/// (spliced into each constructor) or `Expr::Ident(<simple class name>)` for
/// a `static` one (spliced into the synthetic `<clinit>`). Returns a closure
/// suitable for `Iterator::filter_map` so a field with no initializer is
/// silently skipped (it keeps its type's ordinary default value — see
/// `nl_vm::interpreter::default_value_for`).
fn field_init_stmt(
    receiver: Expr,
) -> impl Fn(&nl_syntax::ast::FieldDecl) -> Option<Stmt> {
    move |f| {
        let init = f.init.clone()?;
        Some(Stmt {
            kind: StmtKind::Expr(Expr::Assign(
                LValue::Field(Box::new(receiver.clone()), f.name.clone()),
                Box::new(init),
            )),
            line: 0,
        })
    }
}

/// Splices `inits` (see `field_init_stmt`) into a constructor's body.
/// Skipped for a `this(...)`-delegating overload (compiler.md § Constructor
/// delegation: the target overload it delegates to already carries the same
/// `inits`, and running them twice would double-apply any side-effecting
/// initializer); inserted right after a `super(...)` call if present
/// (superclass fields should already be set by the time this class's own
/// initializers run), otherwise at the very front of the body.
fn prepend_field_inits(ctor: &MethodDecl, inits: &[Stmt]) -> MethodDecl {
    let mut patched = ctor.clone();
    match patched.body.first().map(|s| &s.kind) {
        Some(StmtKind::ThisCall(_)) => {}
        Some(StmtKind::SuperCall(_)) => {
            let rest = patched.body.split_off(1);
            patched.body.extend(inits.iter().cloned());
            patched.body.extend(rest);
        }
        _ => {
            let mut body = inits.to_vec();
            body.extend(patched.body);
            patched.body = body;
        }
    }
    patched
}

fn visibility_field_flag(v: Visibility) -> u16 {
    match v {
        Visibility::Public => field_flags::PUBLIC,
        Visibility::Protected => field_flags::PROTECTED,
        Visibility::Private => field_flags::PRIVATE,
    }
}

fn visibility_method_flag(v: Visibility) -> u16 {
    match v {
        Visibility::Public => method_flags::PUBLIC,
        Visibility::Protected => method_flags::PROTECTED,
        Visibility::Private => method_flags::PRIVATE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// vm.md § Method descriptor (line-number table): entries are sorted by
    /// ascending `start_pc`, one per source line change, so a `main` whose
    /// statements sit on known lines should produce a line table whose
    /// `line`s match those exactly (no gaps, no drift from the coalescing in
    /// `Emitter::record_line`).
    #[test]
    fn line_table_tracks_source_lines() {
        let src = "namespace test;\n\
                    class Program {\n\
                    \x20   public static int main(string[] args) {\n\
                    \x20       int x = 1;\n\
                    \x20       int y = 2;\n\
                    \x20       if (x < y) {\n\
                    \x20           x = y;\n\
                    \x20       }\n\
                    \x20       return x;\n\
                    \x20   }\n\
                    }\n";
        let file = nl_syntax::parse_source_file(src, "Program.nl".to_string()).unwrap();
        let module = compile_source_file(&file).unwrap();
        let method = module.find_method("main").unwrap();

        assert!(
            !method.line_table.is_empty(),
            "expected a non-empty line table for a method with real statements"
        );

        // start_pc strictly increasing (one entry per statement boundary,
        // deduped by line — see `record_line`) and within the method's code.
        let mut prev_pc = None;
        for entry in &method.line_table {
            if let Some(p) = prev_pc {
                assert!(
                    entry.start_pc > p,
                    "line table entries must have strictly increasing start_pc"
                );
            }
            assert!((entry.start_pc as usize) < method.code.len());
            prev_pc = Some(entry.start_pc);
        }

        let lines: Vec<u32> = method.line_table.iter().map(|e| e.line).collect();
        assert_eq!(lines, vec![4, 5, 6, 7, 9]);
    }

    /// vm.md § Method descriptor, "Abstract method representation" — an
    /// `abstract` method must be emitted as a code-less stub (`ABSTRACT`
    /// flag, `code_length = 0`, no locals/stack/tables) instead of being
    /// left out of the module entirely (the pre-Phase-6 behavior this
    /// guards against — see IMPLEMENTATION_STATUS.md § VM / bytecode).
    #[test]
    fn abstract_method_compiles_to_codeless_abstract_stub() {
        let src = "namespace test.abstract_stub;\n\
                    abstract class Shape {\n\
                    \x20   public abstract float area();\n\
                    }\n";
        let file = nl_syntax::parse_source_file(src, "Shape.nl".to_string()).unwrap();
        let module = compile_source_file(&file).unwrap();

        assert_ne!(module.class_flags & class_flags::ABSTRACT, 0);

        let method = module
            .find_method("area")
            .expect("abstract method must still appear in the compiled module");
        assert_ne!(method.flags & method_flags::ABSTRACT, 0);
        assert_eq!(method.max_locals, 0);
        assert_eq!(method.max_stack, 0);
        assert!(method.code.is_empty());
        assert!(method.exception_table.is_empty());
        assert!(method.line_table.is_empty());
    }

    /// Same invariant, but from an interface's signature-only method
    /// (specs.md § Interface inheritance: "Interface methods are implicitly
    /// abstract" — vm.md says they're "encoded the same way").
    #[test]
    fn interface_method_compiles_to_codeless_abstract_stub() {
        let src = "namespace test.interface_stub;\n\
                    interface Shape {\n\
                    \x20   float area();\n\
                    }\n";
        let file = nl_syntax::parse_source_file(src, "Shape.nl".to_string()).unwrap();
        let module = compile_source_file(&file).unwrap();

        assert_ne!(module.class_flags & class_flags::INTERFACE, 0);

        let method = module
            .find_method("area")
            .expect("interface method must appear in the compiled module");
        assert_ne!(method.flags & method_flags::ABSTRACT, 0);
        assert!(method.code.is_empty());
    }

    /// A `final` method must carry the `FINAL` flag (vm.md § Method
    /// descriptor) so the VM's link-time check (`nl_vm::program::
    /// verify_link`) can reject a subclass that overrides it.
    #[test]
    fn final_method_carries_final_flag() {
        let src = "namespace test.final_flag;\n\
                    class Base {\n\
                    \x20   public construct() {}\n\
                    \x20   public final string label() { return \"base\"; }\n\
                    }\n";
        let file = nl_syntax::parse_source_file(src, "Base.nl".to_string()).unwrap();
        let module = compile_source_file(&file).unwrap();

        let method = module.find_method("label").unwrap();
        assert_ne!(method.flags & method_flags::FINAL, 0);
    }
}
