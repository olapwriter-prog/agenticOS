//! An MCP client: agentd calls a remote MCP server (over HTTP, JSON-RPC 2.0) and offers its tools
//! as connectors. Every remote tool goes through the executor, so tiers, the approval key and the
//! ledger apply to it exactly as to a local tool. The network is behind the `Transport` trait, so
//! the tests run with a scripted fake and never touch the network.
use agentos_action::{Action, Tier, Tool};
use agentos_executor::Connector;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

pub const PROTOCOL_VERSION: &str = "2025-06-18";
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_TOOLS: usize = 20;
const MAX_ARGS: usize = 8;
const MAX_RESULT_CHARS: usize = 4000;

/// What a server answered to one POST.
#[derive(Debug, Clone, Default)]
pub struct Reply {
    pub body: String,
    pub content_type: String,
    /// The Mcp-Session-Id header, if the server sent one.
    pub session: Option<String>,
}

/// One round trip to a server. `Http` is the real one; tests use a fake.
pub trait Transport {
    fn post(
        &mut self,
        body: &Value,
        session: Option<&str>,
        timeout: Duration,
    ) -> Result<Reply, String>;
}

/// The JSON in an event-stream body: the text after the last `data:` line.
pub fn from_event_stream(body: &str) -> Option<&str> {
    body.lines()
        .rev()
        .find_map(|l| l.strip_prefix("data:"))
        .map(str::trim)
}

impl Default for Options {
    fn default() -> Self {
        Options {
            default_tier: Tier::High,
            trust_hints: false,
            allow: Vec::new(),
        }
    }
}

/// A remote MCP server over HTTP. Sends the bearer token, if any, and keeps the session id.
pub struct Http {
    pub url: String,
    pub token: Option<String>,
}

