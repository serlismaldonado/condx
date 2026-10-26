# Design: keep working with the Mac off (cloud site, boat workers, Claude)

Status: draft, 2026-10-26. Phase S0 done (Condx Cloud on Convex cloud, site on `condx.heskala.com` through Vercel); the rest is design only. Builds on Condx Cloud
(`cloud.md`), agents (`agents.md`, remote runners in §6.7) and agent flows (`agent-flows.md`).

## 1. Goal

Today everything that acts lives on the Mac: the daemon, the agent host (pi durable), the local
backend, the Keychain, and even the Condx Cloud backend the phone talks to (a self-hosted Convex
behind a cloudflared tunnel). Shut the Mac and the phone goes dark.

We want agents to keep working when the Mac is off: answer in threads, diagnose, fix, run tests and
deploy to **Cloud Dev** and **Vercel previews**, and hand everything back when the Mac returns.
Production stays a Mac-only, human-approved action.

## 2. What can live away from the Mac

| Away from the Mac | Only on the Mac |
| --- | --- |
| Agents (pi durable is Node; Claude Code runs on Linux) | **Production**: promote and rollback, Touch ID |
| Code on a clone of the GitHub remote: `condx/*` branches, PRs | The devbox: local Convex backend in Docker, its data, local services |
| Tests, typecheck, lint in a sandbox | The Keychain and the Mac's own secrets |
| Deploy to Cloud Dev (dev deploy key) and Vercel previews | Anything tied to the machine (simulators, native tools) |
| A throwaway Convex backend in the sandbox (replaces the devbox for tests) | Merging to `main` |
| Phone approvals for non-prod actions (signatures checked against the device list the Mac confirmed) | |

## 3. Shape

```
phone ──► Condx Cloud (Convex cloud, always on) ◄── Mac daemon (when on)
                         ▲
                         └── cloud site: condxd on Linux in a small boat VM
                                  │  orchestrator: long-lived Claude Code + Condx MCP
                                  └─► boat workers: one sandbox per heavy task,
                                      forked from a per-project base snapshot
```

- **Condx Cloud on Convex cloud** (S0). It already is a Convex app; it only needs a real
  deployment in the user's own Convex account. Always on, HTTPS, `*.convex.site` for `/pair`,
  `/ingest`, `/files`. The phone and the daemons sync through it; nothing else changes.
- **Cloud site** (S1): the same `condxd` built for Linux, `condxd serve --site cloud`, in a small
  boat VM (2 vCPU / 4 GB). Its environments are `cloud dev`, `vercel preview` and `throwaway`;
  production does not exist there. It can stop when idle (boat snapshots and stops billing) and be
  woken by the Mac or by Condx Cloud when work arrives.
- **Workers** (S2): one boat sandbox per fix, deploy or test run, forked in seconds from a base
  snapshot per project. Used even when the Mac is on: heavy work leaves the laptop and runs in
  parallel, each agent in its own machine.

### Why boat.dev

boat runs full, persistent Ubuntu VMs made for agents: Docker inside; Node, Bun, Rust, GitHub CLI
and Chrome preinstalled; SSH; a virtual desktop; stop (snapshot, billing paused), resume, and
disk-level fork from snapshots; CLI with `--json`, HTTP API and a TypeScript SDK; `boat prompt`
runs Claude Code inside. Per-second billing (small 2 vCPU/4 GB 0.018 USD/h, default 4 vCPU/8 GB
0.036 USD/h, 20 USD/month minimum). EU regions only for now (Germany, Finland, France).

What it gives Condx that a plain VPS doesn't: fork-per-task isolation that is cheaper and stronger
than worktrees, a browser for end-to-end checks of a Vercel preview, and stop/resume for an idle
cloud site. What it doesn't give: a documented public URL story, so Condx Cloud itself goes to
Convex cloud, not boat.

## 4. Providers and harnesses

A template gets a `harness`:

| Work | Harness | Model |
| --- | --- | --- |
| Chat, triage, explain, summaries | `pi` (agent host, as today) | MiniMax (cheap, fast) |
| **Fixes, deploys, reviewed changes** | **`claude-code`** in a boat worker | **The user's Claude account** |
| **Main orchestrator** | Long-lived `claude-code` session on the cloud site, Condx MCP at level *orchestrate* | The user's Claude account |

- **The Claude account is used through Claude Code**, logged in with the user's account: Condx runs
  `claude -p --output-format stream-json` with the Condx MCP and an allowed tool list. Condx never
  lifts the session's OAuth token into pi (that would also break the session, as with pi's import).
- `stream-json` carries partial text and tool calls, so the phone's live view (`live` items, the
  shimmer, the caret) works as with pi.
