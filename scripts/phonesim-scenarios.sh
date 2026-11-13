#!/bin/bash
# Every phone scenario, end to end, on the simulated phone (crates/phonesim): agentd with its real
# hardware backend (phone = "modemmanager"), ModemManager signals over D-Bus, the approval key,
# the audio session as its own user, and the failure cases. Several boots: normal, quiet hours,
# the daily alert budget, no D-Bus watcher, no key device, and a restart.
# Needs root, dbus-daemon, dbus-monitor, dbus-send and setpriv.
#   sudo scripts/phonesim-scenarios.sh            all scenarios
#   sudo scripts/phonesim-scenarios.sh -v         also print each reply
set -u
cd "$(dirname "$0")/.."
ROOT=$PWD
VERBOSE=${1:-}
W=${PHONESIM_WORK:-/tmp/agentd-phonesim}
[ "$(id -u)" = 0 ] || { echo "run as root"; exit 2; }
for t in dbus-daemon dbus-monitor dbus-send setpriv; do
	command -v $t >/dev/null || { echo "missing $t"; exit 2; }
done
cargo build -q -p agentd -p agentos-phonesim || exit 2
AGENTD=$ROOT/target/debug/agentd
SIM=$ROOT/target/debug/agentos-phonesim
export AGENTD_PASSPHRASE=sim
MUM=+919876543210
RAVI=+919812345678
STRANGER=+918000012345