impl Transport for Http {
    fn post(
        &mut self,
        body: &Value,
        session: Option<&str>,
        timeout: Duration,
    ) -> Result<Reply, String> {
        let agent = ureq::AgentBuilder::new().timeout(timeout).build();
        let mut req = agent
            .post(&self.url)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json, text/event-stream")
            .set("MCP-Protocol-Version", PROTOCOL_VERSION);
        if let Some(t) = &self.token {
            req = req.set("Authorization", &format!("Bearer {t}"));
        }
        if let Some(s) = session {
            req = req.set("Mcp-Session-Id", s);
        }
        let resp = req
            .send_json(body.clone())
            .map_err(|e| format!("{}: {e}", self.url))?;
        let session = resp.header("Mcp-Session-Id").map(str::to_string);
        let content_type = resp.content_type().to_string();
        let text = resp.into_string().map_err(|e| e.to_string())?;
        Ok(Reply {
            body: text,
            content_type,
            session,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// The tier of every remote tool, unless hints say otherwise. High by default: a remote tool
    /// is unknown until the owner says otherwise.
    pub default_tier: Tier,
    /// Believe the server's readOnlyHint and destructiveHint. Off unless the owner asks for it.
    pub trust_hints: bool,
    /// When not empty, only these remote tools are offered.
    pub allow: Vec<String>,
}

struct RemoteTool {
    remote: String,
    /// argument name → JSON schema type ("string", "integer", ...)
    types: BTreeMap<String, String>,
}

/// A connected remote server, offered to the executor as a connector.
pub struct Remote {
    name: String,
    transport: Box<dyn Transport>,
    session: Option<String>,
    next_id: u64,
    tools: Vec<Tool>,
    /// local tool name → its remote description
    by_local: HashMap<String, RemoteTool>,
}

fn local_name(server: &str, remote: &str) -> String {
    format!("{server}_{remote}")
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

impl Remote {
    /// Connect: initialize, confirm, list the tools. Any failure is an error and nothing is kept.
    pub fn connect(
        name: &str,
        transport: Box<dyn Transport>,
        o: Options,
    ) -> Result<Remote, String> {
        let mut r = Remote {
            name: name.to_string(),
            transport,
            session: None,
            next_id: 1,
            tools: Vec::new(),
            by_local: HashMap::new(),
        };
        let init = r.request(
            "initialize",
            json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "agentd", "version": "0.1.0"}}),
            CONNECT_TIMEOUT,
        )?;
        if init.get("protocolVersion").is_none() {
            return Err(format!("{name}: initialize gave no protocol version"));
        }
        r.notify("notifications/initialized", CONNECT_TIMEOUT)?;
        let listed = r.request("tools/list", json!({}), CONNECT_TIMEOUT)?;
        let default_tier = o.default_tier;
        for t in listed["tools"]
            .as_array()
            .ok_or_else(|| format!("{name}: no tool list"))?
        {
            if r.tools.len() >= MAX_TOOLS {
                break;
            }
            let Some(remote) = t["name"].as_str() else {
                continue;
            };
            if !o.allow.is_empty() && !o.allow.iter().any(|a| a == remote) {
                continue;
            }
            let props = t["inputSchema"]["properties"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            if props.len() > MAX_ARGS {
                continue;
            }
            // BTreeMap keeps the argument names sorted, as the tool is declared
            let types: BTreeMap<String, String> = props
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v["type"].as_str().unwrap_or("string").to_string(),
                    )
                })
                .collect();
            let args: Vec<String> = types.keys().cloned().collect();
            let mut tier = default_tier;
            if o.trust_hints {
                let hints = &t["annotations"];
                if hints["readOnlyHint"] == json!(true) {
                    tier = Tier::Low;
                } else if hints["destructiveHint"] == json!(true) {
                    tier = Tier::High;
                }
            }
            let local = local_name(name, remote);
            r.tools.push(Tool::dynamic(&local, &args, tier));
            r.by_local.insert(
                local,
                RemoteTool {
                    remote: remote.to_string(),
                    types,
                },
            );
        }
        Ok(r)
    }

    /// Tools that were accepted, for the connector's list.
    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    fn post(&mut self, body: Value, timeout: Duration) -> Result<Reply, String> {
        let session = self.session.clone();
        let reply = self.transport.post(&body, session.as_deref(), timeout)?;
        if reply.session.is_some() {
            self.session = reply.session.clone();
        }
        Ok(reply)
    }

    fn notify(&mut self, method: &str, timeout: Duration) -> Result<(), String> {
        self.post(json!({"jsonrpc": "2.0", "method": method}), timeout)
            .map(|_| ())
    }

    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let reply = self.post(
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
            timeout,
        )?;
        let text = if reply.content_type.starts_with("text/event-stream") {
            from_event_stream(&reply.body)
                .ok_or_else(|| format!("{}: no data in the event stream", self.name))?
                .to_string()
        } else {
            reply.body
        };
        let v: Value =
            serde_json::from_str(&text).map_err(|e| format!("{}: bad JSON: {e}", self.name))?;
        if let Some(err) = v.get("error") {
            return Err(format!(
                "{}: {}",
                self.name,
                err["message"].as_str().unwrap_or("error")
            ));
        }
        Ok(v["result"].clone())
    }
}

