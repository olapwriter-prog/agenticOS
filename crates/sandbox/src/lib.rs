//! The sandbox: runs a forged tool, compiled to WebAssembly, with hard limits.
//! The tool gets no files, no network and no clock. It can only read its input and write its output.
//!
//! ABI (what a forged tool must export):
//!   memory                      its linear memory
//!   alloc(len: i32) -> i32      returns a pointer to len writable bytes
//!   run(ptr: i32, len: i32) -> i64   input at (ptr, len); returns (out_ptr << 32) | out_len
use wasmi::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Instruction budget. Each executed operation costs about one unit.
    pub fuel: u64,
    /// Memory the tool may use, in bytes.
    pub memory_bytes: usize,
    /// Largest output the tool may return.
    pub max_output: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            fuel: 5_000_000,
            memory_bytes: 16 * 1024 * 1024,
            max_output: 64 * 1024,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SandboxError {
    /// Not valid WebAssembly.
    BadModule,
    /// The tool asks for something from the outside world (an import) that the sandbox never provides.
    ForbiddenImport(String),
    /// Missing `memory`, `alloc` or `run`, or they have the wrong shape.
    BadAbi(String),
    OutOfFuel,
    /// Trapped: out of memory, divide by zero, unreachable, out-of-bounds access.
    Trapped(String),
    OutputTooBig,
}

struct Host {
    limits: StoreLimits,
}

