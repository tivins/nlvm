use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use nl_bytecode::{ConstantPoolEntry, MethodDescriptor, Module, Opcode};

use crate::error::VmError;
use crate::program::Program;
use crate::value::{Object, Value};

pub fn call_static(
    program: &Program,
    module: &Module,
    method: &MethodDescriptor,
    args: Vec<Value>,
) -> Result<Option<Value>, VmError> {
    let mut locals = vec![Value::Null; method.max_locals as usize];
    for (i, arg) in args.into_iter().enumerate() {
        if i < locals.len() {
            locals[i] = arg;
        }
    }
    run_frame(program, module, method, locals)
}

/// Shared by `INVOKE_INSTANCE` and `INVOKE_SPECIAL`: local 0 is the receiver
/// (`this`), parameters follow starting at local 1 — vm.md § Call frame and
/// operand stack.
pub fn call_instance(
    program: &Program,
    module: &Module,
    method: &MethodDescriptor,
    receiver: Value,
    args: Vec<Value>,
) -> Result<Option<Value>, VmError> {
    let mut locals = vec![Value::Null; method.max_locals as usize];
    locals[0] = receiver;
    for (i, arg) in args.into_iter().enumerate() {
        if i + 1 < locals.len() {
            locals[i + 1] = arg;
        }
    }
    run_frame(program, module, method, locals)
}

