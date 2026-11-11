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

/// Parse `waydroid status`: the session is up when its line says RUNNING.
pub fn session_running(text: &str) -> bool {
    text.lines()
        .any(|l| l.starts_with("Session:") && l.contains("RUNNING"))
}

impl Sandbox for Waydroid {
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

/// A sandbox in memory, for tests and for `android = "sim"`.
#[derive(Default)]
pub struct SimSandbox {
    pub installed: Vec<String>,
    pub open: Vec<String>,
    pub stops: u32,
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
        self.stops += 1;
        Ok(())
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
}

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
        ]
    }
    fn call(&mut self, a: &Action) -> Result<String, String> {
        let mut sb = self.sandbox.borrow_mut();
        match a.tool.as_str() {
            "android_status" => {
                let available = sb.available();
                let running = sb.running();
                let n = if running { sb.apps()?.len() } else { 0 };
                Ok(format!(
                    "Android sandbox: {}, {}, {n} apps",
                    if available {
                        "available"
                    } else {
                        "not installed"
                    },
                    if running { "running" } else { "not running" }
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
