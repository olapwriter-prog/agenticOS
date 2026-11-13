#!/bin/sh
# Phase 5, step 2: the scenarios only a real phone can answer, run on the phone with a person and
# a second phone. The script says when to text or ring this phone, when to answer or hang up on
# the other one, and when to hold volume-down; it asks what only a person can tell (did it
# vibrate, can you hear each other) and checks the rest itself. It writes a report to paste back.
#   sudo sh phone-scenarios.sh +91XXXXXXXXXX          the other phone's number
# Optional: WIFI_SSID, WIFI_PASS (join a network), USSD_CODE (your operator's balance code),
#   CONF (default /etc/agentd/agentd.toml), LOG (default /var/log/agentd.log), REPORT.
# --sim NUMBER: the same run on the simulated phone, the simulator playing the person (needs the
#   source tree, root, dbus-daemon and setpriv); this checks the script itself.
# POSIX sh (busybox ash).
SIM_MODE=""
[ "${1:-}" = --sim ] && { SIM_MODE=1; shift; }
OTHER=${1:-}
[ -n "$OTHER" ] || { echo "usage: $0 [--sim] OTHER_PHONE_NUMBER"; exit 2; }
REPORT=${REPORT:-./phase5-report.md}
here=$(cd "$(dirname "$0")" && pwd)

# ---- the simulated phone, for --sim ----
if [ -n "$SIM_MODE" ]; then
	ROOT=$(cd "$here/.." && pwd)
	W=${PHONESIM_WORK:-/tmp/agentd-phone5}
	(cd "$ROOT" && cargo build -q -p agentd -p agentos-phonesim) || exit 2
	AGENTD=$ROOT/target/debug/agentd
	SIM=$ROOT/target/debug/agentos-phonesim
	pkill -f "agentos-phonesim keep-input" 2>/dev/null
	rm -rf "$W" && mkdir -p "$W/sim" "$W/data"
	export PHONESIM_DIR="$W/sim" PHONESIM_REQUIRE_SESSION=1 AGENTD_PASSPHRASE=sim
	eval "$($SIM init)"
	id agentd-session >/dev/null 2>&1 || useradd -r -M -d /nonexistent -s /usr/sbin/nologin agentd-session
	SU=$(id -u agentd-session)
	mkdir -p "/run/user/$SU" && chown agentd-session: "/run/user/$SU" && chmod 700 "/run/user/$SU"
	start_session() {
		/usr/bin/setpriv --reuid="$SU" --regid="$(id -g agentd-session)" --clear-groups env -i PATH=/usr/bin:/bin \
			sh "$ROOT/os/postmarketos/agentd-session.sh" >>"$W/session.log" 2>&1 &
		SESSION=$!
		i=0
		while [ ! -S "/run/user/$SU/bus" ] && [ $i -lt 50 ]; do sleep 0.1; i=$((i + 1)); done
	}
	start_session
	CONF=$W/agentd.toml
	LOG=$W/agentd.log
	cat >"$CONF" <<EOF
data_dir = "$W/data"
listen = "127.0.0.1:18766"
kdf_mib = 1
idle_secs = 3600
phone = "modemmanager"
leds_dir = "$PHONESIM_DIR/leds"
phone_poll_secs = 2
dbus_monitor = "/usr/bin/dbus-monitor"
phone_signal_poll_secs = 60
session_user = "agentd-session"
approval_key = "$PHONESIM_DIR/input-event0"
quiet_start_min = 0
quiet_end_min = 0
daily_budget = 1000
EOF
	$AGENTD init -c "$CONF" >/dev/null
	start_agentd() {
		$AGENTD run -c "$CONF" >>"$LOG" 2>&1 &
		AGENTD_PID=$!
		i=0
		while ! $AGENTD ctl -c "$CONF" status >/dev/null 2>&1 && [ $i -lt 100 ]; do sleep 0.2; i=$((i + 1)); done
		sleep 3
	}
	start_agentd
