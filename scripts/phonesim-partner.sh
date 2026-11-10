#!/bin/bash
# The business partner and personal assistant end to end through agentd: a working day (customers
# on WhatsApp and email, invoices, a payment received, bills, GST) and an evening (shopping from
# the order history, news, a trek and a concert), by chat, with the approval key for everything
# that sends, pays or files. Underneath: the simulated phone and the simulated world.
# Needs root, dbus-daemon, dbus-monitor, dbus-send, setpriv.   sudo scripts/phonesim-partner.sh [-v]
set -u
cd "$(dirname "$0")/.."
ROOT=$PWD
VERBOSE=${1:-}
W=${PHONESIM_WORK:-/tmp/agentd-partner}
[ "$(id -u)" = 0 ] || { echo "run as root"; exit 2; }
cargo build -q -p agentd -p agentos-phonesim || exit 2
AGENTD=$ROOT/target/debug/agentd
SIM=$ROOT/target/debug/agentos-phonesim
PORT=18900
export PHONESIM_WORLD=http://127.0.0.1:$PORT

pass=0
fail=0
FAILED=""
ok() { pass=$((pass + 1)); echo "  PASS  $*"; }
bad() { fail=$((fail + 1)); FAILED="$FAILED
  $*"; echo "  FAIL  $*"; }
section() { echo "== $*"; }
ctl() { $AGENTD ctl -c "$W/agentd.toml" "$@" 2>&1 | tr '\001' '\n'; }
world() { curl -s "$PHONESIM_WORLD$1"; }
say() { # say "what" "chat" "expected pattern"
	local out
	out=$(ctl chat "$2")
	[ -n "$VERBOSE" ] && echo "        > $2" && echo "$out" | head -12 | sed 's/^/          /'
	if echo "$out" | grep -qE -- "$3"; then ok "$1"; else bad "$1: got: $(echo "$out" | head -3 | tr '\n' ' ')"; fi
}
check() { local what=$1; shift; if "$@" >/dev/null 2>&1; then ok "$what"; else bad "$what"; fi; }
# a High action: asked, nothing happens; the key; asked again, done
approved() { # approved "what" "chat" "expected after approval" "world check before (must hold)"
	local out
	out=$(ctl chat "$2")
	if echo "$out" | grep -q "approval required"; then ok "$1: waits for the key"; else bad "$1: did not wait for the key: $(echo "$out" | head -2 | tr '\n' ' ')"; fi
	$SIM key 2500 >/dev/null
	sleep 0.5
	say "$1: done after the key" "$2" "$3"
}

pkill -f "agentos-phonesim (world|keep-input)" 2>/dev/null
rm -rf "$W" && mkdir -p "$W/sim" "$W/data"
$SIM world $PORT >"$W/world.log" 2>&1 &
WORLD=$!
for _ in $(seq 1 50); do curl -s "$PHONESIM_WORLD/health" >/dev/null && break; sleep 0.1; done
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
listen = "127.0.0.1:18768"
kdf_mib = 1
idle_secs = 3600
phone = "modemmanager"
leds_dir = "$PHONESIM_DIR/leds"
dbus_monitor = "/usr/bin/dbus-monitor"
session_user = "agentd-session"
approval_key = "$PHONESIM_DIR/input-event0"
quiet_start_min = 0
quiet_end_min = 0
daily_budget = 1000

[partner]
world_url = "$PHONESIM_WORLD"
business_name = "Lakshmi Widgets"
gstin = "29AAACM1234A1Z4"
vpa = "lakshmiwidgets@upi"
city = "Bengaluru"
interests = ["music", "trekking"]
EOF
$AGENTD init -c "$W/agentd.toml" >/dev/null
$AGENTD run -c "$W/agentd.toml" >"$W/agentd.log" 2>&1 &
AGENTD_PID=$!
for _ in $(seq 1 100); do ctl status >/dev/null 2>&1 && break; sleep 0.2; done
sleep 2

section "1. morning: the inbox, with a reply drafted for each customer"
say "one inbox for WhatsApp and email" "inbox" "5 waiting"
say "the catalog" "add item Widget at 500 gst 18" "Widget added"
say "" "add item Repair at 1500 gst 18" "Repair added"
say "a customer, GSTIN checked" "add customer Patil Stores gstin 27AAPFU0939F1ZV phone 98222 22222" "place of supply: Maharashtra"
say "a typo in a GSTIN is caught" "add customer Typo Ltd gstin 27AAPFU0939F1ZX phone 9800000000" "check character is wrong"
say "Patil's order, priced with GST, both items" "show chat with Patil Stores" "10 x Widget, 2 x Repair comes to ₹9,440.00"
say "a price question answered from the catalog" "show chat with Sharma Traders" "Widget is ₹500.00 \+ 18% GST"
approved "reply to a customer" "reply to Sharma Traders: Hi Sharma, Widget is ₹500 + 18% GST. How many would you like?" "Replied to Sharma Traders"
check "the reply reached WhatsApp" sh -c "curl -s $PHONESIM_WORLD/sim/state | grep -q 'How many would you like'"

section "2. an invoice, sent, paid, matched"
say "an inter-state invoice: IGST" "invoice Patil Stores for 10 x Widget, 2 x Repair" "IGST ₹1,440.00"
INV=$(ctl chat "invoices" | grep -oE "INV/[0-9]{4}-[0-9]{2}/[0-9]{4}" | head -1)
approved "send the invoice on WhatsApp" "send invoice $INV" "Invoice $INV sent"
check "with a UPI payment link" sh -c "curl -s $PHONESIM_WORLD/sim/state | grep -q 'upi://pay?pa=lakshmiwidgets@upi'"
$SIM collect patil@okhdfc 9440 "$INV" >/dev/null
say "the payment is matched to the invoice" "who owes me" "payment received by UPI and matched"

section "3. bills from email, paid from the wallet"
say "bills, soonest first" "what bills are due" "3 bills"
BAL0=$(world /pay/v1/balance | grep -oE '[0-9]+')
approved "pay the broadband bill" "pay bill m-103" "Paid FiberNet Broadband"
BAL1=$(world /pay/v1/balance | grep -oE '[0-9]+')
check "₹1,179 left the wallet, once" test $((BAL0 - BAL1)) -eq 117900

section "4. GST: the month's figures and the return"
say "GST for the month" "gst for this month" "Tax to pay in cash"
approved "file GSTR-3B through the GSP" "file gstr-3b for this month" "GSTR-3B filed"
check "filed once, with an ARN" sh -c "curl -s $PHONESIM_WORLD/gsp/v1/returns | grep -o '\"arn\"' | wc -l | grep -qx 1"
say "wallet and payments" "wallet" "Balance"

section "5. evening: shopping from the order history"
say "what is bought often" "what do i usually buy" "bought 3 times"
say "the best price for the usual atta" "best price for atta" "₹439.00.*on quickmart"
approved "buy it" "buy best" "Ordered"
check "the order reached the platform" sh -c "curl -s $PHONESIM_WORLD/shop/quickmart/orders | grep -o order_id | wc -l | grep -qx 2"

section "6. news and things to do, by interests"
say "news ranked by interests" "news" "because you follow"
say "a stated like" "i like jazz" "Noted: you like jazz"
say "music: jazz first now" "suggest music" "Jazz on the Roof"
say "adventures: the trek first" "suggest adventures" "Sunrise trek to Skandagiri"
approved "book the trek for two" "book EV-102 for 2" "Booked"
check "two tickets issued" sh -c "curl -s $PHONESIM_WORLD/sim/state | grep -q 'BK-[0-9]*-T2'"

section "7. screens instead of text"
check "agentd keeps the last screen for the display" sh -c "$AGENTD ctl -c $W/agentd.toml state | grep -q '\"screen\":{\"tool\":\"book\"'"
check "a screen has buttons that say requests" sh -c "$AGENTD ctl -c $W/agentd.toml screen | grep -q '\"say\"'"
check "every money movement waited for the key" sh -c "[ \$(grep -c 'approved #' $W/data/ledger.log) -ge 6 ]"
check "ledger verifies" sh -c "$AGENTD ctl -c $W/agentd.toml verify | grep -q 'ledger ok'"

kill $AGENTD_PID 2>/dev/null; wait $AGENTD_PID 2>/dev/null
kill $SESSION $WORLD 2>/dev/null
$SIM stop
echo
echo "$pass passed, $fail failed   (logs: $W/agentd.log $W/world.log)"
[ $fail = 0 ] || { echo "failed:$FAILED"; exit 1; }
