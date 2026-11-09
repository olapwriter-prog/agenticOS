//! agentd's real hardware backend (agentos_phone::Linux) against the simulated tools: the
//! output formats the simulator writes are the ones the backend parses.
use agentos_phone::backend::{Backend, CallState};
use agentos_phone::Linux;
use std::process::Command;

#[test]
fn the_modemmanager_backend_runs_on_the_simulator() {
    let sim = env!("CARGO_BIN_EXE_agentos-phonesim");
    let d = std::env::temp_dir().join(format!("phonesim-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::env::set_var("PHONESIM_DIR", &d);
    let path = format!("{}:{}", d.join("bin").display(), std::env::var("PATH").unwrap());
    std::env::set_var("PATH", path);
    assert!(Command::new(sim).arg("init").output().unwrap().status.success());
    let ctl = |args: &[&str]| assert!(Command::new(sim).args(args).status().unwrap().success());
    let mut p = Linux::new(d.join("leds"), d.join("scratch"));

    let s = p.status().unwrap();
    assert_eq!((s.operator.as_str(), s.signal, s.tech.as_str()), ("SimTel", 72, "lte"));
    assert!(s.registered && !s.airplane && s.wifi.is_none());

    // a call out, answered by the other side, hung up by us
    let id = p.dial("+919876543210").unwrap();
    let c = p.calls().unwrap();
    assert_eq!((c[0].id.as_str(), c[0].state, c[0].incoming), (id.as_str(), CallState::Dialing, false));
    ctl(&["pickup"]);
    assert_eq!(p.calls().unwrap()[0].state, CallState::Active);
    p.hangup(&id).unwrap();
    assert_eq!(p.calls().unwrap()[0].state, CallState::Ended);

    // a call in, answered
    ctl(&["call", "+919812345678"]);
    let c = p.calls().unwrap();
    let ring = c.iter().find(|c| c.incoming).unwrap();
    assert_eq!((ring.number.as_str(), ring.state), ("+919812345678", CallState::Ringing));
    p.answer(&ring.id).unwrap();

    // texts both ways; the text never travels on the command line
    p.send_sms("+919876543210", "it's 5pm, number='x'").unwrap();
    ctl(&["sms", "+919876543210", "dinner", "at", "8?"]);
    let m = p.messages().unwrap();
    assert!(m.iter().any(|m| !m.incoming && m.text == "it's 5pm, number='x'"));
    assert!(m.iter().any(|m| m.incoming && m.text == "dinner at 8?" && m.number == "+919876543210"));
    let log = std::fs::read_to_string(d.join("hw.log")).unwrap();
    assert!(!log.contains("5pm"));

    assert!(p.ussd("*123#").unwrap().contains("balance"));
    let nets = p.wifi_scan().unwrap();
    assert_eq!(nets[0].ssid, "HomeNet");
    assert!(p.wifi_connect("HomeNet", "wrong").is_err());
    p.wifi_connect("HomeNet", "sunflower").unwrap();
    assert_eq!(p.status().unwrap().wifi.as_deref(), Some("HomeNet"));
    p.set_mobile_data(true).unwrap();
    assert!(p.status().unwrap().mobile_data);
    p.set_airplane(true).unwrap();
    let s = p.status().unwrap();
    assert!(s.airplane && !s.mobile_data && s.wifi.is_none());
    assert!(p.dial("100").is_err(), "no calls in airplane mode");
    p.set_airplane(false).unwrap();
    assert!(p.location().unwrap().contains("12.9716"));
    p.torch(true).unwrap();
    assert_eq!(std::fs::read_to_string(d.join("leds/white:flash/brightness")).unwrap(), "255");
    p.set_volume(30).unwrap();
    p.feedback("message-new-sms").unwrap();
    let st = String::from_utf8(Command::new(sim).arg("state").output().unwrap().stdout).unwrap();
    assert!(st.contains("\"volume\": 0.3") && st.contains("message-new-sms"), "{st}");
    ctl(&["stop"]);
    let _ = std::fs::remove_dir_all(&d);
}
