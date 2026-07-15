use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use nl_bytecode::{Module, MethodDescriptor};

use crate::error::VmError;
use crate::interpreter::call_static;
use crate::value::Value;

/// A linked program: every module that will be executed together, keyed by
/// fully-qualified class name. Built once per run so cross-file references
/// (`new`, field access, instance/static method calls — see
/// `nl_bytecode::ConstantPoolEntry::{Class,FieldRef,MethodRef}`) resolve to
/// the right module instead of assuming everything lives in one file.
pub struct Program {
    modules: HashMap<String, Module>,
    /// Accumulated output from native `system.Out`/`system.Err` calls (see
    /// `crate::native`) — `Program` is threaded by shared reference through
    /// every call frame, so these are interior-mutable rather than
    /// threaded explicitly through `call_static`/`call_instance`/`run_frame`.
    stdout: RefCell<String>,
    stderr: RefCell<String>,
}

impl Program {
    pub fn new(modules: Vec<Module>) -> Self {
        let mut map = HashMap::with_capacity(modules.len());
        for module in modules {
            if let Some(name) = module.this_class_name() {
                map.insert(name.to_string(), module);
            }
        }
        Program { modules: map, stdout: RefCell::new(String::new()), stderr: RefCell::new(String::new()) }
    }

    pub fn get(&self, fqcn: &str) -> Option<&Module> {
        self.modules.get(fqcn)
    }

    pub fn find_main(&self) -> Option<(&Module, &MethodDescriptor)> {
        self.modules.values().find_map(|m| m.find_method("main").map(|meth| (m, meth)))
    }

    pub fn write_stdout(&self, s: &str) {
        self.stdout.borrow_mut().push_str(s);
    }

    pub fn write_stderr(&self, s: &str) {
        self.stderr.borrow_mut().push_str(s);
    }
}

pub struct RunOutcome {
    pub exit_code: i32,
    /// Everything written via `system.Out.print`/`println` (see `crate::native`).
    pub stdout: String,
    /// Everything written via `system.Err.print`/`println`, plus the
    /// unhandled-exception message if any (see § Program startup, step 7).
    pub stderr: String,
}

/// Program startup — see nlvm-specs/docs/vm.md § Program startup.
pub fn run_program(modules: &[Module], program_args: &[String]) -> RunOutcome {
    let program = Program::new(modules.to_vec());

    let Some((main_module, main)) = program.find_main() else {
        return RunOutcome {
            exit_code: 1,
            stdout: String::new(),
            stderr: format!("{}", VmError::NoMain),
        };
    };

    let args_array = Value::Array(Rc::new(RefCell::new(
        program_args
            .iter()
            .map(|s| Value::Str(Rc::new(s.clone())))
            .collect(),
    )));

    let result = call_static(&program, main_module, main, vec![args_array]);
    let stdout = program.stdout.into_inner();
    let mut stderr = program.stderr.into_inner();
    match result {
        Ok(Some(Value::Int(code))) => RunOutcome { exit_code: code as i32, stdout, stderr },
        Ok(_) => RunOutcome { exit_code: 0, stdout, stderr },
        Err(VmError::Thrown(exc)) => {
            append_line(&mut stderr, &format!("Unhandled exception: {}", describe_exception(&exc)));
            RunOutcome { exit_code: 1, stdout, stderr }
        }
        Err(e) => {
            append_line(&mut stderr, &format!("Unhandled exception: {e}"));
            RunOutcome { exit_code: 1, stdout, stderr }
        }
    }
}

fn append_line(buf: &mut String, line: &str) {
    if !buf.is_empty() && !buf.ends_with('\n') {
        buf.push('\n');
    }
    buf.push_str(line);
}

/// `vm.md § Throw and stack unwinding`, step 5: "the VM prints the
/// exception message ... to stderr". Renders as `ClassName: message` (or
/// bare `ClassName` if `message` is absent/not a string) — matches the
/// implicit-exception wording already used by e.g. `IndexOutOfBoundsException`.
fn describe_exception(exc: &Value) -> String {
    let Value::Object(obj) = exc else {
        return exc.to_display_string();
    };
    let obj = obj.borrow();
    match obj.fields.get("message") {
        Some(Value::Str(s)) if !s.is_empty() => format!("{}: {s}", obj.class_name),
        _ => obj.class_name.clone(),
    }
}
