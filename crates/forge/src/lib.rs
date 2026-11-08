//! The Tool Forge: how a new tool gets onto the phone.
//! Lifecycle: submitted -> tested in the sandbox -> approved by the human -> installed.
//! Every forged tool is pure computation (the sandbox grants it nothing else), so it is Low tier.
use agentos_action::{Action, Tier, Tool};
use agentos_executor::Connector;
use agentos_sandbox::{run, Limits};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct Candidate {
    pub name: String,
    pub args: Vec<String>,
    pub wasm: Vec<u8>,
    /// (input JSON text, expected output text). At least three are required.
    pub examples: Vec<(String, String)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ForgeError {
    BadName(String),
    NameTaken(String),
    TooManyArgs,
    TooFewExamples,
    ExampleFailed { index: usize, got: String },
    NotTested(String),
}

fn valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

pub const MIN_EXAMPLES: usize = 3;

pub struct Forge {
    taken: HashSet<String>,
    tested: HashMap<String, Candidate>,
}

impl Forge {
    /// `existing` is every tool name already on the phone. A forged tool can never reuse one,
    /// so a forged "pay" cannot stand in for the real one.
    pub fn new(existing: &[&str]) -> Self {
        Forge {
            taken: existing.iter().map(|s| s.to_string()).collect(),
            tested: HashMap::new(),
        }
    }

    /// Check the manifest, then run every example in the sandbox. All must match exactly.
    pub fn submit(&mut self, c: Candidate) -> Result<(), ForgeError> {
        if !valid_ident(&c.name) {
            return Err(ForgeError::BadName(c.name));
        }
        if self.taken.contains(&c.name) {
            return Err(ForgeError::NameTaken(c.name));
        }
        if c.args.len() > 8 {
            return Err(ForgeError::TooManyArgs);
        }
        for a in &c.args {
            if !valid_ident(a) {
                return Err(ForgeError::BadName(a.clone()));
            }
        }
        if c.examples.len() < MIN_EXAMPLES {
            return Err(ForgeError::TooFewExamples);
        }
        for (i, (input, expected)) in c.examples.iter().enumerate() {
            let got = match run(&c.wasm, input.as_bytes(), Limits::default()) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
                Err(e) => format!("{e:?}"),
            };
            if &got != expected {
                return Err(ForgeError::ExampleFailed { index: i, got });
            }
        }
        self.tested.insert(c.name.clone(), c);
        Ok(())
    }

    /// The human has seen the manifest and the examples and said yes. Returns the connector to install.
    pub fn approve(&mut self, name: &str) -> Result<ForgedTool, ForgeError> {
        let c = self
            .tested
            .remove(name)
            .ok_or_else(|| ForgeError::NotTested(name.to_string()))?;
        self.taken.insert(c.name.clone());
        Ok(ForgedTool::new(&c))
    }

    /// Withdraw a tool. Its name becomes free again once the executor is rebuilt without it.
    pub fn revoke(&mut self, name: &str) {
        self.taken.remove(name);
        self.tested.remove(name);
    }
}

/// An installed forged tool. Names are leaked to fit the registry's `&'static str` fields;
/// that is a few bytes per tool, bounded by how many tools you approve.
pub struct ForgedTool {
    name: &'static str,
    args: &'static [&'static str],
    wasm: Vec<u8>,
}

impl ForgedTool {
    fn new(c: &Candidate) -> Self {
        let name: &'static str = Box::leak(c.name.clone().into_boxed_str());
        let args: Vec<&'static str> = c
            .args
            .iter()
            .map(|a| &*Box::leak(a.clone().into_boxed_str()))
            .collect();
        ForgedTool {
            name,
            args: Box::leak(args.into_boxed_slice()),
            wasm: c.wasm.clone(),
        }
    }
}

