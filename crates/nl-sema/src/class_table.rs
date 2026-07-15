//! Cross-file class/interface table, mirroring `nl_codegen::class_table`
//! (kept as a separate, independent view rather than a shared dependency —
//! sema only needs enough of it for lenient existence/type-shape checks).

use std::collections::HashMap;

use nl_syntax::ast::{MethodKind, SourceFile, SourceItem, Type};

#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub name: String,
    pub params: Vec<Type>,
    pub return_ty: Type,
}

#[derive(Debug, Clone)]
pub struct ClassInfo {
    /// Resolved FQCNs of directly implemented interfaces (classes only, no
    /// transitivity through interface-`extends` — out of scope this phase).
    pub implements: Vec<String>,
    pub fields: Vec<FieldInfo>,
    pub methods: Vec<MethodInfo>,
}

pub type ClassTable = HashMap<String, ClassInfo>;

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
/// imports (required even within the same namespace — see `nl-codegen`'s
/// equivalent for the fixtures that confirm this).
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

pub fn resolve_type(ty: &Type, imports: &HashMap<String, String>) -> Type {
    match ty {
        Type::Named(name) => Type::Named(imports.get(name).cloned().unwrap_or_else(|| name.clone())),
        Type::Array(inner) => Type::Array(Box::new(resolve_type(inner, imports))),
        Type::Union(members) => Type::Union(members.iter().map(|m| resolve_type(m, imports)).collect()),
        other => other.clone(),
    }
}

pub fn build_class_table(files: &[SourceFile]) -> ClassTable {
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
                let methods = class
                    .methods
                    .iter()
                    .filter(|m| m.kind == MethodKind::Normal)
                    .map(|m| MethodInfo {
                        name: m.name.clone(),
                        params: m.params.iter().map(|p| resolve_type(&p.ty, &imports)).collect(),
                        return_ty: resolve_type(&m.return_type, &imports),
                    })
                    .collect();
                let implements = class
                    .implements
                    .iter()
                    .map(|n| imports.get(n).cloned().unwrap_or_else(|| n.clone()))
                    .collect();
                ClassInfo { implements, fields, methods }
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
                ClassInfo { implements: Vec::new(), fields: Vec::new(), methods }
            }
        };
        table.insert(fqcn, info);
    }
    table
}
