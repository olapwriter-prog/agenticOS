# The Android bridge

Android sends events to Agentic OS and receives commands back. They move as small JSON files in a
spool folder: one file per event or command, written under a `.tmp` name and renamed when complete.
No network, no sockets.

Everything from Android is data. It is shown and quoted, never run as a command and never used as a path.
Ledger lines name the app and never carry message text.

## Folders

| Folder | Written by | Holds |
|---|---|---|
| `out/` | Android | events: `<time_ms>-<seq>.json` |
| `in/` | Agentic OS | commands: `<id>.json` |
| `files/` | either side | attachments the other side should read |

`bridge = "sim"` uses `<data_dir>/run/bridge-sim` (for tests and for `agentctl bridge sim`).
`bridge = "spool"` uses `bridge_dir`. `bridge = "none"` turns the bridge off.

## Events (Android → Agentic OS), version 1

| `kind` | Meaning |
|---|---|
| `notification` | a message or alert from an app (`package, app, text`, plus `conversation, sender, reply_key, attachments`) |
| `file` | a new file in Downloads, WhatsApp Documents or the camera folder |
| `share` | the user chose "Agentic OS" in an Android share menu |
| `removed` | a notification was dismissed (`reply_key`) |
| `hello` | the bridge app started |
| `result` | answer to a command (`id, ok, error`) |

## Commands (Agentic OS → Android), version 1

`reply` (`reply_key, text`), `open` (`package`), `share` (`package, rel, mime`), `dismiss` (`reply_key`).

## Limits

- An event file over 64 KiB is deleted unread.
- Text is cut to 4,000 characters.
- A file over 50 MiB is not copied.
- A file name keeps only its last part, with odd characters replaced, at most 120 characters.
- `rel` must start with `files/` and must not contain `..`.
- The inbox keeps the newest 500 messages; received files keep the newest 200 entries.

## Permissions

Every app starts with read, reply and files all off. A new app appears in settings, and its first message is
dropped. The owner allows each permission:

    bridge allow com.whatsapp read on
    bridge allow com.whatsapp reply on
    bridge allow com.whatsapp files on

Shares from the Android share menu are accepted without a rule, because the owner chose them.

## Tools

| Tool | Tier | What it does |
|---|---|---|
| `android_inbox` | Low | conversations, newest first, with unread counts |
| `android_messages_from` | Low | one conversation; marks it read |
| `android_reply` | Medium | replies through the notification's reply action; needs the reply permission |
| `android_files` | Low | files received from Android |
| `android_share` | High | sends a document to an Android app; needs the approval key |
| `android_bridge_status` | Low | up or down, apps allowed, unread, files |

A conversation is matched by exact name, then by exact sender, then by part of either. More than one
different match is refused with the list.

## Control commands

    bridge status
    bridge allow <package> <read|reply|files> <on|off>
    bridge sim '<event json>'      # only with bridge = "sim"

## Google Play

Google requires an uncertified device to be registered once with its device id before sign-in works. It can
take minutes to hours. Some banking and UPI apps refuse to run on uncertified devices, which is why payments
stay native in Agentic OS. Paid apps and purchases work only after sign-in with the owner's Google account. The
password is typed inside Android; Agentic OS never sees it.

## Not yet built

- The Android companion app (`android/bridge-app`), which needs an Android SDK and a device to build.
- The Google Play setup tools (plan Part C).
- The new screen design and the chat modes (plan Parts D to F).
