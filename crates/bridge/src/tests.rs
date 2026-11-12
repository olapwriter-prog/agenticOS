use crate::transport::{Sim, Spool};
use crate::*;
use agentos_action::Action;
use agentos_executor::Connector;
use std::path::PathBuf;

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bridge-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn sim(tag: &str) -> (PathBuf, SharedBridge) {
    let d = dir(tag);
    let t = Sim::new(d.join("spool")).unwrap();
    let b = Bridge::load(Box::new(t), &d);
    (d, std::rc::Rc::new(std::cell::RefCell::new(b)))
}

fn note(pkg: &str, conv: &str, text: &str, att: Vec<Attachment>) -> Event {
    Event {
        v: 1,
        id: format!("e-{conv}-{}", text.len()),
        kind: "notification".into(),
        time_ms: 1_731_400_000_000,
        package: pkg.into(),
        app: "WhatsApp".into(),
        conversation: conv.into(),
        sender: conv.into(),
        text: text.into(),
        reply_key: format!("0|{pkg}|1|abc|10123"),
        attachments: att,
        ..Default::default()
    }
}

fn push(b: &SharedBridge, e: Event) -> Vec<String> {
    let mut bb = b.borrow_mut();
    bb.transport.inject(e);
    bb.pump(1)
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

fn run(b: &SharedBridge, tool: &str, args: &[(&str, &str)]) -> Result<String, String> {
    BridgeTools(b.clone()).call(&act(tool, args))
}

#[test]
fn names_and_paths_are_cleaned() {
    assert_eq!(clean_name("../../etc/passwd").as_deref(), Some("passwd"));
    assert_eq!(clean_name(".."), None);
    assert_eq!(clean_name("").as_deref(), None);
    let odd = clean_name("a/b\\c:d.pdf").unwrap();
    assert!(!odd.contains('/') && !odd.contains('\\') && !odd.contains(':'));
    assert!(rel_ok("files/a.pdf"));
    assert!(!rel_ok("files/../x"));
    assert!(!rel_ok("/etc/x"));
    assert!(!rel_ok("out/x"));
}

#[test]
fn spool_reads_in_order_skips_tmp_and_drops_bad_files() {
    let d = dir("spool");
    let mut sp = Spool::new(d.join("spool")).unwrap();
    let out = d.join("spool/out");
    std::fs::write(out.join("0002.json"), r#"{"v":1,"kind":"hello"}"#).unwrap();
    std::fs::write(
        out.join("0001.json"),
        r#"{"v":1,"kind":"notification","text":"hi"}"#,
    )
    .unwrap();
    std::fs::write(out.join("0003.json.tmp"), "{").unwrap();
    std::fs::write(out.join("0004.json"), "not json").unwrap();
    std::fs::write(
        out.join("0005.json"),
        vec![b' '; (MAX_EVENT_BYTES + 1) as usize],
    )
    .unwrap();
    let got = sp.take_events();
    let kinds: Vec<&str> = got.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, vec!["notification", "hello"]);
    assert!(
        out.join("0003.json.tmp").exists(),
        "a .tmp file is left alone"
    );
    assert!(sp.take_events().is_empty(), "each event is returned once");
}

#[test]
fn spool_send_leaves_one_json_and_no_tmp() {
    let d = dir("send");
    let mut sp = Spool::new(d.join("spool")).unwrap();
    sp.send(&Command {
        v: 1,
        id: "c-1".into(),
        kind: "reply".into(),
        ..Default::default()
    })
    .unwrap();
    let names: Vec<String> = std::fs::read_dir(d.join("spool/in"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(names, vec!["c-1.json".to_string()]);
}

#[test]
fn long_text_is_cut_to_the_limit_by_characters() {
    let d = dir("cut");
    let mut sp = Spool::new(d.join("spool")).unwrap();
    let text = "é".repeat(5000);
    std::fs::write(
        d.join("spool/out/0001.json"),
        serde_json::json!({"v":1,"kind":"notification","text":text}).to_string(),
    )
    .unwrap();
    assert_eq!(sp.take_events()[0].text.chars().count(), MAX_TEXT_CHARS);
}

#[test]
fn an_app_with_no_rule_is_dropped_and_listed_with_everything_off() {
    let (_d, b) = sim("norule");
    assert!(push(&b, note("com.whatsapp", "Mum", "hi", vec![])).is_empty());
    let bb = b.borrow();
    assert!(bb.state.inbox.is_empty());
    assert_eq!(
        bb.rule("com.whatsapp"),
        AppRule {
            package: "com.whatsapp".into(),
            app: "WhatsApp".into(),
            ..Default::default()
        }
    );
}

#[test]
fn an_allowed_app_lands_in_the_inbox_and_the_ledger_line_has_no_text() {
    let (_d, b) = sim("allowed");
    b.borrow_mut()
        .set_rule("com.whatsapp", "read", true)
        .unwrap();
    let lines = push(
        &b,
        note("com.whatsapp", "Mum", "Running 10 min late", vec![]),
    );
    assert_eq!(lines, vec!["bridge: message from WhatsApp".to_string()]);
    assert!(!lines.join("").contains("Running"));
    assert_eq!(b.borrow().unread(), 1);
}

#[test]
fn files_are_copied_only_when_allowed_and_names_never_collide() {
    let (d, b) = sim("files");
    b.borrow_mut()
        .set_rule("com.whatsapp", "read", true)
        .unwrap();
    let src = d.join("spool/files/Invoice.pdf");
    std::fs::write(&src, b"%PDF-1.4 test").unwrap();
    let att = Attachment {
        name: "Invoice.pdf".into(),
        rel: "files/Invoice.pdf".into(),
        mime: "application/pdf".into(),
        size: 13,
        too_big: false,
    };
    push(
        &b,
        note("com.whatsapp", "Mum", "invoice", vec![att.clone()]),
    );
    assert!(
        !d.join("documents/android/Invoice.pdf").exists(),
        "files off: not copied"
    );
    b.borrow_mut()
        .set_rule("com.whatsapp", "files", true)
        .unwrap();
    push(
        &b,
        note("com.whatsapp", "Mum", "invoice again", vec![att.clone()]),
    );
    push(&b, note("com.whatsapp", "Mum", "third", vec![att]));
    let bb = b.borrow();
    assert!(d.join("documents/android/Invoice.pdf").exists());
    assert!(d.join("documents/android/Invoice (2).pdf").exists());
    assert_eq!(bb.state.received.len(), 2);
}

#[test]
fn a_path_that_leaves_the_spool_is_rejected() {
    let (d, b) = sim("escape");
    b.borrow_mut()
        .set_rule("com.whatsapp", "read", true)
        .unwrap();
    b.borrow_mut()
        .set_rule("com.whatsapp", "files", true)
        .unwrap();
    let att = Attachment {
        name: "secret".into(),
        rel: "files/../../secret".into(),
        ..Default::default()
    };
    push(&b, note("com.whatsapp", "Mum", "x", vec![att]));
    assert!(b.borrow().state.received.is_empty());
    assert_eq!(b.borrow().state.rejected, 1);
    assert!(!d.join("documents/android/secret").exists());
}

#[test]
fn the_inbox_keeps_the_newest_500() {
    let (_d, b) = sim("keep");
    b.borrow_mut()
        .set_rule("com.whatsapp", "read", true)
        .unwrap();
    for i in 0..600 {
        let mut e = note("com.whatsapp", "Mum", &format!("m{i}"), vec![]);
        e.id = format!("e{i}");
        push(&b, e);
    }
    let bb = b.borrow();
    assert_eq!(bb.state.inbox.len(), INBOX_KEEP);
    assert_eq!(bb.state.inbox.last().unwrap().text, "m599");
}

#[test]
fn a_share_is_accepted_without_a_rule() {
    let (d, b) = sim("share");
    let att = Attachment {
        name: "Photo.jpg".into(),
        rel: "files/Photo.jpg".into(),
        ..Default::default()
    };
    std::fs::write(d.join("spool/files/Photo.jpg"), b"jpg").unwrap();
    let e = Event {
        kind: "share".into(),
        attachments: vec![att],
        ..Default::default()
    };
    let lines = push(&b, e);
    assert_eq!(lines, vec!["bridge: file from Android".to_string()]);
    assert_eq!(b.borrow().state.received.len(), 1);
}

#[test]
fn state_survives_a_restart() {
    let (d, b) = sim("persist");
    b.borrow_mut()
        .set_rule("com.whatsapp", "read", true)
        .unwrap();
    push(&b, note("com.whatsapp", "Mum", "kept", vec![]));
    b.borrow().save().unwrap();
    let again = Bridge::load(Box::new(Sim::new(d.join("spool")).unwrap()), &d);
    assert_eq!(again.state.inbox.len(), 1);
    assert!(again.rule("com.whatsapp").read);
}

#[test]
fn resolve_prefers_exact_then_sender_then_part_and_refuses_ties() {
    let (_d, b) = sim("resolve");
    b.borrow_mut()
        .set_rule("com.whatsapp", "read", true)
        .unwrap();
    push(&b, note("com.whatsapp", "Mum", "a", vec![]));
    push(&b, note("com.whatsapp", "Mumbai office", "b", vec![]));
    push(&b, note("com.whatsapp", "Ravi", "c", vec![]));
    assert_eq!(b.borrow().resolve("mum").unwrap(), "Mum");
    assert_eq!(b.borrow().resolve("ravi").unwrap(), "Ravi");
    let err = b.borrow().resolve("mu").unwrap_err();
    assert!(err.starts_with("more than one"), "{err}");
}

#[test]
fn reply_needs_the_reply_permission_and_sends_one_command() {
    let (_d, b) = sim("reply");
    b.borrow_mut()
        .set_rule("com.whatsapp", "read", true)
        .unwrap();
    push(&b, note("com.whatsapp", "Mum", "Running late?", vec![]));
    let refused = run(
        &b,
        "android_reply",
        &[("conversation", "Mum"), ("text", "On my way")],
    )
    .unwrap_err();
    assert!(refused.contains("replies to WhatsApp are off"), "{refused}");
    b.borrow_mut()
        .set_rule("com.whatsapp", "reply", true)
        .unwrap();
    let ok = run(
        &b,
        "android_reply",
        &[("conversation", "Mum"), ("text", "On my way")],
    )
    .unwrap();
    assert_eq!(ok, "Replied to Mum on WhatsApp");
    assert_eq!(b.borrow().transport.sent_count(), 1);
}

#[test]
fn share_refuses_a_path_and_a_missing_file() {
    let (_d, b) = sim("sharefail");
    b.borrow_mut()
        .set_rule("com.whatsapp", "read", true)
        .unwrap();
    let e1 = run(
        &b,
        "android_share",
        &[("app", "WhatsApp"), ("name", "../x")],
    )
    .unwrap_err();
    assert!(e1.contains("plain file name"), "{e1}");
    let e2 = run(
        &b,
        "android_share",
        &[("app", "WhatsApp"), ("name", "missing.pdf")],
    )
    .unwrap_err();
    assert!(e2.contains("no document called"), "{e2}");
}

#[test]
fn the_inbox_tool_draws_a_card_per_conversation() {
    let (_d, b) = sim("ui");
    b.borrow_mut()
        .set_rule("com.whatsapp", "read", true)
        .unwrap();
    push(&b, note("com.whatsapp", "Mum", "hi", vec![]));
    let reply = run(&b, "android_inbox", &[]).unwrap();
    let (ui, text) = agentos_screen::Ui::from_reply(&reply).unwrap();
    assert_eq!(ui.kind, "inbox");
    assert_eq!(ui.cards.len(), 1);
    assert!(text.contains("Mum"));
}

#[test]
fn status_reports_up_and_unread() {
    let (_d, b) = sim("status");
    let s = run(&b, "android_bridge_status", &[]).unwrap();
    assert!(s.starts_with("Android bridge: up"), "{s}");
}