#[allow(unused_assignments)] // GOTO/GOTO_W always overwrite `pc` right after reading their operand
fn run_frame(
    program: &Program,
    module: &Module,
    method: &MethodDescriptor,
    mut locals: Vec<Value>,
) -> Result<Option<Value>, VmError> {
    let mut stack: Vec<Value> = Vec::with_capacity(method.max_stack as usize);
    let code = &method.code;
    let mut pc: usize = 0;

    loop {
        if pc >= code.len() {
            return Ok(None);
        }
        let opcode_pc = pc;
        let byte = code[pc];
        pc += 1;
        let op = Opcode::from_u8(byte).ok_or(VmError::UnknownOpcode(byte))?;

        macro_rules! read_u8 {
            () => {{
                let v = code[pc];
                pc += 1;
                v
            }};
        }
        macro_rules! read_i8 {
            () => {
                read_u8!() as i8
            };
        }
        macro_rules! read_u16 {
            () => {{
                let v = u16::from_be_bytes([code[pc], code[pc + 1]]);
                pc += 2;
                v
            }};
        }
        macro_rules! read_i16 {
            () => {{
                let v = i16::from_be_bytes([code[pc], code[pc + 1]]);
                pc += 2;
                v
            }};
        }

        match op {
            Opcode::Nop => {}
            Opcode::Pop => {
                stack.pop();
            }
            Opcode::Dup => {
                let v = stack.last().cloned().ok_or(VmError::Malformed("stack underflow"))?;
                stack.push(v);
            }
            Opcode::Swap => {
                let len = stack.len();
                if len < 2 {
                    return Err(VmError::Malformed("stack underflow"));
                }
                stack.swap(len - 1, len - 2);
            }
            Opcode::DupX1 => {
                let len = stack.len();
                if len < 2 {
                    return Err(VmError::Malformed("stack underflow"));
                }
                let top = stack[len - 1].clone();
                stack.insert(len - 2, top);
            }

            Opcode::ConstNull => stack.push(Value::Null),
            Opcode::ConstTrue => stack.push(Value::Bool(true)),
            Opcode::ConstFalse => stack.push(Value::Bool(false)),
            Opcode::ConstIZero => stack.push(Value::Int(0)),
            Opcode::ConstIOne => stack.push(Value::Int(1)),
            Opcode::ConstFZero => stack.push(Value::Float(0.0)),
            Opcode::ConstFOne => stack.push(Value::Float(1.0)),
            Opcode::BiPush => {
                let v = read_i8!();
                stack.push(Value::Int(v as i64));
            }
            Opcode::SiPush => {
                let v = read_i16!();
                stack.push(Value::Int(v as i64));
            }
            Opcode::Ldc => {
                let idx = read_u16!();
                let entry = module
                    .constant_pool
                    .get(idx)
                    .ok_or(VmError::Malformed("bad LDC index"))?;
                let value = match entry {
                    ConstantPoolEntry::Int(v) => Value::Int(*v),
                    ConstantPoolEntry::Float(v) => Value::Float(*v),
                    ConstantPoolEntry::Utf8(s) => Value::Str(Rc::new(s.clone())),
                    _ => return Err(VmError::Malformed("LDC target is not a loadable constant")),
                };
                stack.push(value);
            }

            Opcode::Load => {
                let idx = read_u16!();
                stack.push(locals[idx as usize].clone());
            }
            Opcode::Store => {
                let idx = read_u16!();
                let v = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
                locals[idx as usize] = v;
            }
            Opcode::Load0 => stack.push(locals[0].clone()),
            Opcode::Load1 => stack.push(locals[1].clone()),
            Opcode::Load2 => stack.push(locals[2].clone()),
            Opcode::Load3 => stack.push(locals[3].clone()),
            Opcode::Store0 => locals[0] = stack.pop().ok_or(VmError::Malformed("stack underflow"))?,
            Opcode::Store1 => locals[1] = stack.pop().ok_or(VmError::Malformed("stack underflow"))?,
            Opcode::Store2 => locals[2] = stack.pop().ok_or(VmError::Malformed("stack underflow"))?,
            Opcode::Store3 => locals[3] = stack.pop().ok_or(VmError::Malformed("stack underflow"))?,

            Opcode::IAdd => int_binop(&mut stack, |a, b| Ok(a.wrapping_add(b)))?,
            Opcode::ISub => int_binop(&mut stack, |a, b| Ok(a.wrapping_sub(b)))?,
            Opcode::IMul => int_binop(&mut stack, |a, b| Ok(a.wrapping_mul(b)))?,
            Opcode::IDiv => int_binop(&mut stack, |a, b| {
                if b == 0 {
                    Err(VmError::DivisionByZero)
                } else {
                    Ok(a.wrapping_div(b))
                }
            })?,
            Opcode::IMod => int_binop(&mut stack, |a, b| {
                if b == 0 {
                    Err(VmError::DivisionByZero)
                } else {
                    Ok(a.wrapping_rem(b))
                }
            })?,
            Opcode::INeg => {
                let a = pop_int(&mut stack)?;
                stack.push(Value::Int(a.wrapping_neg()));
            }
            Opcode::IInc => {
                let idx = read_u16!();
                let delta = read_i16!();
                let cur = locals[idx as usize]
                    .as_int()
                    .ok_or(VmError::Malformed("IINC on non-int local"))?;
                locals[idx as usize] = Value::Int(cur.wrapping_add(delta as i64));
            }

            Opcode::FAdd => float_binop(&mut stack, |a, b| a + b)?,
            Opcode::FSub => float_binop(&mut stack, |a, b| a - b)?,
            Opcode::FMul => float_binop(&mut stack, |a, b| a * b)?,
            Opcode::FDiv => float_binop(&mut stack, |a, b| a / b)?,
            Opcode::FMod => float_binop(&mut stack, |a, b| a % b)?,
            Opcode::FNeg => {
                let a = pop_float(&mut stack)?;
                stack.push(Value::Float(-a));
            }

            Opcode::I2F => {
                let a = pop_int(&mut stack)?;
                stack.push(Value::Float(a as f64));
            }
            Opcode::F2I => {
                let a = pop_float(&mut stack)?;
                let clamped = if a.is_nan() {
                    0
                } else {
                    a.trunc().clamp(i64::MIN as f64, i64::MAX as f64) as i64
                };
                stack.push(Value::Int(clamped));
            }
            Opcode::I2B => {
                let a = pop_int(&mut stack)?;
                stack.push(Value::Byte((a & 0xFF) as u8));
            }
            Opcode::B2I => {
                let v = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
                let b = match v {
                    Value::Byte(b) => b,
                    _ => return Err(VmError::Malformed("B2I on non-byte")),
                };
                stack.push(Value::Int(b as i64));
            }
            Opcode::ToString => {
                let v = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
                if v.is_null() {
                    return Err(VmError::NullPointer);
                }
                stack.push(Value::Str(Rc::new(v.to_display_string())));
            }

            Opcode::CmpEq => {
                let (a, b) = pop2(&mut stack)?;
                stack.push(Value::Bool(values_equal(&a, &b)));
            }
            Opcode::CmpNe => {
                let (a, b) = pop2(&mut stack)?;
                stack.push(Value::Bool(!values_equal(&a, &b)));
            }
            Opcode::CmpLt => {
                let ord = compare(&mut stack)?;
                stack.push(Value::Bool(ord == std::cmp::Ordering::Less));
            }
            Opcode::CmpGt => {
                let ord = compare(&mut stack)?;
                stack.push(Value::Bool(ord == std::cmp::Ordering::Greater));
            }
            Opcode::CmpLe => {
                let ord = compare(&mut stack)?;
                stack.push(Value::Bool(ord != std::cmp::Ordering::Greater));
            }
            Opcode::CmpGe => {
                let ord = compare(&mut stack)?;
                stack.push(Value::Bool(ord != std::cmp::Ordering::Less));
            }
            Opcode::CmpThreeWay => {
                let ord = compare(&mut stack)?;
                stack.push(Value::Int(match ord {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                }));
            }
            Opcode::IsNull => {
                let v = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
                stack.push(Value::Bool(v.is_null()));
            }
            Opcode::IsNonNull => {
                let v = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
                stack.push(Value::Bool(!v.is_null()));
            }
            Opcode::Not => {
                let v = pop_bool(&mut stack)?;
                stack.push(Value::Bool(!v));
            }

            Opcode::IfTrue => {
                let offset = read_i16!();
                let v = pop_bool(&mut stack)?;
                if v {
                    pc = (opcode_pc as i64 + offset as i64) as usize;
                }
            }
            Opcode::IfFalse => {
                let offset = read_i16!();
                let v = pop_bool(&mut stack)?;
                if !v {
                    pc = (opcode_pc as i64 + offset as i64) as usize;
                }
            }
            Opcode::Goto => {
                let offset = read_i16!();
                pc = (opcode_pc as i64 + offset as i64) as usize;
            }
            Opcode::GotoW => {
                let offset = i32::from_be_bytes([code[pc], code[pc + 1], code[pc + 2], code[pc + 3]]);
                pc += 4;
                pc = (opcode_pc as i64 + offset as i64) as usize;
            }

            Opcode::New => {
                let class_index = read_u16!();
                let fqcn = resolve_class_name(module, class_index)?.to_string();
                let target_module = program
                    .get(&fqcn)
                    .ok_or_else(|| VmError::MethodNotFound(fqcn.clone()))?;
                let mut fields = HashMap::with_capacity(target_module.fields.len());
                for f in &target_module.fields {
                    let name = target_module
                        .constant_pool
                        .utf8_at(f.name_index)
                        .ok_or(VmError::Malformed("bad field name index"))?
                        .to_string();
                    let type_desc = target_module
                        .constant_pool
                        .type_desc_at(f.type_index)
                        .ok_or(VmError::Malformed("bad field type index"))?;
                    fields.insert(name, default_value_for(type_desc));
                }
                stack.push(Value::Object(Rc::new(RefCell::new(Object {
                    class_name: fqcn,
                    fields,
                }))));
            }
            Opcode::InstanceOf => {
                let class_index = read_u16!();
                let target_fqcn = resolve_class_name(module, class_index)?;
                let v = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
                let result = match &v {
                    Value::Object(obj) => {
                        let runtime_class = obj.borrow().class_name.clone();
                        runtime_class == target_fqcn || implements_interface(program, &runtime_class, target_fqcn)
                    }
                    _ => false,
                };
                stack.push(Value::Bool(result));
            }
            Opcode::CheckCast => {
                return Err(VmError::Unsupported(format!("{op:?} lands in a later phase")));
            }

            Opcode::NewArray => {
                let type_index = read_u16!();
                let elem_desc = module.constant_pool.type_desc_at(type_index).unwrap_or("");
                let default = default_value_for(elem_desc);
                let size = pop_int(&mut stack)?;
                if size < 0 {
                    return Err(VmError::Malformed("negative array size"));
                }
                stack.push(Value::Array(Rc::new(RefCell::new(vec![default; size as usize]))));
            }
            Opcode::NewArrayInit => {
                return Err(VmError::Unsupported(format!("{op:?} lands in a later phase")));
            }
            Opcode::ArrayLoad => {
                let (arr, idx) = pop2(&mut stack)?;
                let Value::Array(arr) = arr else {
                    return Err(VmError::Malformed("ARRAY_LOAD on non-array"));
                };
                let idx = idx.as_int().ok_or(VmError::Malformed("array index must be int"))?;
                let arr_ref = arr.borrow();
                if idx < 0 || idx as usize >= arr_ref.len() {
                    return Err(VmError::IndexOutOfBounds { index: idx, length: arr_ref.len() });
                }
                stack.push(arr_ref[idx as usize].clone());
            }
            Opcode::ArrayStore => {
                let value = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
                let idx = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
                let arr = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
                let Value::Array(arr) = arr else {
                    return Err(VmError::Malformed("ARRAY_STORE on non-array"));
                };
                let idx = idx.as_int().ok_or(VmError::Malformed("array index must be int"))?;
                let mut arr_mut = arr.borrow_mut();
                if idx < 0 || idx as usize >= arr_mut.len() {
                    return Err(VmError::IndexOutOfBounds { index: idx, length: arr_mut.len() });
                }
                arr_mut[idx as usize] = value;
            }
            Opcode::ArrayLength => {
                let v = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
                let Value::Array(arr) = v else {
                    return Err(VmError::Malformed("ARRAY_LENGTH on non-array"));
                };
                stack.push(Value::Int(arr.borrow().len() as i64));
            }

            Opcode::GetField => {
                let idx = read_u16!();
                let (_, field_name, _) = resolve_field_ref(module, idx)?;
                let receiver = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
                if receiver.is_null() {
                    return Err(VmError::NullPointer);
                }
                let Value::Object(obj) = receiver else {
                    return Err(VmError::Malformed("GET_FIELD on non-object"));
                };
                let value = obj.borrow().fields.get(&field_name).cloned().unwrap_or(Value::Null);
                stack.push(value);
            }
            Opcode::SetField => {
                let idx = read_u16!();
                let (_, field_name, _) = resolve_field_ref(module, idx)?;
                let value = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
                let receiver = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
                if receiver.is_null() {
                    return Err(VmError::NullPointer);
                }
                let Value::Object(obj) = receiver else {
                    return Err(VmError::Malformed("SET_FIELD on non-object"));
                };
                obj.borrow_mut().fields.insert(field_name, value);
            }
            Opcode::GetStatic | Opcode::SetStatic => {
                return Err(VmError::Unsupported(format!("{op:?} lands in a later phase")));
            }

            Opcode::InvokeStatic => {
                let method_ref_idx = read_u16!();
                let (class_fqcn, name, descriptor) = resolve_method_ref(module, method_ref_idx)?;
                let param_count = count_params(&descriptor);
                if stack.len() < param_count {
                    return Err(VmError::Malformed("stack underflow on INVOKE_STATIC"));
                }
                let call_args = stack.split_off(stack.len() - param_count);
                let target_module = program
                    .get(&class_fqcn)
                    .ok_or_else(|| VmError::MethodNotFound(format!("{class_fqcn}.{name}")))?;
                let target = target_module
                    .find_method_by_descriptor(&name, &descriptor)
                    .ok_or_else(|| VmError::MethodNotFound(name.clone()))?;
                if let Some(result) = call_static(program, target_module, target, call_args)? {
                    stack.push(result);
                }
            }
            Opcode::InvokeInstance => {
                let method_ref_idx = read_u16!();
                let (_static_fqcn, name, descriptor) = resolve_method_ref(module, method_ref_idx)?;
                let param_count = count_params(&descriptor);
                if stack.len() < param_count + 1 {
                    return Err(VmError::Malformed("stack underflow on INVOKE_INSTANCE"));
                }
                let call_args = stack.split_off(stack.len() - param_count);
                let receiver = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
                if receiver.is_null() {
                    return Err(VmError::NullPointer);
                }
                // Virtual dispatch: resolve against the receiver's *runtime*
                // class, not the static type recorded in the method ref —
                // vm.md § Method dispatch, Instance methods.
                let Value::Object(obj) = &receiver else {
                    return Err(VmError::Malformed("INVOKE_INSTANCE on non-object"));
                };
                let runtime_class = obj.borrow().class_name.clone();
                let target_module = program
                    .get(&runtime_class)
                    .ok_or_else(|| VmError::MethodNotFound(format!("{runtime_class}.{name}")))?;
                let target = target_module
                    .find_method_by_descriptor(&name, &descriptor)
                    .ok_or_else(|| VmError::MethodNotFound(format!("{runtime_class}.{name}")))?;
                if let Some(result) = call_instance(program, target_module, target, receiver, call_args)? {
                    stack.push(result);
                }
            }
            Opcode::InvokeSpecial => {
                let method_ref_idx = read_u16!();
                let (class_fqcn, name, descriptor) = resolve_method_ref(module, method_ref_idx)?;
                let param_count = count_params(&descriptor);
                if stack.len() < param_count + 1 {
                    return Err(VmError::Malformed("stack underflow on INVOKE_SPECIAL"));
                }
                let call_args = stack.split_off(stack.len() - param_count);
                let receiver = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
                if receiver.is_null() {
                    return Err(VmError::NullPointer);
                }
                // No virtual dispatch: always the exact class named in the
                // ref (constructors, `super`/private calls in later phases).
                let target_module = program
                    .get(&class_fqcn)
                    .ok_or_else(|| VmError::MethodNotFound(format!("{class_fqcn}.{name}")))?;
                let target = target_module
                    .find_method_by_descriptor(&name, &descriptor)
                    .ok_or_else(|| VmError::MethodNotFound(format!("{class_fqcn}.{name}")))?;
                if let Some(result) = call_instance(program, target_module, target, receiver, call_args)? {
                    stack.push(result);
                }
            }

            Opcode::StrConcat => {
                let (a, b) = pop2(&mut stack)?;
                let sa = as_string(&a)?;
                let sb = as_string(&b)?;
                stack.push(Value::Str(Rc::new(format!("{sa}{sb}"))));
            }

            Opcode::Return => return Ok(None),
            Opcode::ReturnValue => {
                let v = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
                return Ok(Some(v));
            }

            other => {
                return Err(VmError::Unsupported(format!(
                    "{other:?} lands in a later milestone"
                )))
            }
        }
    }
}

