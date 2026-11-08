//! The script runtime: the Rhai interpreter, compiled to WebAssembly and run inside the sandbox.
//!
//! Input (JSON):  {"code": "...", "args": {"name": "value", ...}}
//! Output (JSON): {"ok": "<result>", "calls": [{"tool": "...", "args": {...}}]}  or  {"error": "..."}
//!
//! A function is a Rhai script. It sees its arguments as the map `args` (all values are text),
//! and its last expression is its result. It cannot touch files, the network or the clock: the
//! sandbox provides no imports. To act on the phone it calls `phone("tool", #{...})`, which only
//! records the request; the daemon carries the requests out afterwards, through the executor's
//! tiers, so a function can never send a text without the approval key.
use rhai::{Dynamic, Engine, Map, Scope};
use std::cell::RefCell;
use std::rc::Rc;

/// Requests a script can make of the phone, in one run.
pub const MAX_CALLS: usize = 10;

fn engine(calls: Rc<RefCell<Vec<serde_json::Value>>>) -> Engine {
    let mut e = Engine::new();
    // Limits inside the interpreter, under the sandbox's own fuel and memory caps.
    e.set_max_operations(2_000_000);
    e.set_max_string_size(64 * 1024);
    e.set_max_array_size(10_000);
    e.set_max_map_size(10_000);
    e.set_max_call_levels(32);
    e.set_max_expr_depths(64, 32);
    e.disable_symbol("eval");
    e.on_print(|_| {});
    e.on_debug(|_, _, _| {});
    e.register_fn(
        "phone",
        move |tool: &str, args: Map| -> Result<(), Box<rhai::EvalAltResult>> {
            let mut c = calls.borrow_mut();
            if c.len() >= MAX_CALLS {
                return Err(
                    format!("a function may request at most {MAX_CALLS} phone actions").into(),
                );
            }
            let args: serde_json::Map<String, serde_json::Value> = args
                .into_iter()
                .map(|(k, v)| (k.to_string(), serde_json::Value::String(text(&v))))
                .collect();
            c.push(serde_json::json!({"tool": tool, "args": args}));
            Ok(())
        },
    );
    e
}

/// A value as the text a tool argument or result would carry.
fn text(v: &Dynamic) -> String {
    if v.is_unit() {
        String::new()
    } else if v.is_string() {
        v.clone().into_string().unwrap_or_default()
    } else if v.is_array() || v.is_map() {
        serde_json::to_string(v).unwrap_or_else(|_| v.to_string())
    } else {
        v.to_string()
    }
}

/// Run one script. Never panics; every failure is an `{"error": ...}`.
pub fn execute(input: &[u8]) -> String {
    let parsed: Result<serde_json::Value, _> = serde_json::from_slice(input);
    let Ok(req) = parsed else {
        return serde_json::json!({"error": "input is not JSON"}).to_string();
    };
    let Some(code) = req["code"].as_str() else {
        return serde_json::json!({"error": "no code"}).to_string();
    };
    let mut args = Map::new();
    if let Some(obj) = req["args"].as_object() {
        for (k, v) in obj {
            let s = v
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| v.to_string());
            args.insert(k.as_str().into(), s.into());
        }
    }
    let calls = Rc::new(RefCell::new(Vec::new()));
    let e = engine(calls.clone());
    let mut scope = Scope::new();
    scope.push("args", args);
    match e.eval_with_scope::<Dynamic>(&mut scope, code) {
        Ok(v) => serde_json::json!({"ok": text(&v), "calls": *calls.borrow()}).to_string(),
        Err(err) => serde_json::json!({"error": err.to_string()}).to_string(),
    }
}

#[cfg(target_arch = "wasm32")]
mod abi {
    /// Reserve `len` bytes for the host to write the input into.
    #[no_mangle]
    pub extern "C" fn alloc(len: i32) -> i32 {
        let mut v = Vec::<u8>::with_capacity(len.max(0) as usize);
        let p = v.as_mut_ptr();
        std::mem::forget(v);
        p as i32
    }
    /// Run the script at (ptr, len); return (out_ptr << 32) | out_len. One run per instance.
    #[no_mangle]
    pub extern "C" fn run(ptr: i32, len: i32) -> i64 {
        let input = unsafe { std::slice::from_raw_parts(ptr as *const u8, len.max(0) as usize) };
        let out = super::execute(input).into_bytes().leak();
        ((out.as_ptr() as i64) << 32) | out.len() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(code: &str, args: serde_json::Value) -> serde_json::Value {
        let input = serde_json::json!({"code": code, "args": args}).to_string();
        serde_json::from_str(&execute(input.as_bytes())).unwrap()
    }
    #[test]
    fn args_in_result_out() {
        let v = run(
            r#"let each = parse_float(args.total) / parse_int(args.people); `${each}`"#,
            serde_json::json!({"total": "900", "people": "3"}),
        );
        assert_eq!(v["ok"], "300.0");
    }
    #[test]
    fn arrays_and_maps_come_back_as_json() {
        assert_eq!(run("[1, 2, 3]", serde_json::json!({}))["ok"], "[1,2,3]");
        assert_eq!(run("#{a: 1}", serde_json::json!({}))["ok"], r#"{"a":1}"#);
    }
    #[test]
    fn phone_actions_are_requests_not_actions() {
        let v = run(
            r#"phone("send_sms", #{to: args.who, text: "late"}); "asked""#,
            serde_json::json!({"who": "Mum"}),
        );
        assert_eq!(v["ok"], "asked");
        assert_eq!(v["calls"][0]["tool"], "send_sms");
        assert_eq!(v["calls"][0]["args"]["to"], "Mum");
    }
    #[test]
    fn too_many_requests_is_an_error() {
        let v = run(
            r#"for i in 0..20 { phone("hangup", #{}) }"#,
            serde_json::json!({}),
        );
        assert!(v["error"].as_str().unwrap().contains("at most 10"));
    }
    #[test]
    fn runaway_loops_and_eval_are_stopped() {
        assert!(run("loop {}", serde_json::json!({}))["error"]
            .as_str()
            .unwrap()
            .contains("operations"));
        assert!(run(r#"eval("1")"#, serde_json::json!({}))["error"].is_string());
    }
    #[test]
    fn syntax_errors_say_where() {
        let e = run("let x = ;", serde_json::json!({}))["error"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(e.contains("line 1"), "{e}");
    }
}
