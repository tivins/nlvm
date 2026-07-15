//! Native bindings for the `system.*` stdlib classes — vm.md § Standard
//! library binding: "calling `system.Out.print(s)` is an `INVOKE_STATIC`
//! like any other — the VM intercepts the call and runs the native code."
//! `interpreter::exec_step`'s `INVOKE_STATIC` arm calls `dispatch` for any
//! class name `is_native_class` accepts, before ever consulting `Program`'s
//! module map (these classes have no backing bytecode `Module` — see
//! `nl_codegen::stdlib`/`nl_sema::stdlib`, which are what type-check and
//! emit calls against them).
//!
//! Only the first tranche of stdlib.md is covered (PLAN.md Phase 6): output
//! (`system.Out`/`system.Err`), `system.In.readLine`, and int/float/bool
//! parsing/formatting. File I/O, List/Map, threads, etc. are future work.

use std::rc::Rc;

use crate::error::VmError;
use crate::program::Program;
use crate::value::Value;

pub fn is_native_class(fqcn: &str) -> bool {
    matches!(fqcn, "system.Out" | "system.Err" | "system.In" | "system.Int" | "system.Float" | "system.Bool")
}

/// Dispatches one native call. `args` has already been popped off the
/// operand stack by the caller, in declaration order. Returns `Ok(None)`
/// for a `void` native (nothing to push back).
pub fn dispatch(program: &Program, fqcn: &str, name: &str, mut args: Vec<Value>) -> Result<Option<Value>, VmError> {
    match (fqcn, name) {
        ("system.Out", "print") => {
            program.write_stdout(&expect_str(&mut args)?);
            Ok(None)
        }
        ("system.Out", "println") => {
            let mut s = expect_str(&mut args)?;
            s.push('\n');
            program.write_stdout(&s);
            Ok(None)
        }
        ("system.Err", "print") => {
            program.write_stderr(&expect_str(&mut args)?);
            Ok(None)
        }
        ("system.Err", "println") => {
            let mut s = expect_str(&mut args)?;
            s.push('\n');
            program.write_stderr(&s);
            Ok(None)
        }
        ("system.In", "readLine") => {
            let mut line = String::new();
            match std::io::stdin().read_line(&mut line) {
                Ok(0) => Ok(Some(Value::Null)), // EOF
                Ok(_) => {
                    if line.ends_with('\n') {
                        line.pop();
                        if line.ends_with('\r') {
                            line.pop();
                        }
                    }
                    Ok(Some(Value::Str(Rc::new(line))))
                }
                Err(e) => Err(VmError::Io(e)),
            }
        }
        ("system.Int", "parse") => match expect_str(&mut args)?.trim().parse::<i64>() {
            Ok(v) => Ok(Some(Value::Int(v))),
            Err(_) => Err(throw_format_error("invalid int literal")),
        },
        ("system.Int", "tryParse") => match expect_str(&mut args)?.trim().parse::<i64>() {
            Ok(v) => Ok(Some(Value::Int(v))),
            Err(_) => Ok(Some(Value::Null)),
        },
        ("system.Int", "toString") => Ok(Some(Value::Str(Rc::new(expect_int(&mut args)?.to_string())))),
        ("system.Float", "parse") => match expect_str(&mut args)?.trim().parse::<f64>() {
            Ok(v) => Ok(Some(Value::Float(v))),
            Err(_) => Err(throw_format_error("invalid float literal")),
        },
        ("system.Float", "tryParse") => match expect_str(&mut args)?.trim().parse::<f64>() {
            Ok(v) => Ok(Some(Value::Float(v))),
            Err(_) => Ok(Some(Value::Null)),
        },
        ("system.Float", "toString") => Ok(Some(Value::Str(Rc::new(expect_float(&mut args)?.to_string())))),
        ("system.Bool", "parse") => match expect_str(&mut args)?.as_str() {
            "true" => Ok(Some(Value::Bool(true))),
            "false" => Ok(Some(Value::Bool(false))),
            _ => Err(throw_native("IllegalArgumentException", "expected \"true\" or \"false\"")),
        },
        ("system.Bool", "tryParse") => match expect_str(&mut args)?.as_str() {
            "true" => Ok(Some(Value::Bool(true))),
            "false" => Ok(Some(Value::Bool(false))),
            _ => Ok(Some(Value::Null)),
        },
        ("system.Bool", "toString") => Ok(Some(Value::Str(Rc::new(expect_bool(&mut args)?.to_string())))),
        _ => Err(VmError::MethodNotFound(format!("{fqcn}.{name}"))),
    }
}

fn expect_str(args: &mut Vec<Value>) -> Result<String, VmError> {
    match args.pop() {
        Some(Value::Str(s)) => Ok((*s).clone()),
        _ => Err(VmError::Malformed("expected string argument to native call")),
    }
}

fn expect_int(args: &mut Vec<Value>) -> Result<i64, VmError> {
    args.pop().and_then(|v| v.as_int()).ok_or(VmError::Malformed("expected int argument to native call"))
}

fn expect_float(args: &mut Vec<Value>) -> Result<f64, VmError> {
    args.pop().and_then(|v| v.as_float()).ok_or(VmError::Malformed("expected float argument to native call"))
}

fn expect_bool(args: &mut Vec<Value>) -> Result<bool, VmError> {
    args.pop().and_then(|v| v.as_bool()).ok_or(VmError::Malformed("expected bool argument to native call"))
}

fn throw_format_error(message: impl Into<String>) -> VmError {
    throw_native("NumberFormatException", message)
}

fn throw_native(class_name: &str, message: impl Into<String>) -> VmError {
    use std::cell::RefCell;
    use std::collections::HashMap;

    let mut fields = HashMap::new();
    fields.insert("message".to_string(), Value::Str(Rc::new(message.into())));
    VmError::Thrown(Value::Object(Rc::new(RefCell::new(crate::value::Object {
        class_name: class_name.to_string(),
        fields,
    }))))
}