impl Connector for Remote {
    fn name(&self) -> &str {
        &self.name
    }
    fn tools(&self) -> Vec<Tool> {
        self.tools.clone()
    }
    fn call(&mut self, a: &Action) -> Result<String, String> {
        let remote = self
            .by_local
            .get(&a.tool)
            .ok_or_else(|| format!("{} cannot {}", self.name, a.tool))?;
        let remote_name = remote.remote.clone();
        let mut args = serde_json::Map::new();
        for (k, v) in &a.args {
            if v.is_empty() {
                continue;
            }
            let ty = self.by_local[&a.tool]
                .types
                .get(k)
                .map(String::as_str)
                .unwrap_or("string");
            let value = if ty == "string" {
                json!(v)
            } else {
                serde_json::from_str::<Value>(v).unwrap_or_else(|_| json!(v))
            };
            args.insert(k.clone(), value);
        }
        let result = self.request(
            "tools/call",
            json!({"name": remote_name, "arguments": args}),
            CALL_TIMEOUT,
        )?;
        let text: Vec<&str> = result["content"]
            .as_array()
            .map(|c| c.iter().filter_map(|p| p["text"].as_str()).collect())
            .unwrap_or_default();
        let text: String = text.join("\n").chars().take(MAX_RESULT_CHARS).collect();
        if result["isError"] == json!(true) {
            return Err(text);
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    type Handler = Box<dyn FnMut(&Value) -> Result<Reply, String>>;

    /// A scripted server: `handle` answers each request by its method.
    struct Fake {
        handle: Rc<RefCell<Handler>>,
    }
    impl Transport for Fake {
        fn post(
            &mut self,
            body: &Value,
            _session: Option<&str>,
            _t: Duration,
        ) -> Result<Reply, String> {
            (self.handle.borrow_mut())(body)
        }
    }

    fn json_reply(id: &Value, result: Value) -> Reply {
        Reply {
            body: json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
            content_type: "application/json".into(),
            session: None,
        }
    }

    /// A server with one tool list and a tools/call answer.
    fn server(tools: Value, call_answer: Value) -> Box<dyn Transport> {
        let handle: Handler = Box::new(move |req: &Value| {
            let id = req.get("id").cloned().unwrap_or(Value::Null);
            match req["method"].as_str().unwrap_or("") {
                "initialize" => Ok(json_reply(
                    &id,
                    json!({"protocolVersion": PROTOCOL_VERSION}),
                )),
                "notifications/initialized" => Ok(Reply::default()),
                "tools/list" => Ok(json_reply(&id, json!({"tools": tools.clone()}))),
                "tools/call" => Ok(json_reply(&id, call_answer.clone())),
                _ => Err("unexpected method".into()),
            }
        });
        Box::new(Fake {
            handle: Rc::new(RefCell::new(handle)),
        })
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

    fn tool(name: &str, props: Value) -> Value {
        json!({"name": name, "inputSchema": {"type": "object", "properties": props}})
    }

    #[test]
    fn tool_names_get_the_prefix() {
        let t = server(
            json!([tool("search-food", json!({"query": {"type": "string"}}))]),
            json!({"content": []}),
        );
        let r = Remote::connect(
            "zomato",
            t,
            Options {
                default_tier: Tier::Low,
                ..Options::default()
            },
        )
        .unwrap();
        let names: Vec<String> = r.tools().iter().map(|t| t.name.to_string()).collect();
        assert_eq!(names, vec!["zomato_search_food"]);
    }

    #[test]
    fn a_read_only_hint_is_low_only_when_trusted() {
        let tools = json!([{"name": "list", "inputSchema": {"properties": {}}, "annotations": {"readOnlyHint": true}}]);
        let untrusted =
            Remote::connect("svc", server(tools.clone(), json!({})), Options::default()).unwrap();
        assert_eq!(
            untrusted.tools()[0].tier,
            Tier::High,
            "untrusted hints keep the default tier"
        );
        let trusted = Remote::connect(
            "svc",
            server(tools, json!({})),
            Options {
                trust_hints: true,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(trusted.tools()[0].tier, Tier::Low);
    }

    #[test]
    fn non_string_arguments_are_converted() {
        let sent: Rc<RefCell<Vec<Value>>> = Rc::new(RefCell::new(vec![]));
        let s2 = sent.clone();
        let tools = json!([tool(
            "add",
            json!({"count": {"type": "integer"}, "note": {"type": "string"}, "empty": {"type": "string"}})
        )]);
        let handle: Handler = Box::new(move |req: &Value| {
            let id = req.get("id").cloned().unwrap_or(Value::Null);
            match req["method"].as_str().unwrap_or("") {
                "initialize" => Ok(json_reply(
                    &id,
                    json!({"protocolVersion": PROTOCOL_VERSION}),
                )),
                "notifications/initialized" => Ok(Reply::default()),
                "tools/list" => Ok(json_reply(&id, json!({"tools": tools.clone()}))),
                _ => {
                    s2.borrow_mut().push(req["params"].clone());
                    Ok(json_reply(
                        &id,
                        json!({"content": [{"type": "text", "text": "ok"}]}),
                    ))
                }
            }
        });
        let t = Box::new(Fake {
            handle: Rc::new(RefCell::new(handle)),
        });
        let mut r = Remote::connect(
            "svc",
            t,
            Options {
                default_tier: Tier::Low,
                ..Options::default()
            },
        )
        .unwrap();
        r.call(&act(
            "svc_add",
            &[("count", "3"), ("note", "hi"), ("empty", "")],
        ))
        .unwrap();
        let p = sent.borrow()[0].clone();
        assert_eq!(p["arguments"]["count"], json!(3));
        assert_eq!(p["arguments"]["note"], json!("hi"));
        assert_eq!(p["name"], json!("add"));
    }

    #[test]
    fn empty_arguments_are_dropped() {
        let sent: Rc<RefCell<Vec<Value>>> = Rc::new(RefCell::new(vec![]));
        let s2 = sent.clone();
        let tools = json!([tool(
            "add",
            json!({"note": {"type": "string"}, "empty": {"type": "string"}})
        )]);
        let handle: Handler = Box::new(move |req: &Value| {
            let id = req.get("id").cloned().unwrap_or(Value::Null);
            match req["method"].as_str().unwrap_or("") {
                "initialize" => Ok(json_reply(
                    &id,
                    json!({"protocolVersion": PROTOCOL_VERSION}),
                )),
                "notifications/initialized" => Ok(Reply::default()),
                "tools/list" => Ok(json_reply(&id, json!({"tools": tools.clone()}))),
                _ => {
                    s2.borrow_mut().push(req["params"].clone());
                    Ok(json_reply(&id, json!({"content": []})))
                }
            }
        });
        let t = Box::new(Fake {
            handle: Rc::new(RefCell::new(handle)),
        });
        let mut r = Remote::connect(
            "svc",
            t,
            Options {
                default_tier: Tier::Low,
                ..Options::default()
            },
        )
        .unwrap();
        r.call(&act("svc_add", &[("note", "hi"), ("empty", "")]))
            .unwrap();
        let args = &sent.borrow()[0]["arguments"];
        assert_eq!(args["note"], json!("hi"));
        assert!(args.get("empty").is_none());
    }

    #[test]
    fn an_error_result_becomes_err() {
        let t = server(
            json!([tool("pay", json!({}))]),
            json!({"content": [{"type": "text", "text": "card declined"}], "isError": true}),
        );
        let mut r = Remote::connect(
            "svc",
            t,
            Options {
                default_tier: Tier::Low,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(r.call(&act("svc_pay", &[])).unwrap_err(), "card declined");
    }

    #[test]
    fn an_event_stream_body_is_parsed() {
        let body =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n";
        assert_eq!(
            from_event_stream(body),
            Some("{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}")
        );
        let handle: Handler = Box::new(move |req: &Value| {
            let id = req.get("id").cloned().unwrap_or(Value::Null);
            match req["method"].as_str().unwrap_or("") {
                "initialize" => Ok(Reply {
                    body: format!(
                        "event: message\ndata: {}\n\n",
                        json!({"jsonrpc": "2.0", "id": id, "result": {"protocolVersion": PROTOCOL_VERSION}})
                    ),
                    content_type: "text/event-stream".into(),
                    session: Some("s1".into()),
                }),
                "notifications/initialized" => Ok(Reply::default()),
                _ => Ok(Reply {
                    body: format!(
                        "data: {}\n\n",
                        json!({"jsonrpc": "2.0", "id": id, "result": {"tools": []}})
                    ),
                    content_type: "text/event-stream; charset=utf-8".into(),
                    session: None,
                }),
            }
        });
        let t = Box::new(Fake {
            handle: Rc::new(RefCell::new(handle)),
        });
        let r = Remote::connect("svc", t, Options::default()).unwrap();
        assert_eq!(r.tool_count(), 0);
    }

    #[test]
    fn a_server_that_fails_initialize_is_an_error() {
        let handle: Handler = Box::new(|_req: &Value| Err("connection refused".into()));
        let t = Box::new(Fake {
            handle: Rc::new(RefCell::new(handle)),
        });
        assert!(Remote::connect("svc", t, Options::default()).is_err());
    }
}