fn json_escape(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

/// The exact text a forged tool receives: its arguments as a JSON object, keys sorted.
pub fn input_json(a: &Action) -> String {
    let parts: Vec<String> = a
        .args
        .iter()
        .map(|(k, v)| format!("\"{}\":\"{}\"", json_escape(k), json_escape(v)))
        .collect();
    format!("{{{}}}", parts.join(","))
}

impl Connector for ForgedTool {
    fn name(&self) -> &str {
        self.name
    }
    fn tools(&self) -> Vec<Tool> {
        vec![Tool {
            name: self.name,
            args: self.args,
            tier: Tier::Low,
        }]
    }
    fn call(&mut self, action: &Action) -> Result<String, String> {
        let out = run(&self.wasm, input_json(action).as_bytes(), Limits::default())
            .map_err(|e| format!("{e:?}"))?;
        String::from_utf8(out).map_err(|_| "tool returned text that is not UTF-8".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_executor::{ExecError, Executor};
    use agentos_gate::Gate;
    use agentos_hal::FakeClock;
    use agentos_store::Store;
    use std::sync::Arc;

    const ECHO: &str = r#"(module
      (memory (export "memory") 1)
      (global $n (mut i32) (i32.const 1024))
      (func (export "alloc") (param $l i32) (result i32) (local $p i32)
        (local.set $p (global.get $n)) (global.set $n (i32.add (global.get $n) (local.get $l))) (local.get $p))
      (func (export "run") (param $p i32) (param $l i32) (result i64)
        (i64.or (i64.shl (i64.extend_i32_u (local.get $p)) (i64.const 32)) (i64.extend_i32_u (local.get $l)))))"#;

    const FORTY_TWO: &str = r#"(module
      (memory (export "memory") 1)
      (data (i32.const 0) "42")
      (func (export "alloc") (param i32) (result i32) (i32.const 2048))
      (func (export "run") (param i32 i32) (result i64) (i64.const 2)))"#;

    fn cand(name: &str, wat_text: &str, examples: Vec<(&str, &str)>) -> Candidate {
        Candidate {
            name: name.into(),
            args: vec!["a".into(), "b".into()],
            wasm: wat::parse_str(wat_text).unwrap(),
            examples: examples
                .into_iter()
                .map(|(i, o)| (i.to_string(), o.to_string()))
                .collect(),
        }
    }
    fn echo_examples() -> Vec<(&'static str, &'static str)> {
        vec![("x", "x"), ("hello", "hello"), ("{}", "{}")]
    }

    #[test]
    fn a_good_tool_passes_its_examples_and_can_be_approved() {
        let mut f = Forge::new(&["pay"]);
        f.submit(cand("echo", ECHO, echo_examples())).unwrap();
        assert!(f.approve("echo").is_ok());
    }
    #[test]
    fn a_forged_tool_cannot_take_an_existing_name() {
        let mut f = Forge::new(&["pay", "send_message"]);
        assert_eq!(
            f.submit(cand("pay", ECHO, echo_examples())),
            Err(ForgeError::NameTaken("pay".into()))
        );
    }
    #[test]
    fn names_are_restricted() {
        let mut f = Forge::new(&[]);
        for bad in ["", "Pay", "a b", "x;y", "../etc"] {
            assert!(
                matches!(
                    f.submit(cand(bad, ECHO, echo_examples())),
                    Err(ForgeError::BadName(_))
                ),
                "{bad}"
            );
        }
    }
    #[test]
    fn too_few_examples_is_refused() {
        let mut f = Forge::new(&[]);
        assert_eq!(
            f.submit(cand("echo", ECHO, vec![("x", "x"), ("y", "y")])),
            Err(ForgeError::TooFewExamples)
        );
    }
    #[test]
    fn a_wrong_example_names_the_failing_one() {
        let mut f = Forge::new(&[]);
        let r = f.submit(cand(
            "echo",
            ECHO,
            vec![("x", "x"), ("y", "WRONG"), ("z", "z")],
        ));
        assert_eq!(
            r,
            Err(ForgeError::ExampleFailed {
                index: 1,
                got: "y".into()
            })
        );
    }
    #[test]
    fn an_untested_tool_cannot_be_approved() {
        let mut f = Forge::new(&[]);
        assert_eq!(
            f.approve("ghost").err(),
            Some(ForgeError::NotTested("ghost".into()))
        );
    }
    #[test]
    fn a_tool_that_asks_for_the_outside_world_never_gets_tested() {
        let wat_text = r#"(module (import "env" "net" (func (result i32)))
          (memory (export "memory") 1)
          (func (export "alloc") (param i32) (result i32) (i32.const 0))
          (func (export "run") (param i32 i32) (result i64) (i64.const 0)))"#;
        let mut f = Forge::new(&[]);
        let r = f.submit(cand(
            "sneaky",
            wat_text,
            vec![("1", "0"), ("2", "0"), ("3", "0")],
        ));
        assert!(matches!(r, Err(ForgeError::ExampleFailed { index: 0, .. })));
        assert!(f.approve("sneaky").is_err());
    }
    #[test]
    fn approval_cannot_be_used_twice() {
        let mut f = Forge::new(&[]);
        f.submit(cand("echo", ECHO, echo_examples())).unwrap();
        f.approve("echo").unwrap();
        assert!(f.approve("echo").is_err());
        // and the name is now taken
        assert_eq!(
            f.submit(cand("echo", ECHO, echo_examples())),
            Err(ForgeError::NameTaken("echo".into()))
        );
    }
    #[test]
    fn revoking_frees_the_name() {
        let mut f = Forge::new(&[]);
        f.submit(cand("echo", ECHO, echo_examples())).unwrap();
        f.approve("echo").unwrap();
        f.revoke("echo");
        assert!(f.submit(cand("echo", ECHO, echo_examples())).is_ok());
    }
    #[test]
    fn input_json_escapes_and_sorts() {
        let a = Action {
            tool: "t".into(),
            args: vec![
                ("a".into(), "say \"hi\"\n".into()),
                ("b".into(), "x".into()),
            ],
        };
        assert_eq!(input_json(&a), "{\"a\":\"say \\\"hi\\\"\\n\",\"b\":\"x\"}");
    }
    #[test]
    fn an_installed_tool_runs_through_the_executor_as_low_tier() {
        let mut f = Forge::new(&[]);
        f.submit(cand(
            "answer",
            FORTY_TWO,
            vec![("1", "42"), ("2", "42"), ("3", "42")],
        ))
        .unwrap();
        let tool = f.approve("answer").unwrap();
        let clock = FakeClock::new();
        let mut ex = Executor::new(
            vec![Box::new(tool)],
            Store::in_memory().unwrap(),
            Arc::new(clock.clone()),
            Gate::new(b"k", Arc::new(clock), 60_000),
        );
        let act = Action {
            tool: "answer".into(),
            args: vec![("a".into(), "1".into()), ("b".into(), "2".into())],
        };
        assert_eq!(ex.run(&act), Ok("42".to_string()));
        assert_eq!(ex.registry().tier_of("answer"), Some(Tier::Low));
    }
    #[test]
    fn the_executor_still_validates_forged_tool_arguments() {
        let mut f = Forge::new(&[]);
        f.submit(cand(
            "answer",
            FORTY_TWO,
            vec![("1", "42"), ("2", "42"), ("3", "42")],
        ))
        .unwrap();
        let tool = f.approve("answer").unwrap();
        let clock = FakeClock::new();
        let ex = Executor::new(
            vec![Box::new(tool)],
            Store::in_memory().unwrap(),
            Arc::new(clock.clone()),
            Gate::new(b"k", Arc::new(clock), 60_000),
        );
        assert!(ex
            .registry()
            .parse(r#"{"tool":"answer","args":{"a":"1"}}"#)
            .is_err());
        let mut ex = ex;
        let bogus = Action {
            tool: "nope".into(),
            args: vec![],
        };
        assert_eq!(ex.run(&bogus), Err(ExecError::UnknownTool("nope".into())));
    }
}
