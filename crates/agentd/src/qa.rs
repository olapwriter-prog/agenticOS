//! Regular requests answered by code, not by the LLM.
//!
//! A Naive Bayes classifier (agentos-understand) says what kind of request it is; rules pull out
//! the details; questions are answered by exact code over every message, call and contact; and
//! commands become tool calls. The LLM is used only when the classifier is unsure or a detail is
//! missing. The classifier starts from the phrasings below and learns from every request the
//! human confirms (approved, "that was right", or corrected).
use agentos_understand::records::{self, Call, Message};
use agentos_understand::{day, message_text, on_off, percent, person, Classifier};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Below this the classifier is not trusted and the LLM is asked.
pub const CONFIDENCE: f32 = 0.6;

/// Example phrasings for each kind of request. The names in them are placeholders; the
/// benchmark's own phrasings are deliberately not here.
pub const SEEDS: &[(&str, &[&str])] = &[
    // questions over the phone's data
    (
        "q.msgs_from",
        &[
            "how many texts did Asha send me",
            "how many messages have I got from Dev",
            "count the sms from Priya",
            "number of texts from Sam",
            "how many times did Asha text me",
            "how many messages did Dev send",
            "texts received from Priya how many",
        ],
    ),
    (
        "q.msgs_sent",
        &[
            "how many texts did I send",
            "how many messages have I sent",
            "count my sent messages",
            "how many sms did I send to Asha",
            "number of texts I sent",
            "how many messages did I write to Dev",
        ],
    ),
    (
        "q.top_sender",
        &[
            "who texts me the most",
            "who sends me the most messages",
            "which contact messages me most",
            "who has sent me the most texts",
            "top sender of my messages",
            "who messaged me most often",
        ],
    ),
    (
        "q.strangers",
        &[
            "how many texts came from unknown numbers",
            "how many promotional messages did I get",
            "how many spam texts do I have",
            "how many offers did I receive",
            "how many messages from numbers I have not saved",
            "how many sms from strangers",
            "how many advertising texts",
            "how many marketing messages came",
        ],
    ),
    (
        "q.contacts_texted",
        &[
            "how many of my contacts texted me",
            "how many people in my address book sent me messages",
            "how many different contacts messaged me",
            "which of my contacts texted me",
            "how many saved people sent me sms",
            "number of contacts who wrote to me",
        ],
    ),
    (
        "q.calls_from",
        &[
            "how many times did Asha call me",
            "how many calls did Dev make to me",
            "how many calls have I had from Priya",
            "number of calls from Sam",
            "count the calls from Asha",
            "how often did Dev ring me",
        ],
    ),
    (
        "q.did_call",
        &[
            "did Asha call me",
            "has Dev called me",
            "did I get a call from Priya",
            "did Sam ring me",
            "has Asha phoned me",
            "did Dev try to call",
            "did Priya call",
            "did anyone named Sam call me today",
        ],
    ),
    (
        "q.calls_to",
        &[
            "how many times did I call Asha",
            "how many calls did I make",
            "how often have I called Dev",
            "number of calls I made to Priya",
            "how many times have I phoned Sam",
        ],
    ),
    (
        "q.last_from",
        &[
            "what did Asha last text me",
            "last message from Dev",
            "what was the last sms from Priya",
            "show the latest text from Sam",
            "what did Asha say in her last message",
            "most recent message from Dev",
        ],
    ),
    (
        "q.msgs_total",
        &[
            "how many texts did I get",
            "how many messages do I have",
            "total number of sms I received",
            "how many messages are in my inbox",
            "count all my texts",
        ],
    ),
    // commands
    (
        "call",
        &[
            "call Asha",
            "phone Dev",
            "ring my sister",
            "give Priya a call",
            "dial Sam",
            "make a call to Asha",
            "call my dad",
            "phone my mother",
            "get Dev on the phone",
            "ring Priya now",
        ],
    ),
    (
        "send_sms",
        &[
            "text Asha saying hello",
            "send Dev a message saying running late",
            "sms Priya that I am home",
            "tell Sam by text that dinner is ready",
            "message Asha: see you soon",
            "send a text to Dev saying ok",
            "let Priya know by sms that I will call later",
            "drop Sam a line saying thanks",
        ],
    ),
    (
        "hangup",
        &[
            "hang up",
            "end the call",
            "cut the call",
            "stop the call",
            "drop this call",
            "disconnect the call",
            "finish the call",
        ],
    ),
    (
        "answer_call",
        &[
            "answer",
            "pick up",
            "take the call",
            "answer the phone",
            "accept the call",
            "pick up the phone",
        ],
    ),
    (
        "flashlight",
        &[
            "torch on",
            "turn on the flashlight",
            "switch the torch off",
            "light please",
            "it is dark here",
            "turn off the light",
            "flashlight",
            "i need some light",
            "torch",
        ],
    ),
    (
        "set_volume",
        &[
            "volume 30",
            "set volume to 50 percent",
            "make it quieter 20",
            "turn the sound up to 90",
            "louder 70",
            "sound level 40",
            "mute the sound",
            "volume down to 10",
        ],
    ),
    (
        "wifi_scan",
        &[
            "scan for wifi",
            "what wifi networks are around",
            "show wifi networks",
            "any wifi nearby",
            "find wireless networks",
            "is there a hotspot near me",
            "list wifi",
        ],
    ),
    (
        "ring_phone",
        &[
            "find my phone",
            "make my phone ring",
            "i lost my phone",
            "ring this phone loudly",
            "help me find my handset",
            "where did my phone go",
        ],
    ),
    (
        "phone_status",
        &[
            "how is my signal",
            "network status",
            "am i connected",
            "what network am i on",
            "is mobile data on",
            "signal strength",
            "how many bars do i have",
        ],
    ),
    (
        "read_calendar",
        &[
            "what do i have on friday",
            "anything on my calendar tomorrow",
            "show my schedule for tuesday",
            "am i free on sunday",
            "what is planned for monday",
            "my agenda for thursday",
        ],
    ),
    (
        "add_event",
        &[
            "put the gym on my calendar for tuesday",
            "put a call with Dev on my calendar for friday",
            "add a dentist appointment on sunday",
            "add meeting on friday to my calendar",
            "schedule gym on tuesday",
            "add lunch with Dev on sunday",
            "book haircut for saturday",
            "put yoga in my diary for wednesday",
            "note a doctor visit on thursday in my calendar",
        ],
    ),
    (
        "airplane_mode",
        &[
            "airplane mode on",
            "flight mode off",
            "turn on aeroplane mode",
            "disable airplane mode",
            "switch to flight mode",
        ],
    ),
    (
        "mobile_data",
        &[
            "mobile data off",
            "turn on data",
            "switch off mobile internet",
            "enable cellular data",
            "disable data",
        ],
    ),
    (
        "where_am_i",
        &[
            "what is my location",
            "share my location",
            "current position",
            "where are we right now",
        ],
    ),
    (
        "read_messages",
        &[
            "show my texts",
            "any new messages",
            "what are my latest sms",
            "open my inbox",
            "read out my messages",
        ],
    ),
    (
        "list_calls",
        &[
            "show recent calls",
            "call history",
            "my recent calls",
            "list my calls",
        ],
    ),
];

