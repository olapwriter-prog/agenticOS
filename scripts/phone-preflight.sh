#!/bin/sh
# Phase 5, step 1: run on the phone (as root) before anything else. Checks what the simulated phone
# cannot: the packages and services, the modem and SIM, the audio session and its user, which
# input device has the volume-down key, the vibration motor's permissions, the flash LED and the
# battery; then prints the agentd.toml lines for this phone. POSIX sh (busybox ash).
#   sudo sh phone-preflight.sh [/etc/agentd/agentd.toml]
# PREFLIGHT_ROOT=dir reads /proc, /sys and /dev under dir instead (for testing).
R=${PREFLIGHT_ROOT:-}
CONF=${1:-/etc/agentd/agentd.toml}
pass=0
warn=0
fail=0
ok() { pass=$((pass + 1)); echo "  ok    $*"; }
wn() { warn=$((warn + 1)); echo "  WARN  $*"; }
no() { fail=$((fail + 1)); echo "  FAIL  $*"; }
has() { command -v "$1" >/dev/null 2>&1; }
SUGGEST=""
suggest() { SUGGEST="$SUGGEST
$*"; }

# bits per word in /proc/bus/input/devices bitmaps: the kernel's unsigned long
case $(uname -m) in
aarch64 | x86_64 | riscv64) LONG=64 ;;
*) LONG=32 ;;
esac
[ -n "${PREFLIGHT_LONG:-}" ] && LONG=$PREFLIGHT_LONG

