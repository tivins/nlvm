use std::cell::RefCell;
use std::rc::Rc;

use nl_bytecode::Module;

use crate::error::VmError;
use crate::interpreter::call_static;
use crate::value::Value;

pub struct RunOutcome {
    pub exit_code: i32,
    /// Populated once native stdout bindings exist (milestone 7);
    /// empty for every program today.
    pub stdout: String,
    /// Unhandled-exception message, if any (see § Program startup, step 7).
    pub stderr: String,
}

/// Program startup — see nlvm-specs/docs/vm.md § Program startup.
pub fn run_program(module: &Module, program_args: &[String]) -> RunOutcome {
    let main = match module.find_method("main") {
        Some(m) => m,
        None => {
            return RunOutcome {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("{}", VmError::NoMain),
            };
        }
    };

    let args_array = Value::Array(Rc::new(RefCell::new(
        program_args
            .iter()
            .map(|s| Value::Str(Rc::new(s.clone())))
            .collect(),
    )));

    match call_static(module, main, vec![args_array]) {
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
        Err(e) => RunOutcome {
            exit_code: 1,
            stdout: String::new(),
            stderr: format!("Unhandled exception: {e}"),
        },
    }
}
