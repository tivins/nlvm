//! vm.md § Class flag bits / § Method descriptor — the VM-level safety net
//! for `ABSTRACT`/`FINAL`, exercised the same way `readonly_runtime.rs`
//! exercises `SET_FIELD`'s readonly check: `nl-sema` already rejects these
//! at compile time (E032 instantiating an abstract class, E035 extending a
//! final class, E036 overriding a final method — see `phase7_0170`/
//! `phase7_0200`/`phase7_0210` in `tests/`), so exercising the VM's own
//! checks requires skipping `nl_sema::check_compile` and going straight
//! from parser to `nl_codegen::compile_program` — simulating the "static
//! check got bypassed" scenario each safety net exists for.

fn compile(sources: &[&str]) -> Vec<nl_bytecode::Module> {
    let files: Vec<_> = sources
        .iter()
        .map(|src| nl_syntax::parse_source_file(src, "test").expect("parse"))
        .collect();
    nl_codegen::compile_program(&files).expect("codegen")
}

#[test]
fn new_rejects_abstract_class_at_runtime() {
    let shape = r#"
namespace test.abstract.runtime;
abstract class Shape {
	public construct() {}
	public abstract float area();
}
"#;
    let main = r#"
namespace test.abstract.runtime;
class Main {
	public static int main(string[] args) {
		auto s = new Shape();
		return 0;
	}
}
"#;
    let modules = compile(&[shape, main]);
    let outcome = nl_vm::run_program(&modules, &[]);
    assert_eq!(
        outcome.exit_code, 1,
        "stdout={:?} stderr={:?}",
        outcome.stdout, outcome.stderr
    );
    assert!(
        outcome.stderr.contains("abstract"),
        "stderr={:?}",
        outcome.stderr
    );
}

#[test]
fn verify_link_rejects_extending_a_final_class() {
    let base = r#"
namespace test.final.extend.runtime;
final class Base {
	public construct() {}
}
"#;
    let sub = r#"
namespace test.final.extend.runtime;
class Sub extends Base {
	public construct() {}
}
"#;
    let main = r#"
namespace test.final.extend.runtime;
class Main {
	public static int main(string[] args) {
		return 0;
	}
}
"#;
    let modules = compile(&[base, sub, main]);

    let err = nl_vm::verify_link(&modules).expect_err("extending a final class must be rejected");
    assert!(matches!(err, nl_vm::VmError::Link(_)));
    assert!(format!("{err}").contains("final"));

    // The same check runs (and produces the same observable failure) from
    // `run_program` itself, not just the standalone `verify_link` entry
    // point — this is what actually protects `nlvm`/`nl-test-runner`.
    let outcome = nl_vm::run_program(&modules, &[]);
    assert_eq!(outcome.exit_code, 1);
    assert!(outcome.stderr.contains("final"), "stderr={:?}", outcome.stderr);
}

#[test]
fn verify_link_rejects_overriding_a_final_method() {
    let base = r#"
namespace test.final.override.runtime;
class Base {
	public construct() {}
	public final string label() {
		return "base";
	}
}
"#;
    let derived = r#"
namespace test.final.override.runtime;
class Derived extends Base {
	public construct() {}
	public string label() {
		return "derived";
	}
}
"#;
    let main = r#"
namespace test.final.override.runtime;
class Main {
	public static int main(string[] args) {
		return 0;
	}
}
"#;
    let modules = compile(&[base, derived, main]);

    let err =
        nl_vm::verify_link(&modules).expect_err("overriding a final method must be rejected");
    assert!(matches!(err, nl_vm::VmError::Link(_)));
    assert!(format!("{err}").contains("final"));
}

#[test]
fn verify_link_accepts_ordinary_inheritance() {
    let base = r#"
namespace test.final.ok.runtime;
class Base {
	public construct() {}
	public string label() {
		return "base";
	}
}
"#;
    let derived = r#"
namespace test.final.ok.runtime;
class Derived extends Base {
	public construct() {}
	public string label() {
		return "derived";
	}
}
"#;
    let modules = compile(&[base, derived]);
    nl_vm::verify_link(&modules).expect("ordinary override of a non-final method is legal");
}
