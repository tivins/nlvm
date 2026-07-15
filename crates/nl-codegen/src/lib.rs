pub mod error;
mod expr;
mod type_desc;

use nl_bytecode::{method_flags, ConstantPool, HashAlgo, MethodDescriptor, Module, Opcode};
use nl_syntax::ast::{MethodDecl, SourceFile, Stmt, Visibility};

pub use error::CodegenError;
use expr::Emitter;
use type_desc::method_descriptor;

pub fn compile_source_file(file: &SourceFile) -> Result<Module, CodegenError> {
    let mut cp = ConstantPool::new();

    let fqcn = if file.namespace.is_empty() {
        file.class.name.clone()
    } else {
        format!("{}.{}", file.namespace.join("."), file.class.name)
    };
    let this_class = cp.add_class(&fqcn);

    let mut methods = Vec::with_capacity(file.class.methods.len());
    for method in &file.class.methods {
        methods.push(compile_method(method, &mut cp)?);
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

fn compile_method(method: &MethodDecl, cp: &mut ConstantPool) -> Result<MethodDescriptor, CodegenError> {
    if !method.is_static {
        return Err(CodegenError::Unsupported(
            "instance methods (object model lands in milestone 5)".to_string(),
        ));
    }

    let name_index = cp.add_utf8(method.name.clone());
    let param_types: Vec<_> = method.params.iter().map(|p| p.ty.clone()).collect();
    let descriptor = method_descriptor(&param_types, &method.return_type);
    let descriptor_index = cp.add_type_desc(&descriptor);

    let mut emitter = Emitter::new(cp);
    for stmt in &method.body {
        compile_stmt(stmt, &mut emitter)?;
    }
    // Body without a trailing explicit return in a void method: fall off the end.
    if matches!(method.body.last(), None | Some(Stmt::Expr(_))) {
        if method.return_type == nl_syntax::ast::Type::Void {
            emitter.code.push(Opcode::Return as u8);
        }
    }

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
        max_locals: method.params.len() as u16,
        max_stack: emitter.max_stack(),
        code: emitter.code,
        exception_table: Vec::new(),
        line_table: Vec::new(),
    })
}

fn compile_stmt(stmt: &Stmt, emitter: &mut Emitter) -> Result<(), CodegenError> {
    match stmt {
        Stmt::Return(Some(expr)) => {
            emitter.compile_expr(expr)?;
            emitter.code.push(Opcode::ReturnValue as u8);
        }
        Stmt::Return(None) => {
            emitter.code.push(Opcode::Return as u8);
        }
        Stmt::Expr(expr) => {
            emitter.compile_expr(expr)?;
            emitter.code.push(Opcode::Pop as u8);
        }
    }
    Ok(())
}
