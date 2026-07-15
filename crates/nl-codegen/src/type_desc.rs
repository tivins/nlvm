use nl_syntax::ast::Type;

pub fn type_descriptor(ty: &Type) -> String {
    match ty {
        Type::Int => "int".to_string(),
        Type::Float => "float".to_string(),
        Type::Bool => "bool".to_string(),
        Type::Byte => "byte".to_string(),
        Type::StringT => "string".to_string(),
        Type::Void => "void".to_string(),
        Type::Array(inner) => format!("{}[]", type_descriptor(inner)),
        Type::Named(name) => name.clone(),
        Type::NullT => "null".to_string(),
        Type::Union(members) => members.iter().map(type_descriptor).collect::<Vec<_>>().join("|"),
    }
}

pub fn method_descriptor(params: &[Type], return_type: &Type) -> String {
    let params_str = params
        .iter()
        .map(type_descriptor)
        .collect::<Vec<_>>()
        .join(", ");
    format!("({}) -> {}", params_str, type_descriptor(return_type))
}
