#!/bin/sh
# A virtual Agentic OS phone on a Mac with Apple Silicon: a Linux VM (Ubuntu 24.04, arm64, Apple's
# Virtualization framework) in a phone-shaped window, running the whole phone: agentd, the local
# model, the Agentic OS screen, and Android (Waydroid, with Google Play) inside it.
#
#   scripts/mac/virtual-phone.sh up [--model file.gguf]   create (first time), build, start
#   scripts/mac/virtual-phone.sh rebuild                  build and install the current code again
#   scripts/mac/virtual-phone.sh ctl <command>            talk to agentd (e.g. ctl "chat weather")
#   scripts/mac/virtual-phone.sh sms "Mum|Are you coming?"   a text arrives
#   scripts/mac/virtual-phone.sh call +919845012345       a call rings
#   scripts/mac/virtual-phone.sh android                  set up Android with Google Play (once)
#   scripts/mac/virtual-phone.sh shell | status | down | destroy
#
# Needs: macOS 13 or later on Apple Silicon, Homebrew (https://brew.sh). Installs Lima.
# Sizes: CPUS (default 6), MEMORY in GiB (default 8), DISK in GiB (default 60).
set -eu
cd "$(dirname "$0")/../.."
repo=$(pwd)
vm=agentos-phone
share="$HOME/.lima-share/agentos-phone"     # Lima shares the home folder with the VM
CPUS=${CPUS:-6}
MEMORY=${MEMORY:-8}
DISK=${DISK:-60}

say() { printf '\n== %s\n' "$*"; }
die() { printf 'virtual-phone: %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = Darwin ] || die "run this on the Mac"
[ "$(uname -m)" = arm64 ] || die "needs a Mac with Apple Silicon (M1 or later)"

need_lima() {
	command -v limactl >/dev/null 2>&1 && return
	command -v brew >/dev/null 2>&1 || die "install Homebrew first: https://brew.sh"
	say "installing Lima (runs Linux VMs on the Mac)"
	brew install lima
}

running() { limactl list "$vm" 2>/dev/null | grep -q Running; }
exists() { limactl list -q 2>/dev/null | grep -qx "$vm"; }
inside() { limactl shell --workdir / "$vm" "$@"; }

pack() {
	say "packing the source for the VM"
	rm -rf "$share" && mkdir -p "$share"
	# the working tree as it is, without build output
	tar -C "$repo" --exclude ./target --exclude ./ui/target --exclude ./.git -czf "$share/src.tar.gz" .
}

provision() {
	pack
	model=${1:-}
	if [ -n "$model" ]; then
		[ -f "$model" ] || die "no such model file: $model"
		cp "$model" "$share/model.gguf"
	fi
	say "setting up the phone inside the VM (the first time takes 20 to 40 minutes)"
	inside sudo env SHARE="$share" PHONE_USER="$(inside whoami)" sh -c 'mkdir -p /opt/agentos-src && tar -C /opt/agentos-src -xzf "$SHARE/src.tar.gz" && sh /opt/agentos-src/os/virtual-phone/provision.sh'
}

cmd=${1:-up}
[ $# -gt 0 ] && shift
case "$cmd" in
up)
	need_lima
	model=""
	if [ "${1:-}" = --model ]; then model=${2:-}; fi
	if ! exists; then
		say "creating the VM: $CPUS CPUs, ${MEMORY} GiB, ${DISK} GiB disk, with a screen"
		limactl create --name="$vm" --vm-type=vz --cpus="$CPUS" --memory="$MEMORY" --disk="$DISK" \
			--set '.video.display = "default"' --tty=false template:ubuntu-24.04
		limactl start "$vm"
		provision "$model"
		say "restarting so the phone boots into its screen"
		limactl stop "$vm"
	fi
	running || limactl start "$vm"
	say "the phone is up: its window shows the Agentic OS screen"
	echo "   Try: scripts/mac/virtual-phone.sh ctl \"chat weather\""
	echo "        scripts/mac/virtual-phone.sh sms \"Mum|Are you coming for dinner?\""
	echo "        scripts/mac/virtual-phone.sh android      (Google Play, once)"
	;;
rebuild)
	running || die "start it first: scripts/mac/virtual-phone.sh up"
	provision "${2:-}"
	inside sudo systemctl restart agentd
	inside sudo pkill -f agentos-ui || true   # sway starts it again
	;;
ctl)
	inside sudo /usr/local/bin/agentd ctl -c /etc/agentd/agentd.toml "$*"
	;;
sms)
	inside sudo /usr/local/bin/agentd ctl -c /etc/agentd/agentd.toml "sim-sms $*"
	;;
call)
	inside sudo /usr/local/bin/agentd ctl -c /etc/agentd/agentd.toml "sim-call $*"
	;;
android)
	inside sudo sh /opt/agentos-src/os/virtual-phone/android.sh
	;;
shell) limactl shell "$vm" ;;
status)
	limactl list "$vm"
	if running; then
		inside sudo /usr/local/bin/agentd ctl -c /etc/agentd/agentd.toml "cache status" || true
		inside sudo /usr/local/bin/agentd ctl -c /etc/agentd/agentd.toml "bridge status" || true
	fi
	;;
down) limactl stop "$vm" ;;
destroy)
	printf 'Delete the virtual phone and everything on it? [y/N] '
	read -r yes
	[ "$yes" = y ] && limactl delete -f "$vm" && rm -rf "$share"
	;;
*) die "unknown command $cmd (up, rebuild, ctl, sms, call, android, shell, status, down, destroy)" ;;
esac
