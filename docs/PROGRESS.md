# Progress: the inference engine and the trainer

Kept up to date during development. Numbers are measured unless marked otherwise
(Apple M-series CPU, 4 performance + 6 efficiency cores, unless a phone is named).

## agentos-llm (crates/infer): the inference engine

| Milestone | Status | Measured |
|---|---|---|
| GGUF loader (memory-mapped), all Qwen tensor types | done | loads Qwen3.5-4B in ~0.1 s |
| BPE tokenizer | done | matches `llama-tokenize` token for token (Qwen2.5, Qwen3.5) |
| Dense Qwen2/2.5 and Qwen3 forward pass | done | same text as llama.cpp |
| Qwen3.5 hybrid (Gated DeltaNet + gated attention) | done | same text as llama.cpp |
| Integer kernels, NEON SDOT (runtime-detected), portable fallback | done | Qwen3.5-4B Q4_K_M: ~10 tok/s generate, ~17 tok/s prompt (llama.cpp CPU: ~22 / ~42) |
| f16 attention cache, constant-size recurrent state | done | Qwen3.5-4B state at ctx 4096: 178 MiB |
| System-prompt snapshot (read once, reused) | done | 897-token prompt: 51 s first, 2.8 s after |
| GBNF + JSON-schema constrained output | done | agentd tool choice 10/10 on Qwen3.5-4B |
| HTTP server, drop-in for llama-server | done | 22/22 VM checks on Alpine aarch64 |
| Faster prompt reading (i8mm / repacked weights) | pending | |
| GPU (Vulkan) | pending | |
| LoRA adapters at inference | in progress | |

## The trainer (Phase 3, weight training)

| Milestone | Status | Measured |
|---|---|---|
| Backpropagation, dense Qwen2 + LoRA, gradient check | done | worst relative error 5% (h=0.05), most ≤1% (Qwen2.5-0.5B) |
| Training loop (AdamW, answer-only loss) | done | loss 4.29 → 0.0002 in 30 steps |
| Adapter save / load in agentos-llm | done | `AGLORA1`, `--lora`, `agentos-llm train` |
| Improvement on held-out requests (eval gate) | done | Qwen2.5-0.5B: 46% → 67% on 12 held-out phone requests, adapter kept |
| agentd: overnight on the charger, swap or roll back | done | e2e test passes |
| Qwen3 backward (per-head q/k norms) | done | gradient check on a tiny random model: worst 1.2% |
| Qwen3.5 backward (Gated DeltaNet + gated attention) | done | gradient check: tiny random model worst 2.0%; real Qwen3.5-4B Q4_K_M worst 2.4% |
| Activation checkpointing | done | same gradients as keeping everything; memory of one layer |

## Trainer update (2026-11-09, Qwen2.5-0.5B, Mac CPU)
- Backward pass verified against finite differences: worst relative error 5% (h=0.05), most entries ≤1%.
- AdamW, loss on answer tokens only: 4.29 → 0.0002 in 30 steps (4 short examples). Held-out phrasings went from loose text to `dial X` after training.
- Adapter save/load (`AGLORA1`), `agentos-llm --lora`, `agentos-llm train` (checkpoints, resume, keeps the adapter only if held-out score improves).
- agentd: overnight scheduling (Deep stage, charger, battery ≥50%, cool, ≥20 new confirmed requests), stop/resume, promote/rollback, ledger entries; e2e test passes.
- Real prompt (743 tokens): forward 14 s, forward+backward 30 s per example. The forward runs at the engine's own prompt speed (~50 tok/s), so the engine's prompt reading is the limit, not the trainer.
- Base 0.5B on 10 held-out tool requests: 60%. The full run to measure trained accuracy has not finished yet.
- Open: activation memory (2 GB at 743 tokens for 0.5B) needs checkpointing for 4B; Qwen3.5 DeltaNet backward is not written yet.

