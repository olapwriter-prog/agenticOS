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

## A screen, when you want one

The OS is headless; a screen is optional. `ui/` is `agentos-ui`, a [Rune](https://github.com/anomalyco/rune)
app that is only a client of agentd (the same control socket as `agentd ctl`): status, what is
waiting for approval (Approve / Decline), recent actions, the model and the chip, functions, and a
chat box. It is its own workspace, so the headless build never compiles a GPU stack.

```bash
cd ui && cargo run --release                                   # window (socket: $AGENTD_SOCK)
cargo run --release -- --snap ui.png --scene demo             # headless render, sample data
AGENTD_SOCK=../agentd-data/control.sock cargo run --release -- --snap ui.png --scene live
```

## Its own inference engine

`agentos-llm` (`crates/infer`) is this OS's LLM engine, written in Rust for phones, a drop-in for
llama.cpp's `llama-server` as agentd uses it. It reads GGUF files and runs Qwen2/2.5, Qwen3 and the
Qwen3.5 hybrid (Gated DeltaNet + gated attention), producing the same text as llama.cpp. Built for a
phone: weights memory-mapped and never copied, integer kernels with NEON SDOT chosen at run time
(portable Rust elsewhere, armv7 included), an f16 attention cache, a constant-size recurrent
state, a snapshot of the system prompt so it is read once (agentd warms it at start), GBNF and
JSON-schema constrained output, and thread count following the model's cgroup cores.

## The LLM on the phone

`agentd` runs llama.cpp's `llama-server` itself: a small model always loaded for chat, and a large one
on the charger. Requests no fixed phrase covers go to the model. It is given the tool list with one
line per tool, and a grammar forces its answer to be exactly one registered tool call. The chip is
given to the model first: its own fast cores, top CPU weight, top frequency, the GPU (Vulkan), and
locked memory. One core is always kept for calls, and a temperature limit drops the boost when the
phone gets hot. `agentd ctl prio` shows it; `agentd models pull` fetches models and checks their hashes.

## Functions the agent writes for itself, in Rust

Everything is Rust: the OS, and the functions the agent writes for itself. Say `make a function that
splits a bill between people`. The local model (the large one when it is loaded) writes
`fn run(args: &Args) -> Result<String, String>` with at least three examples. `rustc` compiles it to
WebAssembly, and the sandbox runs the examples. Compiler errors or failed examples go back to the
model with the offending line quoted, up to three attempts. A Claude client can do the same over MCP
with `write_function`.

- **Checked, then sandboxed:** before compiling, the source is refused if it uses anything that reads
  files or the environment while compiling (`include_str!`, `env!`, `#[path]`, `mod x;`, `extern`).
  `rustc` runs in an empty directory with a cleared environment and a time limit. The module runs in
  the sandbox with no imports at all (no files, network or clock), capped fuel and memory.
- **Installed only with the key:** `install_function` is High tier, and the approval covers that
  exact version. `agentd ctl show <name>` shows the code and examples to read first.
- **Versions:** writing again makes a new version (compiled, tested, installed again).
  `roll back <name> to version <n>` returns to any version installed before.
- **Acting on the phone:** a function asks with `phone("send_sms", &[("to", who), ("text", msg)])?`.
  The daemon carries the requests out afterwards through the executor, so a text still waits for the
  key, and a function can never write or install functions.

## Regular requests: code first, the LLM last

Everyday requests are handled without the LLM. A weighted Naive Bayes classifier
(`crates/understand`: words, word pairs and number shapes; inverse-label-frequency weights; online
updates) decides what kind of request it is, rules pull out the details (contacts by name, typo or
"mother", days, numbers, on/off, message text), and questions are answered by exact code over every
message, call and contact. The LLM is asked only when the classifier is unsure (below 0.6) or a detail
is missing. Every request you approve, decline or correct retrains the classifier in milliseconds, so
phrasings that once needed the LLM stop needing it.

Order for each message: feedback words, exact phrases, your shortcuts, the classifier, then the LLM.

With no LLM at all: 10/10 on the tool-choice benchmark, 9/9 on the data questions, and 11/12 on
phrasings it had never seen (the twelfth it declined and would have passed to the LLM), each in
milliseconds. The LLM on the same questions: 3/9, at 15 to 60 s each.

## Questions over the phone's data, answered in the sandbox

Ask `how many texts did Mum send me?` or `look through my calls for missed ones`. The model is
not given the data. It is told only each source's format (`from NAME (NUMBER): TEXT`, ...), and
it writes Rust that loads the sources it needs (read-only tools such as `all_messages`,
`call_log`, `list_contacts`) and returns just the answer. agentd fetches the data through the
executor (Low tier only), compiles the code with rustc and runs it in the sandbox over everything,
so the context window stays small and private data never reaches the model. Compiler errors go
back to the model to fix. Code that tries to make the phone act is refused. A Claude client uses
the same thing over MCP as `analyze_data` (formats from `data_formats`).

## Learning from you

Every request is an episode, rewarded only by you and by what happened: the approval key (+1),
a decline (-1), "that was right" / "that was wrong", and whether the tool worked. A model's answer
that only ran is never treated as right. Say "that was wrong" and then do it the right way, and
that becomes the correction. What it learns, with no retraining:

- **Shortcuts:** a request you approved or corrected runs that action directly next time,
  still through the tiers and the key.
- **Examples:** the model is shown your most similar confirmed requests.
- **Training data:** `agentd ctl learn` writes SFT chats, DPO pairs (from corrections) and a
  held-out eval set; `learn eval` scores the model on it. Weight training is not wired yet:
  llama.cpp's trainer aborts on Qwen models (see the install guide).

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
| `crates/phonesim` | **Simulated phone**: fake mmcli/nmcli/callaudiocli/fbcli/wpctl, ModemManager signals on a private D-Bus, a key device; `scripts/phonesim-scenarios.sh` runs every phone scenario on agentd's real backend |
| `crates/prio` | **Chipset priority**: cgroups, fast cores, uclamp, cpufreq/GPU boost, thermal guard, call core |
| `crates/models` | **Model supervisor**: runs llama-server per stage, restarts, health, verified downloads |
| `crates/infer` | **agentos-llm**: GGUF, BPE tokenizer, Qwen2/3/3.5 forward pass, NEON kernels, grammar, server |
| `crates/understand` | **Understanding without the LLM**: weighted Naive Bayes (online), detail extraction, typed records |
| `crates/learn` | **Learning**: episodes, rewards from human signals, shortcuts, examples, datasets, eval |
| `crates/functions` | **Agent-written Rust functions**: source check, rustc to WebAssembly, examples, versions, install/rollback |
| `crates/{hal,stage,power,wake,freeze,ledger,kerneld}` | Vol 2: power stages, cgroup freezer, cpufreq, hash-chained ledger |
| `crates/{router,firewall,gate,action,store,brain,host}` | Vol 3: model routing, PII firewall, approval tokens, llama.cpp host |
| `crates/{executor,connectors,intents,skills,mcp}` | Vol 4: tool tiers, connectors, phrase intents, skills, MCP |
| `crates/{status,notify,photos,pages}` | Vol 5: status line, notification policy |
| `crates/{sandbox,forge,agents,patterns}` | Vol 6: WASM sandbox, tool forge, multi-agent |
| `crates/{vault,gateway,commercial,update}` | Vol 7: vault, HTTP gateway, commercial model client, signed updates |
| `os/postmarketos` | APKBUILD, OpenRC service, phone config |
| `scripts/sideload.sh` | Cross-compile and push to a phone over USB |
| `scripts/vm-test.sh` | Build the package and test the service in an aarch64 Alpine VM (Mac + Lima) |
| `docs/volumes` | Reference output and the original agentd notes |

## Not done yet

See the *Known gaps* section of the install guide. The volumes' own list still applies too: skills,
forge, agents, photos and pages have crates but no daemon entry points, and the Gate's used-token list
is kept only in memory.
