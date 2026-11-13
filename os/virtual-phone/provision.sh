#!/bin/sh
# Turns an Ubuntu 24.04 arm64 VM into a virtual Agentic OS phone. Run as root, from the source in
# /opt/agentos-src, with PHONE_USER set to the user who sees the screen (scripts/mac/virtual-phone.sh
# does this). Safe to run again: it rebuilds and reinstalls.
set -eu
src=/opt/agentos-src
here=$src/os/virtual-phone
user=${PHONE_USER:?PHONE_USER}
home=$(getent passwd "$user" | cut -d: -f6)
uid=$(id -u "$user")
export DEBIAN_FRONTEND=noninteractive

say() { printf '\n== %s\n' "$*"; }

say "packages: build tools, the screen (sway), software graphics, audio and session"
apt-get update -q
apt-get install -y -q build-essential pkg-config cmake clang curl git ca-certificates \
	sway swaybg foot wvkbd xwayland \
	mesa-vulkan-drivers libvulkan1 libgl1-mesa-dri libegl1 libwayland-client0 libxkbcommon0 \
	libwayland-dev libxkbcommon-dev libfontconfig-dev libsqlite3-dev \
	dbus-user-session pipewire wireplumber fonts-dejavu fonts-noto-color-emoji \
	evtest jq
# the Android binder driver, for Waydroid (in the extra modules of Ubuntu's kernel)
apt-get install -y -q "linux-modules-extra-$(uname -r)" || echo "note: no extra modules for $(uname -r); Android may need: apt install linux-generic, then restart"

say "Rust (the toolchain the source pins)"
if [ ! -x "$home/.cargo/bin/rustup" ]; then
	su - "$user" -c 'curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none'
fi
build=/opt/agentos-build
mkdir -p "$build"
chown -R "$user" "$build" "$src"

say "building agentd, the model server and the screen (the first build takes a while)"
su - "$user" -c "cd $src && CARGO_TARGET_DIR=$build ~/.cargo/bin/cargo build --release -p agentd -p agentos-infer --bin agentd --bin agentos-llm"
su - "$user" -c "cd $src/ui && CARGO_TARGET_DIR=$build/ui ~/.cargo/bin/cargo build --release"
install -m755 "$build/release/agentd" "$build/release/agentos-llm" /usr/local/bin/
install -m755 "$build/ui/release/agentos-ui" /usr/local/bin/

say "agentd: config, vault, service"
install -d -m700 /etc/agentd /var/lib/agentd
if [ ! -f /etc/agentd/agentd.toml ] || ! grep -q "virtual phone" /etc/agentd/agentd.toml; then
	sed -e "s/@USER@/$user/g" -e "s/@UID@/$uid/g" -e "s|@HOME@|$home|g" "$here/agentd.toml" >/etc/agentd/agentd.toml
	chmod 600 /etc/agentd/agentd.toml
fi
if [ ! -s /etc/agentd/passphrase ]; then
	(umask 077; head -c 32 /dev/urandom | base64 >/etc/agentd/passphrase)
fi
if [ ! -s /etc/agentd/gateway-token ]; then
	(umask 077; AGENTD_PASSPHRASE_FILE=/etc/agentd/passphrase /usr/local/bin/agentd init -c /etc/agentd/agentd.toml | tail -n 1 >/etc/agentd/gateway-token)
fi
install -m644 "$here/agentd.service" /etc/systemd/system/agentd.service

say "the model"
models=/var/lib/agentd/models
mkdir -p "$models"
if [ -f "${SHARE:-/nonexistent}/model.gguf" ]; then
	small=$(sed -n '/role = "small"/,/^file/s/^file = "\(.*\)"/\1/p' /etc/agentd/agentd.toml | head -1)
	install -m600 "$SHARE/model.gguf" "$models/$small"
else
	AGENTD_PASSPHRASE_FILE=/etc/agentd/passphrase /usr/local/bin/agentd models pull -c /etc/agentd/agentd.toml || echo "note: model download failed; try later: virtual-phone.sh ctl 'models' or copy one with up --model"
fi

say "the screen: log in on the console and start sway, phone sized"
install -d /etc/agentos
install -m644 "$here/sway.config" /etc/agentos/sway.config
install -d /etc/systemd/system/getty@tty1.service.d
cat >/etc/systemd/system/getty@tty1.service.d/autologin.conf <<EOF
[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin $user --noclear %I \$TERM
EOF
profile="$home/.profile"
if ! grep -q "agentos sway" "$profile" 2>/dev/null; then
	cat >>"$profile" <<'EOF'

# agentos sway: the phone's screen on the console
if [ "$(tty)" = /dev/tty1 ] && [ -z "${WAYLAND_DISPLAY:-}" ]; then
	export XDG_CURRENT_DESKTOP=sway WLR_RENDERER=pixman WLR_NO_HARDWARE_CURSORS=1
	exec sway -c /etc/agentos/sway.config >/tmp/sway.log 2>&1
fi
EOF
	chown "$user" "$profile"
fi
# the screen runs as root next to agentd (its control socket is root's); allow just that
cat >/etc/sudoers.d/agentos-ui <<EOF
$user ALL=(root) NOPASSWD: SETENV: /usr/local/bin/agentos-ui
EOF
chmod 440 /etc/sudoers.d/agentos-ui

say "Android (Waydroid): installed now; Google Play is set up with: virtual-phone.sh android"
if ! command -v waydroid >/dev/null 2>&1; then
	curl -s https://repo.waydro.id | bash
	apt-get install -y -q waydroid
fi
modprobe binder_linux devices="binder,hwbinder,vndbinder" 2>/dev/null || true
echo binder_linux >/etc/modules-load.d/agentos-binder.conf

systemctl daemon-reload
systemctl enable agentd
systemctl restart agentd
say "done: restart the VM (or log out of tty1) to see the phone screen"
