# agentd: the Agentic Phone, running

`agentd` joins Volumes 2 to 7 into one process you can start, talk to and test.

```
 Claude client --HTTP+bearer--> [gateway: token, origin, limits] --> [executor: registry, tiers, Gate] --> connectors
 you at the terminal --control socket--> chat / press (the approval key) / status / power / anchor
 [kernel: stages, freezer, governor, ledger]   [engine: router, firewall, model]   [vault]   [anchors]
```

## Run it
```
export AGENTD_PASSPHRASE=...            # or type it when asked
cargo build --release
./target/release/agentd init -c agentd.toml      # makes the vault, prints the bearer token
./target/release/agentd run  -c agentd.toml      # starts the daemon
./target/release/agentd ctl  -c agentd.toml status
./target/release/agentd ctl  -c agentd.toml chat "remind me to call mum at 6pm"
./target/release/agentd ctl  -c agentd.toml pending      # High actions waiting for you
./target/release/agentd ctl  -c agentd.toml press 1      # the approval key
```
Point a Claude client at `http://127.0.0.1:8765/mcp` with `Authorization: Bearer <token>`.

## What is wired
Vol 2 stage machine, kernel, ledger (simulated cgroup and cpufreq unless `real_hardware = true`);
Vol 3 router, firewall, Gate, store, engine, llama.cpp host; Vol 4 executor, connectors, intents, MCP;
Vol 5 status line, notifier; Vol 7 vault, gateway and HTTP door, commercial client, ledger anchors.
Closed here: secrets come from the vault; the anchor is saved on a timer and checked at boot (a rewritten
ledger stops the daemon).

## Not wired yet
Skills, forge/sandbox, agents, photos and pages (Vols 4 to 6 crates exist but have no daemon entry points);
connectors are the in-memory reference ones; no persistence of connector state; the Gate's used-token list
is still in memory; MCP compatibility with a live Claude client is untested; nothing here has run on a phone.
