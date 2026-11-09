//! agentos-phonesim: a simulated phone, so agentd's real hardware backend (`phone =
//! "modemmanager"`) runs end to end on any Linux machine.
//!
//! One binary, called by several names. Called as a phone tool it behaves like that tool:
//!   mmcli         ModemManager: modem status, calls, SMS, USSD, location
//!   nmcli         NetworkManager: radios, Wi-Fi, the mobile data connection
//!   callaudiocli  callaudiod: call audio routing      } these three must reach a user session
//!   fbcli         feedbackd: vibration and sounds     } bus, as the real ones do: with
//!   wpctl         WirePlumber: volume                 } PHONESIM_REQUIRE_SESSION=1 they fail
//!                                                       without one, like the real tools
//! with the output formats agentd parses, all on one phone state in $PHONESIM_DIR. Every
//! command is logged with the user it ran as (hw.log).
//!
//! The rest of the hardware is real Linux plumbing:
//!   - a private D-Bus daemon stands in for the system bus (DBUS_SYSTEM_BUS_ADDRESS), and
//!     ModemManager's signals (CallAdded, Messaging Added, Call StateChanged) go out on it with
//!     dbus-send, so agentd's dbus-monitor watcher sees real signals;
//!   - the volume-down key is a named pipe that gets real `struct input_event`s (a background
//!     process keeps it open, as a device always is, so agentd's reader never sees its end);
//!   - the flash LED is a sysfs-style directory.
//!
//! Called as agentos-phonesim it is the network and the human's hands:
//!   agentos-phonesim init                  make $PHONESIM_DIR, start the bus, print the env
//!   agentos-phonesim call NUMBER           someone rings this phone
//!   agentos-phonesim sms NUMBER TEXT       a text arrives
//!   agentos-phonesim hangup [ID]           the other side hangs up (default: the last call)
//!   agentos-phonesim pickup [ID]           the other side answers our call
//!   agentos-phonesim key MS [CODE]         hold a key (default 114, volume down) for MS ms
//!   agentos-phonesim waiting NUMBER        a second call comes in during one (call waiting)
//!   agentos-phonesim sms-parts NUMBER TEXT a long text starts arriving (state "receiving")
//!   agentos-phonesim sms-complete ID TEXT  ... and the rest of it arrives
//!   agentos-phonesim service on|off        the network: registered, or searching (no service)
//!   agentos-phonesim fail mmcli|nmcli on|off   the modem or NetworkManager gone
//!   agentos-phonesim gps fix|cell|none     what --location-get reports
//!   agentos-phonesim state | log | stop
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const MM: &str = "/org/freedesktop/ModemManager1";
const TOOLS: [&str; 5] = ["mmcli", "nmcli", "callaudiocli", "fbcli", "wpctl"];

