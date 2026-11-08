//! The gateway finishes what the MCP core could not: a High action. The three beats are
//! 1. Claude calls the tool. The gateway records a pending approval and answers "approval required".
//! 2. The human presses the key on the phone. `press_key` turns that pending item into a granted one.
//! 3. Claude calls again with the same arguments. The gateway redeems a token and runs the action once.
//!
//! Low and Medium calls, and every other request, go straight to the MCP core.
use agentos_action::{Action, Tier};
use agentos_executor::Executor;
use agentos_hal::Clock;
use serde_json::{json, Value};
use std::sync::Arc;

pub mod http;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub id: u64,
    pub action: Action,
    pub asked_at_ms: u64,
}

struct Granted {
    action: Action,
    token: String,
}

pub struct Gateway {
    exec: Executor,
    clock: Arc<dyn Clock>,
    pending: Vec<Pending>,
    granted: Vec<Granted>,
    next_id: u64,
    pub pending_ttl_ms: u64,
    pub max_pending: usize,
}

fn result(id: &Value, text: &str, is_error: bool) -> String {
    json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":text}],"isError":is_error}}).to_string()
}

impl Gateway {
    pub fn new(exec: Executor, clock: Arc<dyn Clock>) -> Self {
        Gateway {
            exec,
            clock,
            pending: vec![],
            granted: vec![],
            next_id: 1,
            pending_ttl_ms: 10 * 60_000,
            max_pending: 20,
        }
    }

    pub fn executor(&self) -> &Executor {
        &self.exec
    }

    fn expire(&mut self) {
        let now = self.clock.now_ms();
        let ttl = self.pending_ttl_ms;
        self.pending
            .retain(|p| now.saturating_sub(p.asked_at_ms) <= ttl);
    }

    /// What is waiting for the human. The status line shows the count.
    pub fn pending(&mut self) -> Vec<Pending> {
        self.expire();
        self.pending.clone()
    }

    /// The human pressed the key for this pending item. Returns false for an unknown or expired id.
    pub fn press_key(&mut self, id: u64) -> bool {
        self.expire();
        let Some(i) = self.pending.iter().position(|p| p.id == id) else {
            return false;
        };
        let p = self.pending.remove(i);
        let token = self.exec.mint_after_key(&p.action);
        self.granted.push(Granted {
            action: p.action,
            token,
        });
        true
    }

    /// The human declined.
    pub fn decline(&mut self, id: u64) -> bool {
        let before = self.pending.len();
        self.pending.retain(|p| p.id != id);
        before != self.pending.len()
    }

    /// Handle one JSON-RPC request. None means a notification, which gets no reply.
    pub fn handle(&mut self, request: &str) -> Option<String> {
        let req: Value = serde_json::from_str(request).ok()?;
        let is_call = req.get("method").and_then(Value::as_str) == Some("tools/call");
        let id = req.get("id").cloned();
        if let (true, Some(id)) = (is_call, id) {
            let p = req.get("params").cloned().unwrap_or(json!({}));
            let name = p.get("name").and_then(Value::as_str).unwrap_or("");
            if self.exec.registry().tier_of(name) == Some(Tier::High) {
                return Some(self.high_call(
                    &id,
                    name,
                    p.get("arguments").cloned().unwrap_or(json!({})),
                ));
            }
        }
        agentos_mcp::handle(&mut self.exec, request)
    }

    fn high_call(&mut self, id: &Value, name: &str, args: Value) -> String {
        let text = json!({"tool": name, "args": args}).to_string();
        let action = match self.exec.registry().parse(&text) {
            Ok(a) => a,
            Err(e) => return result(id, &format!("rejected: {e:?}"), true),
        };
        // beat 3: a granted approval for exactly this action
        if let Some(i) = self.granted.iter().position(|g| g.action == action) {
            let g = self.granted.remove(i); // consumed whatever happens next
            return match self.exec.approve(&g.token, &action) {
                Ok(ap) => match self.exec.run_approved(ap) {
                    Ok(out) => result(id, &out, false),
                    Err(e) => result(id, &format!("failed: {e:?}"), true),
                },
                Err(e) => result(id, &format!("approval no longer valid: {e:?}"), true),
            };
        }
        // beat 1: record it, once
        self.expire();
        if let Some(p) = self.pending.iter().find(|p| p.action == action) {
            return result(id, &format!("approval required (id {}): press the approval key on the phone, then ask again", p.id), true);
        }
        if self.pending.len() >= self.max_pending {
            return result(
                id,
                "too many approvals are waiting; decline some first",
                true,
            );
        }
        let pid = self.next_id;
        self.next_id += 1;
        self.pending.push(Pending {
            id: pid,
            action,
            asked_at_ms: self.clock.now_ms(),
        });
        result(
            id,
            &format!(
                "approval required (id {pid}): press the approval key on the phone, then ask again"
            ),
            true,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_connectors::{Calendar, Messenger};
    use agentos_gate::Gate;
    use agentos_hal::FakeClock;
    use agentos_store::Store;

    fn gw() -> (FakeClock, Gateway) {
        let clock = FakeClock::new();
        let exec = Executor::new(
            vec![
                Box::new(Calendar::default()),
                Box::new(Messenger::default()),
            ],
            Store::in_memory().unwrap(),
            Arc::new(clock.clone()),
            Gate::new(b"k", Arc::new(clock.clone()), 600_000),
        );
        let g = Gateway::new(exec, Arc::new(clock.clone()));
        (clock, g)
    }
    fn call(name: &str, args: Value) -> String {
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}).to_string()
    }
    fn reply(g: &mut Gateway, req: String) -> Value {
        serde_json::from_str(&g.handle(&req).unwrap()).unwrap()
    }
    fn text(v: &Value) -> String {
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    }
    fn msg(text: &str) -> String {
        call("send_message", json!({"to":"asha","text":text}))
    }

