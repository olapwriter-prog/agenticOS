//! An Android sandbox: apps with no usable API run inside Waydroid, an Android container on the
//! same Linux kernel. agentd owns its lifecycle. Version 1 can open, install, list and close apps;
//! it cannot yet read or tap inside them.
//!
//! Waydroid belongs to the user who owns the Wayland display, so every command runs as that user:
//! `su -l <user> -c "<env> waydroid <args>"`.
pub mod governor;

use agentos_action::{Action, Tier, Tool};
use agentos_executor::Connector;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

/// Runs one program. The daemon uses the system; tests use a fake.
pub trait Runner {
    fn run(&mut self, prog: &str, args: &[&str]) -> Result<String, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub name: String,
    pub package: String,
}

pub trait Sandbox {
    /// installed and initialised
    fn available(&mut self) -> bool;
    /// a session is up
    fn running(&mut self) -> bool;
    fn apps(&mut self) -> Result<Vec<App>, String>;
    fn open(&mut self, package: &str) -> Result<(), String>;
    fn install(&mut self, apk_path: &str) -> Result<(), String>;
    /// closes every Android app and ends the session
    fn stop_all(&mut self) -> Result<(), String>;
    /// opens the app's store page, where the owner signs in and installs it by touch
    fn play_page(&mut self, _package: &str) -> Result<(), String> {
        Err("this sandbox cannot open the store page".into())
    }
    /// Show Android's own full screen (its launcher, its apps), drawn by Android itself.
    fn show(&mut self) -> Result<(), String> {
        Err("this sandbox has no screen to show".into())
    }
    /// Back to Agentic OS: the Android window goes away; Android keeps running until paused.
    fn hide(&mut self) -> Result<(), String> {
        Ok(())
    }
    /// Freeze the sandbox: its apps keep their state but use no CPU.
    fn pause(&mut self) -> Result<(), String> {
        Err("this sandbox cannot pause".into())
    }
    fn resume(&mut self) -> Result<(), String> {
        Err("this sandbox cannot pause".into())
    }
    fn paused(&mut self) -> bool {
        false
    }
    /// Download and initialise the Google Play system image.
    fn init_gapps(&mut self) -> Result<String, String> {
        Err("not supported by this sandbox".into())
    }
    /// The device's Google Services Framework id, which the owner registers with Google once.
    fn gsf_id(&mut self) -> Result<String, String> {
        Err("not supported by this sandbox".into())
    }
    /// Is the Play Store package present?
    fn has_play(&mut self) -> Result<bool, String> {
        Ok(self.apps()?.iter().any(|a| a.package == PLAY_PACKAGE))
    }
}

/// Waydroid through its command line.
pub struct Waydroid {
    pub runner: Box<dyn Runner>,
    /// The screen user, who owns the Wayland session.
    pub user: String,
    /// Environment for the command, for example "XDG_RUNTIME_DIR=/run/user/10000 WAYLAND_DISPLAY=wayland-1".
    pub env: String,
    /// Switch sway's workspace with the sandbox: 2 while an app is open, 1 when it is closed.
    pub sway: bool,
}

impl Waydroid {
    fn waydroid(&mut self, args: &str) -> Result<String, String> {
        let cmd = format!("{} waydroid {args}", self.env);
        let user = self.user.clone();
        self.runner.run("su", &["-l", &user, "-c", &cmd])
    }
}

/// Parse `waydroid app list`: one block per app, a Name: line and a packageName: line.
pub fn parse_app_list(text: &str) -> Vec<App> {
    let mut apps = Vec::new();
    let mut name: Option<String> = None;
    for line in text.lines() {
        if let Some(n) = line.strip_prefix("Name:") {
            name = Some(n.trim().to_string());
        } else if let Some(p) = line.strip_prefix("packageName:") {
            if let Some(n) = name.take() {
                apps.push(App {
                    name: n,
                    package: p.trim().to_string(),
                });
            }
        }
    }
    apps
}

