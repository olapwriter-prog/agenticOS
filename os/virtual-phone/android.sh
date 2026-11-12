#!/bin/sh
# Android inside the virtual phone: Waydroid with the Google Play image, set to draw in software
# (a VM has no GPU), and the bridge folder agentd reads. Run as root once; Android then opens from
# the Android tab, and agentd pauses it when nothing needs it.
set -eu
user=$(sed -n 's/^android_user = "\(.*\)"/\1/p' /etc/agentd/agentd.toml)
home=$(getent passwd "$user" | cut -d: -f6)

modprobe binder_linux devices="binder,hwbinder,vndbinder" 2>/dev/null || true
if ! ls /dev/binderfs/binder /dev/binder >/dev/null 2>&1 && ! grep -q binder /proc/filesystems; then
	echo "No Android binder driver in this kernel ($(uname -r))."
	echo "Fix: sudo apt install linux-generic linux-modules-extra-generic, restart the VM, run this again."
	exit 1
fi

if [ ! -f /var/lib/waydroid/waydroid.cfg ]; then
	echo "== downloading Android with Google Play (about 1 GB)"
	waydroid init -s GAPPS -f
fi

# no GPU in the VM: Android draws with SwiftShader
cfg=/var/lib/waydroid/waydroid.cfg
if ! grep -q "ro.hardware.egl" "$cfg"; then
	grep -q '^\[properties\]' "$cfg" || printf '\n[properties]\n' >>"$cfg"
	sed -i '/^\[properties\]/a ro.hardware.gralloc = default\nro.hardware.egl = swiftshader' "$cfg"
	waydroid upgrade --offline
fi
systemctl enable --now waydroid-container

# the bridge app writes its spool here (Android's media folder for os.agentic.bridge)
spool="$home/.local/share/waydroid/data/media/0/Android/media/os.agentic.bridge"
mkdir -p "$spool"
echo "== Android is ready."
echo "   Open the Android tab on the phone, then the Play Store, and sign in."
echo "   Google asks a new Android device to be registered once; get its id with:"
echo "   scripts/mac/virtual-phone.sh ctl 'chat android device id'"
echo "   The bridge folder is $spool"
