# Design: Condx Cloud (local-first, optional)

Status: draft, 2026-10-25. Nothing here is built yet except where a phase says so.

## 1. Goal

Condx stays a local tool: the daemon runs processes, the Convex CLI and the local backend, and keeps
secrets in the Keychain. **Condx Cloud** is an optional layer on top that lets you:

1. **approve from your phone** (or another machine) what the daemon is waiting on;
2. see the daemon's state (environments, jobs, workflow runs) from anywhere, live;
3. later: share workflows, rules and history with a team, and run several Macs against one view.

Without the cloud, everything keeps working as today. With it, the daemon is still the one that acts.

## 2. Principles

- **Local-first, daemon-authoritative.** The daemon's SQLite is the source of truth. The cloud is a
  relay and a mirror. Offline, nothing changes for the local app.
- **Secrets never leave the Mac.** No deploy keys, admin keys, tokens or env var values go to the
  cloud. Only state and metadata (names, statuses, timestamps, redacted logs).
- **The daemon has the last word.** A change made in the cloud (an approval, turning a workflow on)
  is a *request*. The daemon checks it (who, fresh enough, still pending, allowed by its rules) and
  only then acts. Production can keep requiring Touch ID on the Mac.
- **Outbound only.** The daemon connects to Convex (WebSocket). No open ports, no inbound tunnel.
- **Agents can't approve,** wherever the request comes from. The MCP stays as it is.
- **Bring your own Convex.** Phase 1 deploys Condx Cloud's backend into the user's own Convex
  account (a separate project, e.g. `condx-cloud`), never into the deployment Condx manages.

### Why not the managed project's own deployment

- Devtool data would mix with business data and show up in Schema and Data.
- If a deploy breaks that backend, you lose the tool that helps you fix it.
- Rules about who reads what get muddled.

## 3. Building blocks

