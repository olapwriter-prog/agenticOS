# Agentic Bridge (companion app for the Android sandbox)

Sends messages (from apps you allow), new files and shares to Agentic OS, and sends replies and files back.
The protocol is in `docs/bridge.md` of the repository.

## Build

This app is **not built in CI**. Build it on a machine with the Android SDK (API 34) and JDK 17:

    ./gradlew assembleRelease
    ./gradlew testDebugUnitTest       # the JSON format tests

## Install on the sandbox

    waydroid app install app/build/outputs/apk/release/app-release.apk

Then open "Agentic Bridge" in the sandbox and grant the three rows on its screen:
notification access, file access, and run in background.

## What it does not do

- It never logs message text.
- It does not read other apps' screens. It uses only notifications, files and shares.
- Replies work only through a notification's own reply action; if an app has none, the reply fails.