fn resolve_class_name(module: &Module, idx: u16) -> Result<&str, VmError> {
    module.constant_pool.class_name_at(idx).ok_or(VmError::Malformed("bad class index"))
}

fn resolve_field_ref(module: &Module, idx: u16) -> Result<(String, String, String), VmError> {
    match module.constant_pool.get(idx) {
        Some(ConstantPoolEntry::FieldRef { class_index, name_index, type_index }) => {
            let class_name = module
                .constant_pool
                .class_name_at(*class_index)
                .ok_or(VmError::Malformed("bad field class index"))?
                .to_string();
            let field_name = module
                .constant_pool
                .utf8_at(*name_index)
                .ok_or(VmError::Malformed("bad field name index"))?
                .to_string();
            let type_desc = module
                .constant_pool
                .type_desc_at(*type_index)
                .ok_or(VmError::Malformed("bad field type index"))?
                .to_string();
            Ok((class_name, field_name, type_desc))
        }
        _ => Err(VmError::Malformed("field_ref index does not point to a FieldRef")),
    }
}

fn resolve_method_ref(module: &Module, idx: u16) -> Result<(String, String, String), VmError> {
    match module.constant_pool.get(idx) {
        Some(ConstantPoolEntry::MethodRef { class_index, name_index, descriptor_index }) => {
            let class_name = module
                .constant_pool
                .class_name_at(*class_index)
                .ok_or(VmError::Malformed("bad method class index"))?
                .to_string();
            let name = module
                .constant_pool
                .utf8_at(*name_index)
                .ok_or(VmError::Malformed("bad method name index"))?
                .to_string();
            let descriptor = module
                .constant_pool
                .type_desc_at(*descriptor_index)
                .ok_or(VmError::Malformed("bad method descriptor index"))?
                .to_string();
            Ok((class_name, name, descriptor))
        }
        _ => Err(VmError::Malformed("method_ref index does not point to a MethodRef")),
    }
}