/// Parse `waydroid status`: the container is paused when its line says FROZEN.
pub fn container_frozen(text: &str) -> bool {
    text.lines()
        .any(|l| l.starts_with("Container:") && l.contains("FROZEN"))
}

/// Parse `waydroid status`: the session is up when its line says RUNNING.
pub fn session_running(text: &str) -> bool {
    text.lines()
        .any(|l| l.starts_with("Session:") && l.contains("RUNNING"))
}

impl Sandbox for Waydroid {
    fn show(&mut self) -> Result<(), String> {
        // Android's own full UI, in its own window; detached, because it starts the session and
        // stays up (verify on the device)
        let cmd = format!(
            "{} setsid -f waydroid show-full-ui >/dev/null 2>&1",
            self.env
        );
        let user = self.user.clone();
        self.runner.run("su", &["-l", &user, "-c", &cmd])?;
        if self.sway {
            let _ = self.waydroid_sway("workspace 2");
        }
        Ok(())
    }
    fn hide(&mut self) -> Result<(), String> {
        if self.sway {
            let _ = self.waydroid_sway("workspace 1");
        }
        Ok(())
    }
    fn pause(&mut self) -> Result<(), String> {
        // the container belongs to root
        self.runner
            .run("waydroid", &["container", "freeze"])
            .map(|_| ())
    }
    fn resume(&mut self) -> Result<(), String> {
        self.runner
            .run("waydroid", &["container", "unfreeze"])
            .map(|_| ())
    }
    fn paused(&mut self) -> bool {
        self.waydroid("status")
            .map(|s| container_frozen(&s))
            .unwrap_or(false)
    }
    fn init_gapps(&mut self) -> Result<String, String> {
        // verify on the device: the image is chosen when the sandbox is initialised
        self.runner
            .run("waydroid", &["init", "-s", "GAPPS", "-f"])
            .map(|s| s.trim().to_string())
    }
    fn gsf_id(&mut self) -> Result<String, String> {
        let out = self
            .runner
            .run("waydroid", &["shell", "--", "sh", "-c", GSF_ID_CMD])?;
        let digits: String = out.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return Err(
                "Google services have not started yet; open the Play Store once and try again"
                    .into(),
            );
        }
        Ok(digits)
    }
    fn available(&mut self) -> bool {
        self.waydroid("status").is_ok()
    }
    fn running(&mut self) -> bool {
        self.waydroid("status")
            .map(|s| session_running(&s))
            .unwrap_or(false)
    }
    fn apps(&mut self) -> Result<Vec<App>, String> {
        Ok(parse_app_list(&self.waydroid("app list")?))
    }
    fn open(&mut self, package: &str) -> Result<(), String> {
        self.waydroid(&format!("app launch {package}"))?;
        if self.sway {
            // the Android window goes to workspace 2; a failure here is not a failure to open
            let _ = self.waydroid_sway("workspace 2");
        }
        Ok(())
    }
    fn install(&mut self, apk_path: &str) -> Result<(), String> {
        self.waydroid(&format!("app install {apk_path}"))
            .map(|_| ())
    }
    fn play_page(&mut self, package: &str) -> Result<(), String> {
        self.waydroid(&format!(
            "app intent android.intent.action.VIEW market://details?id={package}"
        ))
        .map(|_| ())
    }
    fn stop_all(&mut self) -> Result<(), String> {
        self.waydroid("session stop")?;
        if self.sway {
            let _ = self.waydroid_sway("workspace 1");
        }
        Ok(())
    }
}

impl Waydroid {
    /// swaymsg as the screen user, in the same session as Waydroid.
    fn waydroid_sway(&mut self, args: &str) -> Result<String, String> {
        let cmd = format!("{} swaymsg {args}", self.env);
        let user = self.user.clone();
        self.runner.run("su", &["-l", &user, "-c", &cmd])
    }
}

/// The Play Store's package name.
pub const PLAY_PACKAGE: &str = "com.android.vending";

