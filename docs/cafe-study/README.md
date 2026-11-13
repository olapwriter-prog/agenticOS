# A café run by the phone: the 15-day study

`crates/business` lets agentd run a small business from a profile file (`examples/cafe/profile.toml`):
orders from every channel into one sealed book, bills, rider tracking and settlement, platform
reconciliation and claims, GST figures, a dashboard, and the next day's plan. `agentd` gains the
`biz_*` tools (when `business_profile` is set), the predictive cache (`crates/agentd/src/predict.rs`)
and `ctl metrics`.

Run the study again (about a minute; needs a llama-server for the few requests code cannot place):

```bash
llama-server -m <a 4B instruct model>.gguf --port 8091 -c 8192 -ngl 99
```
```bash
python3 scripts/mac/cafe-sim.py
```
```bash
python3 scripts/mac/cafe-video.py && swift scripts/mac/cafe-video.swift target/cafe-sim/frames target/cafe-sim/cafe-15-days.mp4
```

Results are written to `target/cafe-sim/results.json`. `cafe-15-days.mp4` here is the run of
13 Nov 2026; `sample-bill.pdf` is a bill as a customer receives it.

What is simulated: the shop, its customers and orders (a seeded generator), WhatsApp, the GST
portal and the platforms' partner systems. Prices, commission and GST rates in the profile are
assumptions to replace with the real ones.