/// Default value for a field/array-element type descriptor — specs.md §
/// Null, initialization, and default values.
fn default_value_for(type_desc: &str) -> Value {
    match type_desc {
        "int" => Value::Int(0),
        "float" => Value::Float(0.0),
        "bool" => Value::Bool(false),
        "byte" => Value::Byte(0),
        "string" => Value::Str(Rc::new(String::new())),
        // Arrays, objects, and unions all default to `null`.
        _ => Value::Null,
    }
}

fn implements_interface(program: &Program, class_fqcn: &str, target_fqcn: &str) -> bool {
    let Some(module) = program.get(class_fqcn) else {
        return false;
    };
    module
        .interfaces
        .iter()
        .any(|&i| module.constant_pool.class_name_at(i) == Some(target_fqcn))
}

fn count_params(descriptor: &str) -> usize {
    let Some(inner) = descriptor
        .strip_prefix('(')
        .and_then(|rest| rest.find(") -> ").map(|end| &rest[..end]))
    else {
        return 0;
    };
    if inner.trim().is_empty() {
        0
    } else {
        inner.split(", ").count()
    }
}

fn pop2(stack: &mut Vec<Value>) -> Result<(Value, Value), VmError> {
    let b = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
    let a = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
    Ok((a, b))
}

fn pop_int(stack: &mut Vec<Value>) -> Result<i64, VmError> {
    stack
        .pop()
        .and_then(|v| v.as_int())
        .ok_or(VmError::Malformed("expected int on stack"))
}

