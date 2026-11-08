//! The real phone, on a postmarketOS (or any ModemManager + NetworkManager) system.
//!
//! Every hardware action is one of the distribution's own command-line tools, run without a shell:
//!   mmcli         ModemManager: calls, SMS, USSD, signal, location
//!   nmcli         NetworkManager: Wi-Fi, mobile data, airplane mode
//!   callaudiocli  callaudiod: route audio to the earpiece during a call
//!   fbcli         feedbackd: vibration, ringtone, LED
//!   wpctl         WirePlumber: volume
//! Commands go through `Runner`, so tests check exactly what would be run.
use crate::backend::{Backend, Call, CallState, NetStatus, Sms, WifiNet};
use std::path::PathBuf;

pub trait Runner {
    /// Run `prog` with `args` and return standard output, or an error with standard error.
    fn run(&mut self, prog: &str, args: &[&str]) -> Result<String, String>;
}

/// Runs real programs.
pub struct SystemRunner;
impl Runner for SystemRunner {
    fn run(&mut self, prog: &str, args: &[&str]) -> Result<String, String> {
        let out = std::process::Command::new(prog)
            .args(args)
            .output()
            .map_err(|e| format!("{prog}: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(format!("{prog}: {}", err.trim()))
        }
    }
}

pub struct Linux {
    pub runner: Box<dyn Runner>,
    /// Usually /sys/class/leds; the flash LED is the one named *flash* or *torch*.
    pub leds_dir: PathBuf,
    /// Somewhere private to write SMS text before handing it to mmcli.
    pub scratch_dir: PathBuf,
}

/// Parse `mmcli -K` output: one `key : value` per line, `--` meaning empty.
pub fn kv(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(" : ")?;
            let v = v.trim();
            Some((
                k.trim().to_string(),
                if v == "--" {
                    String::new()
                } else {
                    v.to_string()
                },
            ))
        })
        .collect()
}
fn get<'a>(kv: &'a [(String, String)], key: &str) -> &'a str {
    kv.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .unwrap_or("")
}

/// Every `/org/freedesktop/ModemManager1/<kind>/<n>` in the text, as `n`.
pub fn object_ids(out: &str, kind: &str) -> Vec<String> {
    let marker = format!("/org/freedesktop/ModemManager1/{kind}/");
    let mut ids = Vec::new();
    let mut rest = out;
    while let Some(i) = rest.find(&marker) {
        rest = &rest[i + marker.len()..];
        let n: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !n.is_empty() && !ids.contains(&n) {
            ids.push(n);
        }
    }
    ids
}

/// Split one line of `nmcli -t` output on unescaped colons.
pub fn nm_fields(line: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    out.last_mut().unwrap().push(n);
                }
            }
            ':' => out.push(String::new()),
            c => out.last_mut().unwrap().push(c),
        }
    }
    out
}

fn on_off(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

impl Linux {
    pub fn new(leds_dir: PathBuf, scratch_dir: PathBuf) -> Self {
        Linux {
            runner: Box::new(SystemRunner),
            leds_dir,
            scratch_dir,
        }
    }
    fn mm(&mut self, args: &[&str]) -> Result<String, String> {
        self.runner.run("mmcli", args)
    }
    fn nm(&mut self, args: &[&str]) -> Result<String, String> {
        self.runner.run("nmcli", args)
    }
    /// Audio routing is best effort: a phone without callaudiod still makes calls.
    fn call_audio(&mut self, in_call: bool) {
        let _ = self
            .runner
            .run("callaudiocli", &["-m", if in_call { "1" } else { "0" }]);
    }
    fn torch_led(&self) -> Option<PathBuf> {
        let mut leds: Vec<PathBuf> = std::fs::read_dir(&self.leds_dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let n = p.file_name().unwrap_or_default().to_string_lossy();
                n.contains("flash") || n.contains("torch")
            })
            .collect();
        leds.sort();
        leds.into_iter().next()
    }
}

fn call_state(s: &str) -> CallState {
    match s {
        "ringing-in" | "waiting" => CallState::Ringing,
        "dialing" | "ringing-out" => CallState::Dialing,
        "active" => CallState::Active,
        "held" => CallState::Held,
        _ => CallState::Ended,
    }
}

