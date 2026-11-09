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
