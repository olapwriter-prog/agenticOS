//! The analysis tools as a Claude client sees them over MCP. The client writes the code itself,
//! from the formats; agentd loads the data and runs the code in the sandbox, so the client gets
//! the answer and never the data.
use serde_json::{json, Value};

pub fn tool_specs() -> Vec<Value> {
    vec![
        json!({
            "name": "data_formats",
            "description": "The formats of the phone data analyze_data can load (formats only, no data).",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        }),
        json!({
            "name": "analyze_data",
            "description": "Answer a question over the phone's data without seeing it: name the sources to load and give Rust code `fn run(args: &Args) -> Result<String, String>` that reads each source as args.get(\"NAME\")? and returns only the answer. It is compiled with rustc and run in a sandbox; only the answer comes back. See data_formats.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "sources": {"type": "string", "description": "tool names separated by commas, or a JSON list of {\"tool\": NAME, \"args\": {...}}"},
                    "code": {"type": "string", "description": "the Rust source"},
                    "question": {"type": "string", "description": "optional; passed to the code as args.get(\"question\")"}
                },
                "required": ["sources", "code"],
                "additionalProperties": false
            }
        }),
    ]
}

pub fn reply(id: Value, out: Result<String, String>) -> String {
    let (text, err) = match out {
        Ok(t) => (t, false),
        Err(e) => (e, true),
    };
    json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": text}], "isError": err}})
        .to_string()
}
