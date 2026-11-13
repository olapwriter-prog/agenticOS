#!/bin/bash
# Phase 7 on the simulated phone: the whole learning loop with the real engine and the real small
# model. The human asks for things and confirms them (intents, the key); agentd exports the
# confirmed requests; on the charger, asleep, it trains an adapter with agentos-llm, judges it on
# held-out requests, keeps or discards it, and serves it. Half-way the charger is pulled: training
# stops at its checkpoint (adapter and optimizer state) and continues when it is back.
# Needs root (the audio session user), dbus-daemon, setpriv, and the small model file.
#   sudo MODEL=/path/qwen2.5-0.5b-instruct-q4_k_m.gguf scripts/phonesim-train.sh
set -u
cd "$(dirname "$0")/.."
ROOT=$PWD
W=${PHONESIM_WORK:-/tmp/agentd-phonesim-train}
MODEL=${MODEL:?set MODEL to the small model file (.gguf)}
STEPS=${STEPS:-6}
[ "$(id -u)" = 0 ] || { echo "run as root"; exit 2; }
cargo build -q --release -p agentos-infer --bin agentos-llm || exit 2
cargo build -q -p agentd -p agentos-phonesim || exit 2
AGENTD=$ROOT/target/debug/agentd
SIM=$ROOT/target/debug/agentos-phonesim
LLM=$ROOT/target/release/agentos-llm

pass=0
fail=0
ok() { pass=$((pass + 1)); echo "  PASS  $*"; }
bad() { fail=$((fail + 1)); echo "  FAIL  $*"; }
check() { local what=$1; shift; if "$@" >/dev/null 2>&1; then ok "$what"; else bad "$what"; fi; }
ctl() { $AGENTD ctl -c "$W/agentd.toml" "$@" 2>&1 | tr '\001' '\n'; }
LEDGER=$W/data/ledger.log
# waitfor PATTERN SECS: the ledger gets the pattern within SECS; prints the seconds it took
waitfor() {
	local t0=$(date +%s)
	while [ $(($(date +%s) - t0)) -le "$2" ]; do
		grep -q -- "$1" "$LEDGER" 2>/dev/null && { echo $(($(date +%s) - t0)); return 0; }
		sleep 2
	done
	return 1
}

pkill -f "agentos-phonesim keep-input" 2>/dev/null
pkill -f "$W/" 2>/dev/null
rm -rf "$W" && mkdir -p "$W/sim" "$W/data"
export PHONESIM_DIR=$W/sim PHONESIM_REQUIRE_SESSION=1 AGENTD_PASSPHRASE=sim
eval "$($SIM init)"
id agentd-session >/dev/null 2>&1 || useradd -r -M -d /nonexistent -s /usr/sbin/nologin agentd-session
SU=$(id -u agentd-session)
mkdir -p "/run/user/$SU" && chown agentd-session: "/run/user/$SU" && chmod 700 "/run/user/$SU"
/usr/bin/setpriv --reuid="$SU" --regid="$(id -g agentd-session)" --clear-groups env -i PATH=/usr/bin:/bin \
	sh "$ROOT/os/postmarketos/agentd-session.sh" >"$W/session.log" 2>&1 &
SESSION=$!

cat >"$W/agentd.toml" <<EOF
data_dir = "$W/data"
listen = "127.0.0.1:18765"
kdf_mib = 1
idle_secs = 2
phone = "modemmanager"
leds_dir = "$PHONESIM_DIR/leds"
dbus_monitor = "/usr/bin/dbus-monitor"
session_user = "agentd-session"
approval_key = "$PHONESIM_DIR/input-event0"
quiet_start_min = 0
quiet_end_min = 0
daily_budget = 1000
llama_server = "$LLM"
model_timeout_secs = 600
model_wait_secs = 120
train_enabled = true
train_min_new = 12
train_min_battery = 50
train_steps = $STEPS
train_chunk_secs = 3600
[[models]]
name = "small"
role = "small"
file = "$MODEL"
port = 18850
ctx = 2048
EOF
$AGENTD init -c "$W/agentd.toml" >/dev/null
$AGENTD run -c "$W/agentd.toml" >"$W/agentd.log" 2>&1 &
AGENTD_PID=$!
for _ in $(seq 1 100); do $AGENTD ctl -c "$W/agentd.toml" status >/dev/null 2>&1 && break; sleep 0.2; done
sleep 2

