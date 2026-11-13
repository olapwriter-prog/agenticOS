#!/usr/bin/env bash
# A virtual agenticOS phone on this Mac: agentd with the simulated phone (no modem, no VM), its
# MCP endpoint, and the agentos-ui window attached to it. For the real service in an Alpine VM,
# see scripts/vm-test.sh.
#
#   scripts/mac/virtual-phone.sh up              build, start agentd, open the window
#   scripts/mac/virtual-phone.sh down            stop agentd and the window
#   scripts/mac/virtual-phone.sh status          is it running, stage, phone, pending approvals
#   scripts/mac/virtual-phone.sh chat "call mum" say something to the phone
#   scripts/mac/virtual-phone.sh press 1         the approval key
#   scripts/mac/virtual-phone.sh sms +919876543210 "dinner at 8?"   the network delivers a text
#   scripts/mac/virtual-phone.sh call +919876543210                 ... or a call
#   scripts/mac/virtual-phone.sh ctl <anything>  any agentd ctl command (pending, phone, prio, ...)
#   scripts/mac/virtual-phone.sh token           the MCP bearer token
#   scripts/mac/virtual-phone.sh logs            follow agentd's log
#   scripts/mac/virtual-phone.sh ui              open the window again
#   scripts/mac/virtual-phone.sh reset           stop and delete the phone's data (vault, ledger, contacts)
#
# Settings (environment variables):
#   PHONE_DIR=target/virtual-phone   data, config, log, pid files (target/ is git-ignored)
#   PORT=8765                        MCP endpoint on 127.0.0.1
#   AGENTD_PASSPHRASE=dev            vault passphrase (a dev phone; not for real secrets)
#   SMALL_URL=http://127.0.0.1:8080  a llama-server already running for the model (default: none,
#                                    so only fixed phrases work; free-form requests need a model)
#   NO_UI=1                          do not open the window
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
DIR="${PHONE_DIR:-$ROOT/target/virtual-phone}"
mkdir -p "$DIR"; DIR="$(cd "$DIR" && pwd)"
PORT="${PORT:-8765}"
export AGENTD_PASSPHRASE="${AGENTD_PASSPHRASE:-dev}"
CFG="$DIR/agentd.toml"; LOG="$DIR/agentd.log"; PID="$DIR/agentd.pid"; UIPID="$DIR/ui.pid"
SOCK="$DIR/control.sock"
AGENTD="$ROOT/target/release/agentd"
UI="$ROOT/ui/target/release/agentos-ui"

say() { printf '== %s\n' "$*"; }
die() { echo "virtual-phone: $*" >&2; exit 1; }
alive() { [[ -f "$1" ]] && kill -0 "$(cat "$1")" 2>/dev/null; }
ctl() { "$AGENTD" ctl -c "$CFG" "$@"; }
need_up() { alive "$PID" || die "not running (start it: scripts/mac/virtual-phone.sh up)"; }

write_config() {
	cat >"$CFG" <<EOF
# written by scripts/mac/virtual-phone.sh; edit agentd.example.toml for the meaning of each key
data_dir = "$DIR/data"
listen = "127.0.0.1:$PORT"
control_socket = "$SOCK"
phone = "sim"
real_hardware = false
commercial_enabled = false
online = true
kdf_mib = 8
EOF
	[[ -n "${SMALL_URL:-}" ]] && echo "small_url = \"$SMALL_URL\"" >>"$CFG"
	return 0
}

open_ui() {
	[[ -n "${NO_UI:-}" ]] && return 0
	alive "$UIPID" && { echo "window already open"; return 0; }
	say "building agentos-ui"
	(cd ui && cargo build --release -q) || die "agentos-ui did not build"
	AGENTD_SOCK="$SOCK" nohup "$UI" >"$DIR/ui.log" 2>&1 &
	echo $! >"$UIPID"
	echo "window open (log: $DIR/ui.log)"
}