pub fn run(wasm: &[u8], input: &[u8], limits: Limits) -> Result<Vec<u8>, SandboxError> {
    let mut config = Config::default();
    config.consume_fuel(true);
    let engine = Engine::new(&config);
    let module = Module::new(&engine, wasm).map_err(|_| SandboxError::BadModule)?;

    // Refuse any module that imports anything at all. A tool that wants the outside world is rejected
    // before a single instruction runs, so there is no host function for it to misuse.
    if let Some(imp) = module.imports().next() {
        return Err(SandboxError::ForbiddenImport(format!(
            "{}.{}",
            imp.module(),
            imp.name()
        )));
    }

    let mut store = Store::new(
        &engine,
        Host {
            limits: StoreLimitsBuilder::new()
                .memory_size(limits.memory_bytes)
                .instances(1)
                .build(),
        },
    );
    store.limiter(|h| &mut h.limits);
    store
        .set_fuel(limits.fuel)
        .map_err(|e| SandboxError::Trapped(e.to_string()))?;

    let linker: Linker<Host> = Linker::new(&engine);
    let instance = linker
        .instantiate_and_start(&mut store, &module)
        .map_err(|e| SandboxError::Trapped(e.to_string()))?;

    let memory = instance
        .get_memory(&store, "memory")
        .ok_or_else(|| SandboxError::BadAbi("no memory export".into()))?;
    let alloc = instance
        .get_typed_func::<i32, i32>(&store, "alloc")
        .map_err(|e| SandboxError::BadAbi(format!("alloc: {e}")))?;
    let run = instance
        .get_typed_func::<(i32, i32), i64>(&store, "run")
        .map_err(|e| SandboxError::BadAbi(format!("run: {e}")))?;

    let trap = |e: wasmi::Error| {
        let s = e.to_string();
        if s.to_lowercase().contains("fuel") {
            SandboxError::OutOfFuel
        } else {
            SandboxError::Trapped(s)
        }
    };

    let in_ptr = alloc.call(&mut store, input.len() as i32).map_err(trap)?;
    memory
        .write(&mut store, in_ptr as usize, input)
        .map_err(|e| SandboxError::Trapped(e.to_string()))?;
    let packed = run
        .call(&mut store, (in_ptr, input.len() as i32))
        .map_err(trap)?;

    let out_ptr = (packed >> 32) as u32 as usize;
    let out_len = (packed & 0xFFFF_FFFF) as usize;
    if out_len > limits.max_output {
        return Err(SandboxError::OutputTooBig);
    }
    let mut out = vec![0u8; out_len];
    memory
        .read(&store, out_ptr, &mut out)
        .map_err(|e| SandboxError::Trapped(e.to_string()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wasm(wat_text: &str) -> Vec<u8> {
        wat::parse_str(wat_text).unwrap()
    }

    /// echo: returns its input unchanged. alloc is a bump allocator starting at 1024.
    const ECHO: &str = r#"
    (module
      (memory (export "memory") 1)
      (global $next (mut i32) (i32.const 1024))
      (func (export "alloc") (param $len i32) (result i32)
        (local $p i32)
        (local.set $p (global.get $next))
        (global.set $next (i32.add (global.get $next) (local.get $len)))
        (local.get $p))
      (func (export "run") (param $ptr i32) (param $len i32) (result i64)
        (i64.or
          (i64.shl (i64.extend_i32_u (local.get $ptr)) (i64.const 32))
          (i64.extend_i32_u (local.get $len)))))
    "#;

    #[test]
    fn a_pure_tool_runs_and_returns_its_output() {
        let out = run(&wasm(ECHO), b"hello", Limits::default()).unwrap();
        assert_eq!(out, b"hello");
    }
    #[test]
    fn garbage_is_a_bad_module() {
        assert_eq!(
            run(b"not wasm", b"", Limits::default()),
            Err(SandboxError::BadModule)
        );
    }
    #[test]
    fn a_tool_that_imports_anything_is_refused_before_it_runs() {
        let m = wasm(
            r#"(module
              (import "env" "open_file" (func (param i32) (result i32)))
              (memory (export "memory") 1)
              (func (export "alloc") (param i32) (result i32) (i32.const 0))
              (func (export "run") (param i32 i32) (result i64) (i64.const 0)))"#,
        );
        assert_eq!(
            run(&m, b"", Limits::default()),
            Err(SandboxError::ForbiddenImport("env.open_file".into()))
        );
    }
    #[test]
    fn an_endless_loop_is_stopped_by_the_fuel_limit() {
        let m = wasm(
            r#"(module
              (memory (export "memory") 1)
              (func (export "alloc") (param i32) (result i32) (i32.const 0))
              (func (export "run") (param i32 i32) (result i64)
                (loop $l (br $l))
                (i64.const 0)))"#,
        );
        let limits = Limits {
            fuel: 10_000,
            ..Limits::default()
        };
        assert_eq!(run(&m, b"", limits), Err(SandboxError::OutOfFuel));
    }
    #[test]
    fn memory_growth_beyond_the_limit_fails_instead_of_growing() {
        // tries to grow by 1000 pages (64 MB) and returns the result of memory.grow as the output length marker
        let m = wasm(
            r#"(module
              (memory (export "memory") 1)
              (func (export "alloc") (param i32) (result i32) (i32.const 0))
              (func (export "run") (param i32 i32) (result i64)
                (i64.extend_i32_s (memory.grow (i32.const 1000)))))"#,
        );
        let limits = Limits {
            memory_bytes: 1024 * 1024,
            ..Limits::default()
        };
        // memory.grow returns -1 on failure; as a packed value that is out_len 0xFFFFFFFF, far over max_output
        assert_eq!(run(&m, b"", limits), Err(SandboxError::OutputTooBig));
    }
    #[test]
    fn a_trap_is_reported_not_propagated() {
        let m = wasm(
            r#"(module
              (memory (export "memory") 1)
              (func (export "alloc") (param i32) (result i32) (i32.const 0))
              (func (export "run") (param i32 i32) (result i64) (unreachable)))"#,
        );
        assert!(matches!(
            run(&m, b"", Limits::default()),
            Err(SandboxError::Trapped(_))
        ));
    }
    #[test]
    fn a_missing_export_is_a_bad_abi() {
        let m = wasm(r#"(module (memory (export "memory") 1))"#);
        assert!(matches!(
            run(&m, b"", Limits::default()),
            Err(SandboxError::BadAbi(_))
        ));
    }
    #[test]
    fn output_over_the_cap_is_refused() {
        let limits = Limits {
            max_output: 3,
            ..Limits::default()
        };
        assert_eq!(
            run(&wasm(ECHO), b"hello", limits),
            Err(SandboxError::OutputTooBig)
        );
    }
    #[test]
    fn an_out_of_bounds_output_pointer_traps() {
        // returns ptr = 70000 (beyond the single 64 KiB page), len = 4
        let m = wasm(
            r#"(module
              (memory (export "memory") 1)
              (func (export "alloc") (param i32) (result i32) (i32.const 0))
              (func (export "run") (param i32 i32) (result i64)
                (i64.or (i64.shl (i64.const 70000) (i64.const 32)) (i64.const 4))))"#,
        );
        assert!(matches!(
            run(&m, b"", Limits::default()),
            Err(SandboxError::Trapped(_))
        ));
    }
    #[test]
    fn each_run_starts_fresh() {
        // the bump allocator restarts at 1024 every run, so two runs give identical results
        let m = wasm(ECHO);
        assert_eq!(
            run(&m, b"a", Limits::default()).unwrap(),
            run(&m, b"a", Limits::default()).unwrap()
        );
    }
}
