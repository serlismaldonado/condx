# The phone app (Android)

Condx on your phone: talk with the agents, approve what waits for you and get notified, from
anywhere. It's open source: [condx-mobile](https://github.com/serlismaldonado/condx-mobile).

## What it needs

- An Android phone (the APK is for arm64, which covers almost every phone from the last years).
- **Condx Cloud** linked on the Mac. The phone and the Mac talk through it, so the phone works on
  mobile data and with the Mac asleep. Setting it up takes a few minutes: see
  [Condx Cloud and the cloud site](16-cloud.md).

## Install

1. On the phone, open the
   [latest release](https://github.com/serlismaldonado/condx-mobile/releases/latest) and download
   `condx-mobile-<version>-arm64.apk`.
2. Open the file. Android asks once to allow installs from your browser (**Install unknown apps**):
   allow it, then **Install**.
3. Optional: each release has a `.sha256` file next to the APK to check the download.

Updates come by themselves: when a new version is out, the app shows a banner with **Install**
(and a notification). They are signed with the same key, so they install over the old version and
keep your pairing.

## Pair it with the Mac

1. On the Mac: **Condx → Settings → Condx Cloud → Pair a phone**. A QR code and a code appear.
   The code works once and expires in 5 minutes.
2. On the phone: open Condx and scan the QR (or paste the link).
3. Back on the Mac, check that the fingerprint matches the one the phone shows, and **Confirm**.

The phone creates its own key, kept in Android's Keystore. It never leaves the phone: every
message and every approval you send is signed with it, and the Mac and Condx Cloud check the
signature.

## What you can do

| | |
| --- | --- |
| **Threads** | Chat with the agents. Reply to a message and the answer goes to whoever wrote it; type `@` to bring in someone else. |
| **Approvals** | Deploys, releases and merges that wait for you, with a slider. Production also asks for Touch ID on the Mac. |
| **See changes** | The diff of what an agent changed, before you approve. |
| **Voice notes** | Talk instead of typing. Transcribed on your Mac with Whisper: the audio stays home. |
| **Photos and files** | A screenshot of the bug, a log, a PDF. Agents that see images read them. |
| **Notifications** | Replies, incidents and approvals, as push and in the app's bell. Tap one to open its thread. |

## With the Mac off

The phone keeps the last state. Messages you write wait in Condx Cloud and are delivered when the
Mac comes back. If you set up the cloud site (see [16-cloud.md](16-cloud.md)), the agents answer
right away instead. Voice notes wait for the Mac: they're sealed for it.

## Unpair or lose the phone

- **On the phone:** **Unpair this phone**.
- **On the Mac:** Condx → Settings → Condx Cloud, **Remove** next to the phone. Do this if you lose the phone:
  from then on, nothing signed with its key is accepted.

## Build it yourself

The [repo's README](https://github.com/serlismaldonado/condx-mobile#readme) has the steps
(`nsh`, the Android SDK and NDK).
