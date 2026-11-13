# Test plan: what is tested, where, and what is still open

Covers this repository (agentd, agentos-llm, agentos-ui) and the simulator,
[phonesim](https://github.com/olapwriter-prog/Phone-emulators-Android-iOS-and-Agentic-OS-),
which models agentd's behaviour on real phone hardware. Results are dated; rerun the commands
to refresh them.

## Tiers

| # | Tier | Command | Needs | Runs on |
|---|---|---|---|---|
| T1 | Rust unit and e2e tests (workspace) | `cargo test --workspace --release` | Mac or Linux | every commit |
| T2 | agentos-ui tests | `cd ui && cargo test --release` | same | every commit |
| T3 | Simulator behaviour tests | `npm test` (phonesim) | Node ≥ 18 | every commit, GitHub Actions |
| T4 | Simulator vs. agentd drift | read `crates/agentd/src/{core,trainer}.rs` against `src/os/agentic.js` | — | after each agentd phase |
| T5 | Package + OpenRC service in an Alpine aarch64 VM | `scripts/vm-test.sh <model.gguf>` | lima, ~3 GB model | before a release |
| T6 | Trainer: gradient checks and the eval gate | `cargo run --release -p agentos-infer --example train -- gradcheck MODEL` (`LONG=1` too), then `agentos-llm train` | a Qwen2/3/3.5 GGUF, hours of CPU | when the trainer changes |
| T7 | On a phone | install guide, then calls, SMS, audio, the key, suspend | a postmarketOS phone with a modem | not yet possible here |

## Status (2026-11-13, commit 56d0ef7 "Phase-3")

| # | Result | Notes |
|---|---|---|
| T1 | 404/404 pass | 82 test binaries, release build, Apple M-series |
| T2 | 4/4 pass | |
| T3 | 60/60 pass | was 59; one stale test replaced by two (see T4) |
| T4 | 1 drift fixed, 1 bug still open | below |
| T5 | 22/22 checks pass | Alpine aarch64 VM, Qwen3.5-4B Q4_K_M (sha256 matches agentd.toml); package tests pass inside abuild |
| T6 | pass | Qwen3 and Qwen3.5 trainer support added and gradient-checked; a Qwen3-4B run completed and the eval gate behaved correctly (below) |
| T7 | open | no phone attached; see INSTALL-postmarketos.md, "Untested on a phone" |

### T4: simulator vs. agentd

- **Fixed in agentd, simulator updated.** The simulator still reproduced the old training bug
  (a nightly run stopped one tick after it started). In Phase 3 `trainer.rs` has no nightly
  override and `trained_on` moves only when a run finishes, so a run starts at
  `train_min_new` (20) new confirmed requests and is not stopped by its own count. phonesim
  now models that and tests both sides of it.
- **Still open in agentd: the phone never suspends.** `kerneld::Kernel::may_suspend()` is only
  called from its own unit test. In the simulator a OnePlus 6 loses ~40% over 24 h of standby
  under agenticOS against ~8% under stock Android. Fixing it means agentd writing `mem` to
  `/sys/power/state` (or using `rtcwake`) when `may_suspend()` is true, with the modem's ring
  and SMS interrupts as wake sources, and that can only be verified on a phone (T7).

### T7: what only a phone can test

- ModemManager backend on a real modem (it is unit-tested on recorded `mmcli`/`nmcli` output).
- Call audio routing (callaudiod, feedbackd, PipeWire need a user session).
- The approval key as a hardware button read from `/dev/input`.
- System suspend and wake on ring or SMS, once agentd acts on `may_suspend()`.
- Thermal behaviour and model speeds on the phone's own CPU.

### T6: trainer on Qwen3 and Qwen3.5 (2026-11-13)

- Gradient checks (f32, h=0.01): Qwen3-4B-Instruct-2507, Qwen3.5-4B, and Qwen3.5-4B with a prompt
  past one DeltaNet checkpoint interval. Every gradient above 5e-3 agrees with finite differences
  within 7%; the few outliers are entries near 1e-4 (f32 noise). The trainer's forward pass
  matches the engine (Qwen3.5: 7.668 vs 7.639).
- Training run: Qwen3-4B-Instruct-2507 Q4_K_M, agentd's real system prompt (~750 tokens) and tool
  grammar, 30 confirmed requests (24 train, 6 held out), 16 steps x batch 2, rank 8, lr 1e-3.
  Loss fell to ~0.002; held-out score 75% without the adapter, 67% with it, so the adapter was
  **discarded**, as the gate should. 1 h 52 min on the Mac's CPU (~3 min per example pass,
  close to what phonesim assumes for a 4B step).
- Read: a 4B instruct model already handles these phrasings; 24 examples overfit. A gain needs
  more confirmed requests (the 20-request trigger is a floor, not a target) or fewer steps.
- Known flaky: `e2e::the_door_needs_the_token` failed once in a parallel run (empty HTTP reply),
  passed 3/3 alone. Unrelated to the trainer change.