/// The shell line that reads the device id from Google Services' database (run as root).
pub const GSF_ID_CMD: &str = "ANDROID_RUNTIME_ROOT=/apex/com.android.runtime ANDROID_DATA=/data ANDROID_TZDATA_ROOT=/apex/com.android.tzdata ANDROID_I18N_ROOT=/apex/com.android.i18n sqlite3 /data/data/com.google.android.gsf/databases/gservices.db \"select value from main where name = 'android_id';\"";

/// A sandbox in memory, for tests and for `android = "sim"`.
#[derive(Default)]
pub struct SimSandbox {
    pub installed: Vec<String>,
    pub open: Vec<String>,
    pub stops: u32,
    /// the Google Play image is installed
    pub gapps: bool,
    /// Android's own screen is showing
    pub shown: bool,
    pub frozen: bool,
}

impl Sandbox for SimSandbox {
    fn available(&mut self) -> bool {
        true
    }
    fn running(&mut self) -> bool {
        !self.open.is_empty()
    }
    fn apps(&mut self) -> Result<Vec<App>, String> {
        Ok(self
            .installed
            .iter()
            .map(|p| App {
                name: p.clone(),
                package: p.clone(),
            })
            .collect())
    }
    fn open(&mut self, package: &str) -> Result<(), String> {
        if !self.open.iter().any(|p| p == package) {
            self.open.push(package.to_string());
        }
        Ok(())
    }
    fn install(&mut self, apk_path: &str) -> Result<(), String> {
        self.installed.push(apk_path.to_string());
        Ok(())
    }
    fn stop_all(&mut self) -> Result<(), String> {
        self.open.clear();
        self.shown = false;
        self.frozen = false;
        self.stops += 1;
        Ok(())
    }
    fn play_page(&mut self, package: &str) -> Result<(), String> {
        self.installed.push(package.to_string());
        Ok(())
    }
    fn show(&mut self) -> Result<(), String> {
        self.frozen = false;
        self.shown = true;
        if !self.open.iter().any(|p| p == "android") {
            self.open.push("android".into());
        }
        Ok(())
    }
    fn hide(&mut self) -> Result<(), String> {
        self.shown = false;
        Ok(())
    }
    fn pause(&mut self) -> Result<(), String> {
        if self.open.is_empty() {
            return Err("Android is not running".into());
        }
        self.shown = false;
        self.frozen = true;
        Ok(())
    }
    fn resume(&mut self) -> Result<(), String> {
        self.frozen = false;
        Ok(())
    }
    fn paused(&mut self) -> bool {
        self.frozen
    }
    fn init_gapps(&mut self) -> Result<String, String> {
        self.gapps = true;
        if !self.installed.iter().any(|p| p == PLAY_PACKAGE) {
            self.installed.push(PLAY_PACKAGE.to_string());
        }
        Ok("sim: Google Play image ready".into())
    }
    fn gsf_id(&mut self) -> Result<String, String> {
        Ok("3712345678901234567".into())
    }
    fn has_play(&mut self) -> Result<bool, String> {
        Ok(self.gapps)
    }
}

pub type Shared = Rc<RefCell<Box<dyn Sandbox>>>;

/// The reason the sandbox may not be opened right now, set by the daemon's governor (None when
/// opening is allowed).
pub type Blocked = Rc<RefCell<Option<String>>>;

/// The sandbox's tools for the executor. `apks` is the folder installs must come from; `blocked`
/// is the governor's current word on opening apps.
pub struct AndroidTools {
    pub sandbox: Shared,
    pub apks: PathBuf,
    pub blocked: Blocked,
    pub activity: Activity,
}

/// What the daemon needs to know to pause Android when nothing needs it: a count of uses (each
/// tool that needs Android adds one), and whether Android's screen is showing.
#[derive(Default)]
pub struct ActivityState {
    pub uses: std::cell::Cell<u64>,
    pub showing: std::cell::Cell<bool>,
}
pub type Activity = Rc<ActivityState>;

/// Runs the programs on this machine.
pub struct System;

