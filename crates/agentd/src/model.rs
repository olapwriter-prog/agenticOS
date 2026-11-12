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
        "android_inbox" => {
            "messages that arrived from Android apps (WhatsApp, Telegram...), by conversation"
        }
        "android_messages_from" => "read the messages from one conversation in an Android app",
        "android_reply" => "reply to a conversation in an Android app (Medium tier)",
        "android_files" => "files that arrived from Android apps",
        "android_share" => "send a document to an Android app (High tier, needs the approval key)",
        "android_bridge_status" => "whether the Android bridge is connected and what it may do",
        "android_show" => "open Android's own full screen in the sandbox (its launcher and apps)",
        "android_hide" => "go back from Android to Agentic OS; Android pauses when nothing needs it",
        "android_pause" => "pause the Android sandbox now (apps keep their state, use no CPU)",
        "android_resume" => "resume the paused Android sandbox",
        "android_play_setup" => "install the Google Play image in the Android sandbox (High tier, needs the approval key)",
        "android_device_id" => "the device id to register with Google so Play sign-in works",
        "android_play_status" => "whether the Play Store is installed, and the device id",
        "all_messages" => "every text message (for analysis)",
        "call_log" => "every call (for analysis)",
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
        "list_functions" => "list the functions the agent has written",
        "show_function" => "show a function's code and examples",
        "write_function" => "write, compile and test a new function (code in Rust)",
        "install_function" => "install a tested function version",
        "rollback_function" => "go back to an earlier installed version of a function",
        "remove_function" => "stop using a function",
        "run_function" => "run an installed function; args like a=1; b=2",
        other => [
            agentos_skillpack::tools::describe(other),
            agentos_docs::describe(other),
            agentos_apps::describe(other),
            agentos_partner::describe(other),
        ]
        .into_iter()
        .find(|d| !d.is_empty())
        .unwrap_or(""),
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
    /// Every tool's argument names: a phrase that leaves an argument out ("news", "suggest
    /// something") gets it empty, which the tool reads as its default.
    pub tools: Vec<agentos_action::Tool>,
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
    // the camera and photos
    t.add("take a photo", "camera_take");
    t.add("take a picture", "camera_take");
    t.add("edit photo {photo} {edit}", "photo_edit");
    t.add("show photo {photo}", "photo_show");
    // the Android bridge: messages from apps, replies, files (before the generic SMS phrases)
    t.add("android inbox", "android_inbox");
    t.add("android messages", "android_inbox");
    t.add("whatsapp messages", "android_inbox");
    t.add(
        "android messages from {conversation}",
        "android_messages_from",
    );
    t.add(
        "reply on android to {conversation} saying {text}",
        "android_reply",
    );
    t.add("received files", "android_files");
    t.add("files from android", "android_files");
    t.add("share {name} to android app {app}", "android_share");
    t.add("bridge status", "android_bridge_status");
    t.add("open android", "android_show");
    t.add("show android", "android_show");
    t.add("back to agentic os", "android_hide");
    t.add("close android", "android_hide");
    t.add("pause android", "android_pause");
    t.add("resume android", "android_resume");
    t.add("set up play store", "android_play_setup");
    t.add("android device id", "android_device_id");
    t.add("play store status", "android_play_status");
    t.add("my photos", "photo_list");
    t.add("photos", "photo_list");
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
    // the business partner (with a [partner] table) — specific patterns before general ones
    t.add("inbox", "inbox");
    t.add("business inbox", "inbox");
    t.add("whatsapp", "inbox");
    t.add("who is waiting for a reply", "inbox");
    t.add("show chat with {who}", "read_thread");
    t.add("open {who}'s chat", "read_thread");
    t.add("thread {who}", "read_thread");
    t.add("reply to {to}: {text}", "reply");
    t.add("reply to {to} saying {text}", "reply");
    t.add(
        "add customer {name} gstin {gstin} phone {phone}",
        "add_customer",
    );
    t.add("add customer {name} phone {phone}", "add_customer");
    t.add("add item {name} at {price} gst {gst}", "add_item");
    t.add("add item {name} at {price}", "add_item");
    t.add("invoice {customer} for {items}", "invoice");
    t.add("send invoice {number}", "send_invoice");
    t.add("mark {number} paid", "mark_paid");
    t.add("invoices", "invoices");
    t.add("show invoices", "invoices");
    t.add("who owes me", "invoices");
    t.add("gst for {period}", "gst_summary");
    t.add("gst summary for {period}", "gst_summary");
    t.add("file {form} for {period}", "file_gst");
    t.add("bills", "bills");
    t.add("show bills", "bills");
    t.add("what bills are due", "bills");
    t.add("pay bill {id}", "pay_bill");
    t.add("expenses for {period}", "expenses");
    t.add("wallet", "wallet");
    t.add("wallet balance", "wallet");
    t.add("pay {amount} to {to} for {note}", "pay");
    t.add("my orders", "orders");
    t.add("order history", "orders");
    t.add("what do i usually buy", "orders");
    t.add("best price for {item}", "best_price");
    t.add("compare {item}", "best_price");
    t.add("buy {option}", "buy");
    t.add("news about {topic}", "news");
    t.add("news", "news");
    t.add("my interests", "interests");
    t.add("i like {topic}", "like");
    t.add("suggest {kind}", "suggest");
    t.add("what should i do this weekend", "suggest");
    t.add("book {event} for {qty}", "book");
    t.add("book {event}", "book");
    // skill guides (with a skill library)
    t.add("skills about {topic}", "skills");
    t.add("skills for {topic}", "skills");
    t.add("find a skill for {topic}", "skills");
    t.add("skill {name}: {question}", "skill");
    t.add("read skill {name}", "skill");
    t.add("skill sources", "skill_sources");
    // documents
    t.add("documents", "documents");
    t.add("my documents", "documents");
    t.add("what does {name} say about {question}", "read_document");
    t.add("read document {name}", "read_document");
    // apps over public APIs (after everything else: their phrases are the broadest)
    t.add("weather in {place}", "weather");
    t.add("what's the weather in {place}", "weather");
    t.add("will it rain in {place}", "weather");
    t.add("weather", "weather");
    t.add("air quality in {place}", "air_quality");
    t.add("air quality", "air_quality");
    t.add("what time is it in {place}", "time_in");
    t.add("time in {place}", "time_in");
    t.add("directions from {from} to {to}", "directions");
    t.add("how do i get to {to}", "directions");
    t.add("route to {to}", "directions");
    t.add("find {query} nearby", "places");
    t.add("places {query}", "places");
    t.add("convert {amount} {from} to {to}", "convert_currency");
    t.add("tell me about {topic}", "wiki");
    t.add("wiki {topic}", "wiki");
    t.add("define {word}", "define");
    t.add("meaning of {word}", "define");
    t.add("what does {word} mean", "define");
    t.add("public holidays in {country}", "holidays");
    t.add("holidays in {country}", "holidays");
    t.add("holidays", "holidays");
    t.add("books by {query}", "books");
    t.add("books about {query}", "books");
    t.add("play {query} radio", "radio");
    t.add("radio {query}", "radio");
    t.add("podcasts about {query}", "podcasts");
    t.add("podcasts on {query}", "podcasts");
    t.add("recipe for {dish}", "recipe");
    t.add("how to make {dish}", "recipe");
    t.add("translate {text} from {from} to {to}", "translate");
    t.add("translate {text} to {to}", "translate");
    t.add("price of {coin}", "crypto");
    t.add("{coin} price", "crypto");
    // functions the agent wrote
    t.add("list functions", "list_functions");
    t.add("my functions", "list_functions");
    t.add("show function {name}", "show_function");
    t.add(
        "install function {name} version {version}",
        "install_function",
    );
    t.add("roll back {name} to version {version}", "rollback_function");
    t.add("remove function {name}", "remove_function");
    t.add("run {name} with {args}", "run_function");
    t
}

impl Model for Local {
    fn complete(&mut self, brain: Brain, prompt: &str) -> String {
        // The fast path is for local brains. A commercial call has already been redacted and
        // rewritten by the engine, so it is not matched against phrases.
        if brain != Brain::Commercial {
            if let Some(mut a) = self.intents.resolve(prompt) {
                if let Some(t) = self.tools.iter().find(|t| t.name == a.tool) {
                    for name in t.args {
                        if !a.args.iter().any(|(k, _)| k == name) {
                            a.args.push((name.to_string(), String::new()));
                        }
                    }
                }
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
            tools: vec![],
        };
        let out = m.complete(Brain::LocalSmall, "Remind me to call mum at 6pm");
        assert!(out.contains("set_reminder") && out.contains("call mum"));
    }
    #[test]
    fn an_unknown_phrase_with_no_host_is_empty() {
        let mut m = Local {
            intents: default_intents(),
            host: None,
            tools: vec![],
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
            tools: vec![],
        };
        assert_eq!(m.complete(Brain::Commercial, "what's on friday"), "");
    }
}
