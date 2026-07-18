mod call_stack;
pub mod error;
pub mod interpreter;
mod mini_regex;
mod mini_tz;
mod native;
mod net_http;
pub mod program;
mod text;
pub mod value;

pub use error::VmError;
pub use nl_bytecode::Module;
pub use program::{run_program, RunOutcome};
pub use value::Value;

pub fn load_module(bytes: &[u8]) -> Result<Module, VmError> {
    Ok(Module::decode(bytes)?)
}
