#!/bin/sh
# Cross-compile agentd for the phone and install it over SSH, without rebuilding the image.
#   scripts/sideload.sh user@172.16.42.1 [aarch64|armv7]
# 172.16.42.1 is the phone's address over USB networking on postmarketOS.
# Needs: zig and cargo-zigbuild (brew install zig && cargo install cargo-zigbuild).
set -eu
host="${1:?usage: $0 user@phone [aarch64|armv7]}"
case "${2:-aarch64}" in
	aarch64) target=aarch64-unknown-linux-musl ;;
	armv7) target=armv7-unknown-linux-musleabihf ;;
	*) echo "unknown arch $2" >&2; exit 1 ;;
esac
cd "$(dirname "$0")/.."
cargo zigbuild --release -p agentd --target "$target"
bin="target/$target/release/agentd"
ls -lh "$bin"
scp "$bin" os/postmarketos/agentd.initd os/postmarketos/agentd.confd os/postmarketos/agentd.toml "$host":/tmp/
ssh -t "$host" 'sudo sh -c "
	install -Dm755 /tmp/agentd /usr/bin/agentd &&
	install -Dm755 /tmp/agentd.initd /etc/init.d/agentd &&
	{ [ -e /etc/conf.d/agentd ] || install -Dm644 /tmp/agentd.confd /etc/conf.d/agentd; } &&
	{ [ -e /etc/agentd/agentd.toml ] || install -Dm644 /tmp/agentd.toml /etc/agentd/agentd.toml; } &&
	apk add -q modemmanager networkmanager networkmanager-cli networkmanager-wifi networkmanager-wwan feedbackd callaudiod wireplumber &&
	for s in dbus modemmanager networkmanager feedbackd agentd; do rc-update add \$s default >/dev/null; done &&
	rc-service agentd restart
"'
echo "done. Token: ssh $host sudo cat /etc/agentd/gateway-token"