## Trainer update (2026-11-09, later)
- Qwen3.5 can be trained. Adapters sit on its attention layers (every fourth); the gradient flows back through the Gated DeltaNet layers between them: output projection, gated RMS norm, the delta-rule recurrence, L2-normed q/k, SiLU and the short causal convolution. Gated attention's output gate and the q/k norms (also Qwen3) have their backward passes.
- DeltaNet states are not kept for the backward pass (one d x d matrix per head per token, 2 MiB per token per layer on Qwen3.5-4B): a state is kept every 32 steps and each chunk is recomputed from it.
- Activation checkpointing: when an example's saved activations would pass the budget (default 256 MiB, `agentos-llm train --memory-mb`), only each layer's input is kept and the layer runs again on the way back. Measured on the tiny models: identical gradients. Estimated for 743 tokens: Qwen2.5-0.5B goes from 1 GB to ~65 MB of layer inputs plus one layer (~45 MB); Qwen3.5-4B needs ~245 MB of layer inputs plus one layer. The extra forward is about a third more compute; not yet timed on a real model.
- Gradient checks now run in `cargo test` with no download: tiny random Qwen2, Qwen3 and Qwen3.5 models are written as GGUF files by the test. Each checks the trainer's loss against the inference engine (token by token, adapter on), every adapter tensor against central differences, and checkpointed against kept gradients. Deliberately breaking the DeltaNet backward (β, α, or the state decay) fails the test.

## Trainer measured on real models (2026-11-09, x86-64 cloud VM, 4 cores, 15 GB)
- Gradient check on real weights (16 entries across first, middle and last attention layers): Qwen2.5-0.5B Q4_K_M worst relative error 3.4% (most ≤1%); Qwen3.5-4B Q4_K_M worst 2.4%. On Qwen3.5 the first attention layer's gradients pass back through every DeltaNet layer above it, so the DeltaNet backward is checked on real weights too.
- Loss, trainer vs engine on the same example: 0.5B 5.380 (exact) / 5.586 (8-bit) vs 5.392; 4B 7.668 / 7.521 vs 7.549. The gap is the 8-bit input rounding (it changes sign between models); the gradients match the trainer's own loss.
- Data: 60 phone requests with their tool calls (`crates/infer/examples/phone-requests.tsv`), made into agentd's own export with its real system prompt and grammar (`cargo run -p agentd --example prompt -- phone-requests.tsv DIR`). Every 5th is held out: 48 train, 12 judge.
- Training Qwen2.5-0.5B, rank 8, 30 steps of batch 2 (60 example passes, about 1.25 epochs): loss 0.35 → 0.03–0.07; held-out score 46% → 67%, adapter kept by the gate. 7,538 s in all, including judging before and after.
- Speed on this VM, 743-token example: forward 59 s, forward+backward 137 s keeping activations (986 MiB), 194 s recomputing layers (+42%). The forward is the engine's prompt-reading speed (~12 tok/s here), so faster prompt reading speeds training as much as answering.
- Still open: Qwen3.5-4B training run and its timing on a phone; the AdamW moments are not checkpointed, so a resumed job restarts them.

## Phase 4 on the simulated phone (2026-11-09)
First run (since replaced by the full scenario set below): 24 of 24 checks pass (run three times).
- Texts, incoming calls and missed calls are seen 5–115 ms after they happen, from ModemManager signals through a real dbus-monitor (the backup poll was every 60 s). With the monitor killed, a text was seen after ~1.6 s by the 2 s poll, and the monitor was back within 11 s.
- Approval key: a 300 ms press approved nothing; a 2.5 s hold approved the one waiting call, which then dialled once through mmcli; with two actions waiting, a hold approved neither.
- callaudiocli, fbcli and wpctl ran as the agentd-session user inside its session bus (started by `agentd-session.sh`); as root they fail, as the real tools do.
- Not simulated: PipeWire, callaudiod and feedbackd themselves (the tools only check that the session bus is reachable), a real modem's timing and its error cases.

## Every phone scenario on the simulated phone (2026-11-09)
`scripts/phonesim-scenarios.sh`: 145 of 145 checks pass (run twice), over six boots of agentd with `phone = "modemmanager"`:
1. boot: texts and calls already on the modem are not announced; 2. status (normal, no service, NetworkManager gone); 3. contacts (save, update, find, unknown, bad number, kept on disk); 4. texts in (signal, name or number, read, per person, one arriving in parts); 5. calls in (answer, call waiting, hang up, decline, missed, nothing to answer or end, call log); 6. calls out and the approval key (short press, other key, hold, nothing waiting, two waiting, press N, unknown name); 7. texts out (quotes and `&` reach the modem intact, never on a command line); 8. USSD, Wi-Fi (scan, wrong password, unknown network, join), mobile data, airplane mode (no scan, data, USSD or calls), no service; 9. location (GPS, cell only, none), torch, volume (0, 30, out of range, not a number), find my phone; 10. MCP client (approval by the key, open Wi-Fi, no token); 11. watcher killed and restarted; 12. modem and NetworkManager gone and back; 13. audio session gone and back; 14. session tools refuse root; 15. restart; 16. quiet hours; 17. daily alert budget; 18. no watcher; 19. no key device.