#[derive(Serialize, Deserialize, Clone)]
struct Call {
    number: String,
    direction: String,
    state: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct Sms {
    number: String,
    text: String,
    pdu_type: String,
    state: String,
    timestamp: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct Net {
    ssid: String,
    signal: u8,
    password: String,
}

#[derive(Serialize, Deserialize)]
struct State {
    operator: String,
    signal: u8,
    tech: String,
    radios_on: bool,
    data_up: bool,
    wifi: Option<String>,
    nets: Vec<Net>,
    next_id: u32,
    calls: BTreeMap<u32, Call>,
    sms: BTreeMap<u32, Sms>,
    audio_mode: u8,
    volume: f32,
    feedback: Vec<String>,
    gps: (String, String),
    #[serde(default = "yes")]
    service: bool,
    #[serde(default)]
    fail: Vec<String>,
    #[serde(default = "fix")]
    gps_mode: String,
}

fn yes() -> bool {
    true
}
fn fix() -> String {
    "fix".into()
}

impl Default for State {
    fn default() -> Self {
        State {
            operator: "SimTel".into(),
            signal: 72,
            tech: "lte".into(),
            radios_on: true,
            data_up: false,
            wifi: None,
            nets: vec![
                Net { ssid: "HomeNet".into(), signal: 81, password: "sunflower".into() },
                Net { ssid: "Cafe Guest".into(), signal: 44, password: String::new() },
            ],
            next_id: 1,
            calls: BTreeMap::new(),
            sms: BTreeMap::new(),
            audio_mode: 0,
            volume: 0.5,
            feedback: vec![],
            gps: ("12.9716".into(), "77.5946".into()),
            service: true,
            fail: vec![],
            gps_mode: fix(),
        }
    }
}

fn dir() -> PathBuf {
    match std::env::var("PHONESIM_DIR") {
        Ok(d) => PathBuf::from(d),
        Err(_) => die("PHONESIM_DIR is not set (run: agentos-phonesim init)"),
    }
}

fn die(msg: &str) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(1)
}

/// The state, under an exclusive lock for the whole command.
struct Locked {
    _lock: std::fs::File,
    path: PathBuf,
    s: State,
}

impl Locked {
    fn open() -> Locked {
        let d = dir();
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(d.join("lock"))
            .unwrap_or_else(|e| die(&format!("{}: {e}", d.display())));
        use std::os::fd::AsRawFd;
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
        let path = d.join("state.json");
        let s = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Locked { _lock: lock, path, s }
    }
    /// In place, under the lock: the file stays shared by root and the session user.
    fn save(&self) {
        std::fs::write(&self.path, serde_json::to_string_pretty(&self.s).unwrap())
            .unwrap_or_else(|e| die(&format!("{}: {e}", self.path.display())));
    }
}

fn uid() -> u32 {
    unsafe { libc::getuid() }
}

/// Append to hw.log: the command and the user it ran as.
fn log(line: &str) {
    let d = dir();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(d.join("hw.log")) {
        let _ = writeln!(f, "uid={} {line}", uid());
    }
}

/// Emit a ModemManager signal on the simulated system bus.
fn signal(path: &str, member: &str, args: &[String]) {
    let bus = dir().join("system_bus");
    if !bus.exists() {
        return;
    }
    let st = Command::new("dbus-send")
        .env("DBUS_SYSTEM_BUS_ADDRESS", format!("unix:path={}", bus.display()))
        .args(["--system", "--type=signal", path, member])
        .args(args)
        .status();
    if !st.is_ok_and(|s| s.success()) {
        eprintln!("phonesim: could not send {member}");
    }
}

fn call_added(id: u32) {
    signal(
        &format!("{MM}/Modem/0"),
        "org.freedesktop.ModemManager1.Modem.Voice.CallAdded",
        &[format!("objpath:{MM}/Call/{id}")],
    );
}

/// MMCallState numbers, for the StateChanged signal's arguments.
fn state_no(s: &str) -> i32 {
    match s {
        "dialing" => 1,
        "ringing-out" => 2,
        "ringing-in" => 3,
        "active" => 4,
        "held" => 5,
        "waiting" => 6,
        "terminated" => 7,
        _ => 0,
    }
}

fn set_call_state(st: &mut State, id: u32, new: &str) {
    let Some(c) = st.calls.get_mut(&id) else { die(&format!("no call {id}")) };
    let old = std::mem::replace(&mut c.state, new.to_string());
    signal(
        &format!("{MM}/Call/{id}"),
        "org.freedesktop.ModemManager1.Call.StateChanged",
        &[format!("int32:{}", state_no(&old)), format!("int32:{}", state_no(new)), "uint32:0".into()],
    );
}

fn now() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    format!("{t}")
}

// ---- the tools ----------------------------------------------------------------------------

