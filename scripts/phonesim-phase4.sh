#!/bin/sh
# Run Phase 4 against the simulated phone (crates/phonesim): agentd with its real hardware
# backend (phone = "modemmanager"), the D-Bus watcher, the approval key and the audio session,
# all on this machine. Needs root (it makes the agentd-session user and runs as it), dbus-daemon,
# dbus-monitor, dbus-send and setpriv.
#   sudo scripts/phonesim-phase4.sh
set -u
cd "$(dirname "$0")/.."
ROOT=$PWD
W=${PHONESIM_WORK:-/tmp/agentd-phonesim}
[ "$(id -u)" = 0 ] || { echo "run as root"; exit 2; }
for t in dbus-daemon dbus-monitor dbus-send setpriv; do
	command -v $t >/dev/null || { echo "missing $t"; exit 2; }
done

cargo build -q -p agentd -p agentos-phonesim || exit 2
AGENTD=$ROOT/target/debug/agentd
SIM=$ROOT/target/debug/agentos-phonesim

# ---- the phone ----
pkill -f "$W/" 2>/dev/null
rm -rf "$W" && mkdir -p "$W/sim" "$W/data"
export PHONESIM_DIR=$W/sim
eval "$($SIM init)"
export PHONESIM_REQUIRE_SESSION=1

# ---- the audio session, as the agentd-session user (what /etc/init.d/agentd-session does) ----
id agentd-session >/dev/null 2>&1 || useradd -r -M -d /nonexistent -s /usr/sbin/nologin agentd-session
SU=$(id -u agentd-session)
SG=$(id -g agentd-session)
mkdir -p /run/user/$SU && chown agentd-session:agentd-session /run/user/$SU && chmod 700 /run/user/$SU
setpriv --reuid=$SU --regid=$SG --clear-groups env -i PATH=/usr/bin:/bin \
	sh "$ROOT/os/postmarketos/agentd-session.sh" >"$W/session.log" 2>&1 &
SESSION=$!

# ---- agentd ----
cat >"$W/agentd.toml" <<EOF
data_dir = "$W/data"
listen = "127.0.0.1:18765"
kdf_mib = 1
phone = "modemmanager"
leds_dir = "$PHONESIM_DIR/leds"
phone_poll_secs = 2
dbus_monitor = "/usr/bin/dbus-monitor"
phone_signal_poll_secs = 60
session_user = "agentd-session"
approval_key = "$PHONESIM_DIR/input-event0"
approval_keycode = 114
approval_hold_ms = 2000
EOF
export AGENTD_PASSPHRASE=sim
$AGENTD init -c "$W/agentd.toml" >/dev/null
$AGENTD run -c "$W/agentd.toml" >"$W/agentd.log" 2>&1 &
AGENTD_PID=$!

ctl() { $AGENTD ctl -c "$W/agentd.toml" "$@" | tr '\001' '\n'; }
LEDGER=$W/data/ledger.log
pass=0
fail=0
ok() { pass=$((pass + 1)); echo "PASS  $*"; }
bad() { fail=$((fail + 1)); echo "FAIL  $*"; }
check() { # check "what" command...
	what=$1
	shift
	if "$@" >/dev/null 2>&1; then ok "$what"; else bad "$what"; fi
}
# waitfor PATTERN FILE SECS: wait for a grep pattern in a file; prints the milliseconds it took
waitfor() {
	t0=$(date +%s%N)
	end=$(($(date +%s) + $3))
	while [ "$(date +%s)" -le $end ]; do
		if grep -q "$1" "$2" 2>/dev/null; then
			echo $((($(date +%s%N) - t0) / 1000000))
			return 0
		fi
		sleep 0.1
	done
	return 1
}
count() { grep -c "$1" "$2" 2>/dev/null || true; }
# waitmore PATTERN FILE SECS N: wait until the pattern appears more than N times; prints the ms
waitmore() {
	t0=$(date +%s%N)
	end=$(($(date +%s) + $3))
	while [ "$(date +%s)" -le $end ]; do
		if [ "$(count "$1" "$2")" -gt "$4" ]; then
			echo $((($(date +%s%N) - t0) / 1000000))
			return 0
		fi
		sleep 0.1
	done
	return 1
}
session_ran() { grep -q "^uid=$SU $1" "$PHONESIM_DIR/hw.log"; }
state() { $SIM state | tr -d ' \n'; }

for i in $(seq 1 100); do ctl status >/dev/null 2>&1 && break; sleep 0.2; done
sleep 3 # the first poll learns what is already there; the watcher starts

echo "== the phone, through mmcli and nmcli"
check "status from the modem and NetworkManager" sh -c "$AGENTD ctl -c $W/agentd.toml phone | grep -q SimTel"
check "the D-Bus watcher is running (dbus-monitor on the system bus)" pgrep -f "dbus-monitor --system"