impl Backend for Linux {
    fn status(&mut self) -> Result<NetStatus, String> {
        let m = kv(&self.mm(&["-m", "any", "-K"])?);
        let state = get(&m, "modem.generic.state");
        let mut st = NetStatus {
            operator: get(&m, "modem.3gpp.operator-name").to_string(),
            signal: get(&m, "modem.generic.signal-quality.value")
                .parse()
                .unwrap_or(0),
            tech: m
                .iter()
                .find(|(k, _)| k.starts_with("modem.generic.access-technologies.value"))
                .map(|(_, v)| v.clone())
                .unwrap_or_default(),
            registered: matches!(state, "registered" | "connected" | "connecting"),
            mobile_data: state == "connected",
            ..Default::default()
        };
        if let Ok(radio) = self.nm(&["-t", "radio", "all"]) {
            // WIFI-HW:WIFI:WWAN-HW:WWAN
            let f = nm_fields(radio.trim());
            st.airplane = f.get(1).map(String::as_str) == Some("disabled")
                && f.get(3).map(String::as_str) == Some("disabled");
        }
        if let Ok(devs) = self.nm(&["-t", "-f", "TYPE,STATE,CONNECTION", "device"]) {
            for l in devs.lines() {
                let f = nm_fields(l);
                if f.len() >= 3 && f[0] == "wifi" && f[1] == "connected" {
                    st.wifi = Some(f[2].clone());
                }
            }
        }
        Ok(st)
    }

    fn dial(&mut self, number: &str) -> Result<String, String> {
        let out = self.mm(&["-m", "any", &format!("--voice-create-call=number={number}")])?;
        let id = object_ids(&out, "Call")
            .into_iter()
            .next()
            .ok_or("modem did not create a call")?;
        self.mm(&["-o", &id, "--start"])?;
        self.call_audio(true);
        Ok(id)
    }
    fn answer(&mut self, id: &str) -> Result<(), String> {
        self.mm(&["-o", id, "--accept"])?;
        self.call_audio(true);
        Ok(())
    }
    fn hangup(&mut self, id: &str) -> Result<(), String> {
        self.mm(&["-o", id, "--hangup"])?;
        self.call_audio(false);
        Ok(())
    }
    fn calls(&mut self) -> Result<Vec<Call>, String> {
        let list = self.mm(&["-m", "any", "--voice-list-calls"])?;
        let mut calls = Vec::new();
        for id in object_ids(&list, "Call") {
            let c = kv(&self.mm(&["-o", &id, "-K"])?);
            calls.push(Call {
                number: get(&c, "call.properties.number").to_string(),
                incoming: get(&c, "call.properties.direction") == "incoming",
                state: call_state(get(&c, "call.properties.state")),
                id,
            });
        }
        Ok(calls)
    }

    fn send_sms(&mut self, number: &str, text: &str) -> Result<(), String> {
        // The text goes through a file so quotes and commas in it cannot confuse mmcli's own
        // key=value parser. The number has already been checked to be digits and '+'.
        std::fs::create_dir_all(&self.scratch_dir).map_err(|e| e.to_string())?;
        let file = self.scratch_dir.join("sms.txt");
        write_private(&file, text)?;
        let file_arg = format!("--messaging-create-sms-with-text={}", file.display());
        let created = self.mm(&[
            "-m",
            "any",
            &format!("--messaging-create-sms=number={number}"),
            &file_arg,
        ]);
        let _ = std::fs::remove_file(&file);
        let id = object_ids(&created?, "SMS")
            .into_iter()
            .next()
            .ok_or("modem did not create the message")?;
        self.mm(&["-s", &id, "--send"])?;
        Ok(())
    }
    fn messages(&mut self) -> Result<Vec<Sms>, String> {
        let list = self.mm(&["-m", "any", "--messaging-list-sms"])?;
        let mut out = Vec::new();
        for id in object_ids(&list, "SMS") {
            let s = kv(&self.mm(&["-s", &id, "-K"])?);
            out.push(Sms {
                number: get(&s, "sms.content.number").to_string(),
                text: get(&s, "sms.content.text").to_string(),
                incoming: get(&s, "sms.properties.pdu-type") == "deliver",
                time: get(&s, "sms.properties.timestamp").to_string(),
                id,
            });
        }
        Ok(out)
    }
    fn ussd(&mut self, code: &str) -> Result<String, String> {
        let out = self.mm(&["-m", "any", &format!("--3gpp-ussd-initiate={code}")])?;
        Ok(out.trim().to_string())
    }