- **The orchestrator** already has its tools from the external MCP level *orchestrate*
  (`wait_for_mention`, `give_task`, `run_workflow`, `answer_question`, `post_to_thread`): it listens
  to mentions and events, hands work to agents (MiniMax or Claude in workers), answers flow
  questions and reports in the thread. **Approvals stay human**, on the phone.
- **Limits:** a Claude plan has usage windows. Several fixes at once can hit them; the harness falls
  back by rule (wait, an Anthropic API key from Console if configured, or MiniMax for non-critical
  work). For unattended 24/7 or team use the supported route is an API key; the personal account fits
  work the user asks for.
- **The session lives in the base snapshot.** The snapshot is private (only workers fork it) and the
  session is renewed periodically (`claude login` on the base, re-snapshot).

## 5. Keys (least privilege)

The cloud site pairs like a phone (A6): its own identity, confirmed on the Mac, keys re-sealed to
it, unpairing re-seals without it. It gets only:

- Convex **dev** deploy key (never prod).
- A Vercel token limited to preview deployments of the project.
- A GitHub fine-grained token: push `condx/*` branches and open PRs; no `main`.
- Provider keys it needs (MiniMax), re-encrypted to it. Claude stays in Claude Code's own login.

The daily budget and the agents-at-once limit (with the queue, `threads::drain_queue`) count work
from both sites and from workers.

## 6. Who owns a thread

- Each thread has an owner site: `mac` or `cloud`. Only the owner runs its agents and answers it.
- **Leaving:** the Mac sends a heartbeat through Condx Cloud. If the cloud site misses it for N
  minutes, or the user taps "Continue in the cloud", the cloud site takes the threads and the
  queue. The conversation moves as transcript plus context (more robust than copying pi durable's
  SQLite); the agent resumes with a summary of what was said.
- **Coming back:** the Mac fetches the `condx/*` branches, the threads return to it, and the cloud
  site posts what it did in each thread. Merging and production happen on the Mac, approved.

## 7. Phases

| Phase | What | Status |
| --- | --- | --- |
| **S0** ✅ | Condx Cloud on Convex cloud: deploy into the user's Convex project, relink the Mac daemon, re-pair the phone; the tunnel and the self-hosted backend stay only for tests | done 2026-10-26: the user's own Convex project, site proxy `condx.heskala.com` (Vercel `condx-cloud-site`); verified on the phone: with the daemon stopped it keeps the last state and queues a message, delivered and answered when the daemon returns. The phone can't yet tell that the Mac is off (heartbeat comes with S5) |
| S1 | `condxd` + agent host for Linux in a Docker image; `serve --site cloud`; runs in a small boat VM; paired like a phone with least-privilege keys | design |
| S2 | boat workers: per-project base snapshot, fork → work → push branch → delete; counted by the limit and the budget | design |
| S3 | `harness: claude-code` with live text, for fixes and deploys | design |
| S4 | Orchestrator: long-lived Claude Code on the cloud site with the Condx MCP | design |
| S5 | Thread ownership and handoff Mac ↔ cloud (heartbeat, "Continue in the cloud", return) | design |
| S6 | Git policy: backup push after every run, PR before deploy, direct deploy on request | design |
| S7 | Email fallback through AgentMail (`heskala@agentmail.to`): notifications out, requests in | design |
| S8 | `@mayer` + releases: CI builds (desktop on GitHub Actions macOS, APK on Linux), signed update manifest, daemon-side auto-updater, download links and "update ready" notifications | design |

## 8. S0 in detail

1. Create a Convex project (e.g. `condx-cloud`) in the user's Convex account and a **production
   deploy key** for it (dashboard → Settings → Deploy keys). The key is the user's; Condx never
   fetches admin keys on its own.
2. `scripts/cloud-deploy.sh` (run by the user in a terminal): reads the deploy key without echo,
   deploys `cloud/` with `npx convex deploy` ignoring `cloud/.env.local` (which points at the
   self-hosted test backend), derives the deployment URL from the key and links the installed
   daemon (`condxd cloud link`, key on stdin, used once, not stored).
3. **Domain:** the site (what people see: the QR's `/pair` page, the SDK's `/ingest` endpoint,
   `/files`) goes on `condx.heskala.com`; the API stays on `<deployment>.convex.cloud` (only the
   daemon and the phone use it, and `*.convex.site` keeps working, so the phone needs no change).
   The daemon stores it in `cloud.json` (`site`, set with `condxd cloud site <url>` or
   `CONDX_SITE=… scripts/cloud-deploy.sh`). Two ways to serve it: Convex's custom domains for the
   deployment's HTTP actions (paid plan), or a small Vercel project on the domain whose `vercel.json`
   rewrites everything to `https://<deployment>.convex.site` (free). DNS for `heskala.com` is at
   Namecheap: one CNAME either way.
