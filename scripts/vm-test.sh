#!/bin/sh
# Build the agentd package and test it as a real OpenRC service in an aarch64 Alpine VM on a Mac.
# Alpine is what postmarketOS is made of, and Apple Silicon runs aarch64 natively, so this checks
# the phone's architecture, libc, init system, cgroups and llama-server package. A VM has no
# modem, GPU or thermal sensors; those stay for the phone.
#   scripts/vm-test.sh [path/to/small-model.gguf]
# Needs: brew install lima. Uses a VM named "agentos" (created on first run).
set -eu
cd "$(dirname "$0")/.."
vm=agentos
share="$HOME/.lima-share/agentd-build"   # Lima shares your home folder (read-only) with the VM
model="${1:-}"

if ! limactl list -q 2>/dev/null | grep -qx "$vm"; then
	limactl start --name="$vm" --cpus=6 --memory=6 --disk=30 --vm-type=vz --tty=false template:alpine
elif ! limactl list "$vm" | grep -q Running; then
	limactl start "$vm"
fi

echo "== packing the current commit"
rm -rf "$share" && mkdir -p "$share"
git archive --prefix=agenticOS-0.1.0/ -o "$share/agentd-0.1.0.tar.gz" HEAD
cp os/postmarketos/* "$share/"
sed -i '' 's|^source="\$pkgname-\$pkgver.tar.gz::\$url/archive/refs/tags/v\$pkgver.tar.gz|source="agentd-0.1.0.tar.gz|' "$share/APKBUILD"
[ -n "$model" ] && cp "$model" "$share/model.gguf"

limactl shell "$vm" env SHARE="$share" sh -s <<'VM'
set -eu
if [ ! -f ~/.abuild/abuild.conf ]; then
	sudo apk add -q alpine-sdk cargo cargo-auditable python3 curl
	sudo addgroup "$(whoami)" abuild
	abuild-keygen -a -n >/dev/null 2>&1 || true
	key=$(ls ~/.abuild/*.rsa | head -1)
	echo "PACKAGER_PRIVKEY=\"$key\"" > ~/.abuild/abuild.conf
	sudo cp "$key.pub" /etc/apk/keys/
fi
rm -rf ~/agentd && cp -r "$SHARE" ~/agentd   # the home folder is mounted at the same path
cd ~/agentd
repo=~/packages/$(basename "$HOME")             # where abuild -r puts the packages
# each run is a new release number so apk upgrades
cur=$(apk list -I agentd 2>/dev/null | sed -n 's/^agentd-[^ ]*-r\([0-9]*\) .*/\1/p')
rel=$(( ${cur:--1} + 1 ))
sed -i "s/^pkgrel=.*/pkgrel=$rel/" APKBUILD
abuild checksum >/dev/null 2>&1
echo "== building the package (runs its tests)"
env SUDO_APK="sudo apk --no-interactive" abuild -r >~/build.log 2>&1 || { tail -30 ~/build.log; exit 1; }
grep -E "test result: .* [1-9]+ passed" ~/build.log | sed 's/^/   /'
sudo rc-service agentd stop >/dev/null 2>&1 || true
sudo apk add -q -u --repository "$repo" agentd agentd-openrc
apk list -I agentd | cut -d' ' -f1
if [ -f model.gguf ]; then
	sudo install -Dm600 model.gguf /var/lib/agentd/models/qwen2.5-0.5b-instruct-q4_k_m.gguf
fi
sudo sed -i "s|^power_dir|# power_dir|" /etc/agentd/agentd.toml   # no battery in a VM

C="sudo agentd ctl -c /etc/agentd/agentd.toml"
G=/sys/fs/cgroup/agentos.slice
pass=0; fail=0
check() { if eval "$2"; then echo "  ok   $1"; pass=$((pass+1)); else echo "  FAIL $1"; fail=$((fail+1)); fi; }

sudo rc-service agentd start >/dev/null; sleep 4
echo "== checks"
check "service starts and answers" '[ "$($C stage)" = Asleep ]'
check "secrets are root-only" '[ "$(sudo stat -c %a /etc/agentd/passphrase /etc/agentd/gateway-token | sort -u)" = 600 ]'
check "ledger and anchor verify" '$C verify | grep -q "ledger ok.*anchor Ok"'
check "plugging in (asleep) gives the model the chip" '$C "power 80 charger" >/dev/null; [ "$($C stage)" = Deep ] && [ "$(cat $G/agentos-llm.slice/cpu.weight)" = 10000 ]'
check "unplugging takes it back" '$C "power 90" >/dev/null; [ "$(cat $G/agentos-llm.slice/cpu.weight)" = 100 ]'
check "freezer and model cgroups exist" '[ -d $G/agentos-agents.slice ] && [ -d $G/agentos-build.slice ] && [ -d $G/agentos-llm.slice ]'
check "a core is kept from the model" '! grep -qx "$(cat $G/agentos-llm.slice/cpuset.cpus.effective)" /sys/fs/cgroup/cpuset.cpus.effective'
T=$(sudo cat /etc/agentd/gateway-token)
check "MCP refuses a missing token" '[ "$(curl -s -o /dev/null -w %{http_code} -d {} http://127.0.0.1:8765/mcp)" = 401 ]'
check "MCP lists tools with the token" 'curl -s -H "Authorization: Bearer $T" -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}" http://127.0.0.1:8765/mcp | grep -q send_sms'
check "chat runs a tool and wakes the phone" '$C "chat add dentist to my calendar on monday" | grep -q "^done" && [ "$($C stage)" = Active ]'
check "background is frozen while Active" '[ "$(cat $G/agentos-agents.slice/cgroup.freeze)" = 1 ]'
check "a call waits for the approval key" '$C "chat call mum" | grep -q "approval required"'
if [ -f /var/lib/agentd/models/qwen2.5-0.5b-instruct-q4_k_m.gguf ]; then
	sleep 3
	check "llama-server is running and ready" '$C models | grep -q "small.*ready"'
	check "llama-server is in the model cgroup" 'grep -qx "$(pidof llama-server)" $G/agentos-llm.slice/cgroup.procs'
	check "model weights are protected" '[ "$(cat $G/agentos-llm.slice/memory.min)" -gt 0 ]'
	check "the model answers what intents cannot" '$C "chat what do I have on monday" | grep -q "^done"'
	A=$(pidof agentd); sudo kill -9 "$A"; sleep 6
	check "kill -9: the model server dies too, agentd respawns" '[ -n "$(pidof agentd)" ] && [ "$(pidof agentd)" != "$A" ] && $C models | grep -q running'
else
	echo "  (no model given: skipping the llama-server checks)"
fi
sudo rc-service agentd stop >/dev/null
check "a clean stop leaves nothing running" '[ -z "$(pidof agentd)" ] && [ -z "$(pidof llama-server)" ]'
echo "== $pass passed, $fail failed"
$C prio 2>/dev/null || true
[ "$fail" = 0 ]
VM
