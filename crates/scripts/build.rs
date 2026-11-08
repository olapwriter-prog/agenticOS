//! Compile the script runtime (crates/script-guest) to WebAssembly and embed it in this crate.
//! It uses its own target directory, so it never waits on the outer build's lock.
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("../..");
    let guest = manifest.join("../script-guest");
    println!("cargo:rerun-if-changed={}", guest.join("src").display());
    println!(
        "cargo:rerun-if-changed={}",
        guest.join("Cargo.toml").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        root.join("Cargo.lock").display()
    );

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let target_dir = out.join("guest-target");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(&root)
        .args([
            "build",
            "--release",
            "--locked",
            "-p",
            "agentos-script-guest",
        ])
        .args(["--target", "wasm32-unknown-unknown", "--target-dir"])
        .arg(&target_dir);
    // Flags meant for the host build (and wrappers such as cargo-auditable) are not for the guest.
    for v in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTC_WORKSPACE_WRAPPER",
        "RUSTC_WRAPPER",
        "CARGO_BUILD_TARGET",
        "CARGO_TARGET_DIR",
    ] {
        cmd.env_remove(v);
    }
    let status = cmd
        .status()
        .expect("cannot run cargo to build the script runtime");
    if !status.success() {
        panic!(
            "building the script runtime for wasm32-unknown-unknown failed.\n\
             Install the target: `rustup target add wasm32-unknown-unknown` \
             (on Alpine: `apk add rust-wasm`)."
        );
    }
    let wasm = target_dir.join("wasm32-unknown-unknown/release/agentos_script_guest.wasm");
    std::fs::copy(&wasm, out.join("script_runtime.wasm"))
        .unwrap_or_else(|e| panic!("{}: {e}", wasm.display()));
}