4. Re-pair the phone from the Mac app (the old backend's pairing doesn't carry over).
5. Check: the phone syncs on mobile data with the tunnel down; with the Mac off it shows the last
   state and queues messages, delivered when the Mac returns.

The self-hosted backend in `~/.condx/cloud-test` and its tunnel stay for tests (fixtures, live
tests); nothing in S0 removes them.

## 9. A self-managing software factory

The factory should keep itself running: reachable when the app isn't, able to repair and update
its own apps, and never lose code.

### 9.1 Fallback channel: email (AgentMail, `heskala@agentmail.to`)

When the phone app can't reach the agents, or doesn't answer, the cloud site talks by email.

- **Out:** notifications go by email as well as ntfy when the phone hasn't synced for N minutes or
  an event is critical ("Mayer fixed the phone app: download link…", "a deploy waits for you").
- **In:** an email to `heskala@agentmail.to` from an allowlisted sender (the user's addresses)
  becomes a message to the orchestrator; the answer goes back in the same email thread. Inbound
  events arrive over AgentMail's WebSocket (no public webhook needed, works on the Mac and the
  cloud site).
- **Email can ask, never approve.** A From header can be spoofed: email can request work, status
  or a repair; approvals (deploys, production) still need the phone's signature or the Mac. Mail
  content is passed to agents as the user's request, with that reduced power.
- The AgentMail API key is the user's, stored like any other daemon secret.

### 9.2 Mayer: the agent that maintains Condx (`@mayer`)

A catalog role, **Mayer** (`@mayer`), whose job is Condx itself: the desktop app, the daemon, the
phone app and the cloud backend. Harness `claude-code`, repos `condx` and `condx-mobile`.

- The user asks for it anywhere: in a thread (`@mayer the voice note button does nothing`), by
  email, or the factory calls it when a health check fails (the phone app stopped syncing after an
  update, the desktop app crashes at launch).
- Mayer works like any fixer: branch `condx/mayer-…`, push, PR, checks; and then a release.

### 9.3 Releases and updates

| App | Built where | Delivered how |
| --- | --- | --- |
| **Desktop** (GPUI, macOS only: Metal) | GitHub Actions macOS runner (free on the public `condx` repo), signed with the Condx signing cert kept as an Actions secret | GitHub Release + an update manifest; **the daemon** (launchd, it survives an app crash) checks it, verifies the signature, installs app and daemon (`install-daemon.sh` pattern: new file + rename) and restarts them |
| **Phone** (nsh, Android) | A boat worker or GitHub Actions (Linux + NDK), signed with the release keystore (secret) | The APK on Condx Cloud's site (`condx.heskala.com/download/android`) + a notification with the link (ntfy, email, the app's own banner). Android installs on tap; the signing key keeps updates continuous |
| **Daemon / agent host** | With the desktop release | Same updater |

- **Updates are signed** with an update key (Ed25519). Clients verify it before installing.
- **A desktop update waits for an approval** on the phone (one tap) unless the user turns on
  automatic updates; the notification says when it's installed and running.
- **If the desktop app is what's broken**, the updater still runs (it lives in the daemon), so a
  fixed release reaches it without the user doing anything.

### 9.4 Git policy

- **Always back up:** every agent run that changes code ends by pushing its branch to GitHub. A
  project without a remote gets one proposed (private repo; creating it needs an approval).
- **Deploys go through a PR:** branch → push → PR → checks → approval in the app → merge → deploy
  flow. **Direct deploy** only when the user asks for it explicitly ("deploy directo"); production
  still needs its approval and Touch ID.
- Per project: `git.backupPush` and `deploy.viaPR` (both on by default).

## 10. Costs (estimate)

- Convex cloud: Condx Cloud's traffic fits the free tier for one user.
- boat: the 20 USD/month minimum covers the small cloud site; workers cost cents per task
  (a 20-minute fix on 4 vCPU ≈ 0.01 USD).
- Models: MiniMax tokens as today; Claude through the user's plan (or an API key if configured).

## 11. Open questions

- How a sleeping cloud site gets woken: from Condx Cloud (an HTTP action calling boat's API with a
  token stored in the deployment) or only by the Mac before it sleeps.
- Latency from EU boat regions to a US Convex deployment and to Vercel; acceptable for agents, to
  measure.
- How long a Claude Code session in a snapshot stays valid, and how to detect expiry before a run.
- Whether workers may reach the network for checks (Cloud Dev, Vercel) while the rest of the
  sandbox stays offline-by-default.