fn mmcli(args: &[String]) -> Result<String, String> {
    let mut l = Locked::open();
    if l.s.fail.iter().any(|t| t == "mmcli") {
        return Err("couldn't find modem".into());
    }
    let on_air = l.s.radios_on && l.s.service;
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let id_of = |s: &str| s.parse::<u32>().map_err(|_| format!("couldn't find object '{s}'"));
    let out = match a.as_slice() {
        ["-m", "any", "-K"] => {
            let s = &l.s;
            let state = if !s.radios_on {
                "disabled"
            } else if !s.service {
                "searching"
            } else if s.data_up {
                "connected"
            } else {
                "registered"
            };
            format!(
                "modem.generic.state : {state}\nmodem.3gpp.operator-name : {}\nmodem.generic.signal-quality.value : {}\nmodem.generic.access-technologies.value[1] : {}\n",
                s.operator, s.signal, s.tech
            )
        }
        ["-m", "any", create] if create.starts_with("--voice-create-call=number=") => {
            if !on_air {
                return Err("couldn't create call: modem is not registered".into());
            }
            let number = &create["--voice-create-call=number=".len()..];
            let id = l.s.next_id;
            l.s.next_id += 1;
            l.s.calls.insert(id, Call { number: number.into(), direction: "outgoing".into(), state: "unknown".into() });
            call_added(id);
            format!("Successfully created new call: {MM}/Call/{id}\n")
        }
        ["-o", id, op] => {
            let id = id_of(id)?;
            let c = l.s.calls.get(&id).ok_or(format!("couldn't find call {id}"))?.clone();
            match *op {
                "-K" => format!(
                    "call.dbus-path : {MM}/Call/{id}\ncall.properties.number : {}\ncall.properties.direction : {}\ncall.properties.state : {}\n",
                    c.number, c.direction, c.state
                ),
                "--start" if c.state == "unknown" => {
                    set_call_state(&mut l.s, id, "ringing-out");
                    "successfully started the call\n".into()
                }
                "--accept" if c.state == "ringing-in" || c.state == "waiting" => {
                    set_call_state(&mut l.s, id, "active");
                    "successfully accepted the call\n".into()
                }
                "--hangup" if c.state != "terminated" => {
                    set_call_state(&mut l.s, id, "terminated");
                    "successfully hung up the call\n".into()
                }
                _ => return Err(format!("couldn't {op} call {id} in state {}", c.state)),
            }
        }
        ["-m", "any", "--voice-list-calls"] => {
            let mut o = format!("Found {} calls:\n", l.s.calls.len());
            for (id, c) in &l.s.calls {
                o += &format!("\t{MM}/Call/{id} {} ({})\n", c.direction, c.state);
            }
            o
        }
        ["-m", "any", create, file] if create.starts_with("--messaging-create-sms=number=") => {
            let path = file
                .strip_prefix("--messaging-create-sms-with-text=")
                .ok_or("expected --messaging-create-sms-with-text")?;
            let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
            let number = &create["--messaging-create-sms=number=".len()..];
            let id = l.s.next_id;
            l.s.next_id += 1;
            l.s.sms.insert(id, Sms { number: number.into(), text, pdu_type: "submit".into(), state: "stored".into(), timestamp: String::new() });
            format!("Successfully created new SMS: {MM}/SMS/{id}\n")
        }
        ["-s", id, op] => {
            let id = id_of(id)?;
            let m = l.s.sms.get_mut(&id).ok_or(format!("couldn't find SMS {id}"))?;
            match *op {
                "-K" => format!(
                    "sms.dbus-path : {MM}/SMS/{id}\nsms.content.number : {}\nsms.content.text : {}\nsms.properties.pdu-type : {}\nsms.properties.state : {}\nsms.properties.timestamp : {}\n",
                    m.number,
                    m.text.replace('\n', " "),
                    m.pdu_type,
                    m.state,
                    if m.timestamp.is_empty() { "--" } else { &m.timestamp }
                ),
                "--send" if m.state == "stored" && m.pdu_type == "submit" => {
                    m.state = "sent".into();
                    "successfully sent the SMS\n".into()
                }
                _ => return Err(format!("couldn't {op} SMS {id}")),
            }
        }
        ["-m", "any", "--messaging-list-sms"] => {
            let mut o = format!("Found {} SMS messages:\n", l.s.sms.len());
            for (id, m) in &l.s.sms {
                o += &format!("\t{MM}/SMS/{id} ({})\n", m.state);
            }
            o
        }
        ["-m", "any", u] if u.starts_with("--3gpp-ussd-initiate=") => {
            if !on_air {
                return Err("couldn't initiate USSD session: modem is not registered".into());
            }
            let code = &u["--3gpp-ussd-initiate=".len()..];
            match code {
                "*123#" => "USSD session initiated; new reply from network: 'Main balance Rs 142.50, valid till 30-11-2026'\n".into(),
                "*121#" => "USSD session initiated; new reply from network: 'Data left 1.2 GB of 1.5 GB/day'\n".into(),
                _ => format!("USSD session initiated; new reply from network: 'Unknown code {code}'\n"),
            }
        }
        ["-m", "any", "--location-enable-gps-raw", "--location-enable-gps-nmea"] => "successfully setup location gathering\n".into(),
        ["-m", "any", "--location-get", "-K"] => {
            let (cell, gps) = match l.s.gps_mode.as_str() {
                "fix" => (true, true),
                "cell" => (true, false),
                _ => (false, false),
            };
            let v = |ok: bool, s: &str| if ok { s.to_string() } else { "--".into() };
            format!(
                "modem.location.3gpp.mcc : {}\nmodem.location.3gpp.mnc : {}\nmodem.location.3gpp.lac : {}\nmodem.location.3gpp.cid : {}\nmodem.location.gps.latitude : {}\nmodem.location.gps.longitude : {}\n",
                v(cell, "404"), v(cell, "45"), v(cell, "1A2B"), v(cell, "0042F1"), v(gps, &l.s.gps.0), v(gps, &l.s.gps.1)
            )
        }
        _ => return Err(format!("phonesim mmcli: unsupported arguments {a:?}")),
    };
    l.save();
    Ok(out)
}

