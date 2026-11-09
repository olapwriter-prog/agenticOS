# Installing Agentic OS on a phone

Agentic OS is **postmarketOS with no graphical shell, plus `agentd`**. The phone boots to a console;
`agentd` starts as a system service and *is* the phone: it answers calls, sends and reads texts, joins
Wi-Fi, reports battery and signal, and takes instructions in plain language or from a Claude client over
MCP. Nothing draws a screen.

## Why postmarketOS

| Option | Base | Size, headless | Devices | Telephony stack | Verdict |
|---|---|---|---|---|---|
| **postmarketOS** | Alpine (musl, OpenRC) | ~100 MB, `ui=none` is a supported choice | 600+ ports (dozens well supported) | ModemManager | **Chosen**: smallest, runs on the most phones with a mainline kernel, and treats "no UI" as a normal install |
| Mobian | Debian | ~500 MB | ~10 | ModemManager | Good, but few devices |
| Droidian | Debian + Android kernel (Halium) | ~1 GB | many Android phones | oFono | More phones, but runs a heavy Android layer underneath |
| Ubuntu Touch | Ubuntu + Halium | >1 GB | ~70 | oFono | Built around its own UI |

No Linux distribution installs on *every* phone. Each phone needs a kernel port. postmarketOS covers the
most phones with the least software. `agentd` itself is a single static binary that only needs
ModemManager and NetworkManager, so it also runs on Mobian unchanged. Droidian and Ubuntu Touch would
need an oFono backend: a new `Backend` implementation in `crates/phone`.

Check your phone: <https://wiki.postmarketos.org/wiki/Devices>. Pick one marked **main** or
**community** with working *modem/calls/SMS*. Well-supported examples include the PinePhone and
PinePhone Pro, OnePlus 6/6T, Xiaomi Pocophone F1, Fairphone 4/5, and Samsung Galaxy A3/A5 (2015).

## Install

You need a Linux computer (or a Linux VM) and the phone with an unlocked bootloader.

```bash
pip install --user pmbootstrap        # or: your distribution's package
pmbootstrap init
```
When `init` asks:
- **Channel**: `edge` or the latest stable `v2x.yy`
- **Vendor / device**: your phone
- **User interface**: `none`  ← headless; this is the whole point
- **Extra packages**: `modemmanager,networkmanager,networkmanager-cli,networkmanager-wifi,networkmanager-wwan,feedbackd,callaudiod,wireplumber,pipewire-pulse,evtest`
  (the agentd package also pulls in `llama-server`, `libggml-cpu` and `libggml-vulkan` on aarch64)

Build and add the agentd package from this repository:
```bash
cp -r os/postmarketos "$(pmbootstrap config aports)/temp/agentd"
pmbootstrap checksum agentd
pmbootstrap build agentd --src="$PWD"
pmbootstrap install --fde --add agentd,agentd-openrc
```
`--fde` encrypts the disk, so the vault passphrase file is useless on a stolen phone. Then flash
(for most Android-era phones, in fastboot mode):
```bash
pmbootstrap flasher flash_rootfs
pmbootstrap flasher flash_kernel
```
PinePhones boot from an SD card instead: `pmbootstrap install --sdcard=/dev/sdX ...`.

Enable the services once you are logged in over SSH (`ssh user@172.16.42.1` over USB):
```bash
sudo rc-update add modemmanager default
sudo rc-update add networkmanager default
sudo rc-update add agentd-session default   # call audio, vibration, volume
sudo rc-update add agentd default
sudo rc-service agentd start
sudo cat /etc/agentd/gateway-token      # bearer token for the MCP endpoint
```

The LLM's CPU, GPU and memory priority needs the unified cgroup v2 tree. In `/etc/rc.conf` set
`rc_cgroup_mode="unified"` and reboot (the service warns at start if it is missing).

Download the local models once (0.5 GB for the always-loaded small model, 2.1 GB for the large
one used on the charger). Both are checked against the SHA-256 pinned in the config, and the
daemon starts each one within a second of it arriving:
```bash
sudo agentd models pull -c /etc/agentd/agentd.toml
sudo agentd ctl -c /etc/agentd/agentd.toml models   # running, ready
sudo agentd ctl -c /etc/agentd/agentd.toml prio     # model cores, temperature, protected memory
```
On a phone with 4 GB of RAM or less, delete the large `[[models]]` table.

