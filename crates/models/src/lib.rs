//! The model supervisor: keeps llama.cpp's `llama-server` running for each local brain the
//! current stage allows, restarts it if it dies, and gives up on one that keeps dying.
//! It also downloads model files, checking their SHA-256.
//!
//! agentd runs the servers itself (rather than through the init system) because it decides when a
//! model is loaded (stage and battery) and puts each server in the model's cgroup.
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Small,
    Large,
}

fn default_ctx() -> u32 {
    4096
}
fn default_gpu_layers() -> u32 {
    99
}

/// One model, as written in the config file under `[[models]]`.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelSpec {
    pub name: String,
    pub role: Role,
    /// GGUF file, relative to the models directory unless absolute.
    pub file: String,
    /// Where `agentd models pull` downloads it from.
    #[serde(default)]
    pub url: Option<String>,
    /// Expected SHA-256 of the file, in hex. Without it the download is accepted and its hash printed.
    #[serde(default)]
    pub sha256: Option<String>,
    pub port: u16,
    #[serde(default = "default_ctx")]
    pub ctx: u32,
    /// Layers to put on the GPU (Vulkan). 99 means all; with no usable GPU they stay on the CPU.
    #[serde(default = "default_gpu_layers")]
    pub gpu_layers: u32,
    #[serde(default)]
    pub extra_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Stopped,
    /// Running; `ready` once /health answers.
    Running {
        pid: u32,
        ready: bool,
    },
    /// The file is not there yet.
    Missing,
    /// Died too often, or could not start at all.
    Failed(String),
}

struct Proc {
    child: Child,
    ready: bool,
}

/// At most this many crashes in `CRASH_WINDOW` before a model is marked failed.
pub const MAX_CRASHES: usize = 5;
pub const CRASH_WINDOW: Duration = Duration::from_secs(600);

pub struct Supervisor {
    pub bin: PathBuf,
    pub dir: PathBuf,
    pub log_dir: PathBuf,
    pub specs: Vec<ModelSpec>,
    pub threads: usize,
    wanted: BTreeSet<String>,
    procs: BTreeMap<String, Proc>,
    crashes: BTreeMap<String, Vec<Instant>>,
    failed: BTreeMap<String, String>,
}

impl Supervisor {
    pub fn new(
        bin: PathBuf,
        dir: PathBuf,
        log_dir: PathBuf,
        specs: Vec<ModelSpec>,
        threads: usize,
    ) -> Self {
        Supervisor {
            bin,
            dir,
            log_dir,
            specs,
            threads: threads.max(1),
            wanted: BTreeSet::new(),
            procs: BTreeMap::new(),
            crashes: BTreeMap::new(),
            failed: BTreeMap::new(),
        }
    }