fn nm_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace(':', "\\:")
}

fn nmcli(args: &[String]) -> Result<String, String> {
    let mut l = Locked::open();
    if l.s.fail.iter().any(|t| t == "nmcli") {
        return Err("Error: NetworkManager is not running.".into());
    }
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let on = |b: bool| if b { "enabled" } else { "disabled" };
    let out = match a.as_slice() {
        ["-t", "radio", "all"] => {
            let r = on(l.s.radios_on);
            format!("enabled:{r}:enabled:{r}\n")
        }
        ["-t", "-f", "TYPE,STATE,CONNECTION", "device"] => {
            let wifi = match &l.s.wifi {
                Some(n) => format!("wifi:connected:{}", nm_escape(n)),
                None => "wifi:disconnected:".into(),
            };
            let gsm = if l.s.data_up { "gsm:connected:mobile" } else { "gsm:disconnected:" };
            format!("{wifi}\n{gsm}\nloopback:connected (externally):lo\n")
        }
        ["-t", "-f", "SSID,SIGNAL", "device", "wifi", "list", "--rescan", "auto"] => {
            if !l.s.radios_on {
                String::new()
            } else {
                l.s.nets.iter().map(|n| format!("{}:{}\n", nm_escape(&n.ssid), n.signal)).collect()
            }
        }
        ["device", "wifi", "connect", ssid, rest @ ..] => {
            let given = match rest {
                ["password", p] => *p,
                [] => "",
                _ => return Err("bad arguments".into()),
            };
            let net = l.s.nets.iter().find(|n| n.ssid == *ssid).ok_or(format!("Error: No network with SSID '{ssid}' found."))?;
            if net.password != given {
                return Err("Error: Connection activation failed: Secrets were required, but not provided.".into());
            }
            l.s.wifi = Some(ssid.to_string());
            format!("Device 'wlan0' successfully activated with '{ssid}'.\n")
        }
        ["radio", "all", v @ ("on" | "off")] => {
            l.s.radios_on = *v == "on";
            if !l.s.radios_on {
                l.s.wifi = None;
                l.s.data_up = false;
            }
            String::new()
        }
        ["-t", "-f", "NAME,TYPE", "connection", "show"] => "mobile:gsm\nHomeNet:802-11-wireless\nlo:loopback\n".into(),
        ["connection", v @ ("up" | "down"), "mobile"] => {
            if *v == "up" && !l.s.radios_on {
                return Err("Error: Connection activation failed: radio is off".into());
            }
            l.s.data_up = *v == "up";
            "Connection successfully activated\n".into()
        }
        _ => return Err(format!("phonesim nmcli: unsupported arguments {a:?}")),
    };
    l.save();
    Ok(out)
}

