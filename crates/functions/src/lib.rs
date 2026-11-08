//! Functions the agent writes for itself, in Rust.
//!
//! A function is `fn run(args: &Args) -> Result<String, String>` (plus helpers, std only) with a
//! name, declared arguments, and at least three examples. Lifecycle: write (checked, compiled to
//! WebAssembly with rustc, tested against its examples in the sandbox) -> install (the human
//! presses the approval key for that exact version) -> run. Writing again under the same name
//! makes a new version; earlier installed versions can be rolled back to.
//!
//! The compiled module runs in `agentos-sandbox`: no imports at all, so no files, network or
//! clock, and capped fuel and memory. A function that wants the phone to act calls
//! `phone("send_sms", &[("to", who), ("text", msg)])`; that only records a request, which the
//! daemon carries out afterwards through the executor, so tiers and the approval key still apply.
pub mod compile;

pub use compile::{check_source, Compiler, Outcome};

use agentos_action::{Action, Tier, Tool};
use agentos_executor::Connector;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;

pub const MIN_EXAMPLES: usize = 3;
pub const MAX_EXAMPLES: usize = 20;
pub const MAX_ARGS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Example {
    pub args: BTreeMap<String, String>,
    pub expect: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Version {
    pub version: u32,
    pub description: String,
    pub args: Vec<String>,
    /// Rust source.
    pub code: String,
    pub examples: Vec<Example>,
    /// Has the human ever installed this version? Only such versions can be rolled back to.
    pub approved: bool,
    /// The compiled module; kept in its own file next to the function's JSON.
    #[serde(skip)]
    pub wasm: Vec<u8>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Function {
    pub name: String,
    pub versions: Vec<Version>,
    pub installed: Option<u32>,
}

impl Function {
    fn version(&self, v: u32) -> Option<&Version> {
        self.versions.iter().find(|x| x.version == v)
    }
    pub fn current(&self) -> Option<&Version> {
        self.installed.and_then(|v| self.version(v))
    }
}

fn valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.as_bytes()[0].is_ascii_lowercase()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// `{"a":"1"}` or `a=1; b=two` (the second form needs no quotes, for the local model's grammar).
pub fn parse_args(s: &str) -> Result<BTreeMap<String, String>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(BTreeMap::new());
    }
    if s.starts_with('{') {
        let v: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(s).map_err(|e| format!("args: {e}"))?;
        return Ok(v
            .into_iter()
            .map(|(k, v)| {
                let t = v
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| v.to_string());
                (k, t)
            })
            .collect());
    }
    s.split([';', ','])
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            let (k, v) = p
                .split_once('=')
                .ok_or_else(|| format!("args: expected name=value, got {p:?}"))?;
            Ok((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

pub struct FunctionStore {
    dir: Option<PathBuf>,
    pub funcs: BTreeMap<String, Function>,
    /// Names of the phone's own tools and of these tools: a function can never take one.
    reserved: BTreeSet<String>,
    pub compiler: Compiler,
}

pub type Shared = Rc<RefCell<FunctionStore>>;

impl FunctionStore {
    pub fn in_memory(reserved: &[&str], compiler: Compiler) -> Self {
        FunctionStore {
            dir: None,
            funcs: BTreeMap::new(),
            reserved: reserved
                .iter()
                .map(|s| s.to_string())
                .chain(TOOLS.iter().map(|t| t.name.to_string()))
                .collect(),
            compiler,
        }
    }

    /// One JSON file per function in `dir`, and one .wasm file per version.
    pub fn open(dir: PathBuf, reserved: &[&str], compiler: Compiler) -> Result<Self, String> {
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut s = Self::in_memory(reserved, compiler);
        let rd = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json") {
                let t = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
                let mut f: Function =
                    serde_json::from_str(&t).map_err(|e| format!("{}: {e}", p.display()))?;
                for v in &mut f.versions {
                    v.wasm = std::fs::read(dir.join(format!("{}.v{}.wasm", f.name, v.version)))
                        .unwrap_or_default();
                }
                s.funcs.insert(f.name.clone(), f);
            }
        }
        s.dir = Some(dir);
        Ok(s)
    }

    pub fn shared(self) -> Shared {
        Rc::new(RefCell::new(self))
    }

    fn save(&self, name: &str) -> Result<(), String> {
        let (Some(dir), Some(f)) = (&self.dir, self.funcs.get(name)) else {
            return Ok(());
        };
        for v in &f.versions {
            let w = dir.join(format!("{name}.v{}.wasm", v.version));
            if !w.exists() && !v.wasm.is_empty() {
                std::fs::write(&w, &v.wasm).map_err(|e| e.to_string())?;
            }
        }
        let p = dir.join(format!("{name}.json"));
        let tmp = p.with_extension("tmp");
        let t = serde_json::to_string_pretty(f).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, t).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
    }

    /// Check, compile and test a new version. Only a version that passes every example is kept.
    /// The error is a report meant for whoever wrote the code, so they can fix it.
    pub fn write(
        &mut self,
        name: &str,
        description: &str,
        args: &[String],
        code: &str,
        examples: Vec<Example>,
    ) -> Result<u32, String> {
        if !valid_ident(name) {
            return Err(format!(
                "bad name {name:?}: lowercase letters, digits and _ only"
            ));
        }
        if self.reserved.contains(name) {
            return Err(format!(
                "{name} is one of the phone's own tools; pick another name"
            ));
        }
        if args.len() > MAX_ARGS {
            return Err(format!("at most {MAX_ARGS} arguments"));
        }
        if let Some(a) = args.iter().find(|a| !valid_ident(a)) {
            return Err(format!("bad argument name {a:?}"));
        }
        if examples.len() < MIN_EXAMPLES || examples.len() > MAX_EXAMPLES {
            return Err(format!("give {MIN_EXAMPLES} to {MAX_EXAMPLES} examples"));
        }
        let wasm = self.compiler.compile(code)?;
        let declared: BTreeSet<&String> = args.iter().collect();
        let mut report = Vec::new();
        let mut failed = 0;
        for (i, ex) in examples.iter().enumerate() {
            let given: BTreeSet<&String> = ex.args.keys().collect();
            if given != declared {
                failed += 1;
                report.push(format!(
                    "example {}: its args {:?} are not the declared args {:?}",
                    i + 1,
                    given,
                    declared
                ));
                continue;
            }
            match compile::run(&wasm, &ex.args) {
                Ok(o) if o.result.trim() == ex.expect.trim() => {
                    report.push(format!("example {}: ok", i + 1))
                }
                Ok(o) => {
                    failed += 1;
                    report.push(format!(
                        "example {}: expected {:?}, got {:?}",
                        i + 1,
                        ex.expect,
                        o.result
                    ));
                }
                Err(e) => {
                    failed += 1;
                    report.push(format!("example {}: error: {e}", i + 1));
                }
            }
        }
        if failed > 0 {
            return Err(format!(
                "{failed} of {} examples failed:\n{}",
                examples.len(),
                report.join("\n")
            ));
        }
        let f = self
            .funcs
            .entry(name.to_string())
            .or_insert_with(|| Function {
                name: name.to_string(),
                ..Default::default()
            });
        let version = f.versions.iter().map(|v| v.version).max().unwrap_or(0) + 1;
        f.versions.push(Version {
            version,
            description: description.to_string(),
            args: args.to_vec(),
            code: code.to_string(),
            examples,
            approved: false,
            wasm,
        });
        self.save(name)?;
        Ok(version)
    }

    /// Make a tested version the one that runs. The caller has the human's approval for it.
    pub fn install(&mut self, name: &str, version: u32) -> Result<(), String> {
        let f = self
            .funcs
            .get_mut(name)
            .ok_or_else(|| format!("no function {name}"))?;
        let v = f
            .versions
            .iter_mut()
            .find(|v| v.version == version)
            .ok_or_else(|| format!("{name} has no version {version}"))?;
        v.approved = true;
        f.installed = Some(version);
        self.save(name)
    }

    /// Go back to a version the human installed before. Needs no new approval.
    pub fn rollback(&mut self, name: &str, version: u32) -> Result<(), String> {
        let f = self
            .funcs
            .get_mut(name)
            .ok_or_else(|| format!("no function {name}"))?;
        match f.version(version) {
            Some(v) if v.approved => {
                f.installed = Some(version);
                self.save(name)
            }
            Some(_) => Err(format!(
                "{name} v{version} was never installed; install it (needs the key)"
            )),
            None => Err(format!("{name} has no version {version}")),
        }
    }

    /// Stop running it. Its history stays, so it can be rolled back.
    pub fn remove(&mut self, name: &str) -> Result<(), String> {
        let f = self
            .funcs
            .get_mut(name)
            .ok_or_else(|| format!("no function {name}"))?;
        f.installed = None;
        self.save(name)
    }

    pub fn call(&mut self, name: &str, args: &BTreeMap<String, String>) -> Result<Outcome, String> {
        let f = self
            .funcs
            .get(name)
            .ok_or_else(|| format!("no function {name}"))?;
        let v = f
            .current()
            .ok_or_else(|| format!("{name} is not installed"))?;
        let given: BTreeSet<&String> = args.keys().collect();
        let declared: BTreeSet<&String> = v.args.iter().collect();
        if given != declared {
            return Err(format!("{name} takes ({})", v.args.join(", ")));
        }
        if v.wasm.is_empty() {
            // its module file went missing: build it again from the approved source
            let (vn, code) = (v.version, v.code.clone());
            let wasm = self.compiler.compile(&code)?;
            let f = self.funcs.get_mut(name).unwrap();
            if let Some(v) = f.versions.iter_mut().find(|x| x.version == vn) {
                v.wasm = wasm;
            }
            self.save(name)?;
        }
        let v = self.funcs[name].current().unwrap();
        compile::run(&v.wasm, args)
    }

    pub fn list(&self) -> String {
        let lines: Vec<String> = self
            .funcs
            .values()
            .map(|f| match f.current() {
                Some(v) => format!(
                    "{}({}) v{}: {}",
                    f.name,
                    v.args.join(", "),
                    v.version,
                    v.description
                ),
                None => format!(
                    "{} (not installed; versions {})",
                    f.name,
                    f.versions
                        .iter()
                        .map(|v| v.version.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            })
            .collect();
        if lines.is_empty() {
            "no functions yet".into()
        } else {
            lines.join("\n")
        }
    }

    /// Everything the human should read before pressing the key for `install`.
    pub fn show(&self, name: &str, version: Option<u32>) -> Result<String, String> {
        let f = self
            .funcs
            .get(name)
            .ok_or_else(|| format!("no function {name}"))?;
        let v = match version {
            Some(n) => f.version(n),
            None => f.current().or_else(|| f.versions.last()),
        }
        .ok_or_else(|| format!("{name}: no such version"))?;
        let status = match f.installed {
            Some(i) if i == v.version => "installed",
            _ if v.approved => "installed before (can roll back)",
            _ => "tested, not installed",
        };
        let ex: Vec<String> = v
            .examples
            .iter()
            .map(|e| format!("  {:?} -> {:?}", e.args, e.expect))
            .collect();
        Ok(format!(
            "{name} v{} ({status}): {}\nargs: {}\n--- code (Rust) ---\n{}\n--- examples ---\n{}",
            v.version,
            v.description,
            v.args.join(", "),
            v.code,
            ex.join("\n")
        ))
    }
}

/// The function tools, for the executor.
pub struct FunctionTools(pub Shared);

const TOOLS: &[Tool] = &[
    Tool {
        name: "list_functions",
        args: &[],
        tier: Tier::Low,
    },
    Tool {
        name: "show_function",
        args: &["name"],
        tier: Tier::Low,
    },
    Tool {
        name: "write_function",
        args: &["name", "description", "args", "code", "examples"],
        tier: Tier::Medium,
    },
    Tool {
        name: "install_function",
        args: &["name", "version"],
        tier: Tier::High,
    },
    Tool {
        name: "rollback_function",
        args: &["name", "version"],
        tier: Tier::Medium,
    },
    Tool {
        name: "remove_function",
        args: &["name"],
        tier: Tier::Medium,
    },
    Tool {
        name: "run_function",
        args: &["name", "args"],
        tier: Tier::Low,
    },
];

/// Tools a function's requests may never use: it cannot write, install or run functions.
pub fn is_function_tool(name: &str) -> bool {
    TOOLS.iter().any(|t| t.name == name)
}

fn arg<'a>(a: &'a Action, name: &str) -> Result<&'a str, String> {
    a.args
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| format!("missing {name}"))
}
fn version_arg(a: &Action) -> Result<u32, String> {
    arg(a, "version")?
        .trim()
        .trim_start_matches(['v', 'V'])
        .parse()
        .map_err(|_| "version is a number".to_string())
}

