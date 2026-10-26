# Design: working from the phone without the Mac

Status: draft, 2026-10-26. The phone app (condx-mobile, nsh) today shows threads, approvals and
incidents, pairs with the Mac and the cloud site, and answers through either. This plan brings in
what is missing to work from the phone alone, without copying every desktop view: only what a
person needs away from the Mac, with the same look as the rest of the app and without extra calls.

## 1. What the phone has and lacks

| Area | Desktop | Phone today | Data in Condx Cloud |
| --- | --- | --- | --- |
| Threads (chats, agents) | yes | yes (live, cloud site answers when the Mac is away) | `threads:` |
| Approvals | yes | yes (sign, Touch ID stays on the Mac for prod) | `approvals:` |
| Incidents | yes | yes (diagnose, fix, deploy a fix) | `incidents:` |
| **Environments** (status of each deployment) | yes | **no** | `env:` (name, kind, project, deployment, reachable) — readable by phones, unused |
| **Jobs** (recent runs, status, error) | yes, with full log | **no** | `jobs:` (50 recent: recipe, env, status, requester, error, commit) — readable, unused; **no log** |
| Request a job / run a flow | yes | no (a phone can only decide, ask about incidents and write in threads) | — |
| Flows (workflows) | yes | only through incidents ("deploy the fix with…") | — |
| Agent settings | yes | read-only language | `_settings` row |
| Projects | yes | no (the cloud site clones them) | `_projects` row |
| Data, schema, SDK, terminal, frontend | yes | no — Mac-only by nature (local backend, files) | — |

## 2. Navigation

Five areas don't fit a segmented control. The phone gets a **bottom tab bar** with icons and a
short label — Threads, Approvals, Jobs, Environments, Incidents — and Settings from the top bar.
Badges stay on the tab (what waits). The thread view keeps the whole screen.

## 3. Phases

1. **Visibility** (no backend change): enable `env:` and `jobs:` on the phone; Environments
   (grouped by project, a dot for reachable, production in red) and Jobs (recent, status, error,
   who asked, how long), bottom tab bar, Settings spacing.
2. **Job logs**: the daemon publishes the tail of a job's log (redacted, last ~60 lines) only for
   running and failed jobs, in a field of the `jobs:` row; the phone shows it in the job's detail.
3. **Asking from the phone**: signed requests to run a recipe (non-prod) and a flow, verified like
   incident requests (the Mac or, with the Mac away, the cloud site runs them); production still
   needs the Mac.
4. **Agent settings from the phone**: language (and later default provider) through a signed
   request on the `_settings` row, applied by the Mac.
5. **Phone UI in Spanish**: the texts follow the phone's language (es/en), as the desktop does.

Each phase is tested on the phone (adb) in light and dark before it ships.