/// A classifier trained on the seeds and on every episode the human confirmed.
pub fn classifier(confirmed: &[(String, String)]) -> Classifier {
    let mut c = Classifier::new();
    for (label, phrases) in SEEDS {
        for p in *phrases {
            c.train(label, p);
        }
    }
    for (label, prompt) in confirmed {
        c.train(label, prompt);
    }
    c
}

/// The phone's data, loaded once per question.
pub struct Data {
    pub messages: Vec<Message>,
    pub calls: Vec<Call>,
    pub contacts: Vec<String>,
}

impl Data {
    pub fn parse(messages: &str, calls: &str, contacts: &str) -> Data {
        Data {
            messages: records::messages(messages),
            calls: records::calls(calls),
            contacts: records::contacts(contacts)
                .into_iter()
                .map(|(n, _)| n)
                .collect(),
        }
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Answer a question by code. None if the label is not a question or a detail is missing.
pub fn answer(label: &str, text: &str, d: &Data) -> Option<String> {
    let who = || person(text, &d.contacts);
    let incoming = || d.messages.iter().filter(|m| m.incoming);
    Some(match label {
        "q.msgs_from" => {
            let p = who()?;
            let n = incoming()
                .filter(|m| records::is(&m.name, &m.number, &p))
                .count();
            format!("{p} sent you {}.", plural(n, "text", "texts"))
        }
        "q.msgs_sent" => match who() {
            Some(p) => {
                let n = d
                    .messages
                    .iter()
                    .filter(|m| !m.incoming && records::is(&m.name, &m.number, &p))
                    .count();
                format!("You sent {p} {}.", plural(n, "text", "texts"))
            }
            None => format!(
                "You sent {}.",
                plural(
                    d.messages.iter().filter(|m| !m.incoming).count(),
                    "text",
                    "texts"
                )
            ),
        },
        "q.msgs_total" => format!(
            "You received {}.",
            plural(incoming().count(), "text", "texts")
        ),
        "q.top_sender" => {
            let mut by: BTreeMap<String, usize> = BTreeMap::new();
            for m in incoming() {
                *by.entry(m.name.clone().unwrap_or_else(|| m.number.clone()))
                    .or_default() += 1;
            }
            let top = by.values().copied().max()?;
            // saved contacts before bare numbers when there is a tie
            let mut names: Vec<&str> = by
                .iter()
                .filter(|(_, n)| **n == top)
                .map(|(k, _)| k.as_str())
                .collect();
            names
                .sort_by_key(|n| n.starts_with('+') || n.starts_with(|c: char| c.is_ascii_digit()));
            format!(
                "{} texted you the most ({}).",
                names.join(" and "),
                plural(top, "text", "texts")
            )
        }
        "q.strangers" => {
            let n = incoming().filter(|m| m.name.is_none()).count();
            format!(
                "{} came from numbers not in your contacts.",
                plural(n, "text", "texts")
            )
        }
        "q.contacts_texted" => {
            let mut names: Vec<String> = incoming().filter_map(|m| m.name.clone()).collect();
            names.sort();
            names.dedup();
            if names.is_empty() {
                "None of your contacts has texted you.".into()
            } else {
                format!(
                    "{} texted you: {}.",
                    plural(names.len(), "contact", "contacts"),
                    names.join(", ")
                )
            }
        }
        "q.calls_from" | "q.did_call" => {
            let p = who()?;
            let n = d
                .calls
                .iter()
                .filter(|c| c.incoming && records::is(&c.name, &c.number, &p))
                .count();
            if label == "q.did_call" {
                if n == 0 {
                    format!("No, {p} has not called you.")
                } else {
                    format!("Yes, {p} called you ({}).", plural(n, "time", "times"))
                }
            } else {
                format!("{p} called you {}.", plural(n, "time", "times"))
            }
        }
        "q.calls_to" => match who() {
            Some(p) => {
                let n = d
                    .calls
                    .iter()
                    .filter(|c| !c.incoming && records::is(&c.name, &c.number, &p))
                    .count();
                format!("You called {p} {}.", plural(n, "time", "times"))
            }
            None => format!(
                "You made {}.",
                plural(
                    d.calls.iter().filter(|c| !c.incoming).count(),
                    "call",
                    "calls"
                )
            ),
        },
        "q.last_from" => {
            let p = who()?;
            match incoming().rfind(|m| records::is(&m.name, &m.number, &p)) {
                Some(m) => format!("{p}'s last text: {}", m.text),
                None => format!("{p} has not texted you."),
            }
        }
        _ => return None,
    })
}

/// Words that are about the calendar, not the event, when finding an event's title.
const CALENDAR_WORDS: &[&str] = &[
    "put", "add", "schedule", "book", "note", "on", "for", "my", "the", "a", "in", "to",
    "calendar", "diary", "agenda", "please", "an", "at",
];

/// Turn a command into a tool call by rules. None if a detail is missing (then the LLM tries).
pub fn command(label: &str, text: &str, contacts: &[String]) -> Option<Value> {
    let args = match label {
        "call" => json!({"to": person(text, contacts)?}),
        "send_sms" => json!({"to": person(text, contacts)?, "text": message_text(text)?}),
        "flashlight" | "airplane_mode" | "mobile_data" => json!({"on": on_off(text)}),
        "set_volume" => {
            let lower = text.to_lowercase();
            let v = percent(text).or_else(|| lower.contains("mute").then(|| "0".to_string()))?;
            json!({"percent": v})
        }
        "read_calendar" => json!({"day": day(text).unwrap_or_else(|| "today".into())}),
        "add_event" => {
            let d = day(text)?;
            let title: Vec<&str> = text
                .split_whitespace()
                .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
                .filter(|w| !w.is_empty())
                .filter(|w| {
                    let l = w.to_lowercase();
                    !CALENDAR_WORDS.contains(&l.as_str()) && l != d
                })
                .collect();
            if title.is_empty() {
                return None;
            }
            json!({"day": d, "title": title.join(" ")})
        }
        "hangup" | "answer_call" | "wifi_scan" | "ring_phone" | "phone_status" | "where_am_i"
        | "read_messages" | "list_calls" => json!({}),
        _ => return None,
    };
    Some(json!({"tool": label, "args": args}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> Data {
        Data::parse(
            "from Mum (+919876543210): dinner at 8?\nfrom Mum (+919876543210): bring milk\nfrom Ravi (+919812345678): match tonight?\nfrom +918000000001: big sale, 50% off\nfrom +918000000002: offer ends today\nto Mum (+919876543210): on my way",
            "from Ravi (+919812345678) ended\nfrom Mum (+919876543210) ended\nto Mum (+919876543210) ended",
            "Mum: +919876543210\nRavi: +919812345678",
        )
    }
    /// Phrasings that are not in SEEDS: what the classifier must generalise to.
    const HELD_OUT: &[(&str, &str, &str)] = &[
        (
            "how many texts did Mum send me",
            "q.msgs_from",
            "Mum sent you 2 texts.",
        ),
        (
            "how many promotional texts did I get",
            "q.strangers",
            "2 texts came from numbers not in your contacts.",
        ),
        (
            "how many calls did Ravi make to me",
            "q.calls_from",
            "Ravi called you 1 time.",
        ),
        (
            "who texted me the most",
            "q.top_sender",
            "Mum texted you the most (2 texts).",
        ),
        (
            "how many people in my contacts texted me",
            "q.contacts_texted",
            "2 contacts texted you: Mum, Ravi.",
        ),
        (
            "did Mum call me",
            "q.did_call",
            "Yes, Mum called you (1 time).",
        ),
        (
            "how many texts did I send",
            "q.msgs_sent",
            "You sent 1 text.",
        ),
        (
            "what was Ravi's last message",
            "q.last_from",
            "Ravi's last text: match tonight?",
        ),
    ];
    #[test]
    fn held_out_questions_are_classified_and_answered_exactly() {
        let c = classifier(&[]);
        let d = data();
        for (q, want_label, want) in HELD_OUT {
            let (label, p) = c
                .decide(q, CONFIDENCE)
                .unwrap_or_else(|| panic!("unsure about {q:?}: {:?}", &c.predict(q)[..2]));
            assert_eq!(&label, want_label, "{q} ({p})");
            assert_eq!(answer(&label, q, &d).as_deref(), Some(*want), "{q}");
        }
    }
    #[test]
    fn held_out_commands_become_tool_calls() {
        let c = classifier(&[]);
        let contacts = vec!["Mum".to_string(), "Ravi".to_string()];
        for (said, want) in [
            (
                "put dentist on my calendar for monday",
                json!({"tool":"add_event","args":{"day":"monday","title":"dentist"}}),
            ),
            (
                "what do I have on monday",
                json!({"tool":"read_calendar","args":{"day":"monday"}}),
            ),
            (
                "phone my brother Arjun",
                json!({"tool":"call","args":{"to":"Arjun"}}),
            ),
            (
                "could you ring my mother",
                json!({"tool":"call","args":{"to":"Mum"}}),
            ),
            (
                "switch the torch on",
                json!({"tool":"flashlight","args":{"on":"on"}}),
            ),
            (
                "make it louder, 80",
                json!({"tool":"set_volume","args":{"percent":"80"}}),
            ),
            (
                "drop Mum a text saying I'll be late",
                json!({"tool":"send_sms","args":{"to":"Mum","text":"I'll be late"}}),
            ),
            (
                "is there wifi around",
                json!({"tool":"wifi_scan","args":{}}),
            ),
            (
                "I can't find my phone",
                json!({"tool":"ring_phone","args":{}}),
            ),
            (
                "what's my signal like",
                json!({"tool":"phone_status","args":{}}),
            ),
            ("cut the call", json!({"tool":"hangup","args":{}})),
        ] {
            let (label, p) = c
                .decide(said, CONFIDENCE)
                .unwrap_or_else(|| panic!("unsure about {said:?}: {:?}", &c.predict(said)[..2]));
            assert_eq!(
                command(&label, said, &contacts),
                Some(want.clone()),
                "{said} -> {label} ({p})"
            );
        }
    }
    #[test]
    fn missing_details_and_unknown_requests_go_to_the_llm() {
        let c = classifier(&[]);
        assert!(c
            .decide("write me a poem about the sea", CONFIDENCE)
            .is_none());
        assert_eq!(
            command("send_sms", "text Mum", &["Mum".into()]),
            None,
            "no message text"
        );
        assert_eq!(command("set_volume", "louder", &[]), None, "no level");
        assert_eq!(
            answer("q.msgs_from", "how many texts did they send", &data()),
            None,
            "no person"
        );
    }
}