Find the battery directory and put it in `/etc/agentd/agentd.toml` as `power_dir`:
```bash
ls /sys/class/power_supply/
```

## Using it

```bash
sudo agentd ctl -c /etc/agentd/agentd.toml phone            # operator, signal, data, wifi
sudo agentd ctl -c /etc/agentd/agentd.toml chat "save Mum as +91 98765 43210"
sudo agentd ctl -c /etc/agentd/agentd.toml chat "call mum"  # High: needs the key
sudo agentd ctl -c /etc/agentd/agentd.toml pending
sudo agentd ctl -c /etc/agentd/agentd.toml press 1          # or hold volume-down for 2 s
sudo agentd ctl -c /etc/agentd/agentd.toml chat "call mum"  # now it dials, once
```
A Claude client can drive the same tools. Forward the port over SSH, then point the client at
`http://127.0.0.1:8765/mcp` with the bearer token:
```bash
ssh -L 8765:127.0.0.1:8765 user@172.16.42.1
```

## Faster development loop

Rebuild only the binary on your Mac and push it over USB networking:
```bash
brew install zig && cargo install cargo-zigbuild
scripts/sideload.sh user@172.16.42.1 aarch64
```

## Test the package before flashing (Mac, no phone)

postmarketOS is Alpine plus phone-specific packages, and Apple Silicon runs aarch64 natively. So an
Alpine VM checks the phone's architecture, libc, OpenRC, cgroups and Alpine's own `llama-server`:
```bash
brew install lima
scripts/vm-test.sh path/to/qwen2.5-0.5b-instruct-q4_k_m.gguf
```
It builds the real package with `abuild` (running its tests), installs it, starts the service,
and checks 21 things: first-boot secrets, ledger, charger boost, the reserved core, MCP auth,
chat, freezing, approval, a Rust function compiled by the phone's rustc, tested, installed with the key and run,
llama-server in the model cgroup, real-model answers, crash recovery
(`kill -9` takes the model server down with it, and supervise-daemon brings both back), and a
clean stop. It cannot check the modem, GPU, uclamp or thermal sensors; those need the phone.

## Try it without a phone

postmarketOS has a QEMU target. It has no modem, so run it with `phone = "sim"`:
```bash
pmbootstrap init        # device: qemu-amd64, ui: none
pmbootstrap install --add agentd,agentd-openrc && pmbootstrap qemu
```
Or run `agentd` straight on your computer (see the README); the simulated phone is the default.

## How the chip is given to the LLM

`agentd` starts `llama-server` itself and moves it into `/sys/fs/cgroup/agentos.slice/agentos-llm.slice`.

| | Idle | Answering (chat) or Deep stage (charger) | Too hot (≥ `hot_c` until < `cool_c`) |
|---|---|---|---|
| CPU weight | 100 | 10000, highest on the system | 10000 |
| Cores | the fast cores, minus one reserved core | same | same |
| uclamp.min | 0 | max (top frequency) | 0 |
| cpufreq on those cores | kernel's choice | `performance` | kernel's choice |
| GPU devfreq | kernel's choice | `performance` | kernel's choice |
| cpu.max | unlimited | unlimited | half of those cores |
| Memory | weights protected (`memory.min`), never swapped, `--mlock` | same | same |

The reserved core (the slowest one, or `reserve_cpus`) is never given to the model. The modem,
call audio and `agentd` itself always have it, so a call is never starved. The model's threads
match the cores it owns. Layers go to the GPU through Vulkan where Mesa supports it (Adreno via
Turnip, Mali via PanVK) and stay on the CPU otherwise. Knobs the kernel lacks (for example uclamp
on some ports) are skipped and listed by `agentd ctl prio`.

The phone's NPU (for example Qualcomm Hexagon) is not used: mainline Linux has no driver
llama.cpp can use on these phones.

## Known gaps (honest list)

- **Local models write some functions, not all.** Measured on a laptop with the repair loop
  (3 attempts): Qwen2.5-Coder-3B (loaded on the charger) wrote working Rust functions for 6 of 10
  requests, 5 to 15 s each, including one that texts a contact through `phone()`. Almost
  everything compiles; the failures are logic slips ("1 words") and wrong arithmetic in its own
  examples, and the example tests stopped every one. (In Rhai, tried earlier, it managed 4 of 10.)
  A Claude client over MCP can always write functions with `write_function`.
