pub mod error;
mod expr;
mod stmt;
mod type_desc;

use std::collections::HashMap;

use nl_bytecode::{method_flags, ConstantPool, HashAlgo, MethodDescriptor, Module, Opcode};
use nl_syntax::ast::{MethodDecl, SourceFile, Type, Visibility};

pub use error::CodegenError;
use expr::{expr_ty_of, Emitter, MethodSig};
use type_desc::method_descriptor;

pub fn compile_source_file(file: &SourceFile) -> Result<Module, CodegenError> {
    let mut cp = ConstantPool::new();

    let fqcn = if file.namespace.is_empty() {
        file.class.name.clone()
    } else {
        format!("{}.{}", file.namespace.join("."), file.class.name)
    };
    let this_class = cp.add_class(&fqcn);

    // First pass: register every method's signature so call sites resolve
    // regardless of declaration order (forward references, recursion).
    let mut sigs = HashMap::with_capacity(file.class.methods.len());
    for method in &file.class.methods {
        let name_index = cp.add_utf8(method.name.clone());
        let param_types: Vec<Type> = method.params.iter().map(|p| p.ty.clone()).collect();
        let descriptor = method_descriptor(&param_types, &method.return_type);
        let descriptor_index = cp.add_type_desc(&descriptor);
        let method_ref_index = cp.add_method_ref(this_class, name_index, descriptor_index);
        sigs.insert(
            method.name.clone(),
            MethodSig {
                param_types: param_types.iter().map(expr_ty_of).collect(),
                return_ty: expr_ty_of(&method.return_type),
                method_ref_index,
            },
        );
    }

    let mut methods = Vec::with_capacity(file.class.methods.len());
    for method in &file.class.methods {
        methods.push(compile_method(method, &mut cp, &sigs)?);
    }

    Ok(Module {
        version: nl_bytecode::module::VERSION,
        constant_pool: cp,
        this_class,
        class_flags: 0,
        super_class: 0,
        interfaces: Vec::new(),
        fields: Vec::new(),
        methods,
        hash_algo: HashAlgo::Sha256,
    })
}

fn compile_method(
    method: &MethodDecl,
    cp: &mut ConstantPool,
    sigs: &HashMap<String, MethodSig>,
) -> Result<MethodDescriptor, CodegenError> {
    if !method.is_static {
        return Err(CodegenError::Unsupported(
            "instance methods (object model lands in milestone 5)".to_string(),
        ));
    }

    let name_index = cp.add_utf8(method.name.clone());
    let param_types: Vec<_> = method.params.iter().map(|p| p.ty.clone()).collect();
    let descriptor = method_descriptor(&param_types, &method.return_type);
    let descriptor_index = cp.add_type_desc(&descriptor);

    let mut emitter = Emitter::new(cp, sigs);
    emitter.push_scope();
    for param in &method.params {
        emitter.declare_local(param.name.clone(), expr_ty_of(&param.ty));
    }
    for stmt in &method.body {
        emitter.compile_stmt(stmt)?;
    }
    // Body without a guaranteed trailing return: safe to fall off the end
    // only for void methods (a missing return in a non-void method is a
    // compile error left to milestone 2's control-flow checks).
    if method.return_type == Type::Void {
        emitter.code.push(Opcode::Return as u8);
    }
    emitter.pop_scope();

    let mut flags = 0u16;
    flags |= match method.visibility {
        Visibility::Public => method_flags::PUBLIC,
        Visibility::Protected => method_flags::PROTECTED,
        Visibility::Private => method_flags::PRIVATE,
    };
    flags |= method_flags::STATIC;

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