/// `[{"args": {...}, "expect": "..."}, ...]`
pub fn parse_examples(s: &str) -> Result<Vec<Example>, String> {
    serde_json::from_str(s).map_err(|e| {
        format!("examples must be a JSON list of {{\"args\": {{...}}, \"expect\": \"...\"}}: {e}")
    })
}

impl Connector for FunctionTools {
    fn name(&self) -> &str {
        "functions"
    }
    fn tools(&self) -> Vec<Tool> {
        TOOLS.to_vec()
    }
    fn call(&mut self, a: &Action) -> Result<String, String> {
        let mut s = self.0.borrow_mut();
        match a.tool.as_str() {
            "list_functions" => Ok(s.list()),
            "show_function" => s.show(arg(a, "name")?.trim(), None),
            "write_function" => {
                let name = arg(a, "name")?.trim();
                let args: Vec<String> = arg(a, "args")?
                    .split([',', ' '])
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .map(String::from)
                    .collect();
                let examples = parse_examples(arg(a, "examples")?)?;
                let n = examples.len();
                let v = s.write(
                    name,
                    arg(a, "description")?,
                    &args,
                    arg(a, "code")?,
                    examples,
                )?;
                Ok(format!(
                    "{name} v{v} passed all {n} examples. Installing it needs the approval key: \
                     install_function(name={name}, version={v})"
                ))
            }
            "install_function" => {
                let (name, v) = (arg(a, "name")?.trim(), version_arg(a)?);
                s.install(name, v)?;
                Ok(format!("{name} v{v} installed"))
            }
            "rollback_function" => {
                let (name, v) = (arg(a, "name")?.trim(), version_arg(a)?);
                s.rollback(name, v)?;
                Ok(format!("{name} is back to v{v}"))
            }
            "remove_function" => {
                let name = arg(a, "name")?.trim();
                s.remove(name)?;
                Ok(format!("{name} removed (its versions are kept)"))
            }
            "run_function" => {
                let name = arg(a, "name")?.trim();
                let o = s.call(name, &parse_args(arg(a, "args")?)?)?;
                if o.calls.is_empty() {
                    return Ok(o.result);
                }
                // The daemon carries these out through the executor; an MCP client may do the same.
                let calls: Vec<serde_json::Value> = o
                    .calls
                    .iter()
                    .map(|c| {
                        let m: serde_json::Map<String, serde_json::Value> = c
                            .args
                            .iter()
                            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                            .collect();
                        serde_json::json!({"tool": c.tool, "args": m})
                    })
                    .collect();
                Ok(serde_json::json!({"result": o.result, "calls": calls}).to_string())
            }
            other => Err(format!("functions cannot {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiler() -> Compiler {
        Compiler::new(
            "rustc".into(),
            std::env::temp_dir().join(format!("fn-store-{}", std::process::id())),
        )
    }
    fn ex(args: &[(&str, &str)], expect: &str) -> Example {
        Example {
            args: args
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            expect: expect.into(),
        }
    }
    fn act(tool: &str, args: &[(&str, &str)]) -> Action {
        Action {
            tool: tool.into(),
            args: args
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
    const SPLIT: &str = r#"fn run(args: &Args) -> Result<String, String> {
    let each = args.num("total")? / args.num("people")?;
    Ok(format!("{:.2}", each))
}"#;
    fn split_examples() -> Vec<Example> {
        vec![
            ex(&[("total", "900"), ("people", "3")], "300.00"),
            ex(&[("total", "100"), ("people", "4")], "25.00"),
            ex(&[("total", "10"), ("people", "3")], "3.33"),
        ]
    }
    fn args2() -> Vec<String> {
        vec!["total".into(), "people".into()]
    }

    #[test]
    fn write_install_run() {
        let mut s = FunctionStore::in_memory(&["send_sms"], compiler());
        let v = s
            .write(
                "split_bill",
                "split a bill",
                &args2(),
                SPLIT,
                split_examples(),
            )
            .unwrap();
        assert_eq!(v, 1);
        let a = parse_args("total=1250; people=4").unwrap();
        assert!(s
            .call("split_bill", &a)
            .unwrap_err()
            .contains("not installed"));
        s.install("split_bill", 1).unwrap();
        assert_eq!(s.call("split_bill", &a).unwrap().result, "312.50");
    }
    #[test]
    fn failures_are_reported_and_nothing_is_kept() {
        let mut s = FunctionStore::in_memory(&[], compiler());
        let mut exs = split_examples();
        exs[2].expect = "3.34".into();
        let e = s
            .write("split_bill", "d", &args2(), SPLIT, exs)
            .unwrap_err();
        assert!(e.contains("1 of 3 examples failed"), "{e}");
        assert!(
            e.contains("example 3: expected \"3.34\", got \"3.33\""),
            "{e}"
        );
        let e = s
            .write(
                "broken",
                "d",
                &args2(),
                "fn run(args: &Args) -> Result<String, String> { Ok(1) }",
                split_examples(),
            )
            .unwrap_err();
        assert!(
            e.contains("does not compile") && e.contains("f.rs:1:"),
            "{e}"
        );
        let e = s
            .write("sneaky", "d", &args2(), r#"fn run(a: &Args) -> Result<String, String> { Ok(include_str!("/etc/hosts").into()) }"#, split_examples())
            .unwrap_err();
        assert!(e.contains("not allowed"), "{e}");
        assert!(s.funcs.is_empty());
    }
    #[test]
    fn names_and_shapes_are_checked() {
        let mut s = FunctionStore::in_memory(&["send_sms"], compiler());
        assert!(s
            .write("send_sms", "d", &args2(), SPLIT, split_examples())
            .unwrap_err()
            .contains("phone's own"));
        assert!(s
            .write("install_function", "d", &args2(), SPLIT, split_examples())
            .is_err());
        assert!(s
            .write("Bad-Name", "d", &args2(), SPLIT, split_examples())
            .is_err());
        assert!(s
            .write("f", "d", &args2(), SPLIT, split_examples()[..2].to_vec())
            .unwrap_err()
            .contains("examples"));
        let wrong = vec![ex(&[("total", "1")], "1"); 3];
        assert!(s
            .write("f", "d", &args2(), SPLIT, wrong)
            .unwrap_err()
            .contains("not the declared args"));
    }
    #[test]
    fn editing_makes_a_new_version_and_rollback_needs_a_past_install() {
        let mut s = FunctionStore::in_memory(&[], compiler());
        s.write("split_bill", "v1", &args2(), SPLIT, split_examples())
            .unwrap();
        s.install("split_bill", 1).unwrap();
        let tip = SPLIT.replace("let each = args", "let each = 1.1 * args");
        let tip_ex = vec![
            ex(&[("total", "900"), ("people", "3")], "330.00"),
            ex(&[("total", "100"), ("people", "4")], "27.50"),
            ex(&[("total", "10"), ("people", "1")], "11.00"),
        ];
        assert_eq!(
            s.write("split_bill", "with tip", &args2(), &tip, tip_ex)
                .unwrap(),
            2
        );
        assert_eq!(
            s.funcs["split_bill"].installed,
            Some(1),
            "not live until installed"
        );
        assert!(s
            .rollback("split_bill", 2)
            .unwrap_err()
            .contains("never installed"));
        s.install("split_bill", 2).unwrap();
        let a = parse_args(r#"{"total":"100","people":"4"}"#).unwrap();
        assert_eq!(s.call("split_bill", &a).unwrap().result, "27.50");
        s.rollback("split_bill", 1).unwrap();
        assert_eq!(s.call("split_bill", &a).unwrap().result, "25.00");
        s.remove("split_bill").unwrap();
        assert!(s.call("split_bill", &a).is_err());
        assert!(s
            .show("split_bill", Some(2))
            .unwrap()
            .contains("installed before"));
    }
    #[test]
    fn functions_and_their_modules_survive_a_restart() {
        let d = std::env::temp_dir().join(format!("fns-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mut s = FunctionStore::open(d.clone(), &[], compiler()).unwrap();
        s.write("split_bill", "d", &args2(), SPLIT, split_examples())
            .unwrap();
        s.install("split_bill", 1).unwrap();
        assert!(d.join("split_bill.v1.wasm").exists());
        let mut s2 = FunctionStore::open(d.clone(), &[], compiler()).unwrap();
        assert_eq!(s2.funcs["split_bill"].installed, Some(1));
        let a = parse_args("total=9; people=3").unwrap();
        assert_eq!(s2.call("split_bill", &a).unwrap().result, "3.00");
        // a lost module is rebuilt from the approved source
        std::fs::remove_file(d.join("split_bill.v1.wasm")).unwrap();
        let mut s3 = FunctionStore::open(d.clone(), &[], compiler()).unwrap();
        assert_eq!(s3.call("split_bill", &a).unwrap().result, "3.00");
        assert!(d.join("split_bill.v1.wasm").exists());
        std::fs::remove_dir_all(d).ok();
    }
    #[test]
    fn phone_requests_come_back_to_the_caller() {
        let mut t = FunctionTools(FunctionStore::in_memory(&[], compiler()).shared());
        let code = r#"fn run(args: &Args) -> Result<String, String> {
    let who = args.get("who")?;
    phone("send_sms", &[("to", who), ("text", &format!("Running {} min late", args.get("mins")?))])?;
    Ok(format!("told {who}"))
}"#;
        let exs = serde_json::json!([
            {"args": {"who": "Mum", "mins": "10"}, "expect": "told Mum"},
            {"args": {"who": "Ravi", "mins": "5"}, "expect": "told Ravi"},
            {"args": {"who": "Asha", "mins": "1"}, "expect": "told Asha"}
        ])
        .to_string();
        let r = t
            .call(&act(
                "write_function",
                &[
                    ("name", "tell_late"),
                    ("description", "say I'm late"),
                    ("args", "who, mins"),
                    ("code", code),
                    ("examples", &exs),
                ],
            ))
            .unwrap();
        assert!(
            r.contains("v1 passed all 3") && r.contains("approval key"),
            "{r}"
        );
        t.call(&act(
            "install_function",
            &[("name", "tell_late"), ("version", "1")],
        ))
        .unwrap();
        let out = t
            .call(&act(
                "run_function",
                &[("name", "tell_late"), ("args", "who=Mum; mins=10")],
            ))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["result"], "told Mum");
        assert_eq!(v["calls"][0]["tool"], "send_sms");
        assert_eq!(v["calls"][0]["args"]["text"], "Running 10 min late");
    }
    #[test]
    fn installing_is_high_tier_and_function_tools_are_known() {
        let t = FunctionTools(FunctionStore::in_memory(&[], compiler()).shared());
        let tier = |n| t.tools().iter().find(|x| x.name == n).unwrap().tier;
        assert_eq!(tier("install_function"), Tier::High);
        assert_eq!(tier("run_function"), Tier::Low);
        assert!(is_function_tool("write_function") && !is_function_tool("send_sms"));
    }
}
