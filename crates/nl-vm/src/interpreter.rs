use std::rc::Rc;

use nl_bytecode::{ConstantPoolEntry, MethodDescriptor, Module, Opcode};

use crate::error::VmError;
use crate::value::Value;

#[allow(unused_assignments)] // GOTO/GOTO_W always overwrite `pc` right after reading their operand
pub fn call_static(module: &Module, method: &MethodDescriptor, args: Vec<Value>) -> Result<Option<Value>, VmError> {
    let mut locals = vec![Value::Null; method.max_locals as usize];
    for (i, arg) in args.into_iter().enumerate() {
        if i < locals.len() {
            locals[i] = arg;
        }
    }
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

            Opcode::InvokeStatic => {
                let method_ref_idx = read_u16!();
                let (name, descriptor) = resolve_method_ref(module, method_ref_idx)?;
                let param_count = count_params(&descriptor);
                if stack.len() < param_count {
                    return Err(VmError::Malformed("stack underflow on INVOKE_STATIC"));
                }
                let call_args = stack.split_off(stack.len() - param_count);
                let target = module
                    .find_method(&name)
                    .ok_or_else(|| VmError::MethodNotFound(name.clone()))?;
                if let Some(result) = call_static(module, target, call_args)? {
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

fn resolve_method_ref(module: &Module, idx: u16) -> Result<(String, String), VmError> {
    match module.constant_pool.get(idx) {
        Some(ConstantPoolEntry::MethodRef {
            name_index,
            descriptor_index,
            ..
        }) => {
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
            Ok((name, descriptor))
        }
        _ => Err(VmError::Malformed("method_ref index does not point to a MethodRef")),
    }
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