# bitmap_has "hex words, most significant first" BIT
bitmap_has() {
	set -- $1 "$2"
	bit=$(eval echo \$$#)
	n=$(($# - 1))
	idx=$((bit / LONG))
	[ "$idx" -ge "$n" ] && return 1
	# words are listed most significant first: word idx is the (n-idx)th
	w=$(eval echo \$$((n - idx)))
	# split the word in 32-bit halves so the shell's arithmetic never overflows
	off=$((bit % LONG))
	len=${#w}
	if [ "$off" -ge 32 ]; then
		[ "$len" -le 8 ] && return 1
		part=$(echo "$w" | cut -c1-$((len - 8)))
		off=$((off - 32))
	elif [ "$len" -gt 8 ]; then
		part=$(echo "$w" | cut -c$((len - 7))-$len)
	else
		part=$w
	fi
	[ $(((0x$part >> off) & 1)) = 1 ]
}

# the /dev/input/by-path name of an event node, if there is one (stable across boots)
by_path() {
	for l in "$R"/dev/input/by-path/*; do
		[ -e "$l" ] || continue
		[ "$(readlink -f "$l")" = "$(readlink -f "$R/dev/input/$1")" ] && { echo "${l#$R}"; return; }
	done
	echo "/dev/input/$1"
}

echo "== packages and tools"
for c in mmcli nmcli callaudiocli fbcli wpctl dbus-monitor dbus-daemon pipewire wireplumber pipewire-pulse callaudiod agentd agentos-llm; do
	if has $c; then ok "$c"; else no "$c is missing"; fi
done
if [ -x "$R/usr/libexec/feedbackd" ] || [ -x /usr/libexec/feedbackd ]; then ok "feedbackd daemon"; else no "/usr/libexec/feedbackd is missing (apk add feedbackd)"; fi
has evtest && ok "evtest" || wn "evtest is missing (apk add evtest): only needed to find the key by hand"

echo "== services"
if has rc-update; then
	for s in dbus modemmanager networkmanager agentd-session agentd; do
		if rc-update show default 2>/dev/null | grep -q "^ *$s "; then ok "$s starts at boot"; else no "$s does not start at boot (rc-update add $s default)"; fi
		if rc-service "$s" status >/dev/null 2>&1; then ok "$s is running"; else wn "$s is not running (rc-service $s start)"; fi
	done
else
	wn "no OpenRC here: services not checked"
fi
if [ -e "$R/sys/fs/cgroup/cgroup.controllers" ]; then ok "cgroup v2 (unified)"; else no "cgroup v2 not mounted: rc_cgroup_mode=\"unified\" in /etc/rc.conf, reboot"; fi

echo "== modem and SIM"
if has mmcli; then
	if mmcli -L 2>/dev/null | grep -q "/Modem/"; then
		ok "modem: $(mmcli -L 2>/dev/null | grep /Modem/ | head -1 | sed 's/^ *//')"
		k=$(mmcli -m any -K 2>/dev/null)
		get() { echo "$k" | sed -n "s/^$1 *: *//p" | head -1; }
		st=$(get modem.generic.state)
		sim=$(get modem.generic.sim)
		case $st in
		registered | connected | connecting) ok "registered: $(get modem.3gpp.operator-name), signal $(get modem.generic.signal-quality.value)%" ;;
		locked) no "the SIM is locked: mmcli -i any --pin=XXXX" ;;
		*) no "modem state is '$st' (not registered)" ;;
		esac
		[ -n "$sim" ] && [ "$sim" != "--" ] && ok "SIM present" || no "no SIM"
		echo "$k" | grep -q "modem.generic.supported-capabilities.*\(gsm-umts\|lte\)" && ok "voice-capable modem" || wn "check the modem supports voice calls"
	else
		no "ModemManager sees no modem (rc-service modemmanager restart; dmesg | grep -i modem)"
	fi
fi

echo "== the audio session (call audio, vibration, volume)"
if id agentd-session >/dev/null 2>&1; then
	SU=$(id -u agentd-session)
	ok "user agentd-session (uid $SU)"
	groups=$(id -nG agentd-session)
	for g in audio input feedbackd; do
		case " $groups " in *" $g "*) ok "agentd-session is in group $g" ;; *) no "agentd-session is not in group $g (addgroup agentd-session $g)" ;; esac
	done
	if [ -S "/run/user/$SU/bus" ]; then
		ok "its session bus is up"
		env="XDG_RUNTIME_DIR=/run/user/$SU DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$SU/bus"
		as() { su -s /bin/sh agentd-session -c "$env $*" >/dev/null 2>&1; }
		as wpctl status && ok "WirePlumber answers in the session" || no "wpctl status fails in the session (see /var/log/agentd-session.log)"
		as "dbus-send --session --print-reply --dest=org.sigxcpu.Feedback /org/sigxcpu/Feedback org.freedesktop.DBus.Peer.Ping" && ok "feedbackd answers on the session bus" || no "feedbackd does not answer on the session bus"
		as "dbus-send --session --print-reply --dest=org.mobian_project.CallAudio /org/mobian_project/CallAudio org.freedesktop.DBus.Peer.Ping" && ok "callaudiod answers on the session bus" || no "callaudiod does not answer on the session bus"
	else
		no "no session bus at /run/user/$SU/bus (rc-service agentd-session start)"
	fi
else
	no "no agentd-session user (reinstall agentd: its pre-install makes it)"
fi

