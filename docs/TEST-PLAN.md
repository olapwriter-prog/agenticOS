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
| T6 | Trainer accuracy (Phase 3 eval gate) | `agentos-llm train` on Qwen2.5-0.5B, held-out score | the 0.5B GGUF, hours of CPU | when the trainer changes |
| T7 | On a phone | install guide, then calls, SMS, audio, the key, suspend | a postmarketOS phone with a modem | not yet possible here |

## Status (2026-11-13, commit 56d0ef7 "Phase-3")

| # | Result | Notes |
|---|---|---|
| T1 | 404/404 pass | 82 test binaries, release build, Apple M-series |
| T2 | 4/4 pass | |
| T3 | 60/60 pass | was 59; one stale test replaced by two (see T4) |
| T4 | 1 drift fixed, 1 bug still open | below |
| T5 | 22/22 checks pass | Alpine aarch64 VM, Qwen3.5-4B Q4_K_M (sha256 matches agentd.toml); package tests pass inside abuild |
| T6 | blocked | the Qwen2.5-0.5B GGUF the 2026-11-09 run used is no longer on this Mac |
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