/// The session tools reach their daemon over the user's session bus. The real ones fail when
/// it cannot be reached (a session bus only lets its own user in); so do these, if asked to.
fn session_bus() -> Result<(), String> {
    if std::env::var("PHONESIM_REQUIRE_SESSION").ok().as_deref() != Some("1") {
        return Ok(());
    }
    let addr = std::env::var("DBUS_SESSION_BUS_ADDRESS").map_err(|_| "Failed to connect to the session bus: DBUS_SESSION_BUS_ADDRESS not set")?;
    let ok = Command::new("dbus-send")
        .args(["--session", "--print-reply", "--dest=org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus.ListNames"])
        .output()
        .is_ok_and(|o| o.status.success());
    if ok {
        Ok(())
    } else {
        Err(format!("Failed to connect to the session bus {addr} as uid {}", uid()))
    }
}

fn session_tool(name: &str, args: &[String]) -> Result<String, String> {
    session_bus()?;
    let mut l = Locked::open();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = match (name, a.as_slice()) {
        ("callaudiocli", ["-m", m @ ("0" | "1")]) => {
            l.s.audio_mode = m.parse().unwrap();
            format!("Selected mode {m}\n")
        }
        ("fbcli", ["-E", event]) => {
            l.s.feedback.push(event.to_string());
            String::new()
        }
        ("wpctl", ["set-volume", "@DEFAULT_AUDIO_SINK@", v]) => {
            l.s.volume = v.parse().map_err(|_| format!("bad volume {v}"))?;
            String::new()
        }
        _ => return Err(format!("phonesim {name}: unsupported arguments {a:?}")),
    };
    l.save();
    Ok(out)
}

// ---- the network and the human --------------------------------------------------------------

fn last_call(st: &State) -> u32 {
    *st.calls.keys().next_back().unwrap_or_else(|| die("no calls yet"))
}

fn input_event(ms: u64, ty: u16, code: u16, value: i32) -> Vec<u8> {
    let mut e: libc::input_event = unsafe { std::mem::zeroed() };
    e.time.tv_sec = (ms / 1000) as _;
    e.time.tv_usec = ((ms % 1000) * 1000) as _;
    e.type_ = ty;
    e.code = code;
    e.value = value;
    let p = &e as *const libc::input_event as *const u8;
    unsafe { std::slice::from_raw_parts(p, std::mem::size_of::<libc::input_event>()) }.to_vec()
}

/// Hold a key: press, autorepeat, release, each followed by EV_SYN, as gpio-keys reports it.
fn key(ms: u64, code: u16) {
    let path = dir().join("input-event0");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags_nonblock()
        .open(&path)
        .unwrap_or_else(|e| die(&format!("{}: {e} (run init first)", path.display())));
    let t0 = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    let syn = |t| input_event(t, 0, 0, 0);
    let mut send = |b: Vec<u8>| f.write_all(&b).unwrap_or_else(|e| die(&e.to_string()));
    send([input_event(t0, 1, code, 1), syn(t0)].concat());
    let mut t = 0;
    while t + 250 < ms {
        t += 250;
        std::thread::sleep(std::time::Duration::from_millis(250));
        send([input_event(t0 + t, 1, code, 2), syn(t0 + t)].concat());
    }
    std::thread::sleep(std::time::Duration::from_millis(ms - t));
    send([input_event(t0 + ms, 1, code, 0), syn(t0 + ms)].concat());
}

trait NonBlock {
    fn custom_flags_nonblock(&mut self) -> &mut Self;
}
impl NonBlock for std::fs::OpenOptions {
    fn custom_flags_nonblock(&mut self) -> &mut Self {
        use std::os::unix::fs::OpenOptionsExt;
        self.custom_flags(libc::O_NONBLOCK)
    }
}