- **What the models manage** (benchmark on a laptop, the same 10 unseen phrasings, 10 function
  requests and 5 questions over planted messages and calls):

  | | Qwen2.5 0.5B | Qwen3.5 4B (always on) | Qwen3.5 9B (on the charger) |
  |---|---|---|---|
  | Tool choice | 8/10 | 10/10 | 9/10 |
  | Rust functions written and passing their tests | not tried | 7/10 | 8/10 |
  | Questions answered by code in the sandbox | 0/5 | 2/5 | 3/5 |

  With the code-first layer (no LLM): tool choice 10/10, questions 9/9, never-seen phrasings 11/12.
  The misses are the model's reasoning (an EMI formula, reading "calls Ravi made to me" as text to
  match); every wrong function or answer was caught or is visible, and nothing acted without the key.
- **The screen (agentos-ui)** is packaged apart from agentd (`os/postmarketos-ui`), so the headless
  image never pulls in a GPU stack: `cage` runs it full screen, `seatd` gives cage the display and
  input, and `/etc/init.d/agentos-ui` starts it after agentd, as a client of `/run/agentd.sock`.
  Add your GPU's Vulkan driver to the image (`mesa-vulkan-freedreno` for Adreno,
  `mesa-vulkan-panfrost` for Mali), then `rc-update add seatd default` and
  `rc-update add agentos-ui default`. The package and service are written but not yet built or
  run on a phone: rendering with lavapipe in the VM crashes, and no phone GPU has been tried.
- **Own engine speed.** On an Apple M-series CPU, Qwen3.5-4B Q4_K_M: agentos-llm generates
  ~10 tokens/s and reads prompts at ~17 tokens/s; llama.cpp on the same CPU does ~22 and ~42.
  agentd's requests take 2 to 3 s once the system prompt is cached (it is read once, at start).
  Not yet: GPU (Vulkan), i8mm matrix kernels for faster prompt reading, speculative decoding.
  llama.cpp's llama-server remains a drop-in alternative (`llama_server` in agentd.toml).
- **No weight training yet.** `llama-finetune` (llama.cpp's trainer) aborts with an internal
  assertion on Qwen models in both Homebrew's b8960 and Alpine's b9564, for 4-bit, F16 and F32
  weights alike. The learning that works today needs no training (shortcuts, examples,
  corrections), and the data and eval gate for training are in place. Next step: a Rust LoRA
  trainer (candle), run overnight on the charger, used only if it beats the current model.
- **Function writing on the phone.** Functions are Rust, compiled by the phone's own `rustc`
  (the `rust` and `rust-wasm` packages, a few hundred MB). A compile takes under a second on a
  laptop; on a phone expect a few seconds, during which the daemon is busy.
- **Small-model accuracy.** On a laptop test the 0.5B model chose the right tool for 7 of 10
  unseen phrasings. Every wrong choice failed safely: calls and texts still waited for the key.
  The 3B model on the charger and Phase 3 (learning from approvals) are the fixes.

- **Call audio, vibration and volume** run in a user session that the `agentd-session` service
  starts at boot for the `agentd-session` user (made when the package is installed): a session bus,
  PipeWire, WirePlumber, pipewire-pulse, callaudiod and feedbackd. agentd runs `callaudiocli`,
  `fbcli` and `wpctl` as that user inside it (`session_user` in agentd.toml). Tested with stand-in
  programs only; on a real phone check `/var/log/agentd-session.log`, and set `FEEDBACKD` or
  `CALLAUDIOD` in the service's environment if those daemons live elsewhere on your image.
- **Calls and texts are seen as they arrive**, from ModemManager's D-Bus signals (CallAdded,
  Messaging Added, call StateChanged) through `dbus-monitor`. A signal makes agentd look at once,
  through `mmcli` as before; polling stays as a backup every `phone_signal_poll_secs` (60 s), and
  goes back to every `phone_poll_secs` if the monitor stops. Not yet run against a real modem.
- **The approval key** is volume-down held for two seconds (`approval_key` in agentd.toml; find the
  device with `evtest`). It approves the waiting action only when exactly one is waiting: without a
  screen you could not tell which of several you approved, so then use `agentd ctl press N`. A short
  press is left to the volume. Tested with recorded key events, not yet on a phone's buttons.
- **Untested on a phone.** The package and service pass `scripts/vm-test.sh` on Alpine 3.24 aarch64
  (kernel 6.18), with a real model. The ModemManager backend is only unit-tested against recorded
  `mmcli` and `nmcli` output, and has not yet run on a real modem.
