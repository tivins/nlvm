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
}

impl Program {
    pub fn new(modules: Vec<Module>) -> Self {
        let mut map = HashMap::with_capacity(modules.len());
        for module in modules {
            if let Some(name) = module.this_class_name() {
                map.insert(name.to_string(), module);
            }
        }
        Program { modules: map }
    }

    pub fn get(&self, fqcn: &str) -> Option<&Module> {
        self.modules.get(fqcn)
    }

    pub fn find_main(&self) -> Option<(&Module, &MethodDescriptor)> {
        self.modules.values().find_map(|m| m.find_method("main").map(|meth| (m, meth)))
    }
}

pub struct RunOutcome {
    pub exit_code: i32,
    /// Populated once native stdout bindings exist (milestone 7);
    /// empty for every program today.
    pub stdout: String,
    /// Unhandled-exception message, if any (see § Program startup, step 7).
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

    match call_static(&program, main_module, main, vec![args_array]) {
        Ok(Some(Value::Int(code))) => RunOutcome {
            exit_code: code as i32,
            stdout: String::new(),
            stderr: String::new(),
        },
        Ok(_) => RunOutcome {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        },
        Err(VmError::Thrown(exc)) => RunOutcome {
            exit_code: 1,
            stdout: String::new(),
            stderr: format!("Unhandled exception: {}", describe_exception(&exc)),
        },
        Err(e) => RunOutcome {
            exit_code: 1,
            stdout: String::new(),
            stderr: format!("Unhandled exception: {e}"),
        },
    }
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
