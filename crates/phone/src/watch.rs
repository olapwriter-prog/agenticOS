//! ModemManager's D-Bus signals, so an incoming call or text is seen when it arrives instead of
//! on the next poll. Like the rest of the backend this uses the distribution's own tool:
//! `dbus-monitor --system` with match rules for the signals that matter. A signal only says
//! "look now"; what arrived is still read through mmcli, so the parsing stays in one place.
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// What the watcher reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// The monitor is running: polling can slow down.
    Up,
    /// It stopped (and is restarted after a pause): poll at the normal rate meanwhile.
    Down,
    /// Something arrived or changed: look now.
    Wake(&'static str),
}

/// The match rules given to dbus-monitor.
pub const RULES: [&str; 4] = [
    "type='signal',interface='org.freedesktop.ModemManager1.Modem.Voice',member='CallAdded'",
    "type='signal',interface='org.freedesktop.ModemManager1.Modem.Messaging',member='Added'",
    "type='signal',interface='org.freedesktop.ModemManager1.Call',member='StateChanged'",
    // a text that arrived in parts is whole when its State property changes to received
    "type='signal',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',arg0='org.freedesktop.ModemManager1.Sms'",
];

/// Which signal a dbus-monitor header line is, if it is one of ours.
pub fn wake_of(line: &str) -> Option<&'static str> {
    if !line.starts_with("signal ") {
        return None;
    }
    let field = |name: &str| {
        line.split([';', ' '])
            .find_map(|part| part.strip_prefix(name))
            .unwrap_or("")
    };
    match (field("interface="), field("member=")) {
        ("org.freedesktop.ModemManager1.Modem.Voice", "CallAdded") => Some("call"),
        ("org.freedesktop.ModemManager1.Modem.Messaging", "Added") => Some("sms"),
        ("org.freedesktop.ModemManager1.Call", "StateChanged") => Some("call-state"),
        ("org.freedesktop.DBus.Properties", "PropertiesChanged")
            if field("path=").starts_with("/org/freedesktop/ModemManager1/SMS/") =>
        {
            Some("sms-state")
        }
        _ => None,
    }
}

/// Run the monitor in a thread, restarting it ten seconds after it stops.
pub fn spawn(program: PathBuf, on: impl Fn(Signal) + Send + 'static) {
    std::thread::spawn(move || {
        let mut warned = false;
        loop {
            let child = Command::new(&program)
                .arg("--system")
                .args(RULES)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn();
            match child {
                Ok(mut c) => {
                    warned = false;
                    on(Signal::Up);
                    if let Some(out) = c.stdout.take() {
                        for line in BufReader::new(out).lines().map_while(Result::ok) {
                            if let Some(w) = wake_of(&line) {
                                on(Signal::Wake(w));
                            }
                        }
                    }
                    let _ = c.wait();
                    on(Signal::Down);
                }
                Err(e) if !warned => {
                    eprintln!(
                        "agentd: {}: {e}; polling instead of D-Bus signals",
                        program.display()
                    );
                    warned = true;
                }
                Err(_) => {}
            }
            std::thread::sleep(Duration::from_secs(10));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    // header lines as dbus-monitor prints them
    const CALL: &str = "signal time=1731150000.123456 sender=:1.7 -> destination=(null destination) serial=812 path=/org/freedesktop/ModemManager1/Modem/0; interface=org.freedesktop.ModemManager1.Modem.Voice; member=CallAdded";
    const SMS: &str = "signal time=1731150001.5 sender=:1.7 -> destination=(null destination) serial=815 path=/org/freedesktop/ModemManager1/Modem/0; interface=org.freedesktop.ModemManager1.Modem.Messaging; member=Added";
    const STATE: &str = "signal time=1731150002.0 sender=:1.7 -> destination=(null destination) serial=820 path=/org/freedesktop/ModemManager1/Call/3; interface=org.freedesktop.ModemManager1.Call; member=StateChanged";

    #[test]
    fn modemmanager_signals_are_recognised_and_nothing_else() {
        assert_eq!(wake_of(CALL), Some("call"));
        assert_eq!(wake_of(SMS), Some("sms"));
        assert_eq!(wake_of(STATE), Some("call-state"));
        // the body lines after a header, and other signals, are not wakes
        assert_eq!(
            wake_of("   object path \"/org/freedesktop/ModemManager1/SMS/4\""),
            None
        );
        assert_eq!(wake_of("   boolean true"), None);
        let other = SMS.replace("member=Added", "member=Deleted");
        assert_eq!(wake_of(&other), None);
        let props = "signal time=1 sender=:1.7 -> destination=(null destination) serial=9 path=/org/freedesktop/ModemManager1/Modem/0; interface=org.freedesktop.DBus.Properties; member=PropertiesChanged";
        assert_eq!(wake_of(props), None);
        // ... but a text's own property change (it finished arriving) is
        let sms = props.replace("Modem/0", "SMS/4");
        assert_eq!(wake_of(&sms), Some("sms-state"));
    }

    #[test]
    fn the_watcher_reports_up_wakes_and_down() {
        // a stand-in dbus-monitor: prints a call and a text, then exits
        let dir = std::env::temp_dir().join(format!("agentos-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prog = dir.join("dbus-monitor");
        std::fs::write(
            &prog,
            format!("#!/bin/sh\necho '{CALL}'\necho '   string \"x\"'\necho '{SMS}'\n"),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        spawn(prog, move |sig| s2.lock().unwrap().push(sig));
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while seen.lock().unwrap().len() < 4 && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            seen.lock().unwrap()[..4],
            [
                Signal::Up,
                Signal::Wake("call"),
                Signal::Wake("sms"),
                Signal::Down
            ]
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
