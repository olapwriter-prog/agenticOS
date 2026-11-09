//! Turning the agent's Rust into a sandbox module.
//!
//! The agent writes `fn run(args: &Args) -> Result<String, String>` (plus any helpers, std only).
//! It is placed at the top of one file, above a small harness that provides `Args` and `phone`
//! and the sandbox ABI (`alloc`, `run`), and compiled with `rustc` for wasm32-unknown-unknown.
//! The code comes first so the compiler's line numbers are the agent's line numbers.
//!
//! Compiling untrusted code is itself a risk: macros such as `include_str!` read files while
//! compiling and would bake them into the module. So the source is checked first, and the
//! compiler runs in an empty directory with a cleared environment and a time limit.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub const MAX_CODE: usize = 16 * 1024;
/// Requests a function may make of the phone in one run.
pub const MAX_CALLS: usize = 10;

/// Wire format between host and harness (no JSON parser needed inside the module):
/// input  = key \0 value \0 key \0 value \0 ...
/// output = 'O' result (\x1d tool (\x1f key \x1f value)*)*   or   'E' message
const HARNESS: &str = r#"
// ---- harness (added by agentd; not part of the function) ----
/// The function's arguments. Every value is text.
pub struct Args(Vec<(String, String)>);
impl Args {
    /// The text of an argument.
    pub fn get(&self, name: &str) -> Result<&str, String> {
        self.0.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
            .ok_or_else(|| format!("missing argument {name}"))
    }
    /// An argument as a number.
    pub fn num(&self, name: &str) -> Result<f64, String> {
        self.get(name)?.trim().parse::<f64>().map_err(|_| format!("{name} is not a number"))
    }
    /// An argument as a whole number.
    pub fn int(&self, name: &str) -> Result<i64, String> {
        self.get(name)?.trim().parse::<i64>().map_err(|_| format!("{name} is not a whole number"))
    }
}
thread_local! {
    static __AGENTOS_CALLS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}