else
	AGENTD=agentd
	CONF=${CONF:-/etc/agentd/agentd.toml}
	LOG=${LOG:-/var/log/agentd.log}
fi
DATA=$(sed -n 's/^data_dir *= *"\(.*\)".*/\1/p' "$CONF" | head -1)
LEDGER=${DATA:-/var/lib/agentd}/ledger.log
HW=${PHONESIM_DIR:-/nonexistent}/hw.log

ctl() { $AGENTD ctl -c "$CONF" "$@" 2>&1 | tr '\001' '\n'; }
chat() { ctl chat "$*"; }
count() { c=$(grep -c -- "$1" "$2" 2>/dev/null); echo "${c:-0}"; }
lines() { c=$(wc -l <"$1" 2>/dev/null); echo "${c:-0}"; }
since() { tail -n +"$(($2 + 1))" "$1" 2>/dev/null | grep -v -- "-> FAILED" | grep -q -- "$3"; } # since FILE N PATTERN (failed commands do not count)

pass=0
fail=0
skip=0
n=0
cat >"$REPORT" <<EOF
# Phase 5 report

- date: $(date)
- phone: $(cat /sys/firmware/devicetree/base/model 2>/dev/null | tr -d '\0' || uname -n)$( [ -n "$SIM_MODE" ] && echo " (simulated: --sim)")
- kernel: $(uname -r)
- agentd: $($AGENTD --version 2>/dev/null || echo unknown)

| # | scenario | result | detail |
|---|---|---|---|
EOF
result() { # result ok|fail|skip "scenario" "detail"
	n=$((n + 1))
	case $1 in
	ok) pass=$((pass + 1)); tag=PASS ;;
	fail) fail=$((fail + 1)); tag=FAIL ;;
	*) skip=$((skip + 1)); tag=SKIP ;;
	esac
	echo "  $tag  $2${3:+ — $3}"
	echo "| $n | $2 | $tag | $(echo "${3:-}" | tr '|\n' '/ ' | cut -c1-200) |" >>"$REPORT"
}
expect() { # expect "scenario" "text" "pattern"
	if echo "$2" | grep -qiE -- "$3"; then result ok "$1" "$(echo "$2" | head -1)"; else result fail "$1" "got: $(echo "$2" | head -2 | tr '\n' ' ')"; fi
}
section() { echo; echo "== $*"; }

# a step for the person (real phone) or the simulator (--sim)
human() { # human "instruction" sim-command...
	msg=$1
	shift
	if [ -n "$SIM_MODE" ]; then
		"$@" >/dev/null
	else
		printf '\n  >>> %s\n      press Enter when done (s + Enter to skip): ' "$msg"
		read -r a </dev/tty
		[ "$a" = s ] && return 1
	fi
	return 0
}
# a question only a person can answer; in --sim the simulator's record answers it
ask() { # ask "scenario" "question" sim-check-command...
	what=$1
	q=$2
	shift 2
	if [ -n "$SIM_MODE" ]; then
		if "$@" >/dev/null 2>&1; then result ok "$what" "(simulator record)"; else result fail "$what" "(simulator record)"; fi
		return
	fi
	printf '\n  ??? %s [y/n/s] ' "$q"
	read -r a </dev/tty
	case $a in
	y | Y) result ok "$what" "person: yes" ;;
	n | N) printf '      what happened? '; read -r why </dev/tty; result fail "$what" "person: no. $why" ;;
	*) result skip "$what" "person skipped" ;;
	esac
}
# wait for PATTERN to appear more than N times in FILE, up to SECS; prints the seconds taken
waitmore() { # waitmore PATTERN FILE SECS N
	t0=$(date +%s)
	while [ $(($(date +%s) - t0)) -le "$3" ]; do
		[ "$(count "$1" "$2")" -gt "$4" ] && { echo $(($(date +%s) - t0)); return 0; }
		sleep 0.5
	done
	return 1
}
key_hold() { human "Hold VOLUME-DOWN on this phone for 2 seconds, then let go" "$SIM" key 2500; sleep 1; }
key_tap() { human "Tap VOLUME-DOWN once, quickly (under half a second)" "$SIM" key 300; sleep 1; }
hw_since() { [ -n "$SIM_MODE" ] && since "$HW" "$1" "$2"; } # for the simulator's answers

