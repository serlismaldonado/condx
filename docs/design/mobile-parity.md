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

1. ✅ **Visibility** (done 2026-10-26; the daemon now also publishes `startedAt` and the default
   project's name): enable `env:` and `jobs:` on the phone; Environments
   (grouped by project, a dot for reachable, production in red) and Jobs (recent, status, error,
   who asked, how long), bottom tab bar, Settings spacing.
2. ✅ **Job logs**: the daemon publishes the tail of a job's log (redacted, last ~60 lines) only for
   running and failed jobs, in a field of the `jobs:` row; the phone shows it in the job's detail.
3. ✅ **Asking from the phone** (signed `_run` requests; the daemon publishes the `_catalog` row with recipes and flows): signed requests to run a recipe (non-prod) and a flow, verified like
   incident requests (the Mac or, with the Mac away, the cloud site runs them); production still
   needs the Mac.
4. ✅ **Agent settings from the phone**: language (and later default provider) through a signed
   request on the `_settings` row, applied by the Mac.
5. ✅ **Phone UI in Spanish** (`src/i18n.rs`, translated inside the shared theme helpers): the texts follow the phone's language (es/en), as the desktop does.

Each phase is tested on the phone (adb) in light and dark before it ships.
6. ✅ **Summary and local services** (2026-10-26): the daemon publishes the `_overview` row (per
   project: git, deployer, local services with CPU and memory, what needs attention, each
   environment's latency and deployed commit; every 20 s, every 3 s while a service changes). The
   Environments tab shows the desktop's stat cards on top and, per project, attention, environments,
   repository and local services with Start / Stop / Restart (and Start all / Stop all) through a
   signed `_run` request `{service: {project, name, action}}`. Services need the Mac: with the Mac
   away the buttons hide.
7. ✅ **Continue in the cloud** (2026-10-26, code done; boat test pending): with the Mac away, the
   cloud site runs **one project at a time against its cloud dev** — no local backend in the cloud
   (small machine; tests go to cloud dev). Its services per project: the deployer (`convex dev` with
   the environment's dev deploy key) and the frontend (pointing at the cloud dev URL). Starting a
   project stops the other's.
   - Keys: the Mac seals each cloud-dev deploy key (never production; per environment, saved in the
     Keys tab) only for the site, in `keys:` rows `deploy:<env>`; the site stores it and registers
     the environment.
   - Code: on start, the site's clone follows the branch the Mac was on (from `_overview`), if it's
     on GitHub and the clone has no changes of its own. With the Mac on, the phone offers «Push
     branch» (`_run` `{handoff}`; main branches excluded) when it has commits only on the Mac; with
     the Mac away it shows the cloud's branch and what's left only on the Mac.
   - Requests: the site answers `_run` service requests (`siteFor`/`siteResult`, allowed while the
     Mac is away) and publishes `site-overview` (only once the frontends are hosted, so a rejected
     push can't jam its outbox before Condx Cloud allows the field).
   - Frontend: `scripts/boat-hosts.sh` hosts each project's frontend port privately (token URL) and
     stores the URLs in `/data/condx/hosts.json`; the phone opens it with «Open». The container runs
     with `--network host`. Running services keep the machine on up to 3 h after the last request.
   Projects in the Environments tab fold (remembered on the phone).
