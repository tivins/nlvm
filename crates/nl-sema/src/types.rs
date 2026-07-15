//! Type helpers over `nl_syntax::ast::Type` — union flattening, assignability
//! and numeric widening. See compiler.md §§ Null safety, Type checking.

use nl_syntax::ast::Type;

/// Members of `ty`, treating a non-union type as a single-member list.
pub fn members(ty: &Type) -> Vec<&Type> {
    match ty {
        Type::Union(members) => members.iter().collect(),
        other => vec![other],
    }
}

/// Whether `ty` accepts the `null` literal, i.e. is a union containing the
/// `null` member (`T|null`, `A|B|null`, ...).
pub fn is_nullable(ty: &Type) -> bool {
    members(ty).iter().any(|m| matches!(m, Type::NullT))
}

pub fn is_numeric(ty: &Type) -> bool {
    matches!(ty, Type::Int | Type::Float | Type::Byte)
}

/// Numeric widening lattice: `byte` -> `int` -> `float` (specs.md § Type
/// conversions and casting, implicit conversions table).
fn numeric_rank(ty: &Type) -> Option<u8> {
    match ty {
        Type::Byte => Some(0),
        Type::Int => Some(1),
        Type::Float => Some(2),
        _ => None,
    }
}

/// The common numeric type two operands widen to for arithmetic/comparison,
/// or `None` if either side is not numeric.
pub fn widen_numeric(a: &Type, b: &Type) -> Option<Type> {
    let (ra, rb) = (numeric_rank(a)?, numeric_rank(b)?);
    Some(if ra >= rb { a.clone() } else { b.clone() })
}

/// A single (non-union, non-null) type equals another for assignability
/// purposes: identical primitives, identical class names, or structurally
/// equal arrays.
fn atom_eq(a: &Type, b: &Type) -> bool {
    match (a, b) {
        (Type::Array(ea), Type::Array(eb)) => atom_eq(ea, eb),
        (Type::Named(na), Type::Named(nb)) => na == nb,
        _ => a == b,
    }
}

/// Whether a single (non-union) value type `from` can flow into a single
/// member `to` of a target union, considering implicit numeric widening.
fn atom_assignable(from: &Type, to: &Type) -> bool {
    if atom_eq(from, to) {
        return true;
    }
    matches!(numeric_rank(from).zip(numeric_rank(to)), Some((rf, rt)) if rf <= rt)
}

/// compiler.md § Type checking — is a value of static type `value_ty`
/// assignable to a location of type `target_ty`? Handles the null literal
/// specially (callers distinguish that case for E003 vs. E004) and unions on
/// either side.
pub fn is_assignable(value_ty: &Type, target_ty: &Type) -> bool {
    if matches!(value_ty, Type::NullT) {
        return is_nullable(target_ty);
    }
    let target_members = members(target_ty);
    members(value_ty)
        .iter()
        .all(|vm| {
            if matches!(vm, Type::NullT) {
                return target_members.iter().any(|tm| matches!(tm, Type::NullT));
            }
            target_members.iter().any(|tm| atom_assignable(vm, tm))
        })
}

/// Human-readable type name for error messages (E003/E004/E008/E009).
pub fn display(ty: &Type) -> String {
    match ty {
        Type::Int => "int".to_string(),
        Type::Float => "float".to_string(),
        Type::Bool => "bool".to_string(),
        Type::Byte => "byte".to_string(),
        Type::StringT => "string".to_string(),
        Type::Void => "void".to_string(),
        Type::NullT => "null".to_string(),
        Type::Array(inner) => format!("{}[]", display(inner)),
        Type::Named(name) => name.clone(),
        Type::Union(members) => members.iter().map(display).collect::<Vec<_>>().join("|"),
    }
}