fn __agentos_clean(s: &str) -> String {
    s.chars().map(|c| if c == '\x1d' || c == '\x1f' { ' ' } else { c }).collect()
}
/// Ask the phone to do something after the function ends (through the approval rules).
pub fn phone(tool: &str, args: &[(&str, &str)]) -> Result<(), String> {
    __AGENTOS_CALLS.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= __AGENTOS_MAX_CALLS {
            return Err(format!("a function may ask for at most {} phone actions", __AGENTOS_MAX_CALLS));
        }
        let mut s = __agentos_clean(tool);
        for (k, v) in args {
            s.push('\x1f'); s.push_str(&__agentos_clean(k));
            s.push('\x1f'); s.push_str(&__agentos_clean(v));
        }
        c.push(s);
        Ok(())
    })
}
/// One text message, parsed from `all_messages` (a line `from NAME (NUMBER): TEXT`).
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// true: they sent it to you; false: you sent it.
    pub incoming: bool,
    /// None when the number is not in your contacts.
    pub name: Option<String>,
    pub number: String,
    pub text: String,
}
/// One call, parsed from `call_log` (a line `from NAME (NUMBER) STATE`).
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    /// true: they called you; false: you called them.
    pub incoming: bool,
    pub name: Option<String>,
    pub number: String,
    /// ringing, dialing, active, held or ended
    pub state: String,
}
/// `NAME (NUMBER)rest` or `NUMBERrest` -> (name, number, rest)
fn __agentos_who(s: &str, number_end: &str) -> (Option<String>, String, String) {
    if let Some(open) = s.find(" (") {
        if let Some(close) = s[open..].find(')') {
            let close = open + close;
            return (Some(s[..open].trim().to_string()), s[open + 2..close].to_string(), s[close + 1..].to_string());
        }
    }
    let end = s.find(number_end).unwrap_or(s.len());
    (None, s[..end].trim().to_string(), s[end..].to_string())
}
fn __agentos_dir(line: &str) -> Option<(bool, &str)> {
    line.strip_prefix("from ").map(|r| (true, r)).or_else(|| line.strip_prefix("to ").map(|r| (false, r)))
}
/// Parse `all_messages` into records (lines that are not messages are skipped).
pub fn messages(text: &str) -> Vec<Message> {
    text.lines()
        .filter_map(|line| {
            let (incoming, rest) = __agentos_dir(line)?;
            let (name, number, after) = __agentos_who(rest, ": ");
            let text = after.strip_prefix(": ").or_else(|| after.strip_prefix(':')).unwrap_or(&after).to_string();
            Some(Message { incoming, name, number, text })
        })
        .collect()
}
/// Parse `call_log` into records.
pub fn calls(text: &str) -> Vec<Call> {
    text.lines()
        .filter_map(|line| {
            let (incoming, rest) = __agentos_dir(line)?;
            let (name, number, after) = __agentos_who(rest, " ");
            let state = after.trim().to_string();
            Some(Call { incoming, name, number, state })
        })
        .collect()
}
#[no_mangle]
pub extern "C" fn alloc(len: i32) -> i32 {
    let mut v = Vec::<u8>::with_capacity(len.max(0) as usize);
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p as i32
}
#[export_name = "run"]
pub extern "C" fn __agentos_run(ptr: i32, len: i32) -> i64 {
    let input = unsafe { std::slice::from_raw_parts(ptr as *const u8, len.max(0) as usize) };
    let text = String::from_utf8_lossy(input);
    let mut kv = Vec::new();
    let mut it = text.split('\0');
    while let (Some(k), Some(v)) = (it.next(), it.next()) {
        if !k.is_empty() { kv.push((k.to_string(), v.to_string())); }
    }
    let out = match run(&Args(kv)) {
        Ok(r) => {
            let mut o = format!("O{}", __agentos_clean(&r));
            __AGENTOS_CALLS.with(|c| for x in c.borrow().iter() { o.push('\x1d'); o.push_str(x); });
            o
        }
        Err(e) => format!("E{e}"),
    };
    let b = out.into_bytes().leak();
    ((b.as_ptr() as i64) << 32) | b.len() as i64
}
"#;

/// Words a function may not use, because they read files or the environment while compiling,
/// link to outside code, or change how the file is compiled.
const FORBIDDEN: &[&str] = &[
    "include",
    "include_str",
    "include_bytes",
    "env",
    "option_env",
    "extern",
    "asm",
    "global_asm",
    "naked_asm",
    "cfg_attr",
    "debugger_visualizer",
    "proc_macro",
];

#[derive(Debug, PartialEq)]
enum Tok {
    Ident(String),
    Punct(char),
}

/// A rough Rust tokenizer: identifiers and punctuation, comments skipped. String contents are
/// not skipped, so a forbidden word inside a string is refused too (harmless and simpler).
fn tokens(src: &str) -> Vec<Tok> {
    let b: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == '/' && b.get(i + 1) == Some(&'/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && b.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else if c.is_alphanumeric() || c == '_' {
            let start = i;
            while i < b.len() && (b[i].is_alphanumeric() || b[i] == '_') {
                i += 1;
            }
            let word: String = b[start..i].iter().collect();
            // r#name is the identifier name
            out.push(Tok::Ident(
                word.strip_prefix("r#").unwrap_or(&word).to_string(),
            ));
        } else {
            if !c.is_whitespace() {
                out.push(Tok::Punct(c));
            }
            i += 1;
        }
    }
    out
}