    fn wifi_scan(&mut self) -> Result<Vec<WifiNet>, String> {
        let out = self.nm(&[
            "-t",
            "-f",
            "SSID,SIGNAL",
            "device",
            "wifi",
            "list",
            "--rescan",
            "auto",
        ])?;
        let mut nets: Vec<WifiNet> = out
            .lines()
            .filter_map(|l| {
                let f = nm_fields(l);
                let ssid = f.first()?.clone();
                (!ssid.is_empty()).then(|| WifiNet {
                    ssid,
                    signal: f.get(1).and_then(|s| s.parse().ok()).unwrap_or(0),
                })
            })
            .collect();
        nets.sort_by_key(|n| std::cmp::Reverse(n.signal));
        nets.dedup_by(|a, b| a.ssid == b.ssid);
        Ok(nets)
    }
    fn wifi_connect(&mut self, ssid: &str, password: &str) -> Result<(), String> {
        let mut args = vec!["device", "wifi", "connect", ssid];
        if !password.is_empty() {
            args.extend(["password", password]);
        }
        self.nm(&args).map(|_| ())
    }
    fn set_airplane(&mut self, on: bool) -> Result<(), String> {
        // `radio all off` turns off Wi-Fi and the cellular radio together.
        self.nm(&["radio", "all", on_off(!on)]).map(|_| ())
    }
    fn set_mobile_data(&mut self, on: bool) -> Result<(), String> {
        let conns = self.nm(&["-t", "-f", "NAME,TYPE", "connection", "show"])?;
        let name = conns
            .lines()
            .map(nm_fields)
            .find(|f| f.get(1).map(String::as_str) == Some("gsm"))
            .and_then(|f| f.into_iter().next())
            .ok_or("no mobile data connection is set up (nmcli connection add type gsm ...)")?;
        self.nm(&["connection", if on { "up" } else { "down" }, &name])
            .map(|_| ())
    }
    fn location(&mut self) -> Result<String, String> {
        let _ = self.mm(&[
            "-m",
            "any",
            "--location-enable-gps-raw",
            "--location-enable-gps-nmea",
        ]);
        let l = kv(&self.mm(&["-m", "any", "--location-get", "-K"])?);
        let (lat, lon) = (
            get(&l, "modem.location.gps.latitude"),
            get(&l, "modem.location.gps.longitude"),
        );
        if !lat.is_empty() && !lon.is_empty() {
            return Ok(format!("{lat}, {lon} (gps)"));
        }
        let cid = get(&l, "modem.location.3gpp.cid");
        if !cid.is_empty() {
            return Ok(format!(
                "no GPS fix; cell mcc {} mnc {} lac {} cid {cid}",
                get(&l, "modem.location.3gpp.mcc"),
                get(&l, "modem.location.3gpp.mnc"),
                get(&l, "modem.location.3gpp.lac"),
            ));
        }
        Err("no location available yet".into())
    }
    fn torch(&mut self, on: bool) -> Result<(), String> {
        let led = self
            .torch_led()
            .ok_or("this phone has no flash LED in sysfs")?;
        let level = if on {
            std::fs::read_to_string(led.join("max_brightness"))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "1".into())
        } else {
            "0".into()
        };
        std::fs::write(led.join("brightness"), level).map_err(|e| e.to_string())
    }
    fn set_volume(&mut self, percent: u8) -> Result<(), String> {
        let v = format!("{:.2}", percent.min(100) as f32 / 100.0);
        self.runner
            .run("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", &v])
            .map(|_| ())
    }
    fn feedback(&mut self, event: &str) -> Result<(), String> {
        self.runner.run("fbcli", &["-E", event]).map(|_| ())
    }
}