echo "Phase 5 on $( [ -n "$SIM_MODE" ] && echo 'the simulated phone' || echo 'this phone'), other phone $OTHER; report: $REPORT"

# ============================================================================================
section "1. preflight"
if [ -n "$SIM_MODE" ]; then
	result skip "preflight" "not meaningful on the simulator"
elif [ -f "$here/phone-preflight.sh" ]; then
	if sh "$here/phone-preflight.sh" "$CONF" >"$REPORT.preflight" 2>&1; then
		result ok "preflight" "$(tail -n 1 "$REPORT.preflight")"
	else
		result fail "preflight" "$(grep -c FAIL "$REPORT.preflight") failed; see $REPORT.preflight"
	fi
else
	result skip "preflight" "phone-preflight.sh not next to this script"
fi

section "2. the network"
st=$(chat "phone status")
expect "registered on the network" "$st" "signal [0-9]+%"
expect "saving the other phone as a contact" "$(chat "save Tester as $OTHER")" "saved Tester"

section "3. a text in"
m=$(count "sms received" "$LEDGER")
l=$(lines "$LOG")
h=$(lines "$HW")
if human "From the other phone, text this phone: phase5 hello" "$SIM" sms "$OTHER" "phase5 hello"; then
	if s=$(waitmore "sms received" "$LEDGER" 120 "$m"); then
		result ok "text seen" "${s} s after you pressed Enter"
		since "$LOG" "$l" "phone (sms)" && result ok "seen by ModemManager's D-Bus signal" "" || result fail "seen by ModemManager's D-Bus signal" "found by the backup poll instead"
		ask "alerted with sound or vibration" "Did this phone play a sound or vibrate when the text came?" hw_since "$h" "fbcli -E message-new-sms"
		expect "read it back" "$(chat "read my messages")" "from Tester.*phase5 hello"
	else
		result fail "text seen" "nothing within 120 s"
	fi
else result skip "a text in" ""; fi

section "4. a long text in (several parts)"
LONG="phase5 long: this message is longer than one SMS so the network splits it in parts and the phone must put it back together before saying anything about it. LASTWORD"
m=$(count "sms received" "$LEDGER")
if human "From the other phone, text this phone this whole line (it must be over 160 characters):
      $LONG" sh -c "id=\$($SIM sms-parts $OTHER 'phase5 long: this message' | cut -d' ' -f2 | tr -d ,); sleep 1; $SIM sms-complete \$id 'is longer than one SMS so the network splits it in parts and the phone must put it back together before saying anything about it. LASTWORD'"; then
	if waitmore "sms received" "$LEDGER" 120 "$m" >/dev/null; then
		sleep 2
		if [ "$(count LASTWORD "$LOG")" -ge 1 ] && ! grep "phase5 long" "$LOG" | grep -qv LASTWORD; then
			result ok "announced once, whole" ""
		else
			result fail "announced once, whole" "$(grep 'phase5 long' "$LOG" | tail -2 | tr '\n' ' ')"
		fi
	else result fail "long text seen" "nothing within 120 s"; fi
else result skip "a long text in" ""; fi