    pub fn path(&self, s: &ModelSpec) -> PathBuf {
        let p = Path::new(&s.file);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.dir.join(p)
        }
    }

    pub fn spec(&self, role: Role) -> Option<&ModelSpec> {
        self.specs.iter().find(|s| s.role == role)
    }

    /// The URL the engine should use for a brain.
    pub fn url(&self, role: Role) -> Option<String> {
        self.spec(role)
            .map(|s| format!("http://127.0.0.1:{}", s.port))
    }

    /// The exact command line for one model.
    pub fn args(&self, s: &ModelSpec) -> Vec<String> {
        let mut a: Vec<String> = vec![
            "-m".into(),
            self.path(s).display().to_string(),
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            s.port.to_string(),
            "-c".into(),
            s.ctx.to_string(),
            "-t".into(),
            self.threads.to_string(),
            "-ngl".into(),
            s.gpu_layers.to_string(),
            // keep the weights in RAM: a model paged out to flash answers in seconds, not ms
            "--mlock".into(),
        ];
        a.extend(s.extra_args.iter().cloned());
        a
    }

    pub fn state(&self, name: &str) -> State {
        if let Some(why) = self.failed.get(name) {
            return State::Failed(why.clone());
        }
        if let Some(p) = self.procs.get(name) {
            return State::Running {
                pid: p.child.id(),
                ready: p.ready,
            };
        }
        match self.specs.iter().find(|s| s.name == name) {
            Some(s) if !self.path(s).exists() => State::Missing,
            _ => State::Stopped,
        }
    }

    fn spawn(&mut self, s: &ModelSpec) -> Result<u32, String> {
        if !self.path(s).exists() {
            return Err(format!(
                "{} is missing: run `agentd models pull {}`",
                s.file, s.name
            ));
        }
        std::fs::create_dir_all(&self.log_dir).map_err(|e| e.to_string())?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_dir.join(format!("llama-{}.log", s.name)))
            .map_err(|e| e.to_string())?;
        let mut cmd = Command::new(&self.bin);
        #[cfg(target_os = "linux")]
        {
            // If agentd dies, even by SIGKILL, the kernel kills the model server too, so it never
            // sits on its port and blocks the next start.
            use std::os::unix::process::CommandExt;
            unsafe {
                cmd.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                    Ok(())
                });
            }
        }
        let child = cmd
            .args(self.args(s))
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log)
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", self.bin.display()))?;
        let pid = child.id();
        self.procs.insert(
            s.name.clone(),
            Proc {
                child,
                ready: false,
            },
        );
        Ok(pid)
    }

    fn stop(&mut self, name: &str) {
        if let Some(mut p) = self.procs.remove(name) {
            let _ = p.child.kill();
            let _ = p.child.wait();
        }
    }

    /// Run exactly the models for these roles. Returns (name, pid) of every server started,
    /// so the caller can move it into the model's cgroup.
    pub fn want(&mut self, roles: &[Role]) -> Vec<(String, u32)> {
        let wanted: BTreeSet<String> = self
            .specs
            .iter()
            .filter(|s| roles.contains(&s.role))
            .map(|s| s.name.clone())
            .collect();
        let running: Vec<String> = self.procs.keys().cloned().collect();
        for name in running {
            if !wanted.contains(&name) {
                self.stop(&name);
            }
        }
        self.wanted = wanted;
        self.start_missing()
    }

    fn start_missing(&mut self) -> Vec<(String, u32)> {
        let mut started = Vec::new();
        let todo: Vec<ModelSpec> = self
            .specs
            .iter()
            .filter(|s| self.wanted.contains(&s.name))
            .filter(|s| !self.procs.contains_key(&s.name) && !self.failed.contains_key(&s.name))
            .filter(|s| self.path(s).exists())
            .cloned()
            .collect();
        for s in todo {
            match self.spawn(&s) {
                Ok(pid) => started.push((s.name.clone(), pid)),
                Err(e) => {
                    self.failed.insert(s.name.clone(), e);
                }
            }
        }
        started
    }

    /// Call about once a second: notice dead servers and restart them, within limits.
    pub fn check(&mut self) -> Vec<(String, u32)> {
        let now = Instant::now();
        let mut dead = Vec::new();
        for (name, p) in self.procs.iter_mut() {
            if let Ok(Some(status)) = p.child.try_wait() {
                dead.push((name.clone(), status.to_string()));
            }
        }
        for (name, status) in dead {
            self.procs.remove(&name);
            let c = self.crashes.entry(name.clone()).or_default();
            c.retain(|t| now.duration_since(*t) < CRASH_WINDOW);
            c.push(now);
            if c.len() >= MAX_CRASHES {
                self.failed.insert(
                    name.clone(),
                    format!(
                        "exited {} times in 10 minutes (last: {status}); see llama-{name}.log",
                        c.len()
                    ),
                );
            }
        }
        // a model file that has just been pulled gets started here too
        self.start_missing()
    }

    /// Forget failures, for example after a new model file or binary is installed.
    pub fn reset(&mut self) {
        self.failed.clear();
        self.crashes.clear();
    }

    /// Ask each running server whether it has finished loading.
    pub fn refresh_health(&mut self) {
        let ports: Vec<(String, u16)> = self
            .specs
            .iter()
            .filter(|s| self.procs.contains_key(&s.name))
            .map(|s| (s.name.clone(), s.port))
            .collect();
        for (name, port) in ports {
            let ok = healthy(port);
            if let Some(p) = self.procs.get_mut(&name) {
                p.ready = ok;
            }
        }
    }

    /// Wait for the model of this role to answer /health, up to `timeout`.
    pub fn wait_ready(&mut self, role: Role, timeout: Duration) -> bool {
        let Some(s) = self.spec(role).cloned() else {
            return false;
        };
        let until = Instant::now() + timeout;
        loop {
            if !self.procs.contains_key(&s.name) {
                return false;
            }
            if healthy(s.port) {
                if let Some(p) = self.procs.get_mut(&s.name) {
                    p.ready = true;
                }
                return true;
            }
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Bytes of model weights currently loaded, to protect from reclaim.
    pub fn loaded_bytes(&self) -> u64 {
        self.specs
            .iter()
            .filter(|s| self.procs.contains_key(&s.name))
            .filter_map(|s| std::fs::metadata(self.path(s)).ok())
            .map(|m| m.len())
            .sum()
    }

    pub fn report(&self) -> String {
        if self.specs.is_empty() {
            return "no models configured".into();
        }
        self.specs
            .iter()
            .map(|s| {
                let st = match self.state(&s.name) {
                    State::Stopped => "stopped".to_string(),
                    State::Running { pid, ready } => {
                        format!(
                            "running pid {pid}, {}",
                            if ready { "ready" } else { "loading" }
                        )
                    }
                    State::Missing => format!("missing: agentd models pull {}", s.name),
                    State::Failed(why) => format!("FAILED: {why}"),
                };
                format!("{} ({:?}, port {}): {st}", s.name, s.role, s.port).to_lowercase()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        let names: Vec<String> = self.procs.keys().cloned().collect();
        for n in names {
            self.stop(&n);
        }
    }
}

/// True if llama-server on this port says it has loaded its model.
pub fn healthy(port: u16) -> bool {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(500))
        .build()
        .get(&format!("http://127.0.0.1:{port}/health"))
        .call()
        .map(|r| r.status() == 200)
        .unwrap_or(false)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Download a model to `dest`, through a `.part` file, checking SHA-256 if the spec has one.
/// `progress(done, total)` is called as bytes arrive. Returns the file's SHA-256.
pub fn pull(
    s: &ModelSpec,
    dest: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<String, String> {
    let url = s
        .url
        .as_deref()
        .ok_or_else(|| format!("{} has no url", s.name))?;
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let resp = ureq::get(url).call().map_err(|e| format!("{url}: {e}"))?;
    let total = resp.header("Content-Length").and_then(|v| v.parse().ok());
    let part = dest.with_extension("part");
    let mut out = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut body = resp.into_reader();
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut done = 0u64;
    loop {
        let n = body.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        progress(done, total);
    }
    out.sync_all().map_err(|e| e.to_string())?;
    drop(out);
    let got = hex(&hasher.finalize());
    if let Some(want) = &s.sha256 {
        if !want.eq_ignore_ascii_case(&got) {
            let _ = std::fs::remove_file(&part);
            return Err(format!(
                "checksum mismatch for {}: expected {want}, got {got}",
                s.name
            ));
        }
    }
    if total.is_some_and(|t| t != done) {
        let _ = std::fs::remove_file(&part);
        return Err(format!(
            "download cut short: {done} of {} bytes",
            total.unwrap()
        ));
    }
    std::fs::rename(&part, dest).map_err(|e| e.to_string())?;
    Ok(got)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("models-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn spec(name: &str, role: Role, port: u16) -> ModelSpec {
        ModelSpec {
            name: name.into(),
            role,
            file: format!("{name}.gguf"),
            url: None,
            sha256: None,
            port,
            ctx: 2048,
            gpu_layers: 99,
            extra_args: vec![],
        }
    }
    /// A stand-in for llama-server: a shell script.
    fn fake_bin(d: &Path, body: &str) -> PathBuf {
        let p = d.join("llama-server");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }
    /// Answer HTTP requests with `status` and `body` on a free port, `n` times.
    fn serve(status: &'static str, body: Vec<u8>, n: usize) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for c in l.incoming().take(n).flatten() {
                let mut c = c;
                let mut req = [0u8; 1024];
                let _ = c.read(&mut req);
                let _ = write!(
                    c,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = c.write_all(&body);
            }
        });
        port
    }

    #[test]
    fn the_command_line_pins_loopback_threads_gpu_and_mlock() {
        let d = dir("args");
        let sup = Supervisor::new("llama-server".into(), d.clone(), d.clone(), vec![], 4);
        let mut s = spec("small", Role::Small, 8080);
        s.extra_args = vec!["--flash-attn".into(), "on".into()];
        let a = sup.args(&s).join(" ");
        assert_eq!(
            a,
            format!("-m {}/small.gguf --host 127.0.0.1 --port 8080 -c 2048 -t 4 -ngl 99 --mlock --flash-attn on", d.display())
        );
        assert_eq!(sup.url(Role::Small), None);
    }
    #[test]
    fn config_toml_parses_with_defaults() {
        #[derive(Deserialize)]
        struct W {
            models: Vec<ModelSpec>,
        }
        let w: W = toml::from_str(
            "[[models]]\nname = \"small\"\nrole = \"small\"\nfile = \"q.gguf\"\nport = 8080\n",
        )
        .unwrap();
        assert_eq!(w.models[0].role, Role::Small);
        assert_eq!(w.models[0].ctx, 4096);
        assert_eq!(w.models[0].gpu_layers, 99);
        assert!(w.models[0].sha256.is_none());
        assert!(
            toml::from_str::<W>("[[models]]\nname=\"x\"\nrole=\"huge\"\nfile=\"f\"\nport=1")
                .is_err()
        );
    }
    #[test]
    fn a_missing_file_is_not_started_and_says_how_to_fix_it() {
        let d = dir("missing");
        let bin = fake_bin(&d, "exec sleep 30");
        let mut sup = Supervisor::new(
            bin,
            d.clone(),
            d.clone(),
            vec![spec("small", Role::Small, 1)],
            2,
        );
        assert!(sup.want(&[Role::Small]).is_empty());
        assert_eq!(sup.state("small"), State::Missing);
        assert!(sup.report().contains("agentd models pull small"));
        // the file arrives: the next check starts it
        std::fs::write(d.join("small.gguf"), b"x").unwrap();
        assert_eq!(sup.check().len(), 1);
        assert!(matches!(sup.state("small"), State::Running { .. }));
    }
    #[test]
    fn stages_start_and_stop_servers_by_role() {
        let d = dir("roles");
        let bin = fake_bin(
            &d,
            "echo \"$@\" > \"$(dirname \"$0\")/args-$6\"; exec sleep 30",
        );
        for n in ["small", "large"] {
            std::fs::write(d.join(format!("{n}.gguf")), vec![0u8; 1000]).unwrap();
        }
        let specs = vec![
            spec("small", Role::Small, 18080),
            spec("large", Role::Large, 18081),
        ];
        let mut sup = Supervisor::new(bin, d.clone(), d.join("log"), specs, 3);
        let started = sup.want(&[Role::Small]);
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].0, "small");
        assert_eq!(sup.loaded_bytes(), 1000);
        let started = sup.want(&[Role::Small, Role::Large]);
        assert_eq!(
            started.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(),
            vec!["large"]
        );
        assert_eq!(sup.loaded_bytes(), 2000);
        let file = d.join("args-18081");
        for _ in 0..100 {
            if std::fs::metadata(&file).is_ok_and(|m| m.len() > 0) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let args = std::fs::read_to_string(&file).unwrap();
        assert!(
            args.contains("-t 3") && args.contains("large.gguf"),
            "{args}"
        );
        assert!(sup.want(&[]).is_empty());
        assert_eq!(sup.state("small"), State::Stopped);
        assert_eq!(sup.loaded_bytes(), 0);
    }
    #[test]
    fn a_server_that_keeps_dying_is_restarted_then_given_up_on() {
        let d = dir("crash");
        let bin = fake_bin(&d, "exit 3");
        std::fs::write(d.join("small.gguf"), b"x").unwrap();
        let mut sup = Supervisor::new(
            bin,
            d.clone(),
            d.clone(),
            vec![spec("small", Role::Small, 1)],
            1,
        );
        sup.want(&[Role::Small]);
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(30));
            sup.check();
            if matches!(sup.state("small"), State::Failed(_)) {
                break;
            }
        }
        assert!(matches!(sup.state("small"), State::Failed(w) if w.contains("5 times")));
        sup.reset();
        assert_eq!(sup.check().len(), 1, "reset allows another try");
    }
    #[test]
    fn a_missing_binary_fails_once_instead_of_looping() {
        let d = dir("nobin");
        std::fs::write(d.join("small.gguf"), b"x").unwrap();
        let mut sup = Supervisor::new(
            d.join("nope"),
            d.clone(),
            d.clone(),
            vec![spec("small", Role::Small, 1)],
            1,
        );
        assert!(sup.want(&[Role::Small]).is_empty());
        assert!(matches!(sup.state("small"), State::Failed(w) if w.contains("cannot start")));
        assert!(sup.check().is_empty());
    }
    #[test]
    fn health_is_read_from_the_server() {
        let ok = serve("200 OK", b"{\"status\":\"ok\"}".to_vec(), 1);
        let loading = serve("503 Service Unavailable", b"{}".to_vec(), 1);
        assert!(healthy(ok));
        assert!(!healthy(loading));
    }
    #[test]
    fn pull_checks_the_hash_and_never_leaves_a_bad_file() {
        let d = dir("pull");
        let data = b"GGUF fake weights".to_vec();
        let sum = hex(&Sha256::digest(&data));
        let mut s = spec("small", Role::Small, 1);
        s.url = Some(format!(
            "http://127.0.0.1:{}/m.gguf",
            serve("200 OK", data.clone(), 1)
        ));
        s.sha256 = Some(sum.to_uppercase());
        let dest = d.join("small.gguf");
        let mut seen = 0;
        assert_eq!(pull(&s, &dest, &mut |n, _| seen = n).unwrap(), sum);
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(seen, data.len() as u64);

        let bad = d.join("bad.gguf");
        s.url = Some(format!(
            "http://127.0.0.1:{}/m.gguf",
            serve("200 OK", b"tampered".to_vec(), 1)
        ));
        let e = pull(&s, &bad, &mut |_, _| {}).unwrap_err();
        assert!(e.contains("checksum mismatch"), "{e}");
        assert!(!bad.exists() && !bad.with_extension("part").exists());
    }
}