Found and fixed by these runs:
- A long text arriving in parts was announced half-finished ("Meet me at") and the whole text never was. The backend now leaves texts in state `receiving`, and the watcher wakes on the text's own PropertiesChanged (State) signal, so the whole text is announced once, as soon as it is complete.
- With the audio session down, an alert that never sounded was logged "(alerted)". It now says "(alert failed: ...)" and the ledger records the failed alert.
- "messages from mum" was taken by the code-first question layer and answered wrongly ("1 text came from numbers not in your contacts"); a phrase the phone knows exactly now goes to its tool first, as commands already did.
- Behaviour seen and left as is: a call or text to an unknown name still asks for approval and fails after it (the approval is used up); an approval is also used up by a call that fails (airplane mode); volume above 100 is refused, not clamped.

## Phase 5: bring-up on a real phone (started 2026-11-10)
No phone is reachable from the development machine yet, so this step prepared and checked everything that can be checked without one.
- The package's runtime dependencies were checked against Alpine edge aarch64 (package index and the packages' file lists). Every name exists, and every path agentd and the session script assume is right (`/usr/bin/dbus-monitor` in `dbus`, `/usr/libexec/feedbackd`, `/usr/bin/callaudiod`, `fbcli`, `wpctl`, `mmcli`, `nmcli`). **Found:** `callaudiocli` is its own package, not part of `callaudiod`: without it a real phone would never route call audio to the earpiece (agentd treats call audio as best effort, so silently). Added, with `feedbackd-udev` (LED and vibration device rules) and `evtest`.
- Vibration motors are granted by feedbackd's udev rules through `uaccess`, which needs a logind seat; the headless session user gets them through the `input` group instead, which the preflight checks on the device.
- `scripts/phone-preflight.sh` (busybox sh): tools, services, cgroup v2, modem and SIM, the session and its groups, the volume-down device (from `/proc/bus/input/devices` key bitmaps, 64- and 32-bit), the vibration motor's permissions, flash LED, battery; prints the config lines for the phone. Checked against PinePhone-style fixtures under busybox ash.
- `scripts/phone-scenarios.sh` (busybox sh): the guided run with a second phone; 46 checks. `--sim` runs it on the simulator: 46 passed, 1 skipped (preflight).
- **Found and fixed** while checking these: `agentd-session.sh` took up to 5 s to stop and did not wait for its session bus to exit, so a session started while the old one was stopping lost its bus socket (overlapping restarts: 0/5 sessions reachable before, 5/5 after; stop now ~0.1 s). The scenario scripts' "done" checks also matched "not done", which had hidden that failure; they now require the reply to start with "done".
- Next: run the two scripts on a phone and read the report.

## Phase 6: speed (started 2026-11-10)
**Training shares the system prompt across a batch.** A step's examples are packed into one pass: the prefix they share (agentd's system prompt) once, then each example's own tokens, which see the prefix and their own earlier tokens at the positions they would have alone; on Qwen3.5 each example's DeltaNet state and convolution window continue from the prefix's. The gradients into the shared part add up, so the result is exact, not an approximation: the tests check that packed losses equal each example alone and packed gradients equal their sum on tiny Qwen2, Qwen3 and Qwen3.5 models (prefix and an example crossing a DeltaNet chunk, with and without recomputed layers), and four deliberate breakages (an example seeing another, packed positions, a DeltaNet state not continued, its gradient not summed) each fail them.

Measured on Qwen2.5-0.5B with agentd's real prompt (x86-64 VM, 4 cores), one training step's forward and backward:

| batch | tokens one by one | tokens packed | one by one | packed | speed-up |
|---|---|---|---|---|---|
| 2 | 1,498 | 776 (722 shared) | 276 s | 120 s | 2.3x |
| 4 | 2,985 | 819 (722 shared) | 451 s | 127 s | 3.6x |

Losses identical to four decimals. A batch of 4 now costs about what one example did; the training job (`agentos-llm train`, agentd's overnight runs) uses it.