We reuse the local-first library [`localfirst`](https://github.com/serlismaldonado/localfirst)
(MIT, public; born inside Heskala ERP), which includes the Rust port (`rust/localfirst-core`).

| Piece | Reuse | Where |
| --- | --- | --- |
| Protocol (doorbell → pull → push, cursors, idempotent `opId`, per-field conflicts, sets) | as is | `src/core/types.ts`, `src/core/merge.ts`, `src/core/sets.ts` |
| Server logic in Convex functions (`planOp`, sets, protocol types) | as is | `@heskala/localfirst/server` |
| Web/phone client (IndexedDB, React hooks, Convex transport) | as is | `idb`, `react`, `convex` |
| Server tables and adapter | **new, small** | Condx Cloud backend: tables for Condx's scopes, not `collection_records` |
| Daemon client | **new, Rust** | `localfirst-core` crate: a port of types/merge/sets/view plus a reduced engine on SQLite |

### 3.1 The Rust port, and why it's small

The daemon is Rust; embedding a JavaScript runtime would be heavy and fragile. But the daemon doesn't
need the whole engine:

- **One process.** No tab leader election, no broadcast between tabs.
- **It's the source of truth** for its own data (jobs, runs, environments), so it mostly *publishes*:
  idempotent `push` of what changed, no conflicts expected.
- It *subscribes* only to a few scopes written from outside (approval requests, workflow toggles).

So the port is: types, `planOp`, sets and view (pure, ~250 lines in TS), an outbox and cursors on the
SQLite the daemon already uses, and a transport on the official `convex` Rust crate (a subscription
for the doorbell, a query for `pull`, a mutation for `push`).

The engine is **sans-IO**: it never awaits the network. The daemon asks it for the next push or pull,
makes the call without holding it, and hands back the result (`next_push`/`push_done`,
`next_pull`/`pull_done`). A slow call never blocks writes, and tests control every interleaving.

### 3.2 Same behaviour in both languages

- The TS test kit (`src/testing`) generates convergence sequences. We export cases as **JSON
  fixtures** (`fixtures/*.json`: input + expected output) and run them in both test suites, so the
  Rust port must decide exactly like TS.
- Set elements are compared by their JSON text, as in TS (`JSON.stringify`). The Rust side uses
  `serde_json` with `preserve_order`, so objects keep their key order and compare the same.

## 4. Data in the cloud (scopes)

A scope is what syncs as a unit (as in the library). Proposed:

| Scope | Written by | Contents |
| --- | --- | --- |
| `env:<daemonId>` | daemon | environments: name, kind, health, commit (no URLs with secrets, no keys) |
| `jobs:<daemonId>` | daemon | recent jobs: recipe, env, status, who requested, timestamps; redacted log tail |
| `approvals:<daemonId>` | daemon (pending) / phone (requests) | what waits for approval, and approval requests |
| `runs:<daemonId>` | daemon | workflow runs and node states |
| `workflows:<daemonId>` | daemon / phone | definitions and the on/off switch |

`daemonId` is a random id created on first link, stored in the daemon's config.

### 4.1 Approval request

```ts
// written by the phone client through push (one op)
{ scope: "approvals:<daemonId>", kind: "patch", rowId: "<jobId>",
  set: { decision: "approve" | "reject", decidedBy: "<userId>", decidedAt: <ms> } }
```

The daemon receives it with `pull`, then checks:

1. the job is still `waiting-approval` and the request names this job's current attempt;
2. `decidedAt` is recent (default: 10 minutes) — **no offline approvals for production**;
3. the user is linked to this daemon and allowed by the daemon's rules for that environment kind;
4. for production: the daemon asks for Touch ID on the Mac (the cloud can only *propose*).

If any check fails, the daemon writes back `rejectedReason` and does nothing.

## 5. Linking a daemon

1. The user deploys `cloud/` to a Convex project of theirs (`npx convex deploy`). A guided job that
   does it from the app is a later step.
2. Settings → **Condx Cloud** (or `condxd cloud link`): the URL and a deploy key. Condx registers the
   daemon (`daemons:register`, internal) with a random **daemon token**, kept in `cloud-token`
   (0600) like the daemon's other tokens; the deploy key is used once and not stored.
3. The phone opens the cloud's web app, signs in (Convex Auth), and pairs by scanning a QR the Mac
   shows (short-lived code). Pairing is confirmed on the Mac.
4. *Unlink* removes the token and stops syncing; the cloud keeps nothing the daemon needs.

## 6. Security

- Threat: someone with access to the cloud backend. They can see state and propose approvals; they
  can't run anything the daemon rejects, and production needs Touch ID on the Mac.
- All text going up passes through `devtool_core::redact` (logs, errors).
- The daemon token can only write the daemon's own scopes; phone users only write requests.
- Each push is idempotent (`opId`), so retries never approve twice.
- Stale requests (older than the window, or for an old attempt) are ignored and reported.

## 7. Phases

| Phase | What | Done when |
| --- | --- | --- |
| **C0** ✅ | Clone the library, this doc, Rust `localfirst-core`: types, `planOp`, sets, view, with the TS tests ported and shared JSON fixtures | done 2026-10-25: 400 shared cases match |
| **C1** ✅ | Reduced engine in Rust: SQLite outbox and replica, cursors, backoff; a fake transport and the contract tests | done 2026-10-25: sans-IO engine; 6000 seeds × 100 steps converge on memory and SQLite, with calls in flight across steps |
| **C2** ✅ | Condx Cloud backend (Convex project): tables, doorbell/pull/push for Condx scopes, auth | done 2026-10-25: 15 convex-test tests, differential vs `FakeServer` (300 seeds agree), and the Rust engine against a throwaway local backend |
| **C3** ✅ | Daemon publishes `env`, `jobs`, `approvals` (one-way mirror) through the Rust transport | done 2026-10-25: `cloud.rs` (sync thread, SQLite engine), link/unlink from Settings or `condxd cloud`, tested end to end against a throwaway backend (approval reflected, offline queue flushed on reconnect, no secrets uploaded). The web view comes with C4 |
| C4 | **Approve from the phone**: requests, the daemon's checks, Touch ID for production, pairing by QR | an approval from the phone runs a job on the Mac; a stale one is refused |
| C5 | Workflows on/off and runs from the phone; several daemons per user | — |

### 7.1 Notes from C2

- Convex stores object keys sorted. Fields in `set` and `overwrote` may come back in another order
  (harmless), and set items that are objects get their keys sorted: keep set items scalar, or
  objects with sorted keys, so the local view matches before the server confirms.
- A permanent rejection is a `ConvexError` with `{ code, message, status }`; Convex rolls back the
  whole group. Numbers come back as float64 (`1.0`); the Rust transport normalizes them.
- Registering a daemon is internal (admin or deploy key only): no public function creates daemons.

## 8. Open questions

1. ~~Licence.~~ Settled 2026-10-25: the library is public and MIT at
   `serlismaldonado/localfirst`.
2. Convex component vs. tables in the app's schema (the ERP kept them in its schema; a component
   would let Condx Cloud `app.use(localfirst)`).
3. Hosted Condx Cloud (multi-tenant) later, or only bring-your-own Convex.
4. Push notifications to the phone (web push from a Convex action) for new approvals.