fn pop_float(stack: &mut Vec<Value>) -> Result<f64, VmError> {
    stack
        .pop()
        .and_then(|v| v.as_float())
        .ok_or(VmError::Malformed("expected float on stack"))
}

fn pop_bool(stack: &mut Vec<Value>) -> Result<bool, VmError> {
    stack
        .pop()
        .and_then(|v| v.as_bool())
        .ok_or(VmError::Malformed("expected bool on stack"))
}

fn int_binop(stack: &mut Vec<Value>, f: impl Fn(i64, i64) -> Result<i64, VmError>) -> Result<(), VmError> {
    let (a, b) = pop2(stack)?;
    let a = a.as_int().ok_or(VmError::Malformed("expected int operand"))?;
    let b = b.as_int().ok_or(VmError::Malformed("expected int operand"))?;
    stack.push(Value::Int(f(a, b)?));
    Ok(())
}

fn float_binop(stack: &mut Vec<Value>, f: impl Fn(f64, f64) -> f64) -> Result<(), VmError> {
    let (a, b) = pop2(stack)?;
    let a = a.as_float().ok_or(VmError::Malformed("expected float operand"))?;
    let b = b.as_float().ok_or(VmError::Malformed("expected float operand"))?;
    stack.push(Value::Float(f(a, b)));
    Ok(())
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Null, _) | (_, Value::Null) => false,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Byte(x), Value::Byte(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => Rc::ptr_eq(x, y),
        (Value::Object(x), Value::Object(y)) => Rc::ptr_eq(x, y),
        _ => false,
    }
}

fn compare(stack: &mut Vec<Value>) -> Result<std::cmp::Ordering, VmError> {
    let (a, b) = pop2(stack)?;
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok(x.cmp(&y)),
        (Value::Float(x), Value::Float(y)) => {
            x.partial_cmp(&y).ok_or(VmError::Malformed("NaN comparison"))
        }
        (Value::Byte(x), Value::Byte(y)) => Ok(x.cmp(&y)),
        (Value::Str(x), Value::Str(y)) => Ok(x.cmp(&y)),
        _ => Err(VmError::Malformed("incomparable operands")),
    }
}

fn as_string(v: &Value) -> Result<String, VmError> {
    match v {
        Value::Str(s) => Ok((**s).clone()),
        _ => Err(VmError::Malformed("STR_CONCAT operand is not a string")),
    }
}