impl Runner for System {
    fn run(&mut self, prog: &str, args: &[&str]) -> Result<String, String> {
        let out = std::process::Command::new(prog)
            .args(args)
            .output()
            .map_err(|e| format!("{prog}: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
}

fn arg<'a>(a: &'a Action, name: &str) -> Result<&'a str, String> {
    a.args
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| format!("missing {name}"))
}

/// An install path must end in .apk and sit under the apks folder, with no `..` in it.
pub fn apk_allowed(path: &str, apks: &std::path::Path) -> bool {
    let p = std::path::Path::new(path);
    p.extension().is_some_and(|e| e == "apk")
        && p.starts_with(apks)
        && !p.components().any(|c| c == std::path::Component::ParentDir)
}

impl Connector for AndroidTools {
    fn name(&self) -> &str {
        "android"
    }
    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool {
                name: "android_status",
                args: &[],
                tier: Tier::Low,
            },
            Tool {
                name: "android_apps",
                args: &[],
                tier: Tier::Low,
            },
            Tool {
                name: "android_open",
                args: &["app"],
                tier: Tier::Medium,
            },
            Tool {
                name: "android_close_all",
                args: &[],
                tier: Tier::Low,
            },
            Tool {
                name: "android_install",
                args: &["apk"],
                tier: Tier::High,
            },
            Tool {
                name: "android_show",
                args: &[],
                tier: Tier::Low,
            },
            Tool {
                name: "android_hide",
                args: &[],
                tier: Tier::Low,
            },
            Tool {
                name: "android_pause",
                args: &[],
                tier: Tier::Low,
            },
            Tool {
                name: "android_resume",
                args: &[],
                tier: Tier::Low,
            },
            Tool {
                name: "android_play_setup",
                args: &[],
                tier: Tier::High,
            },
            Tool {
                name: "android_device_id",
                args: &[],
                tier: Tier::Low,
            },
            Tool {
                name: "android_play_status",
                args: &[],
                tier: Tier::Low,
            },
        ]
    }
    fn call(&mut self, a: &Action) -> Result<String, String> {
        let mut sb = self.sandbox.borrow_mut();
        let needs_android = !matches!(
            a.tool.as_str(),
            "android_status" | "android_pause" | "android_hide" | "android_close_all"
        );
        if needs_android {
            self.activity.uses.set(self.activity.uses.get() + 1);
            if sb.paused() {
                sb.resume()?;
            }
        }
        match a.tool.as_str() {
            "android_show" => {
                if let Some(reason) = self.blocked.borrow().clone() {
                    return Err(format!("not now: {reason}"));
                }
                sb.show()?;
                self.activity.showing.set(true);
                Ok("Android is open, full screen. Its own launcher and apps, as Android draws them.".into())
            }
            "android_hide" => {
                sb.hide()?;
                self.activity.showing.set(false);
                Ok("Back to Agentic OS. Android pauses when nothing needs it.".into())
            }
            "android_pause" => {
                sb.pause()?;
                self.activity.showing.set(false);
                Ok("Android paused: its apps keep their state and use no CPU.".into())
            }
            "android_resume" => Ok("Android resumed.".into()),
            "android_play_setup" => {
                let init = sb.init_gapps()?;
                let id = sb.gsf_id().ok();
                let mut ui = agentos_screen::Ui::new("Google Play is set up").kind("setup");
                ui = ui.card(
                    agentos_screen::Card::new("1. Image installed")
                        .subtitle(init)
                        .tone("ok"),
                );
                let mut two = agentos_screen::Card::new("2. Register this device with Google")
                    .subtitle("Open google.com/android/uncertified and enter the device id")
                    .tone("warn");
                if let Some(id) = &id {
                    two = two.line(format!("Device id: {id}"));
                }
                ui = ui.card(two);
                ui = ui.card(
                    agentos_screen::Card::new("3. Open the Play Store and sign in")
                        .subtitle("Sign-in can take minutes to hours after registering")
                        .button("Open Play Store", "open android app Play Store"),
                );
                Ok(ui.to_reply())
            }
            "android_device_id" => sb.gsf_id(),
            "android_play_status" => {
                let has = sb.has_play()?;
                let id = sb.gsf_id().unwrap_or_else(|_| "unknown".into());
                Ok(format!(
                    "Play Store: {}; device id: {id}",
                    if has { "installed" } else { "missing" }
                ))
            }
            "android_status" => {
                let available = sb.available();
                let running = sb.running();
                let paused = running && sb.paused();
                let n = if running { sb.apps()?.len() } else { 0 };
                Ok(format!(
                    "Android sandbox: {}, {}, {n} apps",
                    if available {
                        "available"
                    } else {
                        "not installed"
                    },
                    if paused {
                        "paused"
                    } else if running {
                        "running"
                    } else {
                        "not running"
                    }
                ))
            }
            "android_apps" => {
                let apps = sb.apps()?;
                if apps.is_empty() {
                    return Ok("no Android apps".into());
                }
                Ok(apps
                    .iter()
                    .map(|a| format!("{} ({})", a.name, a.package))
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            "android_open" => {
                if let Some(reason) = self.blocked.borrow().clone() {
                    return Err(format!("not now: {reason}"));
                }
                let want = arg(a, "app")?.to_lowercase();
                let apps = sb.apps()?;
                let hits: Vec<&App> = apps
                    .iter()
                    .filter(|x| x.name.to_lowercase() == want || x.package.to_lowercase() == want)
                    .collect();
                match hits.len() {
                    0 => Err(format!("no Android app called {want}")),
                    1 => {
                        sb.open(&hits[0].package)?;
                        Ok(format!("opened {}", hits[0].name))
                    }
                    _ => Err(format!(
                        "more than one app matches {want}: {}",
                        hits.iter()
                            .map(|x| x.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                }
            }
            "android_close_all" => {
                sb.stop_all()?;
                Ok("closed every Android app".into())
            }
            "android_install" => {
                let apk = arg(a, "apk")?;
                if !apk_allowed(apk, &self.apks) {
                    return Err(format!(
                        "refused: an app must be an .apk under {}",
                        self.apks.display()
                    ));
                }
                sb.install(apk)?;
                Ok(format!("installed {apk}"))
            }
            other => Err(format!("android cannot {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records every command line and answers from a table.
    #[derive(Default)]
    struct Fake {
        calls: Rc<RefCell<Vec<String>>>,
        answers: Vec<(&'static str, String)>,
    }
    impl Runner for Fake {
        fn run(&mut self, prog: &str, args: &[&str]) -> Result<String, String> {
            let line = format!("{prog} {}", args.join(" "));
            self.calls.borrow_mut().push(line.clone());
            let cmd = args.last().copied().unwrap_or("");
            match self.answers.iter().find(|(k, _)| cmd.contains(k)) {
                Some((_, a)) => Ok(a.clone()),
                None => Err("exit 1".into()),
            }
        }
    }

    const ENV: &str = "XDG_RUNTIME_DIR=/run/user/10000 WAYLAND_DISPLAY=wayland-1";

    fn waydroid(answers: Vec<(&'static str, String)>) -> (Waydroid, Rc<RefCell<Vec<String>>>) {
        let calls = Rc::new(RefCell::new(vec![]));
        let fake = Fake {
            calls: calls.clone(),
            answers,
        };
        (
            Waydroid {
                runner: Box::new(fake),
                user: "user".into(),
                env: ENV.into(),
                sway: false,
            },
            calls,
        )
    }

    #[test]
    fn each_method_runs_the_exact_command_as_the_screen_user() {
        let (mut w, calls) = waydroid(vec![
            ("status", "Session:\tRUNNING\n".into()),
            ("app list", "".into()),
            ("app launch", "".into()),
            ("app install", "".into()),
            ("session stop", "".into()),
        ]);
        w.available();
        w.running();
        w.apps().unwrap();
        w.open("org.example.notes").unwrap();
        w.install("/data/apks/a.apk").unwrap();
        w.stop_all().unwrap();
        let want = [
            format!("su -l user -c {ENV} waydroid status"),
            format!("su -l user -c {ENV} waydroid status"),
            format!("su -l user -c {ENV} waydroid app list"),
            format!("su -l user -c {ENV} waydroid app launch org.example.notes"),
            format!("su -l user -c {ENV} waydroid app install /data/apks/a.apk"),
            format!("su -l user -c {ENV} waydroid session stop"),
        ];
        let got: Vec<String> = calls
            .borrow()
            .iter()
            .map(|c| c.replace(&format!("su -l user -c {ENV}"), "su -l user -c ENV"))
            .collect();
        let want: Vec<String> = want
            .iter()
            .map(|c| c.replace(&format!("su -l user -c {ENV}"), "su -l user -c ENV"))
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn with_sway_the_window_moves_to_workspace_2_and_back() {
        let calls = Rc::new(RefCell::new(vec![]));
        let fake = Fake {
            calls: calls.clone(),
            answers: vec![
                ("app launch", "".into()),
                ("session stop", "".into()),
                ("swaymsg", "".into()),
            ],
        };
        let mut w = Waydroid {
            runner: Box::new(fake),
            user: "user".into(),
            env: ENV.into(),
            sway: true,
        };
        w.open("org.example.notes").unwrap();
        w.stop_all().unwrap();
        let got = calls.borrow().clone();
        assert!(
            got.iter().any(|c| c.ends_with("swaymsg workspace 2")),
            "{got:?}"
        );
        assert!(
            got.iter().any(|c| c.ends_with("swaymsg workspace 1")),
            "{got:?}"
        );
    }

    #[test]
    fn the_placeholder_fixtures_parse() {
        let status = include_str!("../tests/fixtures/placeholder_status.txt");
        assert!(session_running(status));
        assert!(!session_running("Session:\tSTOPPED\n"));
        let apps = parse_app_list(include_str!("../tests/fixtures/placeholder_app_list.txt"));
        assert_eq!(
            apps,
            vec![
                App {
                    name: "Placeholder Notes".into(),
                    package: "org.example.notes".into()
                },
                App {
                    name: "Placeholder Maps".into(),
                    package: "org.example.maps".into()
                },
            ]
        );
    }

    #[test]
    fn open_then_stop_all_leaves_nothing_open_and_counts_one_stop() {
        let mut sim = SimSandbox {
            installed: vec!["org.example.notes".into()],
            ..SimSandbox::default()
        };
        sim.open("org.example.notes").unwrap();
        assert!(sim.running());
        sim.stop_all().unwrap();
        assert!(sim.open.is_empty());
        assert!(!sim.running());
        assert_eq!(sim.stops, 1);
    }

    #[test]
    fn an_install_must_be_an_apk_under_the_apks_folder() {
        let apks = std::path::Path::new("/var/lib/agentd/apks");
        assert!(apk_allowed("/var/lib/agentd/apks/a.apk", apks));
        assert!(!apk_allowed("/var/lib/agentd/apks/a.zip", apks));
        assert!(!apk_allowed("/etc/a.apk", apks));
        assert!(!apk_allowed("/var/lib/agentd/apks/../../etc/a.apk", apks));
    }
}

#[cfg(test)]
mod play_tests {
    use super::*;

    struct Answer {
        calls: Rc<RefCell<Vec<String>>>,
        out: String,
    }
    impl Runner for Answer {
        fn run(&mut self, prog: &str, args: &[&str]) -> Result<String, String> {
            self.calls
                .borrow_mut()
                .push(format!("{prog} {}", args.join(" ")));
            Ok(self.out.clone())
        }
    }

    fn waydroid(out: &str) -> (Waydroid, Rc<RefCell<Vec<String>>>) {
        let calls = Rc::new(RefCell::new(vec![]));
        let w = Waydroid {
            runner: Box::new(Answer {
                calls: calls.clone(),
                out: out.into(),
            }),
            user: "user".into(),
            env: String::new(),
            sway: false,
        };
        (w, calls)
    }

    #[test]
    fn the_device_id_is_read_as_root_and_trimmed() {
        let (mut w, calls) = waydroid("  3712345678901234567\n");
        assert_eq!(w.gsf_id().unwrap(), "3712345678901234567");
        let line = calls.borrow()[0].clone();
        assert_eq!(line, format!("waydroid shell -- sh -c {GSF_ID_CMD}"));
    }

    #[test]
    fn an_empty_device_id_says_to_open_the_store_first() {
        let (mut w, _) = waydroid("");
        let err = w.gsf_id().unwrap_err();
        assert!(err.contains("open the Play Store once"), "{err}");
    }

    #[test]
    fn the_image_init_runs_as_root_with_the_gapps_flavour() {
        let (mut w, calls) = waydroid("ok");
        w.init_gapps().unwrap();
        assert_eq!(calls.borrow()[0], "waydroid init -s GAPPS -f");
    }

    #[test]
    fn setup_in_the_simulator_leaves_the_store_present() {
        let mut sb = SimSandbox::default();
        assert_eq!(sb.has_play(), Ok(false));
        let mut tools = AndroidTools {
            sandbox: Rc::new(RefCell::new(Box::new(SimSandbox::default()))),
            apks: PathBuf::from("/tmp"),
            blocked: Rc::new(RefCell::new(None)),
            activity: Activity::default(),
        };
        let a = Action {
            tool: "android_play_setup".into(),
            args: vec![],
        };
        let reply = tools.call(&a).unwrap();
        assert!(reply.contains("Google Play is set up"), "{reply}");
        assert_eq!(tools.sandbox.borrow_mut().has_play(), Ok(true));
        let st = Action {
            tool: "android_play_status".into(),
            args: vec![],
        };
        assert!(tools
            .call(&st)
            .unwrap()
            .starts_with("Play Store: installed"));
    }

    #[test]
    fn waydroid_shows_its_own_ui_and_freezes_as_root() {
        let (mut w, calls) = waydroid("Session:\tRUNNING\nContainer:\tFROZEN\n");
        w.sway = true;
        w.show().unwrap();
        w.pause().unwrap();
        w.resume().unwrap();
        assert!(w.paused());
        let c = calls.borrow();
        assert!(c[0].contains("setsid -f waydroid show-full-ui"), "{}", c[0]);
        assert!(c[1].contains("swaymsg workspace 2"), "{}", c[1]);
        assert_eq!(c[2], "waydroid container freeze");
        assert_eq!(c[3], "waydroid container unfreeze");
        assert!(container_frozen("Container:\tFROZEN"));
        assert!(!container_frozen("Container:\tRUNNING"));
    }

    #[test]
    fn a_tool_that_needs_android_wakes_it_and_counts_a_use() {
        let sb: Shared = Rc::new(RefCell::new(Box::new(SimSandbox::default())));
        let activity = Activity::default();
        let mut tools = AndroidTools {
            sandbox: sb.clone(),
            apks: PathBuf::from("/tmp"),
            blocked: Rc::new(RefCell::new(None)),
            activity: activity.clone(),
        };
        let call = |t: &mut AndroidTools, tool: &str| {
            t.call(&Action {
                tool: tool.into(),
                args: vec![],
            })
        };
        call(&mut tools, "android_show").unwrap();
        assert!(activity.showing.get());
        call(&mut tools, "android_pause").unwrap();
        assert!(sb.borrow_mut().paused());
        assert!(!activity.showing.get());
        let before = activity.uses.get();
        // status does not need Android: it neither wakes it nor counts
        call(&mut tools, "android_status").unwrap();
        assert!(sb.borrow_mut().paused());
        assert_eq!(activity.uses.get(), before);
        call(&mut tools, "android_apps").unwrap();
        assert!(
            !sb.borrow_mut().paused(),
            "a tool that needs Android wakes it"
        );
        assert_eq!(activity.uses.get(), before + 1);
    }
}
