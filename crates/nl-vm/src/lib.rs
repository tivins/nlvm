pub mod error;
pub mod interpreter;
mod mini_regex;
mod native;
mod net_http;
mod text;
pub mod program;
pub mod value;

pub use error::VmError;
pub use nl_bytecode::Module;
pub use program::{run_program, RunOutcome};
pub use value::Value;

pub fn load_module(bytes: &[u8]) -> Result<Module, VmError> {
    Ok(Module::decode(bytes)?)
}
