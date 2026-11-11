//! Threads around the core: the MCP HTTP listener and the control socket. They never touch the
//! core directly; they send it commands and wait for the answer.
use crate::config::Config;
use crate::core::{BootError, Core};
use crate::secrets::Secrets;
use agentos_gateway::http::{serve_one, HttpConfig};
use agentos_hal::Clock;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// Set by SIGTERM or SIGINT. The core loop sees it within a second and shuts down cleanly,
/// which stops the model servers it started.
pub static STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Call once from `main` before `start`.
pub fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
    }
}

enum Cmd {
    Mcp(String, Sender<Option<String>>),
    Ctl(String, Sender<String>),
    Stop,
}

pub struct Running {
    pub http_addr: String,
    pub ctl_path: PathBuf,
    tx: Sender<Cmd>,
    join: Option<JoinHandle<()>>,
}

impl Running {
    pub fn stop(mut self) {
        let _ = self.tx.send(Cmd::Stop);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
    /// Block until the core thread ends.
    pub fn wait(mut self) {
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Start the daemon. The core is built inside its own thread because it is not `Send`.
pub fn start(cfg: Config, secrets: Secrets, clock: Arc<dyn Clock>) -> Result<Running, BootError> {
    let (tx, rx) = channel::<Cmd>();
    let (ready_tx, ready_rx) = channel::<Result<(), String>>();
    let ctl_path = cfg.control_path();
    let listen = cfg.listen.clone();
    let token = secrets.gateway_token.clone();
    let origins = cfg.allowed_origins.clone();
    let ctl_path2 = ctl_path.clone();
    let watch = cfg
        .dbus_monitor
        .clone()
        .filter(|_| cfg.phone == "modemmanager");
    let key = cfg
        .approval_key
        .clone()
        .map(|d| (d, cfg.approval_keycode, cfg.approval_hold_ms));

    let listener = TcpListener::bind(&listen)
        .map_err(|e| BootError::Other(format!("cannot listen on {listen}: {e}")))?;
    let http_addr = listener
        .local_addr()
        .map_err(|e| BootError::Other(e.to_string()))?
        .to_string();

    let tx_core = tx.clone();
    let join = std::thread::spawn(move || {
        let mut core = match Core::new(cfg, secrets, clock) {
            Ok(c) => c,
            Err(e) => {
                let _ = ready_tx.send(Err(e.to_string()));
                return;
            }
        };
        // listeners
        let _ = std::fs::remove_file(&ctl_path2);
        let ul = match UnixListener::bind(&ctl_path2) {
            Ok(ul) => ul,
            Err(e) => {
                let _ = ready_tx.send(Err(format!(
                    "cannot open control socket {}: {e}",
                    ctl_path2.display()
                )));
                return;
            }
        };
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&ctl_path2, std::fs::Permissions::from_mode(0o600));
        }
        let txc = tx_core.clone();
        std::thread::spawn(move || {
            for conn in ul.incoming().flatten() {
                let txc = txc.clone();
                std::thread::spawn(move || {
                    let mut w = conn.try_clone().unwrap();
                    for line in BufReader::new(conn).lines().map_while(Result::ok) {
                        let (rt, rr) = channel();
                        if txc.send(Cmd::Ctl(line, rt)).is_err() {
                            break;
                        }
                        let Ok(ans) = rr.recv() else { break };
                        if writeln!(w, "{}", ans.replace('\n', "\u{1}")).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        if let Some(program) = watch {
            let txw = tx_core.clone();
            agentos_phone::watch::spawn(program, move |sig| {
                use agentos_phone::watch::Signal;
                let what = match sig {
                    Signal::Up => "up",
                    Signal::Down => "down",
                    Signal::Wake(w) => w,
                };
                let (rt, rr) = channel();
                if txw
                    .send(Cmd::Ctl(format!("phone-signal {what}"), rt))
                    .is_ok()
                {
                    // what arrived, for the log (a headless phone's only record besides the ledger)
                    if let Ok(said) = rr.recv() {
                        if said != "nothing new" {
                            eprintln!("agentd: phone ({what}): {}", said.replace('\n', "; "));
                        }
                    }
                }
            });
        }
        if let Some((device, code, hold_ms)) = key {
            let txk = tx_core.clone();
            crate::button::spawn(device, code, hold_ms, move || {
                let (rt, rr) = channel();
                if txk.send(Cmd::Ctl("key".into(), rt)).is_ok() {
                    if let Ok(ans) = rr.recv() {
                        eprintln!("agentd: approval key: {ans}");
                    }
                }
            });
        }
        let _ = ready_tx.send(Ok(()));
        let txh = tx_core.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let txh = txh.clone();
                let mut cfg = HttpConfig::new(&token);
                cfg.allowed_origins = origins.clone();
                std::thread::spawn(move || {
                    let mut s = conn;
                    let _ = serve_one(&mut s, &cfg, &mut |body| {
                        let (rt, rr) = channel();
                        txh.send(Cmd::Mcp(body.to_string(), rt)).ok()?;
                        rr.recv().ok().flatten()
                    });
                });
            }
        });
        // the core loop
        // tick once a second by the clock, not only when the channel is idle: steady traffic
        // must not starve the governor, the phone poll or the learning
        let mut last_tick = std::time::Instant::now();
        loop {
            if STOP.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Cmd::Mcp(b, r)) => {
                    let _ = r.send(core.mcp(&b));
                }
                Ok(Cmd::Ctl(l, r)) => core.ctl_reply(&l, r),
                Ok(Cmd::Stop) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
            if last_tick.elapsed() >= Duration::from_secs(1) {
                core.tick();
                last_tick = std::time::Instant::now();
            }
            core.poll_jobs();
        }
    });
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(Running {
            http_addr,
            ctl_path,
            tx,
            join: Some(join),
        }),
        Ok(Err(e)) => {
            let _ = join.join();
            Err(BootError::Other(e))
        }
        Err(_) => Err(BootError::Other("core thread died".into())),
    }
}

/// Client side of the control socket, used by `agentd ctl` and by tests.
pub fn ctl_send(path: &std::path::Path, line: &str) -> std::io::Result<String> {
    let mut s = std::os::unix::net::UnixStream::connect(path)?;
    // long enough for the local model to write and test a function on a phone
    s.set_read_timeout(Some(Duration::from_secs(1800)))?;
    writeln!(s, "{line}")?;
    let mut out = String::new();
    BufReader::new(s).read_line(&mut out)?;
    Ok(out.trim_end_matches('\n').replace('\u{1}', "\n"))
}
