mod class_table;
pub mod error;
mod expr;
mod stmt;
mod type_desc;

use std::collections::HashMap;

use nl_bytecode::{class_flags, field_flags, method_flags, ConstantPool, HashAlgo, MethodDescriptor, Module};
use nl_syntax::ast::{ClassDecl, MethodKind, SourceFile, SourceItem, Type, Visibility};

pub use error::CodegenError;
use class_table::{build_class_table, fqcn_of, import_map, resolve_type, ClassInfo};
use expr::{expr_ty_of, Emitter, MethodSig};
use type_desc::method_descriptor;

/// Compiles a whole program (every file that will be linked together) in one
/// pass: a shared class table is built first so `new`/field access/instance
/// method calls that cross file boundaries resolve to real constant-pool
/// entries. See `nl_vm::Program` for how these modules are linked at load
/// time.
pub fn compile_program(files: &[SourceFile]) -> Result<Vec<Module>, CodegenError> {
    let classes = build_class_table(files);
    files.iter().map(|f| compile_file(f, &classes)).collect()
}

/// Single-file convenience wrapper — still valid for programs that don't
/// reference any other class (e.g. the milestone 1-4 fixtures).
pub fn compile_source_file(file: &SourceFile) -> Result<Module, CodegenError> {
    compile_program(std::slice::from_ref(file)).map(|mut modules| modules.remove(0))
}

fn compile_file(file: &SourceFile, classes: &HashMap<String, ClassInfo>) -> Result<Module, CodegenError> {
    let imports = import_map(file);
    let fqcn = fqcn_of(file);
    let mut cp = ConstantPool::new();
    let this_class = cp.add_class(&fqcn);

    match &file.item {
        SourceItem::Interface(_) => Ok(Module {
            version: nl_bytecode::module::VERSION,
            constant_pool: cp,
            this_class,
            class_flags: class_flags::INTERFACE,
            super_class: 0,
            interfaces: Vec::new(),
            fields: Vec::new(),
            methods: Vec::new(),
            hash_algo: HashAlgo::Sha256,
        }),
        SourceItem::Class(class) => {
            let interfaces = class
                .implements
                .iter()
                .map(|name| {
                    let iface_fqcn = imports.get(name).cloned().unwrap_or_else(|| name.clone());
                    cp.add_class(&iface_fqcn)
                })
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
                    nl_bytecode::FieldDescriptor { flags, name_index, type_index }
                })
                .collect();

            // First pass: register every static method's signature so bare
            // (unqualified) calls resolve regardless of declaration order —
            // instance methods/constructors are only reachable via `expr.m(...)`
            // /`new`/`this(...)`, resolved directly at their call site instead.
            let mut static_sigs = HashMap::new();
            for m in &class.methods {
                if m.is_static && m.kind == MethodKind::Normal {
                    let name_index = cp.add_utf8(m.name.clone());
                    let params: Vec<Type> = m.params.iter().map(|p| resolve_type(&p.ty, &imports)).collect();
                    let return_ty = resolve_type(&m.return_type, &imports);
                    let descriptor = method_descriptor(&params, &return_ty);
                    let descriptor_index = cp.add_type_desc(&descriptor);
                    let method_ref_index = cp.add_method_ref(this_class, name_index, descriptor_index);
                    static_sigs.insert(
                        m.name.clone(),
                        MethodSig {
                            param_types: params.iter().map(expr_ty_of).collect(),
                            return_ty: expr_ty_of(&return_ty),
                            method_ref_index,
                        },
                    );
                }
            }

            let mut methods = Vec::with_capacity(class.methods.len());
            for m in &class.methods {
                methods.push(compile_method(m.name.as_str(), m, class, &mut cp, this_class, &fqcn, &imports, classes, &static_sigs)?);
            }

            Ok(Module {
                version: nl_bytecode::module::VERSION,
                constant_pool: cp,
                this_class,
                class_flags: 0,
                super_class: 0,
                interfaces,
                fields,
                methods,
                hash_algo: HashAlgo::Sha256,
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn compile_method(
    name: &str,
    method: &nl_syntax::ast::MethodDecl,
    class: &ClassDecl,
    cp: &mut ConstantPool,
    this_class: u16,
    this_fqcn: &str,
    imports: &HashMap<String, String>,
    classes: &HashMap<String, ClassInfo>,
    static_sigs: &HashMap<String, MethodSig>,
) -> Result<MethodDescriptor, CodegenError> {
    let _ = class;
    let name_index = cp.add_utf8(name.to_string());
    let resolved_params: Vec<Type> = method.params.iter().map(|p| resolve_type(&p.ty, imports)).collect();
    let resolved_return = resolve_type(&method.return_type, imports);
    let descriptor = method_descriptor(&resolved_params, &resolved_return);
    let descriptor_index = cp.add_type_desc(&descriptor);

    let mut emitter = Emitter::new(cp, static_sigs, classes, imports, this_class, this_fqcn.to_string());
    emitter.push_scope();
    if !method.is_static {
        emitter.declare_local("this".to_string(), expr::ExprTy::Object(this_fqcn.to_string()));
    }
    for (param, resolved_ty) in method.params.iter().zip(&resolved_params) {
        emitter.declare_local(param.name.clone(), expr_ty_of(resolved_ty));
    }
    for stmt in &method.body {
        emitter.compile_stmt(stmt)?;
    }
    if resolved_return == Type::Void {
        emitter.code.push(nl_bytecode::Opcode::Return as u8);
    }
    emitter.pop_scope();

    let mut flags = visibility_method_flag(method.visibility);
    if method.is_static {
        flags |= method_flags::STATIC;
    }
    match method.kind {
        MethodKind::Constructor => flags |= method_flags::CONSTRUCTOR,
        MethodKind::Destructor => flags |= method_flags::DESTRUCTOR,
        MethodKind::Normal => {}
    }

    Ok(MethodDescriptor {
        flags,
        name_index,
        descriptor_index,
        throws_types: Vec::new(),
        max_locals: emitter.max_locals(),
        max_stack: emitter.max_stack(),
        code: emitter.code,
        exception_table: Vec::new(),
        line_table: Vec::new(),
    })
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
