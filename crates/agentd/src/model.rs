//! The model the engine talks to: intents first (no model needed), then the local llama.cpp host.
//! The commercial client wraps this when it is enabled.
use agentos_action::Action;
use agentos_brain::Model;
use agentos_host::LlamaHost;
use agentos_intents::IntentTable;
use agentos_router::Brain;

pub struct Local {
    pub intents: IntentTable,
    pub host: Option<LlamaHost>,
}

pub fn action_json(a: &Action) -> String {
    let args: serde_json::Map<String, serde_json::Value> = a
        .args
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    serde_json::json!({"tool": a.tool, "args": args}).to_string()
}

/// The phrases the phone understands without a model. Specific patterns first.
pub fn default_intents() -> IntentTable {
    let mut t = IntentTable::new();
    t.add("remind me to {text} at {when}", "set_reminder");
    t.add("add {title} to my calendar on {day}", "add_event");
    t.add("what's on {day}", "read_calendar");
    t.add("what is on {day}", "read_calendar");
    t.add("message {to} {text}", "send_message");
    // the phone
    t.add("save {name} as {number}", "add_contact");
    t.add("what's {name}'s number", "find_contact");
    t.add("text {to} {text}", "send_sms");
    t.add("sms {to} {text}", "send_sms");
    t.add("call {to}", "call");
    t.add("answer", "answer_call");
    t.add("pick up", "answer_call");
    t.add("hang up", "hangup");
    t.add("read my messages", "read_messages");
    t.add("messages from {from}", "messages_from");
    t.add("missed calls", "list_calls");
    t.add("check balance {code}", "ussd");
    t.add("ussd {code}", "ussd");
    t.add("signal", "phone_status");
    t.add("phone status", "phone_status");
    t.add("scan wifi", "wifi_scan");
    t.add("join wifi {ssid} password {password}", "wifi_connect");
    t.add("airplane mode {on}", "airplane_mode");
    t.add("mobile data {on}", "mobile_data");
    t.add("where am i", "where_am_i");
    t.add("flashlight {on}", "flashlight");
    t.add("torch {on}", "flashlight");
    t.add("volume {percent}", "set_volume");
    t.add("find my phone", "ring_phone");
    t
}

impl Model for Local {
    fn complete(&mut self, brain: Brain, prompt: &str) -> String {
        // The fast path is for local brains. A commercial call has already been redacted and
        // rewritten by the engine, so it is not matched against phrases.
        if brain != Brain::Commercial {
            if let Some(a) = self.intents.resolve(prompt) {
                return action_json(&a);
            }
        }
        match &mut self.host {
            Some(h) => h.complete(brain, prompt),
            None => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_known_phrase_becomes_an_action_with_no_model() {
        let mut m = Local {
            intents: default_intents(),
            host: None,
        };
        let out = m.complete(Brain::LocalSmall, "Remind me to call mum at 6pm");
        assert!(out.contains("set_reminder") && out.contains("call mum"));
    }
    #[test]
    fn an_unknown_phrase_with_no_host_is_empty() {
        let mut m = Local {
            intents: default_intents(),
            host: None,
        };
        assert_eq!(m.complete(Brain::LocalSmall, "write me a poem"), "");
    }
    #[test]
    fn commercial_calls_skip_the_phrase_table() {
        let mut m = Local {
            intents: default_intents(),
            host: None,
        };
        assert_eq!(m.complete(Brain::Commercial, "what's on friday"), "");
    }
}
