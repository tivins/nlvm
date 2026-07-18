use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use nl_bytecode::{
    class_flags, field_flags, method_flags, ConstantPoolEntry, MethodDescriptor, Module, Opcode,
};

use crate::error::VmError;
use crate::program::Program;
use crate::value::{lock, Object, Value};

pub fn call_static(
    program: &Arc<Program>,
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
    program: &Arc<Program>,
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

/// One instruction's outcome — `exec_step` executes exactly one opcode so
/// `run_frame`'s loop can intercept a `VmError::Thrown` between
/// instructions and consult `method.exception_table` before either
/// resuming at a handler or propagating the exception to the caller (vm.md
/// § Throw and stack unwinding).
enum Step {
    Continue,
    Return(Option<Value>),
}

fn run_frame(
    program: &Arc<Program>,
    module: &Module,
    method: &MethodDescriptor,
    mut locals: Vec<Value>,
) -> Result<Option<Value>, VmError> {
    let mut stack: Vec<Value> = Vec::with_capacity(method.max_stack as usize);
    let code = &method.code;
    let mut pc: usize = 0;
    // vm.md § SET_FIELD: readonly enforcement is exempted only inside the
    // declaring class's own constructor — computed once per frame since it
    // never changes across instructions.
    let is_constructor = method.flags & method_flags::CONSTRUCTOR != 0;

    // vm.md § Stack trace construction — this frame is on the shadow call
    // stack for as long as this function is on the real Rust one; the guard
    // pops it again on every exit path (`crate::call_stack`).
    let class_fqcn = module.this_class_name().unwrap_or("").to_string();
    let method_name = module
        .constant_pool
        .utf8_at(method.name_index)
        .unwrap_or("")
        .to_string();
    let _frame_guard = crate::call_stack::push_frame(class_fqcn, method_name);

    loop {
        if pc >= code.len() {
            return Ok(None);
        }
        let opcode_pc = pc;
        crate::call_stack::set_current_line(&method.line_table, opcode_pc);
        match exec_step(
            program,
            module,
            is_constructor,
            &mut locals,
            &mut stack,
            code,
            &mut pc,
        ) {
            Ok(Step::Continue) => {}
            Ok(Step::Return(v)) => return Ok(v),
            Err(VmError::Thrown(exc)) => {
                match find_handler(program, module, method, opcode_pc, &exc) {
                    Some(handler_pc) => {
                        stack.clear();
                        stack.push(exc);
                        pc = handler_pc;
                    }
                    None => return Err(VmError::Thrown(exc)),
                }
            }
            Err(other) => return Err(other),
        }
    }
}

/// Executes a single opcode. On `Err(VmError::Thrown(_))`, `run_frame`
/// searches `method.exception_table` using the *pre-instruction* pc (the
/// position of the opcode that raised it, whether via `THROW` or an
/// implicit exception) — see the `match` below in `run_frame`.
#[allow(unused_assignments)] // GOTO/GOTO_W always overwrite `pc` right after reading their operand
fn exec_step(
    program: &Arc<Program>,
    module: &Module,
    is_constructor: bool,
    locals: &mut [Value],
    stack: &mut Vec<Value>,
    code: &[u8],
    pc_ref: &mut usize,
) -> Result<Step, VmError> {
    let opcode_pc = *pc_ref;
    let mut pc = opcode_pc;
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
            let v = stack
                .last()
                .cloned()
                .ok_or(VmError::Malformed("stack underflow"))?;
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
                ConstantPoolEntry::Utf8(s) => Value::Str(Arc::new(s.clone())),
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

        Opcode::IAdd => int_binop(stack, |a, b| Ok(a.wrapping_add(b)))?,
        Opcode::ISub => int_binop(stack, |a, b| Ok(a.wrapping_sub(b)))?,
        Opcode::IMul => int_binop(stack, |a, b| Ok(a.wrapping_mul(b)))?,
        Opcode::IDiv => int_binop(stack, |a, b| {
            if b == 0 {
                Err(throw_native("ArithmeticException", "division by zero"))
            } else {
                Ok(a.wrapping_div(b))
            }
        })?,
        Opcode::IMod => int_binop(stack, |a, b| {
            if b == 0 {
                Err(throw_native("ArithmeticException", "division by zero"))
            } else {
                Ok(a.wrapping_rem(b))
            }
        })?,
        Opcode::INeg => {
            let a = pop_int(stack)?;
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

        Opcode::FAdd => float_binop(stack, |a, b| a + b)?,
        Opcode::FSub => float_binop(stack, |a, b| a - b)?,
        Opcode::FMul => float_binop(stack, |a, b| a * b)?,
        Opcode::FDiv => float_binop(stack, |a, b| a / b)?,
        Opcode::FMod => float_binop(stack, |a, b| a % b)?,
        Opcode::FNeg => {
            let a = pop_float(stack)?;
            stack.push(Value::Float(-a));
        }

        Opcode::I2F => {
            let a = pop_int(stack)?;
            stack.push(Value::Float(a as f64));
        }
        Opcode::F2I => {
            let a = pop_float(stack)?;
            let clamped = if a.is_nan() {
                0
            } else {
                a.trunc().clamp(i64::MIN as f64, i64::MAX as f64) as i64
            };
            stack.push(Value::Int(clamped));
        }
        Opcode::I2B => {
            let a = pop_int(stack)?;
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
                return Err(throw_native(
                    "NullPointerException",
                    "null pointer dereference",
                ));
            }
            stack.push(Value::Str(Arc::new(v.to_display_string())));
        }

        Opcode::CmpEq => {
            let (a, b) = pop2(stack)?;
            stack.push(Value::Bool(values_equal(&a, &b)));
        }
        Opcode::CmpNe => {
            let (a, b) = pop2(stack)?;
            stack.push(Value::Bool(!values_equal(&a, &b)));
        }
        Opcode::CmpLt => {
            let ord = compare(stack)?;
            stack.push(Value::Bool(ord == std::cmp::Ordering::Less));
        }
        Opcode::CmpGt => {
            let ord = compare(stack)?;
            stack.push(Value::Bool(ord == std::cmp::Ordering::Greater));
        }
        Opcode::CmpLe => {
            let ord = compare(stack)?;
            stack.push(Value::Bool(ord != std::cmp::Ordering::Greater));
        }
        Opcode::CmpGe => {
            let ord = compare(stack)?;
            stack.push(Value::Bool(ord != std::cmp::Ordering::Less));
        }
        Opcode::CmpThreeWay => {
            let ord = compare(stack)?;
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
            let v = pop_bool(stack)?;
            stack.push(Value::Bool(!v));
        }

        Opcode::IfTrue => {
            let offset = read_i16!();
            let v = pop_bool(stack)?;
            if v {
                pc = (opcode_pc as i64 + offset as i64) as usize;
            }
        }
        Opcode::IfFalse => {
            let offset = read_i16!();
            let v = pop_bool(stack)?;
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
            // `system.List<T>`/`system.Map<K,V>` — see
            // `nl_vm::native`'s module doc comment: no backing bytecode
            // `Module`, so the usual field-walk below would fail to
            // resolve `fqcn` at all.
            if crate::native::is_native_generic_class(&fqcn) {
                stack.push(crate::native::new_generic_object(&fqcn));
                *pc_ref = pc;
                return Ok(Step::Continue);
            }
            // `new system.Random()`/`new system.Random(int seed)` —
            // same no-backing-Module situation, non-generic (see
            // `nl_vm::native::is_random_class`'s doc comment).
            if crate::native::is_random_class(&fqcn) {
                stack.push(crate::native::new_random_object());
                *pc_ref = pc;
                return Ok(Step::Continue);
            }
            // `new system.net.TcpListener(...)`/`new system.net.UdpSocket()`
            // — same no-backing-Module situation as `system.Random`
            // above (see `nl_vm::native`'s network section doc
            // comment); the actual OS socket is only created on
            // `INVOKE_SPECIAL <construct>` below, so `NEW` here just
            // pushes a placeholder object with `__fd__ = -1`.
            if crate::native::is_net_listener_class(&fqcn) {
                stack.push(crate::native::new_tcp_listener_object());
                *pc_ref = pc;
                return Ok(Step::Continue);
            }
            if crate::native::is_net_udp_class(&fqcn) {
                stack.push(crate::native::new_udp_socket_object());
                *pc_ref = pc;
                return Ok(Step::Continue);
            }
            // `new system.thread.Thread(...)`/`Mutex()`/`Semaphore(n)`
            // — same no-backing-Module situation as `system.Random`.
            if crate::native::is_thread_class(&fqcn) {
                stack.push(crate::native::new_thread_object());
                *pc_ref = pc;
                return Ok(Step::Continue);
            }
            if crate::native::is_mutex_class(&fqcn) {
                stack.push(crate::native::new_mutex_object());
                *pc_ref = pc;
                return Ok(Step::Continue);
            }
            if crate::native::is_semaphore_class(&fqcn) {
                stack.push(crate::native::new_semaphore_object());
                *pc_ref = pc;
                return Ok(Step::Continue);
            }
            // Fields are collected across the whole `extends` chain (a
            // subclass's own fields, if any, take precedence over a
            // same-named ancestor field) so an inherited field like
            // `Exception.message` is present on every subclass instance.
            let mut fields = HashMap::new();
            let mut current = fqcn.clone();
            loop {
                let m = program
                    .get(&current)
                    .ok_or_else(|| VmError::MethodNotFound(current.clone()))?;
                for f in &m.fields {
                    let name = m
                        .constant_pool
                        .utf8_at(f.name_index)
                        .ok_or(VmError::Malformed("bad field name index"))?
                        .to_string();
                    let type_desc = m
                        .constant_pool
                        .type_desc_at(f.type_index)
                        .ok_or(VmError::Malformed("bad field type index"))?;
                    fields
                        .entry(name)
                        .or_insert_with(|| default_value_for(type_desc));
                }
                if m.super_class == 0 {
                    break;
                }
                current = m
                    .constant_pool
                    .class_name_at(m.super_class)
                    .ok_or(VmError::Malformed("bad super_class index"))?
                    .to_string();
            }
            stack.push(Value::Object(Arc::new(Mutex::new(Object {
                class_name: fqcn,
                fields,
                // The GC-contract destructor hook — see `Object`'s `Drop`
                // impl. Only user-class instances (the only objects with
                // a bytecode `<destruct>` to call) get a live hook.
                program: Arc::downgrade(program),
                destroyed: false,
            }))));
        }
        Opcode::InstanceOf => {
            let class_index = read_u16!();
            let target_fqcn = resolve_class_name(module, class_index)?;
            let v = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
            let result = match &v {
                Value::Object(obj) => {
                    let runtime_class = lock(&obj).class_name.clone();
                    is_instance_of(program, runtime_class, target_fqcn)
                }
                _ => false,
            };
            stack.push(Value::Bool(result));
        }
        Opcode::CheckCast => {
            let class_index = read_u16!();
            let target_fqcn = resolve_class_name(module, class_index)?;
            let v = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
            if let Value::Object(obj) = &v {
                let runtime_class = lock(&obj).class_name.clone();
                if !is_instance_of(program, runtime_class, target_fqcn) {
                    return Err(throw_native(
                        "InvalidCastException",
                        format!("Cannot cast to {target_fqcn}"),
                    ));
                }
            }
            // `null` passes any check (vm.md § CHECKCAST); a non-object,
            // non-null value reaching here would be an nl-codegen bug
            // (CHECKCAST is only ever emitted for object-to-object
            // casts) — pushed back unchanged rather than rejected.
            stack.push(v);
        }

        Opcode::NewArray => {
            let type_index = read_u16!();
            let elem_desc = module.constant_pool.type_desc_at(type_index).unwrap_or("");
            let default = default_value_for(elem_desc);
            let size = pop_int(stack)?;
            if size < 0 {
                return Err(VmError::Malformed("negative array size"));
            }
            stack.push(Value::Array(Arc::new(Mutex::new(vec![
                default;
                size as usize
            ]))));
        }
        Opcode::NewArrayInit => {
            let _type_index = read_u16!();
            let count = read_u16!() as usize;
            if stack.len() < count {
                return Err(VmError::Malformed("stack underflow"));
            }
            let elements = stack.split_off(stack.len() - count);
            stack.push(Value::Array(Arc::new(Mutex::new(elements))));
        }
        Opcode::ArrayLoad => {
            let (arr, idx) = pop2(stack)?;
            let Value::Array(arr) = arr else {
                return Err(VmError::Malformed("ARRAY_LOAD on non-array"));
            };
            let idx = idx
                .as_int()
                .ok_or(VmError::Malformed("array index must be int"))?;
            let arr_ref = lock(&arr);
            if idx < 0 || idx as usize >= arr_ref.len() {
                return Err(throw_native(
                    "IndexOutOfBoundsException",
                    format!("index {idx}, length {}", arr_ref.len()),
                ));
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
            let idx = idx
                .as_int()
                .ok_or(VmError::Malformed("array index must be int"))?;
            let mut arr_mut = lock(&arr);
            if idx < 0 || idx as usize >= arr_mut.len() {
                return Err(throw_native(
                    "IndexOutOfBoundsException",
                    format!("index {idx}, length {}", arr_mut.len()),
                ));
            }
            // Same lock-ordering care as SET_FIELD: release the array's
            // lock before the replaced element (and thus a possible
            // destructor) drops.
            let replaced = std::mem::replace(&mut arr_mut[idx as usize], value);
            drop(arr_mut);
            drop(replaced);
        }
        Opcode::ArrayLength => {
            let v = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
            let Value::Array(arr) = v else {
                return Err(VmError::Malformed("ARRAY_LENGTH on non-array"));
            };
            stack.push(Value::Int(lock(&arr).len() as i64));
        }

        Opcode::GetField => {
            let idx = read_u16!();
            let (_, field_name, _) = resolve_field_ref(module, idx)?;
            let receiver = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
            if receiver.is_null() {
                return Err(throw_native(
                    "NullPointerException",
                    "null pointer dereference",
                ));
            }
            let Value::Object(obj) = receiver else {
                return Err(VmError::Malformed("GET_FIELD on non-object"));
            };
            let value = lock(&obj)
                .fields
                .get(&field_name)
                .cloned()
                .unwrap_or(Value::Null);
            stack.push(value);
        }
        Opcode::SetField => {
            let idx = read_u16!();
            let (_, field_name, _) = resolve_field_ref(module, idx)?;
            let value = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
            let receiver = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
            if receiver.is_null() {
                return Err(throw_native(
                    "NullPointerException",
                    "null pointer dereference",
                ));
            }
            let Value::Object(obj) = receiver else {
                return Err(VmError::Malformed("SET_FIELD on non-object"));
            };
            // vm.md § SET_FIELD: "the VM must reject writes to readonly
            // fields outside constructors at runtime (as a safety net;
            // the compiler should have caught this)". A field is
            // readonly if its own descriptor says so, or if the class
            // that declares it is itself `readonly` (all fields
            // immutable — specs.md § Readonly). The only exempt write is
            // `this.field = ...` inside the *declaring* class's own
            // `<construct>` (compiler.md's E013/E014 rule, mirrored here
            // as a coarser runtime check).
            let runtime_class = lock(&obj).class_name.clone();
            if let Some((owner_module, field)) =
                resolve_field_owner(program, &runtime_class, &field_name)
            {
                let readonly = field.flags & field_flags::READONLY != 0
                    || owner_module.class_flags & class_flags::READONLY != 0;
                if readonly {
                    let is_this = matches!(&locals[0], Value::Object(this_obj) if Arc::ptr_eq(this_obj, &obj));
                    let in_owner_constructor = is_constructor
                        && owner_module.this_class_name() == module.this_class_name();
                    if !(is_this && in_owner_constructor) {
                        return Err(VmError::Malformed(
                            "SET_FIELD: write to readonly field outside constructor",
                        ));
                    }
                }
            }
            // Bind the replaced value so it outlives the statement: the
            // object's lock (a temporary guard) is released at the end of
            // this statement, *then* the old value drops — its destructor
            // (see `Object::drop`) may call back into `obj` and must not
            // find it still locked.
            let replaced = lock(&obj).fields.insert(field_name, value);
            drop(replaced);
        }
        Opcode::GetStatic | Opcode::SetStatic => {
            return Err(VmError::Unsupported(format!(
                "{op:?} lands in a later phase"
            )));
        }

        Opcode::InvokeStatic => {
            let method_ref_idx = read_u16!();
            let (class_fqcn, name, descriptor) = resolve_method_ref(module, method_ref_idx)?;
            let param_count = count_params(&descriptor);
            if stack.len() < param_count {
                return Err(VmError::Malformed("stack underflow on INVOKE_STATIC"));
            }
            let call_args = stack.split_off(stack.len() - param_count);
            // `system.*` stdlib classes have no backing bytecode
            // `Module` — vm.md § Standard library binding — so they're
            // intercepted here, before the ordinary module lookup below.
            if crate::native::is_native_class(&class_fqcn) {
                if let Some(result) =
                    crate::native::dispatch(program, &class_fqcn, &name, call_args)?
                {
                    stack.push(result);
                }
                *pc_ref = pc;
                return Ok(Step::Continue);
            }
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
                return Err(throw_native(
                    "NullPointerException",
                    "null pointer dereference",
                ));
            }
            // `numbers.slice/map/filter/forEach/sort/find(...)` — vm.md §
            // Standard library binding: dispatched by the receiver's
            // `Value::Array` variant, not by class name (arrays have no
            // class of their own — `nl_codegen::expr::emit_array_call`'s
            // method ref just carries a placeholder class name).
            if let Value::Array(_) = &receiver {
                if let Some(result) =
                    crate::native::dispatch_array(program, &name, &receiver, call_args)?
                {
                    stack.push(result);
                }
                *pc_ref = pc;
                return Ok(Step::Continue);
            }
            // Virtual dispatch: resolve against the receiver's *runtime*
            // class, not the static type recorded in the method ref —
            // vm.md § Method dispatch, Instance methods.
            let Value::Object(obj) = &receiver else {
                return Err(VmError::Malformed("INVOKE_INSTANCE on non-object"));
            };
            let runtime_class = lock(&obj).class_name.clone();
            // `list.size()`/`map.get(k)` etc. — see `nl_vm::native`'s
            // module doc comment. Keyed by the *runtime* class like the
            // ordinary path below it, so this also covers a `List<T>`
            // reference held through e.g. an interface-typed variable
            // (not that either collection implements one today).
            if crate::native::is_native_generic_class(&runtime_class) {
                if let Some(result) = crate::native::dispatch_instance(
                    program,
                    &runtime_class,
                    &name,
                    &receiver,
                    call_args,
                )? {
                    stack.push(result);
                }
                *pc_ref = pc;
                return Ok(Step::Continue);
            }
            // `handle.read(...)` etc. on a `system.io.FileHandle` — same
            // interception pattern, but stateful through `program`'s
            // file-handle table (see `nl_vm::native`'s doc comments).
            if crate::native::is_native_instance_class(&runtime_class) {
                if let Some(result) =
                    crate::native::dispatch_native_instance(program, &name, &receiver, call_args)?
                {
                    stack.push(result);
                }
                *pc_ref = pc;
                return Ok(Step::Continue);
            }
            // Not necessarily declared on `runtime_class` itself — walk
            // the `extends` chain for an inherited-but-not-overridden
            // method (vm.md § Method dispatch, Instance methods).
            let (target_module, target) =
                resolve_virtual(program, &runtime_class, &name, &descriptor)
                    .ok_or_else(|| VmError::MethodNotFound(format!("{runtime_class}.{name}")))?;
            if let Some(result) =
                call_instance(program, target_module, target, receiver, call_args)?
            {
                stack.push(result);
            }
        }
        Opcode::InvokeClosure => {
            // vm.md § Closures: "pops the closure reference and the
            // arguments from the stack, then calls the closure's
            // invoke method." A closure is just an ordinary object
            // whose synthetic class has one `invoke` method and one
            // field per captured variable (see nl_codegen::closure) —
            // dispatch is therefore identical to INVOKE_INSTANCE.
            let method_ref_idx = read_u16!();
            let (_static_fqcn, name, descriptor) = resolve_method_ref(module, method_ref_idx)?;
            let param_count = count_params(&descriptor);
            if stack.len() < param_count + 1 {
                return Err(VmError::Malformed("stack underflow on INVOKE_CLOSURE"));
            }
            let call_args = stack.split_off(stack.len() - param_count);
            let receiver = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
            if receiver.is_null() {
                return Err(throw_native(
                    "NullPointerException",
                    "null pointer dereference",
                ));
            }
            let Value::Object(obj) = &receiver else {
                return Err(VmError::Malformed("INVOKE_CLOSURE on non-object"));
            };
            let runtime_class = lock(&obj).class_name.clone();
            let (target_module, target) =
                resolve_virtual(program, &runtime_class, &name, &descriptor)
                    .ok_or_else(|| VmError::MethodNotFound(format!("{runtime_class}.{name}")))?;
            if let Some(result) =
                call_instance(program, target_module, target, receiver, call_args)?
            {
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
                return Err(throw_native(
                    "NullPointerException",
                    "null pointer dereference",
                ));
            }
            // `system.List<T>(...)`/`system.Map<K,V>()` construction —
            // see `nl_vm::native`'s module doc comment.
            if crate::native::is_native_generic_class(&class_fqcn) {
                crate::native::construct_generic(&receiver, &class_fqcn, call_args)?;
                *pc_ref = pc;
                return Ok(Step::Continue);
            }
            if crate::native::is_random_class(&class_fqcn) {
                crate::native::construct_random(&receiver, call_args)?;
                *pc_ref = pc;
                return Ok(Step::Continue);
            }
            if crate::native::is_net_listener_class(&class_fqcn) {
                crate::native::construct_tcp_listener(program, &receiver, call_args)?;
                *pc_ref = pc;
                return Ok(Step::Continue);
            }
            if crate::native::is_net_udp_class(&class_fqcn) {
                crate::native::construct_udp_socket(program, &receiver)?;
                *pc_ref = pc;
                return Ok(Step::Continue);
            }
            if crate::native::is_thread_class(&class_fqcn) {
                crate::native::construct_thread(&receiver, call_args)?;
                *pc_ref = pc;
                return Ok(Step::Continue);
            }
            if crate::native::is_mutex_class(&class_fqcn) {
                crate::native::construct_mutex(program, &receiver)?;
                *pc_ref = pc;
                return Ok(Step::Continue);
            }
            if crate::native::is_semaphore_class(&class_fqcn) {
                crate::native::construct_semaphore(program, &receiver, call_args)?;
                *pc_ref = pc;
                return Ok(Step::Continue);
            }
            // No virtual dispatch: always the exact class named in the
            // ref (constructors, `super.method(...)`) — but that class
            // may itself only *inherit* the method (e.g. `super(...)`
            // delegating to a grandparent constructor's class), so this
            // still walks the chain starting at `class_fqcn` rather than
            // requiring an exact match there.
            let (target_module, target) = resolve_virtual(program, &class_fqcn, &name, &descriptor)
                .ok_or_else(|| VmError::MethodNotFound(format!("{class_fqcn}.{name}")))?;
            if let Some(result) =
                call_instance(program, target_module, target, receiver, call_args)?
            {
                stack.push(result);
            }
        }

        Opcode::StrConcat => {
            let (a, b) = pop2(stack)?;
            let sa = as_string(&a)?;
            let sb = as_string(&b)?;
            stack.push(Value::Str(Arc::new(format!("{sa}{sb}"))));
        }

        Opcode::Return => return Ok(Step::Return(None)),
        Opcode::ReturnValue => {
            let v = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
            return Ok(Step::Return(Some(v)));
        }

        Opcode::Throw => {
            let v = stack.pop().ok_or(VmError::Malformed("stack underflow"))?;
            if v.is_null() {
                return Err(throw_native("NullPointerException", "cannot throw null"));
            }
            return Err(VmError::Thrown(v));
        }
    }
    *pc_ref = pc;
    Ok(Step::Continue)
}

