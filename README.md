# agenticOS

A phone OS where every process and integration goes headless. There is no app grid and no screen
to tap. One daemon, `agentd`, *is* the phone: it places and answers calls, sends and reads texts,
manages Wi-Fi and mobile data, watches the battery, and takes instructions in plain language or from a
Claude client over MCP. Risky actions (calling someone, texting, paying) wait for a human to press the
approval key.

The base system is **postmarketOS** (Alpine Linux for phones) installed with no user interface. See
[docs/INSTALL-postmarketos.md](docs/INSTALL-postmarketos.md) for why, which phones work, and how to flash.

```
 you (plain language) ─┐                       ┌─ intents (no model) ─┐
 Claude client (MCP) ──┼─► gateway ─► engine ──┼─ local llama.cpp ────┼─► executor ─► connectors
 approval key ─────────┘   token,     router,  └─ commercial model ───┘   tiers +     calendar, reminders,
                           limits     firewall                            Gate        messenger, PHONE
                                                                                         │
 kernel: stages · freezer · governor · ledger      vault · anchors     ModemManager · NetworkManager
                                                                       feedbackd · callaudiod · sysfs
```

## Phone features, headless

| Say (or call over MCP) | Tool | Tier |
|---|---|---|
| `call mum` | `call` | **High**: approval key |
| `text mum running late` | `send_sms` | **High**: approval key |
| `answer` / `hang up` | `answer_call` / `hangup` | Medium / Low |
| `read my messages`, `messages from mum` | `read_messages`, `messages_from` | Low |
| `save Mum as +91 98765 43210`, `what's Mum's number` | `add_contact`, `find_contact` | Medium / Low |
| `missed calls` | `list_calls` | Low |
| `check balance *123#` | `ussd` | Medium |
| `signal`, `phone status` | `phone_status` | Low |
| `scan wifi`, `join wifi Home password ...` | `wifi_scan`, `wifi_connect` | Low / Medium |
| `airplane mode on`, `mobile data off` | `airplane_mode`, `mobile_data` | Medium |
| `where am i` | `where_am_i` | Medium |
| `flashlight on`, `volume 40`, `find my phone` | `flashlight`, `set_volume`, `ring_phone` | Low |

Incoming calls and texts are picked up in the background. They wake the phone, get written to the
tamper-evident ledger, and alert the user through vibration and sound. Texts that arrive during quiet
hours are held for the digest, but incoming calls always ring.

## Run it on your computer

The simulated phone is the default, so this works on macOS or Linux with no hardware:
```bash
cargo build --release
export AGENTD_PASSPHRASE=dev
cp agentd.example.toml agentd.toml
./target/release/agentd init -c agentd.toml        # prints the MCP bearer token
./target/release/agentd run  -c agentd.toml &
./target/release/agentd ctl  -c agentd.toml chat "save Mum as +919876543210"
./target/release/agentd ctl  -c agentd.toml chat "call mum"     # approval required
./target/release/agentd ctl  -c agentd.toml press 1
./target/release/agentd ctl  -c agentd.toml chat "call mum"     # calling Mum
./target/release/agentd ctl  -c agentd.toml "sim-sms +919876543210|dinner at 8?"
./target/release/agentd ctl  -c agentd.toml chat "read my messages"
```
`cargo test --workspace` runs the whole system, including real sockets, the vault and the ledger.

## Layout

| Path | What |
|---|---|
| `crates/agentd` | The daemon: wires everything together, MCP over HTTP, control socket |
| `crates/phone` | **Headless phone services**: calls, SMS, contacts, USSD, Wi-Fi, data, GPS, torch, feedback |
| `crates/{hal,stage,power,wake,freeze,ledger,kerneld}` | Vol 2: power stages, cgroup freezer, cpufreq, hash-chained ledger |
| `crates/{router,firewall,gate,action,store,brain,host}` | Vol 3: model routing, PII firewall, approval tokens, llama.cpp host |
| `crates/{executor,connectors,intents,skills,mcp}` | Vol 4: tool tiers, connectors, phrase intents, skills, MCP |
| `crates/{status,notify,photos,pages}` | Vol 5: status line, notification policy |
| `crates/{sandbox,forge,agents,patterns}` | Vol 6: WASM sandbox, tool forge, multi-agent |
| `crates/{vault,gateway,commercial,update}` | Vol 7: vault, HTTP gateway, commercial model client, signed updates |
| `os/postmarketos` | APKBUILD, OpenRC service, phone config |
| `scripts/sideload.sh` | Cross-compile and push to a phone over USB |
| `docs/volumes` | Reference output and the original agentd notes |

## Not done yet

See the *Known gaps* section of the install guide. The volumes' own list still applies too: skills,
forge, agents, photos and pages have crates but no daemon entry points, and the Gate's used-token list
is kept only in memory.