echo "== 1. the human uses the phone; every confirmed request is a training example"
ctl chat "save Mum as +91 98765 43210" >/dev/null
ctl chat "save Ravi as +91 98123 45678" >/dev/null
for r in "call mum" "text ravi see you at 6" "call ravi" "text mum landed safely"; do
	ctl chat "$r" >/dev/null
	$SIM key 2500 >/dev/null
	sleep 0.5
	ctl chat "$r" >/dev/null
	ctl chat "hang up" >/dev/null 2>&1
done
for r in "what's Ravi's number" "read my messages" "missed calls" "torch on" "torch off" "volume 40" \
	"scan wifi" "airplane mode on" "airplane mode off" "mobile data on" "mobile data off" "where am i" "find my phone" "signal"; do
	ctl chat "$r" >/dev/null
done
st=$(ctl learn status)
echo "$st" | sed 's/^/     /' | head -4
check "not training yet: not on the charger" sh -c "echo '$st' | grep -q 'not on the charger'"

echo "== 2. on the charger and asleep, training starts with the real engine"
ctl power 90 charger >/dev/null
if s=$(waitfor "training small on" 120); then ok "training started after ${s} s (Deep stage, charger, enough new requests)"; else bad "training did not start"; fi
check "on at least 12 confirmed requests" sh -c "grep -oE 'training small on [0-9]+' $LEDGER | awk '{exit !(\$4 >= 12)}'"
check "the data is agentd's own export" test -s "$W/data/learn/sft.jsonl" -a -s "$W/data/learn/grammar.gbnf"
check "agentos-llm train is running" pgrep -f "agentos-llm train"
n_ex=$(grep -c . "$W/data/learn/sft.jsonl")
held=$((n_ex / 5))
train_n=$((n_ex - held))
echo "     $n_ex chats exported; $train_n to train, $held held out"

echo "== 3. the charger is pulled: training stops at its checkpoint, and continues when it is back"
# wait for the first step so there is a checkpoint
t0=$(date +%s)
while [ $(($(date +%s) - t0)) -le 1500 ] && ! grep -q "step 1/" "$W/data/learn/train.log" 2>/dev/null; do sleep 5; done
check "a step was done" grep -q "step 1/" "$W/data/learn/train.log"
ctl power 60 >/dev/null
if s=$(waitfor "training stopped" 60); then ok "stopped ${s} s after the charger was pulled"; else bad "did not stop"; fi
check "the checkpoint has the adapter and the optimizer state" test -s "$W/data/learn/adapter.new.ckpt" -a -s "$W/data/learn/adapter.new.ckpt.opt"
check "the trainer process is gone" sh -c "! pgrep -f 'agentos-llm train'"
ctl power 90 charger >/dev/null
if s=$(waitfor "training small on" 120 >/dev/null) && [ "$(grep -c 'training small on' "$LEDGER")" -ge 2 ]; then ok "training resumed"; else bad "did not resume"; fi
sleep 20
check "it continued from the checkpoint, not from scratch" grep -q "continuing from step" "$W/data/learn/train.log"

echo "== 4. the result: judged on held-out requests, kept or discarded, served if kept"
if s=$(waitfor "training kept\|training discarded\|training failed" 7200); then
	line=$(grep -E "training (kept|discarded|failed)" "$LEDGER" | tail -1 | sed 's/^[^ ]* //')
	case $line in
	*failed*) bad "training finished: $line" ;;
	*) ok "training finished after ${s} s: $line" ;;
	esac
	if echo "$line" | grep -q kept; then
		check "the adapter is on disk" test -s "$W/data/learn/adapter.lora"
		for _ in $(seq 1 60); do ctl learn status | grep -q "adapter in use: .*adapter.lora" && break; sleep 2; done
		check "the model is served with it" sh -c "$AGENTD ctl -c $W/agentd.toml learn status | grep -q 'adapter in use: .*adapter.lora'"
		check "the model answers with the adapter loaded" sh -c "$AGENTD ctl -c $W/agentd.toml chat 'could you ring my mother' | grep -qi 'approval required\|calling'"
		check "rollback works" sh -c "$AGENTD ctl -c $W/agentd.toml learn rollback | grep -qi 'rolled back\|no adapter'"
	else
		echo "     not kept: the gate judged the adapter no better; with $STEPS steps on $train_n examples that can happen"
	fi
else
	bad "training did not finish within 2 h"
fi
echo "     train.log:"; sed 's/^/       /' "$W/data/learn/train.log" | tail -12

kill $AGENTD_PID 2>/dev/null; wait $AGENTD_PID 2>/dev/null
kill $SESSION 2>/dev/null
$SIM stop
echo
echo "$pass passed, $fail failed   (logs: $W/agentd.log $W/data/learn/train.log)"
[ $fail = 0 ]