/// Refuse source that could read files or escape while compiling. The message says why.
pub fn check_source(code: &str) -> Result<(), String> {
    if code.len() > MAX_CODE {
        return Err(format!("the code is over {MAX_CODE} bytes"));
    }
    if !code.contains("fn run") {
        return Err("define fn run(args: &Args) -> Result<String, String>".into());
    }
    let t = tokens(code);
    for (i, tok) in t.iter().enumerate() {
        match tok {
            Tok::Ident(w) if FORBIDDEN.contains(&w.as_str()) => {
                return Err(format!("`{w}` is not allowed in a function"));
            }
            // `mod name;` loads another file
            Tok::Ident(w) if w == "mod" => {
                if matches!(
                    (t.get(i + 1), t.get(i + 2)),
                    (Some(Tok::Ident(_)), Some(Tok::Punct(';')))
                ) {
                    return Err("`mod name;` (a module in another file) is not allowed".into());
                }
            }
            Tok::Punct('#') => match (t.get(i + 1), t.get(i + 2)) {
                (Some(Tok::Punct('!')), _) => {
                    return Err("inner attributes (#![...]) are not allowed".into())
                }
                (Some(Tok::Punct('[')), Some(Tok::Ident(a))) if a == "path" || a == "link" => {
                    return Err(format!("#[{a}] is not allowed"))
                }
                _ => {}
            },
            _ => {}
        }
    }
    Ok(())
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// `<sysroot>/bin/rustc` if `rustc --print sysroot` names one, else `rustc` unchanged.
fn resolve(rustc: PathBuf) -> PathBuf {
    let Ok(out) = Command::new(&rustc).args(["--print", "sysroot"]).output() else {
        return rustc;
    };
    let real = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()).join("bin/rustc");
    if out.status.success() && real.is_file() {
        real
    } else {
        rustc
    }
}

/// rustc, with the WebAssembly target, run safely.
#[derive(Debug, Clone)]
pub struct Compiler {
    pub rustc: PathBuf,
    /// A private directory for compiling; each compile gets an empty subdirectory.
    pub work: PathBuf,
    pub timeout: Duration,
}

impl Compiler {
    /// `rustc` may be rustup's proxy, which picks a toolchain by the current directory and would
    /// pick a different one inside the empty build directory. So the real compiler is found once,
    /// here, and every check and compile uses exactly that binary.
    pub fn new(rustc: PathBuf, work: PathBuf) -> Self {
        Compiler {
            rustc: resolve(rustc),
            work,
            timeout: Duration::from_secs(120),
        }
    }

    /// Is the compiler there, with the WebAssembly target? Returns its version.
    pub fn check(&self) -> Result<String, String> {
        let out = Command::new(&self.rustc)
            .args([
                "--print",
                "target-libdir",
                "--target",
                "wasm32-unknown-unknown",
            ])
            .output()
            .map_err(|e| {
                format!(
                    "{}: {e} (install Rust: apk add rust rust-wasm)",
                    self.rustc.display()
                )
            })?;
        let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.status.success() || !Path::new(&dir).exists() {
            return Err("rustc has no wasm32-unknown-unknown target (apk add rust-wasm, or rustup target add wasm32-unknown-unknown)".into());
        }
        let v = Command::new(&self.rustc)
            .arg("--version")
            .output()
            .map_err(|e| e.to_string())?;
        Ok(String::from_utf8_lossy(&v.stdout).trim().to_string())
    }

