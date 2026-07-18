//! vm.md § Stack trace construction — a thread-local shadow stack of active
//! NL call frames, maintained in parallel to the interpreter's native Rust
//! recursion. `interpreter::run_frame` has no explicit `Frame`/`CallStack`
//! struct of its own (a method call is just a recursive Rust call through
//! `call_static`/`call_instance`), so there is nothing else to walk when an
//! `Exception` needs to capture where it was constructed. Real OS threads
//! (`native::construct_thread`) each get their own independent stack for
//! free, since `thread_local!` storage is per-OS-thread.

use std::cell::{Cell, RefCell};

use nl_bytecode::LineTableEntry;

/// One active NL call frame — enough to resolve an `ExecutionPoint` (line +
/// declaring class) at any point during this frame's execution.
struct FrameInfo {
    class_fqcn: String,
    method_name: String,
    line: Cell<u32>,
}

thread_local! {
    static STACK: RefCell<Vec<FrameInfo>> = const { RefCell::new(Vec::new()) };
}

/// RAII guard returned by `push_frame`: pops the frame when dropped, which
/// happens on every exit path out of `run_frame` (normal return, `?`
/// propagation, or an unhandled `VmError`) without needing a matching manual
/// pop at each of those sites.
pub struct FrameGuard {
    _private: (),
}

impl Drop for FrameGuard {
    fn drop(&mut self) {
        STACK.with(|s| {
            s.borrow_mut().pop();
        });
    }
}

/// Pushes a frame for a method about to start executing. Called once at the
/// top of `run_frame`, before its instruction loop.
pub fn push_frame(class_fqcn: String, method_name: String) -> FrameGuard {
    STACK.with(|s| {
        s.borrow_mut().push(FrameInfo {
            class_fqcn,
            method_name,
            line: Cell::new(0),
        });
    });
    FrameGuard { _private: () }
}

/// Updates the topmost (current) frame's source line — called once per
/// instruction from `run_frame`'s loop, before executing it, so the frame
/// always reflects the line of whichever instruction is about to run.
pub fn set_current_line(line_table: &[LineTableEntry], pc: usize) {
    let line = line_for_pc(line_table, pc);
    STACK.with(|s| {
        if let Some(frame) = s.borrow().last() {
            frame.line.set(line);
        }
    });
}

/// vm.md § Method descriptor (line-number table): entries are sorted by
/// ascending `start_pc`, each covering offsets up to the next entry's
/// `start_pc`. Yields `0` if the table is absent (stripped build, or a
/// closure with an expression body — see `nl_codegen`'s `record_line`) or
/// `pc` precedes every entry.
fn line_for_pc(line_table: &[LineTableEntry], pc: usize) -> u32 {
    let pc = pc as u16;
    let idx = line_table.partition_point(|e| e.start_pc <= pc);
    idx.checked_sub(1).map(|i| line_table[i].line).unwrap_or(0)
}

/// Snapshots every currently active frame on this thread, innermost
/// (current) first — vm.md § Stack trace construction: "the VM natively
/// walks the current call stack". `skip` drops that many innermost frames;
/// the `Exception` constructor machinery uses this to exclude the exception
/// hierarchy's own constructor chain so the trace starts at the `new` site.
///
/// Not yet called anywhere — wired up once `Exception.stackTrace` capture
/// lands (TODO_stack_trace.md step 4).
#[allow(dead_code)]
pub fn snapshot(skip: usize) -> Vec<(String, String, u32)> {
    STACK.with(|s| {
        s.borrow()
            .iter()
            .rev()
            .skip(skip)
            .map(|f| (f.class_fqcn.clone(), f.method_name.clone(), f.line.get()))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_for_pc_picks_covering_entry() {
        let table = vec![
            LineTableEntry {
                start_pc: 0,
                line: 3,
            },
            LineTableEntry {
                start_pc: 5,
                line: 4,
            },
            LineTableEntry {
                start_pc: 12,
                line: 6,
            },
        ];
        assert_eq!(line_for_pc(&table, 0), 3);
        assert_eq!(line_for_pc(&table, 4), 3);
        assert_eq!(line_for_pc(&table, 5), 4);
        assert_eq!(line_for_pc(&table, 11), 4);
        assert_eq!(line_for_pc(&table, 12), 6);
        assert_eq!(line_for_pc(&table, 999), 6);
    }

    #[test]
    fn line_for_pc_empty_table_is_zero() {
        assert_eq!(line_for_pc(&[], 0), 0);
        assert_eq!(line_for_pc(&[], 42), 0);
    }

    #[test]
    fn push_frame_tracks_line_and_pops_on_drop() {
        // Isolated by thread_local — safe to run alongside other tests.
        assert_eq!(snapshot(0), Vec::<(String, String, u32)>::new());
        {
            let _f1 = push_frame("Ns.A".to_string(), "main".to_string());
            let table = vec![LineTableEntry {
                start_pc: 0,
                line: 10,
            }];
            set_current_line(&table, 0);
            {
                let _f2 = push_frame("Ns.B".to_string(), "helper".to_string());
                set_current_line(&table, 0);
                assert_eq!(
                    snapshot(0),
                    vec![
                        ("Ns.B".to_string(), "helper".to_string(), 10),
                        ("Ns.A".to_string(), "main".to_string(), 10),
                    ]
                );
                assert_eq!(
                    snapshot(1),
                    vec![("Ns.A".to_string(), "main".to_string(), 10)]
                );
            }
            assert_eq!(
                snapshot(0),
                vec![("Ns.A".to_string(), "main".to_string(), 10)]
            );
        }
        assert_eq!(snapshot(0), Vec::<(String, String, u32)>::new());
    }
}
