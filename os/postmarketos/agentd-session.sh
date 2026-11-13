#!/bin/sh
# The user session agentd's phone needs on an image without a desktop: a session bus, PipeWire,
# WirePlumber, pipewire-pulse (callaudiod speaks PulseAudio), callaudiod (call audio routing) and
# feedbackd (vibration, ringtone, LED). Runs as the agentd-session user, started and restarted
# by /etc/init.d/agentd-session; agentd runs callaudiocli, fbcli and wpctl inside it.
set -u
export XDG_RUNTIME_DIR="/run/user/$(id -u)"
export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
: "${FEEDBACKD:=/usr/libexec/feedbackd}"
: "${CALLAUDIOD:=/usr/bin/callaudiod}"

pids=""
stop() {
	# shellcheck disable=SC2086
	if [ -n "$pids" ]; then
		kill $pids 2>/dev/null
		# wait until they are gone: a session bus still shutting down would delete the socket
		# of the next one when it exits (a restart would then leave the session unreachable)
		for p in $pids; do
			while kill -0 "$p" 2>/dev/null; do sleep 0.1; done
		done
	fi
	exit 0
}
trap stop TERM INT

rm -f "$XDG_RUNTIME_DIR/bus"
dbus-daemon --session --address="$DBUS_SESSION_BUS_ADDRESS" --nofork --nopidfile &
pids="$!"
i=0
while [ ! -S "$XDG_RUNTIME_DIR/bus" ]; do
	i=$((i + 1))
	[ "$i" -gt 100 ] && { echo "agentd-session: no session bus" >&2; stop; }
	sleep 0.1
done
for prog in pipewire wireplumber pipewire-pulse "$CALLAUDIOD" "$FEEDBACKD"; do
	if command -v "$prog" >/dev/null 2>&1; then
		"$prog" &
		pids="$pids $!"
	else
		echo "agentd-session: $prog not found, skipped" >&2
	fi
done
# If any of them dies, end the session so the supervisor starts it all again in order.
while :; do
	for p in $pids; do
		kill -0 "$p" 2>/dev/null || { echo "agentd-session: process $p exited" >&2; stop; }
	done
	# in the background, so a stop signal is handled at once rather than after the sleep
	sleep 5 &
	wait $!
done