pass=0
fail=0
FAILED=""
ok() { pass=$((pass + 1)); echo "  PASS  $*"; }
bad() { fail=$((fail + 1)); FAILED="$FAILED
  $*"; echo "  FAIL  $*"; }
section() { echo "== $*"; }

# ---- plumbing ----
start_session() {
	id agentd-session >/dev/null 2>&1 || useradd -r -M -d /nonexistent -s /usr/sbin/nologin agentd-session
	SU=$(id -u agentd-session)
	SG=$(id -g agentd-session)
	mkdir -p /run/user/$SU && chown agentd-session:agentd-session /run/user/$SU && chmod 700 /run/user/$SU
	setpriv --reuid=$SU --regid=$SG --clear-groups env -i PATH=/usr/bin:/bin \
		sh "$ROOT/os/postmarketos/agentd-session.sh" >>"$W/session.log" 2>&1 &
	SESSION=$!
	for _ in $(seq 1 50); do [ -S /run/user/$SU/bus ] && break; sleep 0.1; done
}
stop_session() {
	kill $SESSION 2>/dev/null
	wait $SESSION 2>/dev/null
}
new_phone() {
	[ -n "${PHONESIM_DIR:-}" ] && $SIM stop 2>/dev/null
	rm -rf "$W/sim" && mkdir -p "$W/sim"
	export PHONESIM_DIR=$W/sim
	eval "$($SIM init)"
	export PHONESIM_REQUIRE_SESSION=1
}
# boot NAME [extra toml lines]: a fresh data dir unless NAME is "restart"
boot() {
	local name line key
	name=$1
	shift
	[ "$name" = restart ] || { rm -rf "$W/data" && mkdir -p "$W/data"; }
	cat >"$W/agentd.toml" <<EOF
data_dir = "$W/data"
listen = "127.0.0.1:18765"
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
	for line in "$@"; do
		key=${line%% =*}
		sed -i "/^$key =/d" "$W/agentd.toml"
		echo "$line" >>"$W/agentd.toml"
	done
	[ "$name" = restart ] || $AGENTD init -c "$W/agentd.toml" >/dev/null
	$AGENTD run -c "$W/agentd.toml" >>"$W/agentd.log" 2>&1 &
	AGENTD_PID=$!
	for _ in $(seq 1 100); do $AGENTD ctl -c "$W/agentd.toml" status >/dev/null 2>&1 && break; sleep 0.2; done
	sleep 3 # the first poll learns what is already there; the watcher starts
}
halt() {
	kill $AGENTD_PID 2>/dev/null
	wait $AGENTD_PID 2>/dev/null
}
ctl() { $AGENTD ctl -c "$W/agentd.toml" "$@" 2>&1 | tr '\001' '\n'; }
LEDGER() { echo "$W/data/ledger.log"; }
count() { grep -c -- "$1" "$2" 2>/dev/null || true; }
# waitmore PATTERN FILE SECS N: until PATTERN appears more than N times; prints the ms taken
waitmore() {
	local t0 end
	t0=$(date +%s%N)
	end=$(($(date +%s) + $3))
	while [ "$(date +%s)" -le "$end" ]; do
		if [ "$(count "$1" "$2")" -gt "$4" ]; then
			echo $((($(date +%s%N) - t0) / 1000000))
			return 0
		fi
		sleep 0.05
	done
	return 1
}
# say "what" "chat text" "expected (grep -E, case-insensitive)"
say() {
	local out
	out=$(ctl chat "$2")
	[ -n "$VERBOSE" ] && echo "        > $2  =>  $(echo "$out" | head -3 | tr '\n' '|')"
	if echo "$out" | grep -qiE -- "$3"; then ok "$1"; else bad "$1: got: $(echo "$out" | head -2 | tr '\n' ' ')"; fi
}
expect() { # expect "what" "text" "pattern"
	if echo "$2" | grep -qiE -- "$3"; then ok "$1"; else bad "$1: got: $(echo "$2" | head -2 | tr '\n' ' ')"; fi
}
check() { # check "what" command...
	local what=$1
	shift
	if "$@" >/dev/null 2>&1; then ok "$what"; else bad "$what"; fi
}
hw() { grep -cE -- "$1" "$PHONESIM_DIR/hw.log" 2>/dev/null || true; }
# hw_after N PATTERN: PATTERN in hw.log after line N (what a step ran)
hw_after() { tail -n +"$(($1 + 1))" "$PHONESIM_DIR/hw.log" | grep -qE -- "$2"; }
hwlines() { wc -l <"$PHONESIM_DIR/hw.log"; }
announced() { grep -c -- "$1" "$W/agentd.log" 2>/dev/null || true; }
# an event the network causes, seen in the ledger within SECS: arrive "what" PATTERN SECS sim-args...
arrive() {
	local what pat secs n ms
	what=$1
	pat=$2
	secs=$3
	shift 3
	n=$(count "$pat" "$(LEDGER)")
	$SIM "$@" >/dev/null
	if ms=$(waitmore "$pat" "$(LEDGER)" "$secs" "$n"); then ok "$what (${ms} ms)"; else bad "$what: not seen in ${secs} s"; fi
}
no_new() { # no_new "what" PATTERN SECS sim-args...: nothing new within SECS
	local what pat secs n
	what=$1
	pat=$2
	secs=$3
	shift 3
	n=$(count "$pat" "$(LEDGER)")
	$SIM "$@" >/dev/null
	if waitmore "$pat" "$(LEDGER)" "$secs" "$n" >/dev/null; then bad "$what"; else ok "$what"; fi
}
approve() { $SIM key 2500 >/dev/null; sleep 0.5; }
pending_count() { ctl pending | grep -c '^#' || true; }
decline_all() { for id in $(ctl pending | grep -o '^#[0-9]*' | tr -d '#'); do ctl decline "$id" >/dev/null; done; }

rm -rf "$W" && mkdir -p "$W"
pkill -f "agentos-phonesim keep-input" 2>/dev/null
new_phone
start_session

# ============================================================================================
section "1. boot: what was on the modem before agentd started is not announced"
$SIM sms $MUM "old text from before boot" >/dev/null
$SIM call $STRANGER >/dev/null
$SIM hangup >/dev/null
boot main
check "no alert for the old text or call" sh -c "! grep -q 'phone:' $(LEDGER)"
check "the D-Bus watcher started" pgrep -f "dbus-monitor --system"

section "2. status"
say "status: operator, technology, signal, data, wifi" "phone status" "SimTel lte signal 72%, mobile data off, wifi off"
$SIM service off
say "status: no service" "signal" "no service"
$SIM service on
$SIM fail nmcli on
say "status without NetworkManager still reports the modem" "signal" "SimTel"
$SIM fail nmcli off

section "3. contacts"
say "save a contact" "save Mum as +91 98765 43210" "saved Mum as \\$MUM"
say "save another" "save Ravi as 98123 45678" "saved Ravi"
say "update a number" "save Ravi as +91 98123 45678" "saved Ravi as \\$RAVI"
say "find by name" "what's Ravi's number" "Ravi: \\$RAVI"
say "unknown name" "what's Bob's number" "no contact called Bob"
say "not a number" "save Bob as 12" "not a phone number"
out=$(ctl chat "list contacts" ; ctl chat "phone status")
check "contacts are kept on disk" grep -q Ravi "$W/data/contacts.json"

section "4. texts in"
arrive "a text arrives: seen by D-Bus signal" "sms received" 5 sms $MUM "dinner at 8?"
sleep 0.5
check "announced with the contact's name" grep -qF "sms: dinner at 8? from Mum ($MUM) (alerted)" "$W/agentd.log"
check "alerted through feedbackd, as the session user" grep -q "^uid=$SU fbcli -E message-new-sms" "$PHONESIM_DIR/hw.log"
arrive "a text from a stranger" "sms received" 5 sms $STRANGER "your parcel is here"
sleep 0.5
check "a stranger is shown by number" grep -qF "your parcel is here from $STRANGER (alerted)" "$W/agentd.log"
say "read my messages" "read my messages" "from Mum \(\\$MUM\): dinner at 8\?"
say "messages from one person" "messages from mum" "dinner at 8\?"
check "messages from one person leave out others" sh -c "! $AGENTD ctl -c $W/agentd.toml chat 'messages from mum' | grep -q parcel"
say "messages from an unknown name" "messages from bob" "no number for bob"
n=$(announced "the station")
PART=$($SIM sms-parts $RAVI "Meet me at" | awk '{print $2}' | tr -d ,)
sleep 1
check "a text still arriving is not announced half-finished" sh -c "! grep -q 'Meet me at from' $W/agentd.log"
check "nor listed" sh -c "! $AGENTD ctl -c $W/agentd.toml chat 'read my messages' | grep -q 'Meet me'"
$SIM sms-complete "$PART" "the station at 6" >/dev/null
if waitmore "the station" "$W/agentd.log" 5 "$n" >/dev/null; then ok "the whole text is announced once complete"; else bad "the completed text was not announced"; fi
check "announced once, whole" sh -c "[ \$(grep -c 'Meet me at the station at 6' $W/agentd.log) = 1 ]"

section "5. calls in"
arrive "a call rings: seen by D-Bus signal" "incoming call" 5 call $MUM
sleep 0.5
check "ringtone and vibration (urgent)" grep -q "^uid=$SU fbcli -E phone-incoming-call" "$PHONESIM_DIR/hw.log"
n=$(hwlines)
say "answer it" "answer" "answered Mum"
check "accepted through ModemManager" hw_after "$n" "mmcli -o [0-9]+ --accept"
check "audio to the earpiece" hw_after "$n" "^uid=$SU callaudiocli -m 1"
arrive "a second call waits during this one" "incoming call" 5 waiting $RAVI
n=$(hwlines)
say "hang up ends the first call" "hang up" "ended call with Mum"
say "answer the waiting call" "answer" "answered Ravi"
no_new "an answered call that the caller ends is not missed" "missed call" 3 hangup
say "nothing to hang up" "hang up" "no call to end"
say "nothing to answer" "answer" "nothing is ringing"
arrive "a stranger rings" "incoming call" 5 call $STRANGER
say "declining a ringing call" "hang up" "ended call with \\$STRANGER"
check "a declined call is not missed" sh -c "! grep -q 'missed call' $(LEDGER)"
arrive "a call rings" "incoming call" 5 call $MUM
arrive "the caller gives up: a missed call (Call StateChanged signal)" "missed call" 5 hangup
sleep 0.5
check "missed-call alert" grep -q "^uid=$SU fbcli -E phone-missed-call" "$PHONESIM_DIR/hw.log"
say "the call log" "missed calls" "from Mum \\(\\$MUM\\) ended"

section "6. calls out and the approval key"
say "a call needs approval" "call mum" "approval required"
$SIM key 300 >/dev/null
sleep 0.8
expect "a short press approves nothing" "$(ctl pending)" "call to=mum"
$SIM key 2500 115 >/dev/null
sleep 0.5
expect "another key (volume up) approves nothing" "$(ctl pending)" "call to=mum"
approve
expect "a 2.5 s hold of volume-down approves it" "$(ctl pending)" "nothing waiting"
check "the ledger says how it was approved" grep -q "approved #[0-9]* call with the approval key" "$(LEDGER)"
n=$(hwlines)
say "the approved call dials" "call mum" "calling Mum \(\\$MUM\)"
check "dialled through ModemManager" hw_after "$n" "mmcli -m any --voice-create-call=number=\\$MUM"
check "started" hw_after "$n" "mmcli -o [0-9]+ --start"
check "audio to the earpiece" hw_after "$n" "^uid=$SU callaudiocli -m 1"
say "one approval, one call" "call mum" "approval required"
decline_all
$SIM pickup >/dev/null
n=$(hwlines)
say "hang up our call" "hang up" "ended call with Mum"
check "audio back" hw_after "$n" "^uid=$SU callaudiocli -m 0"
no_new "an unanswered outgoing call is never 'missed'" "missed call" 2 hangup
say "call a number directly" "call 100" "approval required"
approve
say "dials the number" "call 100" "calling 100"
ctl chat "hang up" >/dev/null
$SIM key 2500 >/dev/null
sleep 0.5
check "a hold with nothing waiting approves nothing" sh -c "! tail -1 $W/agentd.log | grep -q approved"
ctl chat "call mum" >/dev/null
ctl chat "text mum see you soon" >/dev/null
before=$(count "approved #" "$(LEDGER)")
approve
check "two waiting: the key approves neither" sh -c "[ $before = \$(grep -c 'approved #' $(LEDGER)) ] && grep -q 'actions waiting; none approved' $(LEDGER)"
ctl press "$(ctl pending | grep -o '^#[0-9]*' | head -1 | tr -d '#')" >/dev/null
expect "press N still works for one of several" "$(ctl pending)" "send_sms"
decline_all
say "... and the pressed one runs" "call mum" "calling Mum"
ctl chat "hang up" >/dev/null
say "calling an unknown name" "call bob" "approval required|no number for bob"
if [ "$(pending_count)" -gt 0 ]; then approve; say "... fails once approved, with a reason" "call bob" "no number for bob"; fi
decline_all

section "7. texts out"
say "a text needs approval" "text mum it's 5pm, number='x' & \"quotes\"" "approval required"
approve
n=$(hwlines)
say "the approved text is sent" "text mum it's 5pm, number='x' & \"quotes\"" "sms sent to Mum"
check "created and sent through ModemManager" hw_after "$n" "mmcli -s [0-9]+ --send"
check "the text never appears on a command line" sh -c "! grep -q '5pm' $PHONESIM_DIR/hw.log"
check "the text reached the modem intact" sh -c "$SIM state | grep -qF \"it's 5pm, number='x' & \\\\\\\"quotes\\\\\\\"\""
say "sent texts are in the list" "read my messages" "to Mum \(\\$MUM\): it's 5pm"

section "8. USSD, Wi-Fi, mobile data, airplane mode, no service"
say "balance" "check balance *123#" "Main balance Rs 142.50"
say "data left" "ussd *121#" "Data left"
say "not a USSD code" "ussd abc" "digits, \\* and #"
say "Wi-Fi scan, strongest first" "scan wifi" "HomeNet \(81%\).*Cafe Guest \(44%\)|HomeNet \(81%\)"
say "wrong Wi-Fi password" "join wifi HomeNet password daisy" "Secrets were required"
say "unknown network" "join wifi Nowhere password x" "No network with SSID"
say "join Wi-Fi" "join wifi HomeNet password sunflower" "^done"
say "status shows Wi-Fi" "phone status" "wifi HomeNet"
say "mobile data on" "mobile data on" "^done"
say "status shows data" "phone status" "mobile data on"
say "mobile data off" "mobile data off" "^done"
say "airplane mode on" "airplane mode on" "^done"
say "status: airplane mode" "phone status" "airplane mode"
say "no Wi-Fi networks with the radios off" "scan wifi" "no networks found"
say "no mobile data in airplane mode" "mobile data on" "^not done|radio is off"
say "no USSD in airplane mode" "check balance *123#" "^not done|not registered"
say "calls need approval even in airplane mode" "call mum" "approval required"
approve
say "... and fail with the modem's reason" "call mum" "not registered"
decline_all
say "airplane mode off" "airplane mode off" "^done"
$SIM service off
approve 2>/dev/null
say "no service: a call fails" "call mum" "approval required|not registered"
decline_all
$SIM service on

section "9. location, torch, volume, ring"
say "GPS fix" "where am i" "12.9716, 77.5946 \(gps\)"
$SIM gps cell
say "no GPS fix: the cell" "where am i" "no GPS fix; cell mcc 404 mnc 45 lac 1A2B cid 0042F1"
$SIM gps none
say "no location at all" "where am i" "no location available"
$SIM gps fix
say "torch on" "torch on" "^done"
check "flash LED at full brightness" grep -qx 255 "$PHONESIM_DIR/leds/white:flash/brightness"
say "torch off" "flashlight off" "^done"
check "flash LED off" grep -qx 0 "$PHONESIM_DIR/leds/white:flash/brightness"
say "not on or off" "torch maybe" "on or off"
n=$(hwlines)
say "volume 30" "volume 30" "^done"
check "WirePlumber, as the session user" hw_after "$n" "^uid=$SU wpctl set-volume @DEFAULT_AUDIO_SINK@ 0.30"
say "volume 0" "volume 0" "^done"
n=$(hwlines)
say "volume above 100 is refused" "volume 150" "volume is 0 to 100"
check "... without touching the hardware" sh -c "! tail -n +$((n + 1)) $PHONESIM_DIR/hw.log | grep -q wpctl"
say "not a volume" "volume loud" "^not done|percent"
n=$(hwlines)
say "find my phone" "find my phone" "ringing"
check "rings through feedbackd" hw_after "$n" "^uid=$SU fbcli -E phone-incoming-call"

section "10. a Claude client over MCP gets the same tiers and the same key"
TOKEN=$($AGENTD init -c "$W/agentd.toml" | tail -1)
mcp() { curl -s -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
	-d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}" \
	http://127.0.0.1:18765/mcp; }
expect "MCP: a call needs approval" "$(mcp call '{"to":"Ravi"}')" "approval required"
approve
expect "MCP: the key approves it; it dials" "$(mcp call '{"to":"Ravi"}')" "calling Ravi"
ctl chat "hang up" >/dev/null
expect "MCP: open Wi-Fi without a password" "$(mcp wifi_connect '{"ssid":"Cafe Guest","password":""}')" "connected to Cafe Guest"
expect "MCP: low tools need nothing" "$(mcp phone_status '{}')" "wifi Cafe Guest"
expect "MCP: without the token, nothing" "$(curl -s -o /dev/null -w '%{http_code}' -d '{}' http://127.0.0.1:18765/mcp)" "401"

section "11. the D-Bus watcher fails and comes back"
pkill -f "dbus-monitor --system"
sleep 0.5
arrive "with the watcher down, the 2 s poll finds a text" "sms received" 4 sms $RAVI "watcher down"
sleep 11
check "the watcher was restarted" pgrep -f "dbus-monitor --system"
arrive "signals work again" "sms received" 2 sms $RAVI "watcher back"

section "12. the modem or NetworkManager goes away"
$SIM fail mmcli on
say "status reports the modem is gone" "phone status" "couldn't find modem"
say "a call fails" "call mum" "approval required|couldn't find modem"
decline_all
check "agentd is still running" kill -0 $AGENTD_PID
$SIM sms $MUM "sent while the modem was away" >/dev/null
sleep 1
$SIM fail mmcli off
n=$(count "sms received" "$(LEDGER)")
if ms=$(waitmore "sms received" "$(LEDGER)" 8 "$n"); then ok "a text that came while the modem was away is found soon after it is back (${ms} ms)"; else bad "a text that came while the modem was away was not found within 8 s of it coming back"; fi
$SIM fail nmcli on
say "Wi-Fi scan without NetworkManager says why" "scan wifi" "^not done|NetworkManager is not running"
$SIM fail nmcli off

section "13. the audio session goes away"
stop_session
arrive "calls are still seen with the session down" "incoming call" 5 call $RAVI
sleep 0.5
check "the failed alert is logged as failed, not as alerted" grep -q "incoming call from Ravi.*alert failed" "$W/agentd.log"
say "answering still works without call audio" "answer" "answered Ravi"
ctl chat "hang up" >/dev/null
say "volume says why it failed" "volume 40" "^not done|session bus"
start_session
n=$(hwlines)
say "with the session back, volume works" "volume 40" "^done"
check "as the session user" hw_after "$n" "^uid=$SU wpctl"

section "14. the session tools need the session user"
check "as root, outside the session, they fail like the real ones" \
	sh -c "! DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$SU/bus $PHONESIM_DIR/bin/fbcli -E test"

section "15. restart"
halt
boot restart
say "contacts survive a restart" "what's Mum's number" "Mum: \\$MUM"
check "old texts are not announced again" sh -c "[ \$(grep -c 'dinner at 8? from' $W/agentd.log) = 1 ]"
arrive "new texts are" "sms received" 5 sms $MUM "after restart"
expect "ledger chain verifies" "$(ctl verify)" "ledger ok"
halt

section "16. quiet hours: texts wait for the digest, calls still ring"
boot quiet "quiet_start_min = 0" "quiet_end_min = 1440"
n=$(hwlines)
arrive "a text in quiet hours is logged" "sms received" 5 sms $MUM "late night"
sleep 0.5
check "... but not alerted" sh -c "! tail -n +$((n + 1)) $PHONESIM_DIR/hw.log | grep -q 'fbcli -E message-new-sms'"
check "... held for the digest" grep -q "late night from .* (held for the digest)" "$W/agentd.log"
arrive "a call in quiet hours" "incoming call" 5 call $MUM
sleep 0.5
check "... still rings (urgent)" sh -c "tail -5 $PHONESIM_DIR/hw.log | grep -q 'fbcli -E phone-incoming-call'"
$SIM hangup >/dev/null
expect "the digest has the held text" "$(ctl digest)" "late night"
halt

section "17. the daily alert budget"
boot budget "daily_budget = 2"
for i in 1 2 3; do $SIM sms $MUM "budget $i" >/dev/null; sleep 1; done
sleep 1
check "the first two texts alert" sh -c "grep -q 'budget 1 from.*(alerted)' $W/agentd.log && grep -q 'budget 2 from.*(alerted)' $W/agentd.log"
check "the third is held" grep -q "budget 3 from.*(held for the digest)" "$W/agentd.log"
halt

section "18. no D-Bus watcher: polling only"
boot nowatch 'dbus_monitor = "/nonexistent/dbus-monitor"'
check "agentd runs without it" kill -0 $AGENTD_PID
check "and says so" grep -q "nonexistent/dbus-monitor.*polling instead" "$W/agentd.log"
arrive "texts are found by the 2 s poll" "sms received" 4 sms $MUM "polling only"
halt

section "19. no key device"
boot nokey "approval_key = \"/nonexistent/event9\""
ctl chat "save Mum as $MUM" >/dev/null
check "agentd runs without it" kill -0 $AGENTD_PID
check "and says so" grep -q "approval key /nonexistent/event9" "$W/agentd.log"
say "approval by press N still works" "call mum" "approval required"
ctl press 1 >/dev/null
say "... and the call goes through" "call mum" "calling Mum"
halt

stop_session
$SIM stop
echo
echo "$pass passed, $fail failed   (logs: $W/agentd.log $W/session.log $PHONESIM_DIR/hw.log)"
[ $fail = 0 ] || { echo "failed:$FAILED"; exit 1; }
