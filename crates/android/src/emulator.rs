//! The Android Emulator as the sandbox: for running Agentic OS on a Mac (or any desktop) where
//! Waydroid cannot run. A Google Play system image gives the real Play Store; the emulator is
//! driven with `adb` and its console (`adb emu ...`).
//!
//! The bridge: the bridge app writes its spool under its media folder on the device. A desktop
//! cannot share that folder the way Waydroid does, so `sync_bridge` mirrors it over adb: new
//! events (and their attachments) come down, commands (and files to share) go up.
use crate::{App, Runner, Sandbox, PLAY_PACKAGE};
use std::path::{Path, PathBuf};

/// Where the bridge app keeps its spool on the device (its first external media folder).
pub const DEVICE_SPOOL: &str = "/sdcard/Android/media/os.agentic.bridge";

pub struct Emulator {
    pub runner: Box<dyn Runner>,
    /// the Android SDK (ANDROID_HOME)
    pub sdk: PathBuf,
    /// the virtual device's name
    pub avd: String,
    /// adb's name for it
    pub serial: String,
    /// show the emulator's window (false: headless)
    pub window: bool,
}

/// Friendly names for packages Android's package manager lists without labels.
pub fn app_name(package: &str) -> String {
    let known = [
        ("com.whatsapp", "WhatsApp"),
        ("org.telegram.messenger", "Telegram"),
        ("com.android.vending", "Play Store"),
        ("com.google.android.youtube", "YouTube"),
        ("com.google.android.apps.maps", "Maps"),
        ("com.google.android.gm", "Gmail"),
        ("com.android.chrome", "Chrome"),
        ("com.instagram.android", "Instagram"),
        ("net.one97.paytm", "Paytm"),
        ("com.phonepe.app", "PhonePe"),
        ("com.google.android.apps.nbu.paisa.user", "Google Pay"),
        ("in.swiggy.android", "Swiggy"),
        ("com.application.zomato", "Zomato"),
        ("com.ubercab", "Uber"),
        ("os.agentic.bridge", "Agentic OS bridge"),
    ];
    if let Some((_, n)) = known.iter().find(|(p, _)| *p == package) {
        return n.to_string();
    }
    let last = package.rsplit('.').next().unwrap_or(package);
    let mut c = last.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => package.to_string(),
    }
}

/// `pm list packages`: one `package:<name>` per line.
pub fn parse_packages(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("package:"))
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

impl Emulator {
    pub fn adb_path(&self) -> String {
        self.sdk
            .join("platform-tools")
            .join("adb")
            .display()
            .to_string()
    }
    fn emulator_path(&self) -> String {
        self.sdk
            .join("emulator")
            .join("emulator")
            .display()
            .to_string()
    }
    fn adb(&mut self, args: &[&str]) -> Result<String, String> {
        let adb = self.adb_path();
        let serial = self.serial.clone();
        let mut a = vec!["-s", serial.as_str()];
        a.extend_from_slice(args);
        self.runner.run(&adb, &a)
    }
    fn shell(&mut self, cmd: &str) -> Result<String, String> {
        self.adb(&["shell", cmd])
    }
    /// Boot the virtual device, detached (it keeps running after this returns).
    fn start(&mut self) -> Result<(), String> {
        let window = if self.window { "" } else { " -no-window" };
        let cmd = format!(
            "nohup '{}' -avd '{}' -no-boot-anim -netdelay none -netspeed full{window} >/dev/null 2>&1 &",
            self.emulator_path(),
            self.avd.replace('\'', "")
        );
        self.runner.run("sh", &["-c", &cmd]).map(|_| ())
    }

