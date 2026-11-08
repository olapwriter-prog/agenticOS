//! A minimal MCP (Model Context Protocol) server core, transport-free.
//! It turns JSON-RPC 2.0 requests into executor calls and back. The HTTP layer that carries
//! the requests is added in Volume 7. Check the method names and fields against the current
//! MCP specification before you connect a real client.
use agentos_action::{Action, Tier};
use agentos_executor::{ExecError, Executor};
use serde_json::{json, Value};

pub const PROTOCOL_VERSION: &str = "2025-06-18";

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn tool_result(id: &Value, text: &str, is_error: bool) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": text}], "isError": is_error}})
}

fn list_tools(exec: &Executor) -> Vec<Value> {
    exec.tools()
        .iter()
        .map(|t| {
            let props: serde_json::Map<String, Value> = t.args.iter().map(|a| (a.to_string(), json!({"type": "string"}))).collect();
            let mut tool = json!({
                "name": t.name,
                "description": format!("{:?}-risk tool with arguments: {}", t.tier, t.args.join(", ")),
                "inputSchema": {"type": "object", "properties": props, "required": t.args, "additionalProperties": false}
            });
            // High tools ask the Claude app to put its own approval step in front of the call.
            if t.tier == Tier::High {
                tool["_meta"] = json!({"anthropic/requiresUserInteraction": true});
            }
            tool
        })
        .collect()
}

/// Handle one request. Returns None for notifications (requests with no id).
pub fn handle(exec: &mut Executor, request: &str) -> Option<String> {
    let req: Value = match serde_json::from_str(request) {
        Ok(v) => v,
        Err(_) => return Some(error(&Value::Null, -32700, "parse error").to_string()),
    };
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    // A request with no id is a notification: it gets no reply.
    let id = req.get("id").cloned()?;
    let resp = match method {
        "initialize" => json!({"jsonrpc": "2.0", "id": id, "result": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "agentos-mcp", "version": "0.1.0"}
        }}),
        "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
        "tools/list" => json!({"jsonrpc": "2.0", "id": id, "result": {"tools": list_tools(exec)}}),
        "tools/call" => call(exec, &id, req.get("params")),
        _ => error(&id, -32601, "method not found"),
    };
    Some(resp.to_string())
}

fn call(exec: &mut Executor, id: &Value, params: Option<&Value>) -> Value {
    let Some(p) = params else {
        return error(id, -32602, "missing params");
    };
    let Some(name) = p.get("name").and_then(Value::as_str) else {
        return error(id, -32602, "missing tool name");
    };
    let args = p.get("arguments").cloned().unwrap_or(json!({}));
    // Rebuild the call as the canonical action text and let the registry judge it.
    let text = json!({"tool": name, "args": args}).to_string();
    let action: Action = match exec.registry().parse(&text) {
        Ok(a) => a,
        Err(e) => return tool_result(id, &format!("rejected: {e:?}"), true),
    };
    match exec.run(&action) {
        Ok(out) => tool_result(id, &out, false),
        Err(ExecError::NeedsApproval) => tool_result(
            id,
            "approval required: press the approval key on the phone, then ask again",
            true,
        ),
        Err(e) => tool_result(id, &format!("failed: {e:?}"), true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_connectors::{Calendar, Messenger, Reminders};
    use agentos_gate::Gate;
    use agentos_hal::FakeClock;
    use agentos_store::Store;
    use std::sync::Arc;

    fn exec() -> Executor {
        let clock = FakeClock::new();
        Executor::new(
            vec![
                Box::new(Calendar::default()),
                Box::new(Reminders::default()),
                Box::new(Messenger::default()),
            ],
            Store::in_memory().unwrap(),
            Arc::new(clock.clone()),
            Gate::new(b"k", Arc::new(clock), 600_000),
        )
    }
    fn rpc(e: &mut Executor, req: Value) -> Value {
        serde_json::from_str(&handle(e, &req.to_string()).unwrap()).unwrap()
    }

    #[test]
    fn initialize_reports_tools_capability() {
        let mut e = exec();
        let r = rpc(
            &mut e,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        );
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert!(r["result"]["capabilities"]["tools"].is_object());
    }
    #[test]
    fn tools_list_includes_every_tool_and_flags_only_high_ones() {
        let mut e = exec();
        let r = rpc(
            &mut e,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        );
        let tools = r["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 4);
        for t in tools {
            let flagged = t.get("_meta").is_some();
            assert_eq!(flagged, t["name"] == "send_message", "tool {}", t["name"]);
        }
    }
    #[test]
    fn a_low_tool_call_runs() {
        let mut e = exec();
        let r = rpc(
            &mut e,
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_calendar","arguments":{"day":"mon"}}}),
        );
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(r["result"]["content"][0]["text"], "nothing");
    }
    #[test]
    fn a_high_tool_call_is_refused_and_nothing_is_sent() {
        let mut e = exec();
        let r = rpc(
            &mut e,
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"send_message","arguments":{"to":"asha","text":"hi"}}}),
        );
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("approval required"));
    }
    #[test]
    fn bad_arguments_are_rejected_by_the_registry() {
        let mut e = exec();
        let r = rpc(
            &mut e,
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"read_calendar","arguments":{"day":"mon","extra":"x"}}}),
        );
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("ExtraArg"));
    }
    #[test]
    fn an_unknown_tool_is_rejected() {
        let mut e = exec();
        let r = rpc(
            &mut e,
            json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"format_disk","arguments":{}}}),
        );
        assert_eq!(r["result"]["isError"], true);
    }
    #[test]
    fn unknown_methods_get_a_json_rpc_error() {
        let mut e = exec();
        let r = rpc(
            &mut e,
            json!({"jsonrpc":"2.0","id":7,"method":"resources/list"}),
        );
        assert_eq!(r["error"]["code"], -32601);
    }
    #[test]
    fn notifications_get_no_reply() {
        let mut e = exec();
        assert!(handle(
            &mut e,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
        )
        .is_none());
    }
    #[test]
    fn garbage_gets_a_parse_error() {
        let mut e = exec();
        let r: Value = serde_json::from_str(&handle(&mut e, "not json").unwrap()).unwrap();
        assert_eq!(r["error"]["code"], -32700);
    }
}