fn write_private(path: &std::path::Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path).map_err(|e| e.to_string())?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Answers from a script and remembers every command.
    struct Script {
        log: Rc<RefCell<Vec<String>>>,
        answers: Vec<(&'static str, &'static str)>,
    }
    impl Runner for Script {
        fn run(&mut self, prog: &str, args: &[&str]) -> Result<String, String> {
            let cmd = format!("{prog} {}", args.join(" "));
            self.log.borrow_mut().push(cmd.clone());
            self.answers
                .iter()
                .find(|(prefix, _)| cmd.starts_with(prefix))
                .map(|(_, a)| a.to_string())
                .ok_or_else(|| format!("unexpected: {cmd}"))
        }
    }
    fn linux(answers: Vec<(&'static str, &'static str)>) -> (Linux, Rc<RefCell<Vec<String>>>) {
        let log = Rc::new(RefCell::new(vec![]));
        let dir = std::env::temp_dir().join(format!("phone-linux-{}", std::process::id()));
        let l = Linux {
            runner: Box::new(Script {
                log: log.clone(),
                answers,
            }),
            leds_dir: dir.join("leds"),
            scratch_dir: dir,
        };
        (l, log)
    }

    #[test]
    fn kv_parses_mmcli_keys_and_empty_values() {
        let k = kv(
            "modem.3gpp.operator-name : Jio\nmodem.generic.signal-quality.value : 64\nx.y : --\n",
        );
        assert_eq!(get(&k, "modem.3gpp.operator-name"), "Jio");
        assert_eq!(get(&k, "x.y"), "");
    }
    #[test]
    fn object_ids_finds_each_path_once() {
        let out = "Found 2 calls:\n /org/freedesktop/ModemManager1/Call/3 incoming\n /org/freedesktop/ModemManager1/Call/12 outgoing\n/org/freedesktop/ModemManager1/Call/3";
        assert_eq!(object_ids(out, "Call"), vec!["3", "12"]);
        assert!(object_ids(out, "SMS").is_empty());
    }
    #[test]
    fn nmcli_escaped_colons_stay_in_the_field() {
        assert_eq!(nm_fields(r"my\:net:70"), vec!["my:net", "70"]);
    }
    #[test]
    fn dialing_creates_then_starts_the_call_and_routes_audio() {
        let (mut l, log) = linux(vec![
            (
                "mmcli -m any --voice-create-call",
                "Successfully created new call: /org/freedesktop/ModemManager1/Call/7\n",
            ),
            ("mmcli -o 7 --start", "successfully started the call\n"),
            ("callaudiocli", ""),
        ]);
        assert_eq!(l.dial("+919876543210").unwrap(), "7");
        assert_eq!(
            *log.borrow(),
            vec![
                "mmcli -m any --voice-create-call=number=+919876543210",
                "mmcli -o 7 --start",
                "callaudiocli -m 1"
            ]
        );
    }
    #[test]
    fn sms_text_goes_through_a_file_and_is_cleaned_up() {
        let (mut l, log) = linux(vec![
            (
                "mmcli -m any --messaging-create-sms",
                "Successfully created new SMS: /org/freedesktop/ModemManager1/SMS/4\n",
            ),
            ("mmcli -s 4 --send", "successfully sent the SMS\n"),
        ]);
        l.send_sms("+919876543210", "it's 5pm, number='x'").unwrap();
        let log = log.borrow();
        assert!(log[0].contains("number=+919876543210 --messaging-create-sms-with-text="));
        assert!(
            !log[0].contains("5pm"),
            "text must not be on the command line"
        );
        assert_eq!(log[1], "mmcli -s 4 --send");
        assert!(!l.scratch_dir.join("sms.txt").exists());
    }
    #[test]
    fn incoming_calls_are_read_from_modemmanager() {
        let (mut l, _) = linux(vec![
            ("mmcli -m any --voice-list-calls", "Found 1 call:\n\t/org/freedesktop/ModemManager1/Call/2 incoming (ringing-in)\n"),
            ("mmcli -o 2 -K", "call.properties.number : +919876543210\ncall.properties.direction : incoming\ncall.properties.state : ringing-in\n"),
        ]);
        let c = l.calls().unwrap();
        assert_eq!(c.len(), 1);
        assert!(c[0].incoming);
        assert_eq!(c[0].state, CallState::Ringing);
        assert_eq!(c[0].number, "+919876543210");
    }
    #[test]
    fn status_combines_modem_and_network_manager() {
        let (mut l, _) = linux(vec![
            ("mmcli -m any -K", "modem.generic.state : registered\nmodem.3gpp.operator-name : Airtel\nmodem.generic.signal-quality.value : 55\nmodem.generic.access-technologies.value[1] : lte\n"),
            ("nmcli -t radio all", "enabled:enabled:enabled:enabled\n"),
            ("nmcli -t -f TYPE,STATE,CONNECTION device", "wifi:connected:HomeNet\ngsm:disconnected:\n"),
        ]);
        let s = l.status().unwrap();
        assert_eq!(s.operator, "Airtel");
        assert_eq!(s.signal, 55);
        assert_eq!(s.tech, "lte");
        assert!(s.registered && !s.airplane && !s.mobile_data);
        assert_eq!(s.wifi.as_deref(), Some("HomeNet"));
    }
    #[test]
    fn torch_writes_max_brightness_to_the_flash_led() {
        let (mut l, _) = linux(vec![]);
        let led = l.leds_dir.join("white:flash");
        std::fs::create_dir_all(&led).unwrap();
        std::fs::write(led.join("max_brightness"), "255\n").unwrap();
        l.torch(true).unwrap();
        assert_eq!(
            std::fs::read_to_string(led.join("brightness")).unwrap(),
            "255"
        );
        l.torch(false).unwrap();
        assert_eq!(
            std::fs::read_to_string(led.join("brightness")).unwrap(),
            "0"
        );
        std::fs::remove_dir_all(&l.leds_dir).ok();
    }
}