echo "== incoming texts and calls arrive by D-Bus signal (backup poll: every 60 s)"
n=$(count "sms received" "$LEDGER")
$SIM sms +919876543210 dinner at 8 >/dev/null
if ms=$(waitmore "sms received" "$LEDGER" 5 "$n"); then
	ok "text seen ${ms} ms after it arrived"
else bad "text not seen within 5 s"; fi
sleep 0.5
check "alerted through feedbackd, run in the session as uid $SU" session_ran "fbcli -E message-new-sms"
$SIM call +919812345678 >/dev/null
if ms=$(waitfor "incoming call" "$LEDGER" 5); then ok "incoming call seen ${ms} ms after it rang"; else bad "incoming call not seen"; fi
sleep 0.5
check "ringtone and vibration through feedbackd" session_ran "fbcli -E phone-incoming-call"
$SIM hangup >/dev/null
if ms=$(waitfor "missed call" "$LEDGER" 5); then ok "missed call seen ${ms} ms after the caller hung up (Call StateChanged)"; else bad "missed call not seen"; fi

echo "== the approval key (volume-down held 2 s, on the input device)"
ctl chat "save Mum as +91 98765 43210" >/dev/null
check "calling needs approval" sh -c "$AGENTD ctl -c $W/agentd.toml chat 'call mum' | grep -q 'approval required'"
$SIM key 300 >/dev/null
sleep 1
check "a short press does not approve" sh -c "! grep -q 'with the approval key' $LEDGER"
$SIM key 2500 >/dev/null
if ms=$(waitfor "with the approval key" "$LEDGER" 5); then ok "a 2.5 s hold approved the waiting call"; else bad "the hold did not approve"; fi
out=$(ctl chat "call mum")
case $out in *"calling Mum"*) ok "the approved call dials: $out" ;; *) bad "call after approval: $out" ;; esac
check "dialled through ModemManager" grep -q "mmcli -o [0-9]* --start" "$PHONESIM_DIR/hw.log"
check "call audio routed to the earpiece by callaudiod, in the session" session_ran "callaudiocli -m 1"
check "one approval, one call" sh -c "$AGENTD ctl -c $W/agentd.toml chat 'call mum' | grep -q 'approval required'"
ctl decline 2 >/dev/null 2>&1
$SIM pickup >/dev/null
out=$(ctl chat "hang up")
case $out in *"ended call"*) ok "hung up: $out" ;; *) bad "hang up: $out" ;; esac
check "call audio back to normal" session_ran "callaudiocli -m 0"
ctl chat "call mum" >/dev/null
ctl chat "text mum see you soon" >/dev/null
before=$(count "approved #" "$LEDGER")
$SIM key 2500 >/dev/null
if waitfor "actions waiting; none approved" "$LEDGER" 5 >/dev/null && [ "$(count "approved #" "$LEDGER")" = "$before" ]; then
	ok "with two actions waiting, the key approves neither"
else bad "two waiting: the key approved something"; fi

echo "== volume and torch"
ctl chat "volume 30" >/dev/null
check "volume set by WirePlumber, in the session" session_ran "wpctl set-volume @DEFAULT_AUDIO_SINK@ 0.30"
check "volume is 0.3" sh -c "$SIM state | grep -q '\"volume\": 0.3'"
ctl chat "torch on" >/dev/null
check "flash LED at full brightness" grep -qx 255 "$PHONESIM_DIR/leds/white:flash/brightness"
ctl chat "torch off" >/dev/null

echo "== the session is really needed"
check "as root (not the session user) the session tools fail, as the real ones do" \
	sh -c "! DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$SU/bus $PHONESIM_DIR/bin/fbcli -E test"

echo "== when the watcher dies, polling takes over; it comes back"
pkill -f "dbus-monitor --system"
sleep 0.5
n=$(count "sms received" "$LEDGER")
$SIM sms +919876543210 still there? >/dev/null
if ms=$(waitmore "sms received" "$LEDGER" 6 "$n"); then
	ok "text seen ${ms} ms after it arrived, by the 2 s poll"
else bad "text not seen while the watcher was down"; fi
sleep 11
check "the watcher was restarted" pgrep -f "dbus-monitor --system"

echo "== the ledger"
check "ledger chain verifies" sh -c "$AGENTD ctl -c $W/agentd.toml verify | grep -q 'ledger ok'"

kill $AGENTD_PID 2>/dev/null
wait $AGENTD_PID 2>/dev/null
kill $SESSION 2>/dev/null
$SIM stop
echo
echo "$pass passed, $fail failed   (logs: $W/agentd.log $W/session.log $PHONESIM_DIR/hw.log)"
[ $fail = 0 ]