echo "== input: the approval key, and the vibration motor"
DEVS=$R/proc/bus/input/devices
key_dev=""
if [ -r "$DEVS" ]; then
	# one block per device; remember name, handlers and bitmaps, decide at each blank line
	name=""
	handlers=""
	keys=""
	ff=""
	vibra=""
	# (a blank line after the last block flushes it too)
	{ cat "$DEVS"; echo; } | {
		while IFS= read -r line; do
			case $line in
			N:*) name=$(echo "$line" | sed 's/^N: Name="\(.*\)"$/\1/') ;;
			H:*) handlers=${line#*Handlers=} ;;
			"B: KEY="*) keys=${line#B: KEY=} ;;
			"B: FF="*) ff=${line#B: FF=} ;;
			"")
				ev=$(echo "$handlers" | tr ' ' '\n' | grep '^event' | head -1)
				if [ -n "$ev" ] && [ -n "$keys" ] && bitmap_has "$keys" 114; then
					echo "KEY $ev $name"
				fi
				if [ -n "$ev" ] && [ -n "$ff" ] && [ "$ff" != "0" ]; then
					echo "VIBRA $ev $name"
				fi
				name=""
				handlers=""
				keys=""
				ff=""
				;;
			esac
		done
	} >"${TMPDIR:-/tmp}/preflight.$$"
	key_line=$(grep '^KEY ' "${TMPDIR:-/tmp}/preflight.$$" | head -1)
	if [ -n "$key_line" ]; then
		key_ev=$(echo "$key_line" | cut -d' ' -f2)
		key_name=$(echo "$key_line" | cut -d' ' -f3-)
		key_dev=$(by_path "$key_ev")
		ok "volume-down is on \"$key_name\" ($key_ev): $key_dev"
		case $key_dev in /dev/input/event*) wn "no by-path name for it: eventN numbers can change between boots" ;; esac
		suggest "approval_key = \"$key_dev\""
		suggest "approval_keycode = 114"
	else
		no "no input device reports KEY_VOLUMEDOWN (114); run evtest to find the key"
	fi
	grep '^VIBRA ' "${TMPDIR:-/tmp}/preflight.$$" | while read -r _ ev vname; do
		node=$R/dev/input/$ev
		if [ -e "$node" ]; then
			perm=$(stat -c '%G %a' "$node" 2>/dev/null)
			case $perm in
			"input "6[67]* | "input "?6?) ok "vibration motor \"$vname\" ($ev): group input, mode ${perm#* }" ;;
			*) no "vibration motor \"$vname\" ($ev) is '$perm': feedbackd (as agentd-session) needs group input with read/write" ;;
			esac
		else
			ok "vibration motor \"$vname\" ($ev)"
		fi
	done
	grep -q '^VIBRA ' "${TMPDIR:-/tmp}/preflight.$$" || wn "no force-feedback input device: this phone may not vibrate"
	rm -f "${TMPDIR:-/tmp}/preflight.$$"
else
	no "cannot read $DEVS"
fi

echo "== LEDs and battery"
torch=""
for l in "$R"/sys/class/leds/*; do
	[ -e "$l" ] || continue
	case ${l##*/} in *flash* | *torch*) torch=${l##*/} ;; esac
done
if [ -n "$torch" ]; then
	ok "flash LED: $torch (max brightness $(cat "$R/sys/class/leds/$torch/max_brightness" 2>/dev/null))"
	suggest "leds_dir = \"/sys/class/leds\""
else
	wn "no flash or torch LED under /sys/class/leds: 'torch on' will say so"
fi
bat=""
for p in "$R"/sys/class/power_supply/*; do
	[ -e "$p/type" ] || continue
	[ "$(cat "$p/type")" = Battery ] && { bat=${p#$R}; break; }
done
if [ -n "$bat" ]; then
	ok "battery: $bat ($(cat "$R$bat/capacity" 2>/dev/null)%)"
	suggest "power_dir = \"$bat\""
else
	no "no battery under /sys/class/power_supply"
fi

echo "== $CONF"
if [ -r "$CONF" ]; then
	val() { sed -n "s/^$1 *= *\"\{0,1\}\([^\"#]*\)\"\{0,1\}.*/\1/p" "$CONF" | head -1 | sed 's/ *$//'; }
	[ "$(val phone)" = modemmanager ] && ok "phone = modemmanager" || no "phone is '$(val phone)', not modemmanager"
	for k in approval_key power_dir dbus_monitor; do
		v=$(val $k)
		if [ -z "$v" ]; then wn "$k is not set"
		elif [ -e "$R$v" ] || [ -e "$v" ]; then ok "$k = $v"
		else no "$k = $v does not exist"; fi
	done
	[ -n "$key_dev" ] && [ "$(val approval_key)" != "$key_dev" ] && wn "approval_key differs from the device found above ($key_dev)"
	[ "$(val session_user)" = agentd-session ] && ok "session_user = agentd-session" || no "session_user is not agentd-session"
else
	wn "no config at $CONF"
fi

echo
echo "$pass ok, $warn warnings, $fail failed"
if [ -n "$SUGGEST" ]; then
	echo
	echo "For $CONF on this phone:$SUGGEST"
fi
[ "$fail" = 0 ]
