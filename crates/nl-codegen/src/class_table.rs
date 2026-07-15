//! Cross-file class/interface table — built once from every `SourceFile` in
//! a program so codegen can resolve `new`, field access, and instance method
//! calls that reference a class defined in a different file. Mirrors the
//! (deliberately lenient) approach `nl-sema` takes for cross-file lookups:
//! this crate owns its own view rather than depending on `nl-sema`.

use std::collections::HashMap;

use nl_syntax::ast::{MethodKind, SourceFile, SourceItem, Type};

#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub name: String,
    /// Resolved (FQCN, not source-simple-name) type.
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub struct CtorInfo {
    /// Resolved parameter types.
    pub params: Vec<Type>,
}

#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub name: String,
    /// Resolved parameter types.
    pub params: Vec<Type>,
    /// Resolved return type.
    pub return_ty: Type,
}

#[derive(Debug, Clone)]
pub struct ClassInfo {
    pub fields: Vec<FieldInfo>,
    pub ctors: Vec<CtorInfo>,
    pub methods: Vec<MethodInfo>,
}

pub fn fqcn_of(file: &SourceFile) -> String {
    let name = match &file.item {
        SourceItem::Class(c) => c.name.as_str(),
        SourceItem::Interface(i) => i.name.as_str(),
    };
    if file.namespace.is_empty() {
        name.to_string()
    } else {
        format!("{}.{}", file.namespace.join("."), name)
    }
}

/// Simple name -> FQCN, from this file's own declaration plus its `use`
/// imports. `use` is required even for classes in the same namespace (see
/// `m5_0020`/`m5_0030` fixtures), so no implicit same-namespace visibility.
pub fn import_map(file: &SourceFile) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let fqcn = fqcn_of(file);
    let simple = match &file.item {
        SourceItem::Class(c) => c.name.clone(),
        SourceItem::Interface(i) => i.name.clone(),
    };
    map.insert(simple, fqcn);
    for u in &file.uses {
        if let Some(simple) = u.rsplit('.').next() {
            map.insert(simple.to_string(), u.clone());
        }
    }
    map
}

/// Resolves every `Named` component of `ty` from a simple name to its FQCN
/// using `imports`; unresolvable names are left as-is (lenient — surfaces as
/// a clear "unknown class" error at the point of use, not here).
pub fn resolve_type(ty: &Type, imports: &HashMap<String, String>) -> Type {
    match ty {
        Type::Named(name) => Type::Named(imports.get(name).cloned().unwrap_or_else(|| name.clone())),
        Type::Array(inner) => Type::Array(Box::new(resolve_type(inner, imports))),
        Type::Union(members) => Type::Union(members.iter().map(|m| resolve_type(m, imports)).collect()),
        other => other.clone(),
    }
}

pub fn build_class_table(files: &[SourceFile]) -> HashMap<String, ClassInfo> {
    let mut table = HashMap::with_capacity(files.len());
    for file in files {
        let fqcn = fqcn_of(file);
        let imports = import_map(file);
        let info = match &file.item {
            SourceItem::Class(class) => {
                let fields = class
                    .fields
                    .iter()
                    .map(|f| FieldInfo {
                        name: f.name.clone(),
                        ty: resolve_type(&f.ty, &imports),
                    })
                    .collect();

                let mut ctors = Vec::new();
                let mut methods = Vec::new();
                for m in &class.methods {
                    let params: Vec<Type> = m.params.iter().map(|p| resolve_type(&p.ty, &imports)).collect();
                    match m.kind {
                        MethodKind::Constructor => ctors.push(CtorInfo { params }),
                        MethodKind::Destructor => {}
                        MethodKind::Normal => methods.push(MethodInfo {
                            name: m.name.clone(),
                            params,
                            return_ty: resolve_type(&m.return_type, &imports),
                        }),
                    }
                }

                ClassInfo { fields, ctors, methods }
            }
            SourceItem::Interface(iface) => {
                let methods = iface
                    .methods
                    .iter()
                    .map(|m| MethodInfo {
                        name: m.name.clone(),
                        params: m.params.iter().map(|p| resolve_type(&p.ty, &imports)).collect(),
                        return_ty: resolve_type(&m.return_type, &imports),
                    })
                    .collect();
                ClassInfo { fields: Vec::new(), ctors: Vec::new(), methods }
            }
        };
        table.insert(fqcn, info);
    }
    table
}

/// Best-effort overload resolution: matches by argument count only. Good
/// enough while the only overloads in scope (constructor chaining) are
/// distinguished by arity; ambiguous same-arity overloads pick the first
/// declared, which is a known, documented limitation of this phase.
pub fn find_ctor<'c>(classes: &'c HashMap<String, ClassInfo>, fqcn: &str, argc: usize) -> Option<&'c CtorInfo> {
    classes.get(fqcn)?.ctors.iter().find(|c| c.params.len() == argc)
}

pub fn find_method<'c>(
    classes: &'c HashMap<String, ClassInfo>,
    fqcn: &str,
    name: &str,
    argc: usize,
) -> Option<&'c MethodInfo> {
    classes
        .get(fqcn)?
        .methods
        .iter()
        .find(|m| m.name == name && m.params.len() == argc)
}