fn init() {
    let d = dir();
    std::fs::create_dir_all(d.join("bin")).unwrap();
    std::fs::create_dir_all(d.join("leds/white:flash")).unwrap();
    std::fs::write(d.join("leds/white:flash/max_brightness"), "255\n").unwrap();
    std::fs::write(d.join("leds/white:flash/brightness"), "0\n").unwrap();
    std::fs::create_dir_all(d.join("leds/red:status")).unwrap();
    std::fs::write(d.join("leds/red:status/brightness"), "0\n").unwrap();
    let exe = std::env::current_exe().unwrap();
    for t in TOOLS {
        let link = d.join("bin").join(t);
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&exe, &link).unwrap();
    }
    // the session tools run as another user: they need the state and log too
    use std::os::unix::fs::PermissionsExt;
    for p in [d.clone(), d.join("bin")] {
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    {
        let mut l = Locked::open();
        l.s = State::default();
        l.save();
    }
    for f in ["state.json", "lock", "hw.log"] {
        let p = d.join(f);
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&p);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o666)).unwrap();
    }
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o1777)).unwrap();
    // the volume-down key's input device
    let key = d.join("input-event0");
    if !key.exists() {
        let c = std::ffi::CString::new(key.to_str().unwrap()).unwrap();
        if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
            die("mkfifo failed");
        }
    }
    // keep the key device open, as the kernel keeps a real one (see keep_input)
    stop_bus(&d);
    #[allow(clippy::zombie_processes)] // it outlives init on purpose; `stop` ends it
    let keeper = Command::new(&exe)
        .arg("keep-input")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap_or_else(|e| die(&format!("keep-input: {e}")));
    std::fs::write(d.join("input.pid"), keeper.id().to_string()).unwrap();
    // the system bus: a private dbus-daemon
    let bus = d.join("system_bus");
    // (without dbus-daemon the tools still work; only the signals are missing)
    match Command::new("dbus-daemon")
        .args(["--session", "--fork", "--print-pid"])
        .arg(format!("--address=unix:path={}", bus.display()))
        .output()
    {
        Ok(out) if out.status.success() => {
            let pid = String::from_utf8_lossy(&out.stdout).trim().to_string();
            std::fs::write(d.join("bus.pid"), pid).unwrap();
        }
        Ok(out) => eprintln!("phonesim: dbus-daemon: {}; no signals", String::from_utf8_lossy(&out.stderr).trim()),
        Err(e) => eprintln!("phonesim: dbus-daemon: {e}; no signals"),
    }
    println!("export PHONESIM_DIR={}", d.display());
    println!("export PATH={}:$PATH", d.join("bin").display());
    println!("export DBUS_SYSTEM_BUS_ADDRESS=unix:path={}", bus.display());
    println!("# agentd.toml: phone = \"modemmanager\", leds_dir = \"{}\", approval_key = \"{}\"", d.join("leds").display(), key.display());
}