    /// Mirror the bridge's spool between the device and `local`: events and attachments down
    /// (removed from the device once copied), commands up (removed here once on the device),
    /// files offered for sharing up. Returns (down, up).
    pub fn sync_spool(&mut self, local: &Path) -> Result<(usize, usize), String> {
        for sub in ["out", "in", "files"] {
            std::fs::create_dir_all(local.join(sub)).map_err(|e| e.to_string())?;
        }
        let list = |me: &mut Self, dir: &str| -> Vec<String> {
            me.shell(&format!("ls {DEVICE_SPOOL}/{dir} 2>/dev/null"))
                .map(|t| {
                    t.lines()
                        .map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty() && !l.ends_with(".tmp") && !l.contains('/'))
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut down = 0;
        // attachments first, so an event never arrives before its file
        for name in list(self, "files") {
            let dst = local.join("files").join(&name);
            if !dst.exists() {
                self.adb(&[
                    "pull",
                    &format!("{DEVICE_SPOOL}/files/{name}"),
                    &dst.display().to_string(),
                ])?;
                down += 1;
            }
        }
        for name in list(self, "out")
            .into_iter()
            .filter(|n| n.ends_with(".json"))
        {
            let dst = local.join("out").join(&name);
            self.adb(&[
                "pull",
                &format!("{DEVICE_SPOOL}/out/{name}"),
                &dst.display().to_string(),
            ])?;
            self.shell(&format!("rm -f {DEVICE_SPOOL}/out/{name}"))?;
            down += 1;
        }
        let mut up = 0;
        // files to share, before the commands that name them
        let on_device: Vec<String> = list(self, "files");
        for e in std::fs::read_dir(local.join("files"))
            .map_err(|e| e.to_string())?
            .flatten()
        {
            let name = e.file_name().to_string_lossy().to_string();
            if !on_device.contains(&name) && !name.ends_with(".tmp") {
                self.adb(&[
                    "push",
                    &e.path().display().to_string(),
                    &format!("{DEVICE_SPOOL}/files/{name}"),
                ])?;
                up += 1;
            }
        }
        let mut cmds: Vec<PathBuf> = std::fs::read_dir(local.join("in"))
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        cmds.sort();
        for p in cmds {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            // under a .tmp name, then renamed: the app never reads half a command
            self.adb(&[
                "push",
                &p.display().to_string(),
                &format!("{DEVICE_SPOOL}/in/{name}.tmp"),
            ])?;
            self.shell(&format!(
                "mv {DEVICE_SPOOL}/in/{name}.tmp {DEVICE_SPOOL}/in/{name}"
            ))?;
            let _ = std::fs::remove_file(&p);
            up += 1;
        }
        Ok((down, up))
    }
}

impl Sandbox for Emulator {
    fn available(&mut self) -> bool {
        let emu = self.emulator_path();
        self.runner
            .run(&emu, &["-list-avds"])
            .map(|t| t.lines().any(|l| l.trim() == self.avd))
            .unwrap_or(false)
    }
    fn running(&mut self) -> bool {
        self.adb(&["get-state"])
            .map(|s| s.trim() == "device")
            .unwrap_or(false)
    }
    fn apps(&mut self) -> Result<Vec<App>, String> {
        let mut pk = parse_packages(&self.shell("pm list packages -3")?);
        if parse_packages(&self.shell(&format!("pm list packages {PLAY_PACKAGE}"))?)
            .iter()
            .any(|p| p == PLAY_PACKAGE)
        {
            pk.push(PLAY_PACKAGE.to_string());
        }
        pk.sort();
        pk.dedup();
        Ok(pk
            .into_iter()
            .map(|p| App {
                name: app_name(&p),
                package: p,
            })
            .collect())
    }
    fn open(&mut self, package: &str) -> Result<(), String> {
        if !self.running() {
            return Err("the virtual phone is not running; open Android first".into());
        }
        let out = self.shell(&format!(
            "monkey -p {package} -c android.intent.category.LAUNCHER 1"
        ))?;
        if out.contains("No activities found") {
            return Err(format!("{package} is not installed"));
        }
        Ok(())
    }
    fn install(&mut self, apk_path: &str) -> Result<(), String> {
        self.adb(&["install", "-r", apk_path]).map(|_| ())
    }
    fn play_page(&mut self, package: &str) -> Result<(), String> {
        self.shell(&format!(
            "am start -a android.intent.action.VIEW -d market://details?id={package}"
        ))
        .map(|_| ())
    }
    fn stop_all(&mut self) -> Result<(), String> {
        self.adb(&["emu", "kill"]).map(|_| ())
    }
    fn show(&mut self) -> Result<(), String> {
        if !self.running() {
            return self.start();
        }
        // already up: bring its window forward (macOS; elsewhere the window manager decides)
        if cfg!(target_os = "macos") {
            let _ = self.runner.run(
                "osascript",
                &["-e", "tell application \"System Events\" to set frontmost of (first process whose name starts with \"qemu-system\") to true"],
            );
        }
        Ok(())
    }
    fn pause(&mut self) -> Result<(), String> {
        // the emulator console: the virtual CPU stops, apps keep their state (verify on the Mac)
        self.adb(&["emu", "avd", "stop"]).map(|_| ())
    }
    fn resume(&mut self) -> Result<(), String> {
        self.adb(&["emu", "avd", "start"]).map(|_| ())
    }
    fn paused(&mut self) -> bool {
        self.adb(&["emu", "avd", "status"])
            .map(|s| s.contains("stopped"))
            .unwrap_or(false)
    }
    fn init_gapps(&mut self) -> Result<String, String> {
        if self.has_play().unwrap_or(false) {
            Ok("This virtual phone has the Google Play Store.".into())
        } else {
            Err("This virtual phone was made without Google Play. Run scripts/mac/setup.sh, which creates one from a google_apis_playstore image.".into())
        }
    }
    fn gsf_id(&mut self) -> Result<String, String> {
        Err("Not needed: the emulator's Play Store images are certified by Google. Open the Play Store and sign in.".into())
    }
    fn sync_bridge(&mut self, local: &Path) -> Result<(usize, usize), String> {
        if !self.running() || self.paused() {
            return Ok((0, 0));
        }
        self.sync_spool(local)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    /// A fake device: a file system of spool files, and a log of every command.
    #[derive(Default)]
    struct Device {
        files: HashMap<String, String>,
        log: Vec<String>,
        state: String,
    }
    struct Fake(Rc<RefCell<Device>>);
    impl Runner for Fake {
        fn run(&mut self, prog: &str, args: &[&str]) -> Result<String, String> {
            let mut d = self.0.borrow_mut();
            d.log.push(format!(
                "{} {}",
                prog.rsplit('/').next().unwrap(),
                args.join(" ")
            ));
            let a: Vec<&str> = args
                .iter()
                .skip_while(|x| **x == "-s" || x.starts_with("emulator-"))
                .copied()
                .collect();
            match a.as_slice() {
                ["get-state"] => Ok("device\n".into()),
                ["emu", "avd", "status"] => Ok(format!("virtual device is {}\nOK\n", d.state)),
                ["emu", "avd", s] => {
                    d.state = if *s == "stop" {
                        "stopped".into()
                    } else {
                        "running".into()
                    };
                    Ok("OK".into())
                }
                ["shell", cmd] if cmd.starts_with("ls ") => {
                    let dir = cmd[3..].split_whitespace().next().unwrap().to_string() + "/";
                    let mut v: Vec<String> = d
                        .files
                        .keys()
                        .filter_map(|k| k.strip_prefix(&dir).map(String::from))
                        .collect();
                    v.sort();
                    Ok(v.join("\n"))
                }
                ["shell", cmd] if cmd.starts_with("rm -f ") => {
                    d.files.remove(&cmd[6..]);
                    Ok(String::new())
                }
                ["shell", cmd] if cmd.starts_with("mv ") => {
                    let p: Vec<&str> = cmd[3..].split(' ').collect();
                    let body = d.files.remove(p[0]).unwrap();
                    d.files.insert(p[1].to_string(), body);
                    Ok(String::new())
                }
                ["shell", "pm list packages -3"] => {
                    Ok("package:com.whatsapp\npackage:org.telegram.messenger\n".into())
                }
                ["shell", cmd] if cmd.starts_with("pm list packages com.android.vending") => {
                    Ok("package:com.android.vending\n".into())
                }
                ["shell", cmd] if cmd.starts_with("monkey") => Ok("Events injected: 1".into()),
                ["pull", from, to] => {
                    let body = d.files.get(*from).cloned().ok_or("no file")?;
                    std::fs::write(to, body).map_err(|e| e.to_string())?;
                    Ok(String::new())
                }
                ["push", from, to] => {
                    let body = std::fs::read_to_string(from).map_err(|e| e.to_string())?;
                    d.files.insert(to.to_string(), body);
                    Ok(String::new())
                }
                _ => Ok(String::new()),
            }
        }
    }

    fn emu() -> (Emulator, Rc<RefCell<Device>>) {
        let dev = Rc::new(RefCell::new(Device {
            state: "running".into(),
            ..Default::default()
        }));
        let e = Emulator {
            runner: Box::new(Fake(dev.clone())),
            sdk: PathBuf::from("/sdk"),
            avd: "AgenticPhone".into(),
            serial: "emulator-5554".into(),
            window: true,
        };
        (e, dev)
    }

    #[test]
    fn apps_have_names_and_open_pause_and_resume_go_through_adb() {
        let (mut e, dev) = emu();
        let apps = e.apps().unwrap();
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["Play Store", "WhatsApp", "Telegram"]);
        assert!(e.has_play().unwrap());
        e.open("com.whatsapp").unwrap();
        assert!(dev
            .borrow()
            .log
            .iter()
            .any(|l| l.contains("monkey -p com.whatsapp")));
        e.pause().unwrap();
        assert!(e.paused());
        e.resume().unwrap();
        assert!(!e.paused());
        assert_eq!(app_name("com.example.notes"), "Notes");
    }

    #[test]
    fn the_spool_is_mirrored_both_ways_and_nothing_twice() {
        let (mut e, dev) = emu();
        let local = std::env::temp_dir().join(format!("emu-spool-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&local);
        {
            let mut d = dev.borrow_mut();
            d.files.insert(
                format!("{DEVICE_SPOOL}/out/1-1.json"),
                r#"{"id":"1-1"}"#.into(),
            );
            d.files
                .insert(format!("{DEVICE_SPOOL}/out/1-2.json.tmp"), "half".into());
            d.files
                .insert(format!("{DEVICE_SPOOL}/files/in-photo.jpg"), "jpeg".into());
        }
        std::fs::create_dir_all(local.join("in")).unwrap();
        std::fs::create_dir_all(local.join("files")).unwrap();
        std::fs::write(local.join("in").join("c1.json"), r#"{"id":"c1"}"#).unwrap();
        std::fs::write(local.join("files").join("out-invoice.pdf"), "pdf").unwrap();
        let (down, up) = e.sync_bridge(&local).unwrap();
        assert_eq!((down, up), (2, 2));
        assert!(local.join("out/1-1.json").exists());
        assert_eq!(
            std::fs::read_to_string(local.join("files/in-photo.jpg")).unwrap(),
            "jpeg"
        );
        {
            let d = dev.borrow();
            // the event left the device; the half-written one stayed
            assert!(!d
                .files
                .contains_key(&format!("{DEVICE_SPOOL}/out/1-1.json")));
            assert!(d
                .files
                .contains_key(&format!("{DEVICE_SPOOL}/out/1-2.json.tmp")));
            assert!(d.files.contains_key(&format!("{DEVICE_SPOOL}/in/c1.json")));
            assert!(d
                .files
                .contains_key(&format!("{DEVICE_SPOOL}/files/out-invoice.pdf")));
        }
        assert!(!local.join("in/c1.json").exists());
        // a second sync moves nothing
        assert_eq!(e.sync_bridge(&local).unwrap(), (0, 0));
        // a paused phone is not synced
        e.pause().unwrap();
        dev.borrow_mut()
            .files
            .insert(format!("{DEVICE_SPOOL}/out/2-1.json"), "{}".into());
        assert_eq!(e.sync_bridge(&local).unwrap(), (0, 0));
        let _ = std::fs::remove_dir_all(&local);
    }
}