fn resolve_class_name(module: &Module, idx: u16) -> Result<&str, VmError> {
    module
        .constant_pool
        .class_name_at(idx)
        .ok_or(VmError::Malformed("bad class index"))
}

fn resolve_field_ref(module: &Module, idx: u16) -> Result<(String, String, String), VmError> {
    match module.constant_pool.get(idx) {
        Some(ConstantPoolEntry::FieldRef {
            class_index,
            name_index,
            type_index,
        }) => {
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
        _ => Err(VmError::Malformed(
            "field_ref index does not point to a FieldRef",
        )),
    }
}

fn resolve_method_ref(module: &Module, idx: u16) -> Result<(String, String, String), VmError> {
    match module.constant_pool.get(idx) {
        Some(ConstantPoolEntry::MethodRef {
            class_index,
            name_index,
            descriptor_index,
        }) => {
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
        _ => Err(VmError::Malformed(
            "method_ref index does not point to a MethodRef",
        )),
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
        "string" => Value::Str(Arc::new(String::new())),
        // Arrays, objects, and unions all default to `null`.
        _ => Value::Null,
    }
}

fn implements_interface(program: &Arc<Program>, class_fqcn: &str, target_fqcn: &str) -> bool {
    let Some(module) = program.get(class_fqcn) else {
        return false;
    };
    module
        .interfaces
        .iter()
        .any(|&i| module.constant_pool.class_name_at(i) == Some(target_fqcn))
}

/// `instanceof`/exception-catch-type test: is `current` (or, transitively,
/// any of its `extends` ancestors) equal to `target_fqcn`, or does one of
/// them `implements` it? — vm.md § Object operations, § Exception table.
fn is_instance_of(program: &Arc<Program>, mut current: String, target_fqcn: &str) -> bool {
    loop {
        if current == target_fqcn || implements_interface(program, &current, target_fqcn) {
            return true;
        }
        let Some(module) = program.get(&current) else {
            return false;
        };
        if module.super_class == 0 {
            return false;
        }
        match module.constant_pool.class_name_at(module.super_class) {
            Some(parent) => current = parent.to_string(),
            None => return false,
        }
    }
}

/// Builds a native `Value::Object` for a VM-raised exception (division by
/// zero, null dereference, out-of-bounds access, `throw null`) without going
/// through the class's actual constructor — vm.md § Exception table lists
/// these as VM-thrown directly. Only sets `message`; the full `Exception`
/// hierarchy's other fields (e.g. a captured stack trace) are not populated
/// this phase (see nl_syntax::prelude).
fn throw_native(class_name: &str, message: impl Into<String>) -> VmError {
    let mut fields = HashMap::new();
    fields.insert("message".to_string(), Value::Str(Arc::new(message.into())));
    VmError::Thrown(Value::Object(Arc::new(Mutex::new(Object::native(
        class_name, fields,
    )))))
}

/// Searches `method`'s exception table for an entry whose protected range
/// covers `opcode_pc` and whose `catch_type` matches `exc`'s runtime class
/// (or is `0`, catch-all/`finally`) — vm.md § Throw and stack unwinding,
/// steps 2-3. Entries are already in specificity order (nl-codegen emits
/// per-`catch` entries before a trailing `finally` catch-all).
fn find_handler(
    program: &Arc<Program>,
    module: &Module,
    method: &MethodDescriptor,
    opcode_pc: usize,
    exc: &Value,
) -> Option<usize> {
    let Value::Object(obj) = exc else {
        return None;
    };
    let exc_class = lock(&obj).class_name.clone();
    for entry in &method.exception_table {
        if (entry.start_pc as usize) > opcode_pc || opcode_pc >= (entry.end_pc as usize) {
            continue;
        }
        if entry.catch_type == 0 {
            return Some(entry.handler_pc as usize);
        }
        if let Some(catch_fqcn) = module.constant_pool.class_name_at(entry.catch_type) {
            if is_instance_of(program, exc_class.clone(), catch_fqcn) {
                return Some(entry.handler_pc as usize);
            }
        }
    }
    None
}

/// Resolves an instance method call starting from `start_fqcn`, walking the
/// `extends` chain when the method isn't declared directly on that class
/// (an inherited-but-not-overridden method) — used by both `INVOKE_INSTANCE`
/// (`start_fqcn` = the receiver's runtime class) and `INVOKE_SPECIAL`
/// (`start_fqcn` = the exact class named in the method ref).
pub(crate) fn resolve_virtual<'m>(
    program: &'m Arc<Program>,
    start_fqcn: &str,
    name: &str,
    descriptor: &str,
) -> Option<(&'m Module, &'m MethodDescriptor)> {
    let mut current = start_fqcn.to_string();
    loop {
        let module = program.get(&current)?;
        if let Some(target) = module.find_method_by_descriptor(name, descriptor) {
            return Some((module, target));
        }
        if module.super_class == 0 {
            return None;
        }
        current = module
            .constant_pool
            .class_name_at(module.super_class)?
            .to_string();
    }
}

/// Resolves the field named `name`, walking the `extends` chain from
/// `start_fqcn` (the receiver's runtime class) up to whichever ancestor
/// actually declares it — mirrors `resolve_virtual` for methods. Returns
/// `None` for a field the VM can't account for (e.g. a native/stdlib
/// object), which leaves `SET_FIELD` unrestricted rather than guessing.
fn resolve_field_owner<'m>(
    program: &'m Arc<Program>,
    start_fqcn: &str,
    name: &str,
) -> Option<(&'m Module, &'m nl_bytecode::FieldDescriptor)> {
    let mut current = start_fqcn.to_string();
    loop {
        let module = program.get(&current)?;
        if let Some(field) = module.find_field(name) {
            return Some((module, field));
        }
        if module.super_class == 0 {
            return None;
        }
        current = module
            .constant_pool
            .class_name_at(module.super_class)?
            .to_string();
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

fn int_binop(
    stack: &mut Vec<Value>,
    f: impl Fn(i64, i64) -> Result<i64, VmError>,
) -> Result<(), VmError> {
    let (a, b) = pop2(stack)?;
    let a = a
        .as_int()
        .ok_or(VmError::Malformed("expected int operand"))?;
    let b = b
        .as_int()
        .ok_or(VmError::Malformed("expected int operand"))?;
    stack.push(Value::Int(f(a, b)?));
    Ok(())
}

fn float_binop(stack: &mut Vec<Value>, f: impl Fn(f64, f64) -> f64) -> Result<(), VmError> {
    let (a, b) = pop2(stack)?;
    let a = a
        .as_float()
        .ok_or(VmError::Malformed("expected float operand"))?;
    let b = b
        .as_float()
        .ok_or(VmError::Malformed("expected float operand"))?;
    stack.push(Value::Float(f(a, b)));
    Ok(())
}

pub(crate) fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Null, _) | (_, Value::Null) => false,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Byte(x), Value::Byte(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => Arc::ptr_eq(x, y),
        (Value::Object(x), Value::Object(y)) => Arc::ptr_eq(x, y),
        _ => false,
    }
}

fn compare(stack: &mut Vec<Value>) -> Result<std::cmp::Ordering, VmError> {
    let (a, b) = pop2(stack)?;
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok(x.cmp(&y)),
        (Value::Float(x), Value::Float(y)) => x
            .partial_cmp(&y)
            .ok_or(VmError::Malformed("NaN comparison")),
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