/// Hold the key's pipe open for reading and writing, never reading from it: a reader that opens
/// it then does not wait for a writer, and does not see end-of-file between key presses.
fn keep_input() {
    let path = dir().join("input-event0");
    let _f = std::fs::OpenOptions::new().read(true).write(true).open(&path).unwrap_or_else(|e| die(&e.to_string()));
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

fn stop_bus(d: &Path) {
    for f in ["bus.pid", "input.pid"] {
        if let Ok(pid) = std::fs::read_to_string(d.join(f)) {
            if let Ok(pid) = pid.trim().parse::<i32>() {
                unsafe { libc::kill(pid, libc::SIGTERM) };
            }
            let _ = std::fs::remove_file(d.join(f));
        }
    }
    let _ = std::fs::remove_file(d.join("system_bus"));
}

fn control(args: &[String]) {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["init"] => init(),
        ["stop"] => stop_bus(&dir()),
        ["keep-input"] => keep_input(),
        ["call", number] => {
            let mut l = Locked::open();
            let id = l.s.next_id;
            l.s.next_id += 1;
            l.s.calls.insert(id, Call { number: number.to_string(), direction: "incoming".into(), state: "ringing-in".into() });
            l.save();
            call_added(id);
            println!("call {id} ringing from {number}");
        }
        ["sms", number, text @ ..] if !text.is_empty() => {
            let mut l = Locked::open();
            let id = l.s.next_id;
            l.s.next_id += 1;
            l.s.sms.insert(id, Sms { number: number.to_string(), text: text.join(" "), pdu_type: "deliver".into(), state: "received".into(), timestamp: now() });
            l.save();
            signal(&format!("{MM}/Modem/0"), "org.freedesktop.ModemManager1.Modem.Messaging.Added", &[format!("objpath:{MM}/SMS/{id}"), "boolean:true".into()]);
            println!("sms {id} from {number}");
        }
        ["waiting", number] => {
            let mut l = Locked::open();
            let id = l.s.next_id;
            l.s.next_id += 1;
            l.s.calls.insert(id, Call { number: number.to_string(), direction: "incoming".into(), state: "waiting".into() });
            l.save();
            call_added(id);
            println!("call {id} waiting from {number}");
        }
        ["sms-parts", number, text @ ..] if !text.is_empty() => {
            let mut l = Locked::open();
            let id = l.s.next_id;
            l.s.next_id += 1;
            l.s.sms.insert(id, Sms { number: number.to_string(), text: text.join(" "), pdu_type: "deliver".into(), state: "receiving".into(), timestamp: now() });
            l.save();
            signal(&format!("{MM}/Modem/0"), "org.freedesktop.ModemManager1.Modem.Messaging.Added", &[format!("objpath:{MM}/SMS/{id}"), "boolean:true".into()]);
            println!("sms {id} from {number}, first part");
        }
        ["sms-complete", id, text @ ..] => {
            let mut l = Locked::open();
            let id: u32 = id.parse().unwrap_or_else(|_| die("bad id"));
            let m = l.s.sms.get_mut(&id).unwrap_or_else(|| die("no such SMS"));
            m.text = format!("{} {}", m.text, text.join(" "));
            m.state = "received".into();
            l.save();
            // ModemManager announces the new State as a property change on the SMS object.
            // (dbus-send cannot build the a{sv} argument; the interface name, which is what
            // a match rule's arg0 looks at, is the real one.)
            signal(&format!("{MM}/SMS/{id}"), "org.freedesktop.DBus.Properties.PropertiesChanged", &["string:org.freedesktop.ModemManager1.Sms".into()]);
            println!("sms {id} complete");
        }
        ["service", v @ ("on" | "off")] => {
            let mut l = Locked::open();
            l.s.service = *v == "on";
            l.save();
        }
        ["fail", tool @ ("mmcli" | "nmcli"), v @ ("on" | "off")] => {
            let mut l = Locked::open();
            l.s.fail.retain(|t| t != tool);
            if *v == "on" {
                l.s.fail.push(tool.to_string());
            }
            l.save();
        }
        ["gps", m @ ("fix" | "cell" | "none")] => {
            let mut l = Locked::open();
            l.s.gps_mode = m.to_string();
            l.save();
        }
        [op @ ("hangup" | "pickup"), rest @ ..] => {
            let mut l = Locked::open();
            let id = rest.first().map(|s| s.parse().unwrap_or_else(|_| die("bad id"))).unwrap_or_else(|| last_call(&l.s));
            set_call_state(&mut l.s, id, if *op == "hangup" { "terminated" } else { "active" });
            l.save();
            println!("call {id} {}", l.s.calls[&id].state);
        }
        ["key", ms, rest @ ..] => {
            let ms = ms.parse().unwrap_or_else(|_| die("key MS [CODE]"));
            let code = rest.first().map(|c| c.parse().unwrap_or_else(|_| die("bad code"))).unwrap_or(114);
            key(ms, code);
            println!("held key {code} for {ms} ms");
        }
        ["state"] => {
            let l = Locked::open();
            let mut v = serde_json::to_value(&l.s).unwrap();
            let torch = std::fs::read_to_string(dir().join("leds/white:flash/brightness")).unwrap_or_default();
            v["torch"] = json!(torch.trim());
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
        }
        ["log"] => print!("{}", std::fs::read_to_string(dir().join("hw.log")).unwrap_or_default()),
        _ => die("usage: agentos-phonesim init|call N|sms N TEXT|hangup [ID]|pickup [ID]|key MS [CODE]|waiting N|sms-parts N TEXT|sms-complete ID TEXT|service on|off|fail mmcli|nmcli on|off|gps fix|cell|none|state|log|stop"),
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let name = Path::new(&argv[0]).file_name().unwrap_or_default().to_string_lossy().to_string();
    let args = &argv[1..];
    let r = match name.as_str() {
        "mmcli" => mmcli(args),
        "nmcli" => nmcli(args),
        "callaudiocli" | "fbcli" | "wpctl" => session_tool(&name, args),
        _ => return control(args),
    };
    log(&format!("{name} {}{}", args.join(" "), if r.is_err() { "  -> FAILED" } else { "" }));
    match r {
        Ok(out) => print!("{out}"),
        Err(e) => die(&e),
    }
}
