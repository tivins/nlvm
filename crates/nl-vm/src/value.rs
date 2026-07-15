use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// A heap-allocated class instance — see nlvm-specs/docs/vm.md § Object
/// layout. Fields are keyed by name rather than a declaration-order offset:
/// simpler than replicating the exact header/offset layout, and equivalent
/// as far as anything observable (no code inspects raw memory layout).
#[derive(Debug)]
pub struct Object {
    pub class_name: String,
    pub fields: HashMap<String, Value>,
}

/// Tagged runtime value — see nlvm-specs/docs/vm.md § Value representation.
#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Int(i64),
    Float(f64),
    Bool(bool),
    Byte(u8),
    Str(Rc<String>),
    Array(Rc<RefCell<Vec<Value>>>),
    Object(Rc<RefCell<Object>>),
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Bool(_) => "bool",
            Value::Byte(_) => "byte",
            Value::Str(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(v) => Some(*v),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn to_display_string(&self) -> String {
        match self {
            Value::Null => "null".to_string(),
            Value::Int(v) => v.to_string(),
            Value::Float(v) => format!("{v}"),
            Value::Bool(v) => v.to_string(),
            Value::Byte(v) => v.to_string(),
            Value::Str(s) => (**s).clone(),
            Value::Array(_) => "[array]".to_string(),
            // Stringable dispatch (calling `toString()`) isn't implemented
            // this phase; nl-codegen never emits TO_STRING for an object
            // operand (string concatenation only accepts primitives/strings
            // — compiler.md's Stringable check is future work), so this is
            // an unreachable fallback, not a real code path.
            Value::Object(obj) => format!("[object {}]", obj.borrow().class_name),
        }
    }
}
