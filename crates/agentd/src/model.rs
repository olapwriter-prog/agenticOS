//! The model the engine talks to: intents first (no model needed), then the local llama.cpp host.
//! The commercial client wraps this when it is enabled.
use agentos_action::Action;
use agentos_brain::Model;
use agentos_host::LlamaHost;
use agentos_intents::IntentTable;
use agentos_router::Brain;

/// One line per tool, in plain words, so a small model can tell `call` from `ring_phone`.
pub fn describe(tool: &str) -> &'static str {
    match tool {
        "read_calendar" => "what is on the calendar on a day",
        "add_event" => "put an event on the calendar",
        "set_reminder" => "remind the user of something at a time",
        "send_message" => "send a chat-app message (not SMS) to a person",
        "phone_status" => "mobile network, signal, data and Wi-Fi status",
        "find_contact" => "look up a saved person's phone number",
        "list_contacts" => "list everyone in the address book",
        "add_contact" => "save a person's name and phone number",
        "call" => "place a phone call to a person or number",
        "answer_call" => "pick up the phone call that is ringing now",
        "hangup" => "end the current phone call",
        "list_calls" => "recent and missed calls",
        "send_sms" => "send a text message (SMS) to a person or number",
        "read_messages" => "read the latest text messages",
        "messages_from" => "read text messages from one person",
        "ussd" => "send a USSD code like *123# (balance, data pack)",
        "wifi_scan" => "list Wi-Fi networks nearby",
        "wifi_connect" => "join a Wi-Fi network",
        "airplane_mode" => "turn airplane mode on or off (on = all radios off)",
        "mobile_data" => "turn mobile data on or off",
        "where_am_i" => "the phone's current location",
        "flashlight" => "turn the flashlight (torch) on or off",
        "set_volume" => "set the sound volume, 0 to 100",
        "ring_phone" => "make this phone ring loudly so the user can find it",
        _ => "",
    }
}

/// What the local model is told: what it is, the tools it may call, and how to answer.
/// The grammar already forces the shape; this tells it which tool means what.
pub fn system_prompt(tools: &[agentos_action::Tool]) -> String {
    let mut s = String::from(
        "You are the agent that runs this phone. There is no screen: every request is done by \
         calling exactly one tool. Answer with one JSON object {\"tool\": name, \"args\": {...}} \
         and nothing else. Use the user's words for argument values; on/off switches take \"on\" or \
         \"off\". Tools marked [approval] are only carried out after the human presses the \
         approval key; still choose them when asked.\n\nTools:\n",
    );
    for t in tools {
        let mark = if t.tier == agentos_action::Tier::High {
            " [approval]"
        } else {
            ""
        };
        let d = describe(t.name);
        let d = if d.is_empty() {
            String::new()
        } else {
            format!(": {d}")
        };
        s.push_str(&format!("- {}({}){mark}{d}\n", t.name, t.args.join(", ")));
    }
    // Examples only for tools that exist here, so the model is never shown one it cannot call.
    let has = |n: &str| tools.iter().any(|t| t.name == n);
    let examples: &[(&str, &str, &str)] = &[
        (
            "could you ring my mother",
            "call",
            r#"{"tool":"call","args":{"to":"mother"}}"#,
        ),
        (
            "it's dark, light please",
            "flashlight",
            r#"{"tool":"flashlight","args":{"on":"on"}}"#,
        ),
        (
            "tell Ravi by text I'm outside",
            "send_sms",
            r#"{"tool":"send_sms","args":{"to":"Ravi","text":"I'm outside"}}"#,
        ),
        (
            "anything on for tuesday?",
            "read_calendar",
            r#"{"tool":"read_calendar","args":{"day":"tuesday"}}"#,
        ),
        (
            "quieter, 30 percent",
            "set_volume",
            r#"{"tool":"set_volume","args":{"percent":"30"}}"#,
        ),
        (
            "where did I leave my phone",
            "ring_phone",
            r#"{"tool":"ring_phone","args":{}}"#,
        ),
    ];
    let shown: Vec<_> = examples.iter().filter(|(_, t, _)| has(t)).collect();
    if !shown.is_empty() {
        s.push_str("\nExamples:\n");
        for (say, _, json) in shown {
            s.push_str(&format!("User: {say}\n{json}\n"));
        }
    }
    s
}

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
    fn the_system_prompt_lists_tools_and_marks_the_risky_ones() {
        use agentos_action::{Tier, Tool};
        let p = system_prompt(&[
            Tool {
                name: "call",
                args: &["to"],
                tier: Tier::High,
            },
            Tool {
                name: "hangup",
                args: &[],
                tier: Tier::Low,
            },
        ]);
        assert!(p.contains("- call(to) [approval]: place a phone call"));
        assert!(p.contains("- hangup(): end the current phone call\n"));
        assert!(p.contains(r#"{"tool":"call","args":{"to":"mother"}}"#));
        assert!(
            !p.contains("flashlight"),
            "no examples for tools that are not registered"
        );
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
