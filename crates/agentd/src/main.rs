//! agentd: run the phone's brain stem on this machine.
//!   agentd init   [-c config.toml]     create the vault and print the gateway token
//!   agentd run    [-c config.toml]     start the daemon
//!   agentd ctl    [-c config.toml] ... send a control command (status, pending, press 1, chat ...)
//!   agentd set-commercial-key [-c config.toml]
//! The vault passphrase comes from AGENTD_PASSPHRASE, the file named by AGENTD_PASSPHRASE_FILE,
//! or one line on standard input.
use agentd::config::Config;
use agentd::{secrets, server};
use agentos_hal::ScaledClock;
use std::path::PathBuf;
use std::sync::Arc;

fn passphrase() -> String {
    if let Ok(p) = std::env::var("AGENTD_PASSPHRASE") {
        return p;
    }
    // A headless phone has nobody to type at boot; the init script keeps it in a root-only file.
    if let Ok(f) = std::env::var("AGENTD_PASSPHRASE_FILE") {
        match std::fs::read_to_string(&f) {
            Ok(p) => return p.trim_end_matches(['\n', '\r']).to_string(),
            Err(e) => fail(&format!("{f}: {e}")),
        }
    }
    eprint!("vault passphrase: ");
    let mut s = String::new();
    std::io::stdin().read_line(&mut s).ok();
    s.trim_end_matches(['\n', '\r']).to_string()
}

fn fail(msg: &str) -> ! {
    eprintln!("agentd: {msg}");
    std::process::exit(1)
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut cfg_path: Option<PathBuf> = None;
    if let Some(i) = args.iter().position(|a| a == "-c") {
        if i + 1 >= args.len() {
            fail("-c needs a file");
        }
        cfg_path = Some(PathBuf::from(args.remove(i + 1)));
        args.remove(i);
    }
    let cfg = match &cfg_path {
        Some(p) => Config::load(p).unwrap_or_else(|e| fail(&e)),
        None => Config::default(),
    };
    let Some(cmd) = args.first().cloned() else {
        fail("usage: agentd init|run|ctl|set-commercial-key [-c config.toml]");
    };
    match cmd.as_str() {
        "init" => {
            let s = secrets::open_or_create(&cfg.data_dir, &passphrase(), cfg.kdf_mib)
                .unwrap_or_else(|e| fail(&e));
            println!("vault ready in {}", cfg.data_dir.display());
            println!(
                "gateway token (give this to the Claude client as a bearer token):\n{}",
                s.gateway_token
            );
        }
        "set-commercial-key" => {
            let pass = passphrase();
            eprint!("api key: ");
            let mut k = String::new();
            std::io::stdin().read_line(&mut k).ok();
            secrets::set_commercial_key(&cfg.data_dir, &pass, k.trim())
                .unwrap_or_else(|e| fail(&e));
            println!("stored in the vault");
        }
        "run" => {
            let s = secrets::open_or_create(&cfg.data_dir, &passphrase(), cfg.kdf_mib)
                .unwrap_or_else(|e| fail(&e));
            let listen = cfg.listen.clone();
            let ctl = cfg.control_path();
            let r = server::start(cfg, s, Arc::new(ScaledClock::new(1)))
                .unwrap_or_else(|e| fail(&e.to_string()));
            eprintln!(
                "agentd up: MCP at http://{} (POST /mcp), control at {}",
                r.http_addr,
                ctl.display()
            );
            let _ = listen;
            r.wait();
        }
        "ctl" => {
            let line = args[1..].join(" ");
            match server::ctl_send(&cfg.control_path(), &line) {
                Ok(a) => println!("{a}"),
                Err(e) => fail(&format!("cannot reach the daemon: {e}")),
            }
        }
        other => fail(&format!("unknown command {other}")),
    }
}