up() {
	if alive "$PID"; then
		echo "already running (pid $(cat "$PID"))"
		open_ui
		return
	fi
	if lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
		die "port $PORT is in use by another program (pick another: PORT=8766 scripts/mac/virtual-phone.sh up)"
	fi
	say "building agentd"
	cargo build --release -q -p agentd || die "agentd did not build"
	write_config
	# creates the vault the first time; afterwards it opens it and prints the same token
	"$AGENTD" init -c "$CFG" | tail -1 >"$DIR/token"
	chmod 600 "$DIR/token"
	rm -f "$SOCK"
	say "starting agentd (simulated phone)"
	nohup "$AGENTD" run -c "$CFG" >>"$LOG" 2>&1 &
	echo $! >"$PID"
	for _ in $(seq 1 50); do
		ctl stage >/dev/null 2>&1 && break
		alive "$PID" || { tail -20 "$LOG"; die "agentd stopped while starting"; }
		sleep 0.2
	done
	ctl stage >/dev/null 2>&1 || { tail -20 "$LOG"; die "agentd did not answer on $SOCK"; }
	open_ui
	cat <<EOF

The virtual phone is up.
  MCP:     http://127.0.0.1:$PORT/mcp   (bearer token: scripts/mac/virtual-phone.sh token)
  control: $SOCK
  stage:   $(ctl stage)
Try:
  scripts/mac/virtual-phone.sh chat "save Mum as +919876543210"
  scripts/mac/virtual-phone.sh chat "call mum"     # waits for the approval key
  scripts/mac/virtual-phone.sh press 1
  scripts/mac/virtual-phone.sh sms +919876543210 "dinner at 8?"
  scripts/mac/virtual-phone.sh chat "read my messages"
Stop it: scripts/mac/virtual-phone.sh down
EOF
}

stop_pid() {
	local f="$1" name="$2"
	if alive "$f"; then
		local p; p="$(cat "$f")"
		kill "$p" 2>/dev/null || true
		for _ in $(seq 1 50); do kill -0 "$p" 2>/dev/null || break; sleep 0.1; done
		kill -0 "$p" 2>/dev/null && kill -9 "$p" 2>/dev/null || true
		echo "$name stopped"
	fi
	rm -f "$f"
}

down() {
	stop_pid "$UIPID" "window"
	stop_pid "$PID" "agentd"
	rm -f "$SOCK"
}

status() {
	if ! alive "$PID"; then echo "virtual phone: down"; return 1; fi
	echo "virtual phone: up (agentd pid $(cat "$PID"); window: $(alive "$UIPID" && echo open || echo closed))"
	echo "MCP: http://127.0.0.1:$PORT/mcp"
	echo "stage: $(ctl stage)"
	echo "-- phone";   ctl phone
	echo "-- pending"; ctl pending
}

[[ "$(uname -s)" == "Darwin" ]] || echo "note: written for macOS; it should work on Linux too" >&2
cmd="${1:-up}"; shift || true
case "$cmd" in
	up) up ;;
	down|stop) down ;;
	status) status ;;
	chat) need_up; ctl chat "$*" ;;
	press) need_up; ctl press "${1:?which pending action? see: scripts/mac/virtual-phone.sh ctl pending}" ;;
	sms) need_up; [[ $# -ge 2 ]] || die "usage: sms FROM TEXT"; from="$1"; shift; ctl "sim-sms $from|$*" ;;
	call) need_up; ctl sim-call "${1:?usage: call FROM}" ;;
	ctl) need_up; ctl "$@" ;;
	token) [[ -f "$DIR/token" ]] && cat "$DIR/token" || die "no token yet (run: up)" ;;
	logs) exec tail -f "$LOG" ;;
	ui) need_up; open_ui ;;
	reset) down; rm -rf "$DIR"; echo "deleted $DIR" ;;
	*) die "unknown command: $cmd (up | down | status | chat | press | sms | call | ctl | token | logs | ui | reset)" ;;
esac