section "5. a call in, answered"
m=$(count "incoming call" "$LEDGER")
h=$(lines "$HW")
if human "From the other phone, call this phone and let it ring" "$SIM" call "$OTHER"; then
	if s=$(waitmore "incoming call" "$LEDGER" 60 "$m"); then
		result ok "incoming call seen" "${s} s"
		ask "it rang or vibrated" "Is this phone ringing or vibrating?" hw_since "$h" "fbcli -E phone-incoming-call"
		h=$(lines "$HW")
		expect "answered by asking" "$(chat answer)" "answered Tester"
		ask "earpiece and microphone work" "Speak on both phones: can each side hear the other?" hw_since "$h" "callaudiocli -m 1"
		h=$(lines "$HW")
		expect "hung up by asking" "$(chat "hang up")" "ended call"
		ask "the other phone saw the call end" "Did the call end on the other phone?" hw_since "$h" "callaudiocli -m 0"
	else result fail "incoming call seen" "nothing within 60 s"; fi
else result skip "a call in" ""; fi

section "6. a missed call"
m=$(count "missed call" "$LEDGER")
if human "Call this phone from the other one, let it ring twice, then hang up on the other phone" sh -c "$SIM call $OTHER; sleep 2; $SIM hangup"; then
	if s=$(waitmore "missed call" "$LEDGER" 60 "$m"); then result ok "missed call recorded" "${s} s"; else result fail "missed call recorded" "nothing within 60 s"; fi
else result skip "a missed call" ""; fi

section "7. a call out, approved with the key"
expect "a call needs approval" "$(chat "call tester")" "approval required"
key_tap
expect "a quick tap approves nothing" "$(ctl pending)" "call to=tester"
key_hold
expect "a 2-second hold approves it" "$(ctl pending)" "nothing waiting"
h=$(lines "$HW")
expect "it dials" "$(chat "call tester")" "calling Tester"
ask "the other phone rings" "Is the other phone ringing?" hw_since "$h" "voice-create-call"
if human "Answer on the other phone" "$SIM" pickup; then
	ask "both sides hear each other" "Can each side hear the other?" hw_since "$h" "callaudiocli -m 1"
fi
expect "hang up" "$(chat "hang up")" "ended call"

section "8. a text out, approved with the key"
expect "a text needs approval" "$(chat "text tester phase5 from the agent, it's working")" "approval required"
key_hold
expect "sent once approved" "$(chat "text tester phase5 from the agent, it's working")" "sms sent to Tester"
ask "it arrived" "Did 'phase5 from the agent, it's working' arrive on the other phone, apostrophe intact?" true
ctl chat "call tester" >/dev/null
ctl chat "text tester two at once" >/dev/null
key_hold
expect "with two waiting, the key approves neither" "$(ctl pending | grep -c '^#')" "^2$"
for id in $(ctl pending | grep -o '^#[0-9]*' | tr -d '#'); do ctl decline "$id" >/dev/null; done

section "9. sound, torch"
h=$(lines "$HW")
expect "volume 30" "$(chat "volume 30")" "^done"
ask "the volume changed" "Play a sound (or ring this phone): is it quieter than before?" hw_since "$h" "wpctl set-volume @DEFAULT_AUDIO_SINK@ 0.30"
chat "volume 80" >/dev/null
expect "torch on" "$(chat "torch on")" "^done"
ask "the torch is on" "Is the torch (camera flash) lit?" sh -c "grep -qx 255 $PHONESIM_DIR/leds/white:flash/brightness"
expect "torch off" "$(chat "torch off")" "^done"

