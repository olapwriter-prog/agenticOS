# A virtual Agentic OS phone on a Mac

A Linux VM on your Mac (Apple Silicon) shown in a phone-shaped window, running the whole phone:

- agentd, the agent, with its vault, ledger, tools and predictive cache;
- the local model (Qwen3.5 4B), served by agentos-llm on the Mac's CPU cores;
- the Agentic OS screen (agentos-ui) under sway, 390 × 844 points like the prototype;
- Android in Waydroid, with the Google Play Store, on workspace 2, paused when nothing needs it.

The modem and battery are simulated: texts and calls are sent in from the Mac (below).

## Start it

In Terminal on the Mac, in this repository:

```sh
git checkout ccr-5b2c3e9c-xxflh1      # the branch with the virtual phone
scripts/mac/virtual-phone.sh up
```

The first run installs Lima (with Homebrew), creates the VM (6 CPUs, 8 GB, 60 GB disk), builds
everything inside it and downloads the model (about 2.7 GB). It takes 20 to 40 minutes. When it
finishes, the VM restarts and its window shows the Agentic OS home screen. On a Mac with 8 GB of
memory, use `MEMORY=6 scripts/mac/virtual-phone.sh up`; to skip the download, pass a model you have:
`up --model ~/Downloads/Qwen3.5-4B-Q4_K_M.gguf`.

## Use it

- Click to tap; the Mac keyboard types. **F1** is the Agentic OS screen, **F2** is Android.
- From the Mac:
  - `scripts/mac/virtual-phone.sh sms "Mum|Are you coming for dinner?"`: a text arrives.
  - `scripts/mac/virtual-phone.sh call +919845012345`: a call rings.
  - `scripts/mac/virtual-phone.sh ctl "chat weather tomorrow"`: ask the agent.
  - `scripts/mac/virtual-phone.sh ctl "cache status"`: what the predictive cache holds.
  - `status`, `shell`, `down` (stop), `rebuild` (after changing the code), `destroy`.

## Android and Google Play (once)

```sh
scripts/mac/virtual-phone.sh android
```

This loads the binder driver, downloads Android with Google Play (about 1 GB), sets it to draw in
software (the VM has no GPU) and starts it. Then open the Android tab on the phone, open the Play
Store and sign in. Google asks for a new Android device to be registered once:
`ctl "chat android device id"` gives the id to register.

Messages from WhatsApp, Telegram and other apps reach the agent through the bridge app
(`android/bridge-app`). Build it in Android Studio on the Mac, copy the APK into the VM, and install
it with `waydroid app install`. agentd already reads its folder (`bridge_dir` in
`/etc/agentd/agentd.toml`).

## What is not checked yet

This was written in a cloud session without a Mac, so nothing here has been run on one:

- **Lima:** the VM window comes from Lima's `video.display` with Apple's Virtualization framework.
  It needs a recent Lima and macOS 13 or later.
- **Android's binder driver:** it comes from Ubuntu's `linux-modules-extra`. If the cloud image's
  kernel lacks it, `android.sh` says so and how to switch kernels.
- **Software drawing:** sway draws in software (pixman), and the UI uses Mesa's software Vulkan or
  GL. Expect it to work but not to be smooth.
- **The model on the CPU:** the predictive cache keeps the system prompt read, so only the first
  request after a start is slow.

Run it from a Claude session on the Mac (the desktop app's Code tab with a local folder, or
`claude` in Terminal in this repository), so that session can fix what the Mac reports.