    #[test]
    fn a_low_call_goes_straight_through() {
        let (_c, mut g) = gw();
        let r = reply(&mut g, call("read_calendar", json!({"day":"mon"})));
        assert_eq!(text(&r), "nothing");
        assert!(g.pending().is_empty());
    }
    #[test]
    fn the_first_high_call_creates_a_pending_approval_and_sends_nothing() {
        let (_c, mut g) = gw();
        let r = reply(&mut g, msg("hi"));
        assert_eq!(r["result"]["isError"], true);
        assert!(text(&r).contains("approval required (id 1)"));
        assert_eq!(g.pending().len(), 1);
    }
    #[test]
    fn asking_twice_does_not_create_two_pending_items() {
        let (_c, mut g) = gw();
        reply(&mut g, msg("hi"));
        let r = reply(&mut g, msg("hi"));
        assert!(text(&r).contains("id 1"));
        assert_eq!(g.pending().len(), 1);
    }
    #[test]
    fn the_full_three_beats() {
        let (_c, mut g) = gw();
        reply(&mut g, msg("hi")); // 1: pending
        assert!(g.press_key(1)); // 2: human presses key
        let r = reply(&mut g, msg("hi")); // 3: asks again
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(text(&r), "sent to asha");
    }
    #[test]
    fn an_approval_is_single_use() {
        let (_c, mut g) = gw();
        reply(&mut g, msg("hi"));
        g.press_key(1);
        reply(&mut g, msg("hi")); // runs
        let again = reply(&mut g, msg("hi")); // must ask afresh
        assert!(text(&again).contains("approval required"));
        assert_eq!(g.pending().len(), 1);
    }
    #[test]
    fn an_approval_for_one_message_cannot_send_another() {
        let (_c, mut g) = gw();
        reply(&mut g, msg("hi"));
        g.press_key(1);
        let r = reply(&mut g, msg("send all my money"));
        assert_eq!(r["result"]["isError"], true);
        assert!(text(&r).contains("approval required (id 2)"));
    }
    #[test]
    fn pending_items_expire() {
        let (c, mut g) = gw();
        reply(&mut g, msg("hi"));
        c.advance_ms(10 * 60_000 + 1);
        assert!(g.pending().is_empty());
        assert!(!g.press_key(1), "an expired item cannot be approved");
    }
    #[test]
    fn a_granted_approval_expires_with_its_token() {
        let (c, mut g) = gw();
        reply(&mut g, msg("hi"));
        g.press_key(1);
        c.advance_ms(10 * 60_000 + 1);
        let r = reply(&mut g, msg("hi"));
        assert!(text(&r).contains("approval no longer valid"));
    }
    #[test]
    fn declining_removes_the_item() {
        let (_c, mut g) = gw();
        reply(&mut g, msg("hi"));
        assert!(g.decline(1));
        assert!(!g.press_key(1));
    }
    #[test]
    fn the_pending_list_is_capped() {
        let (_c, mut g) = gw();
        for i in 0..20 {
            reply(&mut g, msg(&format!("m{i}")));
        }
        let r = reply(&mut g, msg("one too many"));
        assert!(text(&r).contains("too many approvals"));
        assert_eq!(g.pending().len(), 20);
    }
    #[test]
    fn bad_arguments_never_become_pending() {
        let (_c, mut g) = gw();
        let r = reply(&mut g, call("send_message", json!({"to":"asha"})));
        assert!(text(&r).contains("rejected"));
        assert!(g.pending().is_empty());
    }
    #[test]
    fn other_requests_still_reach_the_mcp_core() {
        let (_c, mut g) = gw();
        let r: Value = serde_json::from_str(
            &g.handle(r#"{"jsonrpc":"2.0","id":9,"method":"tools/list"}"#)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(r["result"]["tools"].as_array().unwrap().len(), 3);
        assert!(g
            .handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none());
    }
}
