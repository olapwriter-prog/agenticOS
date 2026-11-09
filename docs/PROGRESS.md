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
| Backpropagation, dense Qwen2 + LoRA, gradient check | in progress | |
| Training loop (AdamW, answer-only loss) | pending | |
| Adapter save / load in agentos-llm | pending | |
| Improvement on held-out requests (eval gate) | pending | |
| agentd: overnight on the charger, swap or roll back | pending | |
| Qwen3.5 DeltaNet backward | pending | |

## Trainer update (2026-11-09, Qwen2.5-0.5B, Mac CPU)
- Backward pass verified against finite differences: worst relative error 5% (h=0.05), most entries ≤1%.
- AdamW, loss on answer tokens only: 4.29 → 0.0002 in 30 steps (4 short examples). Held-out phrasings went from loose text to `dial X` after training.
- Adapter save/load (`AGLORA1`), `agentos-llm --lora`, `agentos-llm train` (checkpoints, resume, keeps the adapter only if held-out score improves).
- agentd: overnight scheduling (Deep stage, charger, battery ≥50%, cool, ≥20 new confirmed requests), stop/resume, promote/rollback, ledger entries; e2e test passes.
- Real prompt (743 tokens): forward 14 s, forward+backward 30 s per example. The forward runs at the engine's own prompt speed (~50 tok/s), so the engine's prompt reading is the limit, not the trainer.
- Base 0.5B on 10 held-out tool requests: 60%. The full run to measure trained accuracy has not finished yet.
- Open: activation memory (2 GB at 743 tokens for 0.5B) needs checkpointing for 4B; Qwen3.5 DeltaNet backward is not written yet.