section "10. USSD, Wi-Fi, mobile data, airplane mode, location"
if [ -n "${USSD_CODE:-}" ] || [ -n "$SIM_MODE" ]; then
	code=${USSD_CODE:-*123#}
	r=$(chat "ussd $code")
	if echo "$r" | grep -qi "not done"; then result fail "USSD $code" "$r"; else result ok "USSD $code" "$r"; fi
else result skip "USSD" "set USSD_CODE to your operator's balance code"; fi
r=$(chat "scan wifi")
if echo "$r" | grep -q "%)"; then result ok "Wi-Fi scan" "$(echo "$r" | head -3 | tr '\n' ' ')"; else result fail "Wi-Fi scan" "$r"; fi
ssid=${WIFI_SSID:-}
pw=${WIFI_PASS:-}
[ -n "$SIM_MODE" ] && { ssid=HomeNet; pw=sunflower; }
if [ -n "$ssid" ]; then
	expect "join $ssid" "$(chat "join wifi $ssid password $pw")" "^done"
	expect "status shows Wi-Fi" "$(chat "phone status")" "wifi $ssid"
else result skip "join Wi-Fi" "set WIFI_SSID and WIFI_PASS"; fi
expect "mobile data on" "$(chat "mobile data on")" "^done"
expect "status shows mobile data" "$(chat "phone status")" "mobile data on"
expect "mobile data off" "$(chat "mobile data off")" "^done"
expect "airplane mode on" "$(chat "airplane mode on")" "^done"
expect "status: airplane mode" "$(chat "phone status")" "airplane mode"
expect "airplane mode off" "$(chat "airplane mode off")" "^done"
t0=$(date +%s)
reg=""
while [ $(($(date +%s) - t0)) -le 120 ]; do
	chat "phone status" | grep -q "signal [0-9]*%" && { reg=1; break; }
	sleep 2
done
[ -n "$reg" ] && result ok "back on the network after airplane mode" "$(($(date +%s) - t0)) s" || result fail "back on the network after airplane mode" "not registered within 120 s"
r=$(chat "where am i")
case $r in *"(gps)"*) result ok "location" "GPS: $r" ;; *"cell"*) result ok "location" "no GPS fix yet; cell: $r" ;; *) result fail "location" "$r" ;; esac

section "11. when the audio session restarts"
if [ -n "$SIM_MODE" ]; then
	kill "$SESSION" 2>/dev/null
	wait "$SESSION" 2>/dev/null
	start_session
else
	rc-service agentd-session restart >/dev/null 2>&1
	sleep 3
fi
m=$(count "sms received" "$LEDGER")
h=$(lines "$HW")
l=$(lines "$LOG")
if human "Text this phone again from the other one: phase5 after restart" "$SIM" sms "$OTHER" "phase5 after restart"; then
	if waitmore "sms received" "$LEDGER" 120 "$m" >/dev/null; then
		sleep 1
		if since "$LOG" "$l" "alert failed"; then result fail "alerts work after the session restarts" "$(grep 'alert failed' "$LOG" | tail -1)"
		else ask "alerts work after the session restarts" "Did this phone sound or vibrate for that text?" hw_since "$h" "fbcli -E message-new-sms"; fi
	else result fail "text after the session restart" "nothing within 120 s"; fi
else result skip "audio session restart" ""; fi

section "12. when agentd restarts"
if [ -n "$SIM_MODE" ]; then
	kill "$AGENTD_PID"
	wait "$AGENTD_PID" 2>/dev/null
	start_agentd
else
	rc-service agentd restart >/dev/null 2>&1
	i=0
	while ! ctl status >/dev/null 2>&1 && [ $i -lt 60 ]; do sleep 1; i=$((i + 1)); done
	sleep 3
fi
expect "contacts survive" "$(chat "what's Tester's number")" "Tester: "
expect "the ledger verifies" "$(ctl verify)" "ledger ok"
expect "the watcher is back" "$(pgrep -f 'dbus-monitor --system' >/dev/null && echo running)" "running"

# ============================================================================================
echo
echo "$pass passed, $fail failed, $skip skipped"
{
	echo
	echo "**$pass passed, $fail failed, $skip skipped**"
	echo
	echo "agentd log, last 40 lines:"
	echo '```'
	tail -n 40 "$LOG" 2>/dev/null
	echo '```'
} >>"$REPORT"
echo "report: $REPORT (paste it back to Claude)"
if [ -n "$SIM_MODE" ]; then
	kill "$AGENTD_PID" 2>/dev/null
	wait "$AGENTD_PID" 2>/dev/null
	kill "$SESSION" 2>/dev/null
	$SIM stop
fi
[ "$fail" = 0 ]