    /// Check, wrap and compile. The error is the compiler's own short messages, with the
    /// agent's line numbers, ready to show to whoever wrote the code.
    pub fn compile(&self, code: &str) -> Result<Vec<u8>, String> {
        check_source(code)?;
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = self.work.join(format!("build-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let result = self.compile_in(&dir, code);
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    fn compile_in(&self, dir: &Path, code: &str) -> Result<Vec<u8>, String> {
        let src = format!(
            "{code}\n{}\nconst __AGENTOS_MAX_CALLS: usize = {MAX_CALLS};\n",
            HARNESS
        );
        std::fs::write(dir.join("f.rs"), src).map_err(|e| e.to_string())?;
        let mut cmd = Command::new(&self.rustc);
        cmd.current_dir(dir)
            .args([
                "--edition",
                "2021",
                "--crate-type",
                "cdylib",
                "--crate-name",
                "function",
            ])
            .args([
                "--target",
                "wasm32-unknown-unknown",
                "--error-format",
                "short",
            ])
            .args([
                "-C",
                "opt-level=s",
                "-C",
                "panic=abort",
                "-C",
                "strip=symbols",
            ])
            .args(["-A", "warnings", "-o", "f.wasm", "f.rs"])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        // the compiler is a resolved binary, so it needs no rustup settings
        for v in ["PATH", "HOME"] {
            if let Ok(x) = std::env::var(v) {
                cmd.env(v, x);
            }
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("cannot run {}: {e}", self.rustc.display()))?;
        let mut stderr = child.stderr.take().unwrap();
        let reader = std::thread::spawn(move || {
            let mut s = String::new();
            use std::io::Read;
            let _ = stderr.read_to_string(&mut s);
            s
        });
        let until = Instant::now() + self.timeout;
        let status = loop {
            if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
                break st;
            }
            if Instant::now() >= until {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("compiling took over {} s", self.timeout.as_secs()));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let errors = reader.join().unwrap_or_default();
        if !status.success() && errors.contains("internal compiler error") {
            // rustc itself crashed (a compiler bug); say where, without the compiler's internals
            let at = errors
                .lines()
                .find_map(|l| {
                    l.find("f.rs:")
                        .map(|i| l[i..].split(": ").next().unwrap_or("").to_string())
                })
                .unwrap_or_else(|| "f.rs".into());
            return Err(format!(
                "does not compile:\n{at}: error: the Rust compiler crashed on this line (a compiler \
                 bug). The code there is probably wrong too: check the types, and write it another way."
            ));
        }
        if !status.success() {
            let lines: Vec<&str> = errors
                .lines()
                .filter(|l| l.contains("error") && !l.starts_with("error: aborting"))
                .take(12)
                .collect();
            return Err(format!("does not compile:\n{}", lines.join("\n")));
        }
        std::fs::read(dir.join("f.wasm")).map_err(|e| e.to_string())
    }
}

/// What one run produced: its result, and the phone actions it asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub result: String,
    pub calls: Vec<agentos_action::Action>,
}

/// Run a compiled function in the sandbox.
pub fn run(
    wasm: &[u8],
    args: &std::collections::BTreeMap<String, String>,
) -> Result<Outcome, String> {
    let mut input = Vec::new();
    for (k, v) in args {
        input.extend(k.replace('\0', " ").as_bytes());
        input.push(0);
        input.extend(v.replace('\0', " ").as_bytes());
        input.push(0);
    }
    let limits = agentos_sandbox::Limits {
        fuel: 100_000_000,
        memory_bytes: 32 << 20,
        max_output: 64 << 10,
    };
    let out = agentos_sandbox::run(wasm, &input, limits).map_err(|e| match e {
        agentos_sandbox::SandboxError::OutOfFuel => "ran too long (out of fuel)".to_string(),
        agentos_sandbox::SandboxError::Trapped(t) if t.contains("unreachable") => {
            "the function panicked (for example unwrap on bad input); return Err(...) instead"
                .to_string()
        }
        other => format!("sandbox: {other:?}"),
    })?;
    let text = String::from_utf8_lossy(&out).to_string();
    let mut parts = text.split('\x1d');
    let head = parts.next().unwrap_or("");
    if let Some(e) = head.strip_prefix('E') {
        return Err(e.to_string());
    }
    let result = head
        .strip_prefix('O')
        .ok_or("the function returned garbage")?
        .to_string();
    let calls = parts
        .map(|c| {
            let mut f = c.split('\x1f');
            let tool = f.next().unwrap_or_default().to_string();
            let mut args = Vec::new();
            while let (Some(k), Some(v)) = (f.next(), f.next()) {
                args.push((k.to_string(), v.to_string()));
            }
            args.sort();
            agentos_action::Action { tool, args }
        })
        .collect();
    Ok(Outcome { result, calls })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiler(tag: &str) -> Compiler {
        Compiler::new(
            "rustc".into(),
            std::env::temp_dir().join(format!("fn-compile-{tag}-{}", std::process::id())),
        )
    }

    #[test]
    fn file_reading_and_escapes_are_refused_before_compiling() {
        let ok = "fn run(args: &Args) -> Result<String, String> { Ok(String::new()) }";
        assert!(check_source(ok).is_ok());
        for (bad, why) in [
            (
                r#"fn run(a: &Args) -> Result<String, String> { Ok(include_str!("/etc/passwd").into()) }"#,
                "include_str",
            ),
            (
                r#"fn run(a: &Args) -> Result<String, String> { Ok(include_str ! ("/etc/passwd").into()) }"#,
                "include_str",
            ),
            (
                r#"use std::include_bytes as x; fn run(a: &Args) -> Result<String, String> { Ok(String::new()) }"#,
                "include_bytes",
            ),
            (
                r#"fn run(a: &Args) -> Result<String, String> { Ok(env!("HOME").into()) }"#,
                "env",
            ),
            (
                r#"#[path = "/etc/x.rs"] mod y; fn run(a: &Args) -> Result<String, String> { Ok(String::new()) }"#,
                "path",
            ),
            (
                "mod secrets; fn run(a: &Args) -> Result<String, String> { Ok(String::new()) }",
                "mod name;",
            ),
            (
                "#![feature(x)] fn run(a: &Args) -> Result<String, String> { Ok(String::new()) }",
                "inner attributes",
            ),
            (
                r#"extern "C" { fn f(); } fn run(a: &Args) -> Result<String, String> { Ok(String::new()) }"#,
                "extern",
            ),
            (
                r#"#[cfg_attr(all(), path = "/x")] mod z {} fn run(a: &Args) -> Result<String, String> { Ok(String::new()) }"#,
                "cfg_attr",
            ),
            ("fn helper() {}", "fn run"),
        ] {
            let e = check_source(bad).unwrap_err();
            assert!(e.contains(why), "{bad}: {e}");
        }
        // comments do not count; a module with a body is fine
        assert!(check_source("// include_str!\nmod m { pub fn f() {} }\nfn run(a: &Args) -> Result<String, String> { Ok(m::f()).map(|_| String::new()) }").is_ok());
    }
    #[test]
    fn a_function_compiles_and_runs_in_the_sandbox() {
        let c = compiler("ok");
        let wasm = c
            .compile(
                r#"fn run(args: &Args) -> Result<String, String> {
    let each = args.num("total")? / args.num("people")?;
    phone("send_sms", &[("to", args.get("who")?), ("text", &format!("you owe {each:.2}"))])?;
    Ok(format!("{each:.2}"))
}"#,
            )
            .unwrap();
        let args = [("total", "1250"), ("people", "4"), ("who", "Ravi")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let o = run(&wasm, &args).unwrap();
        assert_eq!(o.result, "312.50");
        assert_eq!(o.calls[0].tool, "send_sms");
        assert_eq!(
            o.calls[0].args,
            vec![
                ("text".into(), "you owe 312.50".into()),
                ("to".into(), "Ravi".into())
            ]
        );
        let mut bad = args.clone();
        bad.insert("total".into(), "lots".into());
        assert_eq!(run(&wasm, &bad).unwrap_err(), "total is not a number");
    }
    #[test]
    fn compiler_errors_use_the_agents_line_numbers() {
        let e = compiler("err")
            .compile("fn run(args: &Args) -> Result<String, String> {\n    let x: i64 = \"text\";\n    Ok(x.to_string())\n}")
            .unwrap_err();
        assert!(e.starts_with("does not compile"), "{e}");
        assert!(e.contains("f.rs:2:"), "{e}");
        assert!(e.contains("mismatched types"), "{e}");
    }
    #[test]
    fn a_compiler_crash_is_reported_plainly() {
        // rustc 1.88 crashes on this; newer compilers report mismatched types. Either way the
        // message points at line 2 and carries no compiler internals.
        let e = compiler("ice")
            .compile("fn run(args: &Args) -> Result<String, String> {\n    let each: f64 = args.get(\"total\")? / args.num(\"people\")?;\n    Ok(format!(\"{:.2}\", each))\n}")
            .unwrap_err();
        assert!(e.contains("f.rs:2:"), "{e}");
        assert!(!e.contains("HirId") && !e.contains("delayed bugs"), "{e}");
    }
    #[test]
    fn the_harness_parses_messages_and_calls_into_records() {
        let w = compiler("parse").compile(r#"fn run(args: &Args) -> Result<String, String> {
    let m = messages(args.get("m")?);
    let c = calls(args.get("c")?);
    let contacts: std::collections::BTreeSet<String> =
        m.iter().filter(|x| x.incoming).filter_map(|x| x.name.clone()).collect();
    let strangers = m.iter().filter(|x| x.incoming && x.name.is_none()).count();
    let ravi_called = c.iter().filter(|x| x.incoming && x.name.as_deref() == Some("Ravi K")).count();
    Ok(format!("{}|{}|{}|{}|{}", contacts.len(), strangers, ravi_called, m[0].text, c[1].state))
}"#).unwrap();
        let mut args = std::collections::BTreeMap::new();
        args.insert("m".to_string(), "from Mum (+91999): dinner: at 8?\nfrom Ravi K (+91888): hi\nto Mum (+91999): ok\nfrom +91800: sale!\nfrom Mum (+91999): milk".to_string());
        args.insert("c".to_string(), "from Ravi K (+91888) ended\nto +91800 active\nno more".to_string());
        // two contacts texted; one stranger; Ravi K called once; text keeps its colon; state parsed
        assert_eq!(run(&w, &args).unwrap().result, "2|1|1|dinner: at 8?|active");
    }
    #[test]
    fn panics_and_runaway_loops_are_stopped() {
        let c = compiler("trap");
        let panics = c.compile("fn run(args: &Args) -> Result<String, String> { let v: Vec<u8> = vec![]; Ok(v[3].to_string()) }").unwrap();
        assert!(run(&panics, &Default::default())
            .unwrap_err()
            .contains("panicked"));
        let spins = c.compile("fn run(args: &Args) -> Result<String, String> { let mut i: u64 = 0; loop { i = std::hint::black_box(i.wrapping_add(1)); if i == 0 { break; } } Ok(i.to_string()) }").unwrap();
        assert!(run(&spins, &Default::default())
            .unwrap_err()
            .contains("out of fuel"));
    }
    #[test]
    fn too_many_phone_requests_is_an_error() {
        let c = compiler("calls");
        let w = c.compile("fn run(args: &Args) -> Result<String, String> { for _ in 0..20 { phone(\"hangup\", &[])?; } Ok(String::new()) }").unwrap();
        assert!(run(&w, &Default::default())
            .unwrap_err()
            .contains("at most 10"));
    }
    #[test]
    fn the_compiler_is_resolved_so_the_build_directory_cannot_change_it() {
        let c = compiler("resolve");
        assert!(
            c.rustc.is_absolute() && c.rustc.ends_with("bin/rustc"),
            "{}",
            c.rustc.display()
        );
        // without rustup's variables, from another directory, it still finds wasm32's std
        let out = Command::new(&c.rustc)
            .current_dir(std::env::temp_dir())
            .env_remove("RUSTUP_TOOLCHAIN")
            .args([
                "--print",
                "target-libdir",
                "--target",
                "wasm32-unknown-unknown",
            ])
            .output()
            .unwrap();
        assert!(Path::new(String::from_utf8_lossy(&out.stdout).trim()).exists());
    }
    #[test]
    fn the_compiler_reports_itself() {
        let v = compiler("check").check().unwrap();
        assert!(v.starts_with("rustc "), "{v}");
        let missing = Compiler::new("/nonexistent/rustc".into(), std::env::temp_dir());
        assert!(missing.check().unwrap_err().contains("apk add rust"));
    }
}
