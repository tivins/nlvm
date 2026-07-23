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
pub use program::{run_program, verify_link, RunOutcome};
pub use value::Value;

pub fn load_module(bytes: &[u8]) -> Result<Module, VmError> {
    Ok(Module::decode(bytes)?)
}

/// Loads either a bare `.nlm` module image or a `.nlp` program container
/// (told apart by magic, not file extension) into the module list
/// `run_program` expects.
pub fn load_modules(bytes: &[u8]) -> Result<Vec<Module>, VmError> {
    if nl_bytecode::is_program(bytes) {
        Ok(nl_bytecode::decode_program(bytes)?)
    } else {
        Ok(vec![Module::decode(bytes)?])
    }
}
