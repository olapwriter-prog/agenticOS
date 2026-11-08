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
- **Extra packages**: `modemmanager,networkmanager,networkmanager-cli,networkmanager-wifi,networkmanager-wwan,feedbackd,callaudiod,wireplumber`
  (the agentd package also pulls in `llama-server`, `libggml-cpu` and `libggml-vulkan` on aarch64)

Build and add the agentd package from this repository:
```bash
cp -r os/postmarketos "$(pmbootstrap config aports)/temp/agentd"
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
sudo agentd ctl -c /etc/agentd/agentd.toml press 1          # the approval key
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

- **Small-model accuracy.** On a laptop test the 0.5B model chose the right tool for 7 of 10
  unseen phrasings. Every wrong choice failed safely: calls and texts still waited for the key.
  The 3B model on the charger and Phase 3 (learning from approvals) are the fixes.

- **Call audio, vibration and volume.** callaudiod, feedbackd and PipeWire run inside a *user session*
  on postmarketOS. On a `ui=none` image no session starts, so `callaudiocli`, `fbcli` and `wpctl` fail
  softly. Calls still connect and SMS still works, but the earpiece may not be routed. Next step: start
  a session for a dedicated user at boot and run those daemons there.
- **Polling, not signals.** Incoming calls and texts are found by polling `mmcli` every
  `phone_poll_secs`. Subscribing to ModemManager's D-Bus signals would save battery and is the next
  change.
- **The approval key** is `agentd ctl press N` over SSH. On the phone it should be a hardware button
  (for example volume-down held for two seconds) read from `/dev/input`.
- **Untested on hardware.** The ModemManager backend is unit-tested against recorded `mmcli` and
  `nmcli` output, but has not yet run on a real modem.
