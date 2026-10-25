# Design: agents that respond to incidents

Status: all phases done (A0-A6, 2026-10-25, local). Next: push inside Condx and the QR camera when nsh releases its host channel; remote runners (A7, designed in §6.7). Builds on Condx Cloud (`cloud.md`, phone approvals).

## 1. Goal

When something breaks (a failed job, a failed frontend build, CI in red, Convex not responding),
Condx tells you on your phone and, if you want, puts an agent on it: it diagnoses, prepares a fix
on a branch with a commit, and waits for your approval to run a deploy workflow.

## 2. Principles

1. **The daemon decides, the agent proposes, you approve.** The daemon detects incidents, starts
   agents, gives them their tools and enforces the limits. An agent never approves, never pushes,
   never deploys and never changes the rules.
2. **Nothing leaves the Mac without your approval.** A push or a deploy goes through the same
   approvals as today (signed from the phone; Touch ID on the Mac for production).
3. **Logs are untrusted.** Errors carry your users' data and can carry instructions aimed at the
   agent. The agent works without secrets, in its own git worktree, with tools that can't reach
   production; whatever it proposes goes through your approval.
4. **Configurable from the app.** Providers, models, keys, autonomy and budgets are set in the app
   (Settings → Agents). The MCP can read incidents, not change any of this.
5. **Keys are encrypted at rest and never shown again.** See §5.

## 3. Pieces

| Piece | Where | What |
| --- | --- | --- |
| Incidents | daemon (`incidents.rs`) | Detects, groups (fingerprint), resolves on its own when the signal clears |
| Agent settings | daemon (`agents.rs`) | Providers (encrypted keys), default model, autonomy per source, budgets |
| Agent host | Node sidecar with pi durable (A2) | One durable conversation per incident; resumes after a crash |
| Mac app | Incidents page, Settings → Agents | List, ignore/resolve, assign an agent, the conversation |
| Phone | Condx Cloud scope `incidents` (A0b) | See incidents, assign an agent, approve the fix, chat |

### 3.1 Why pi durable

pi durable (Earendil, TypeScript, experimental) gives what a long-running agent needs: every step
checkpointed in SQLite (it resumes after a crash), exactly-once submissions (`requestId` = the
incident's run id), conversations that can be watched from several clients and steered while they
work, and many providers through pi-ai. It is the agent *runtime*; approvals and limits stay in the
daemon (pi has no approval primitive: a `beforeTool` hook asks the daemon). Risks: the API may
change and one process owns a storage; the sidecar is pinned to an exact version.

## 4. Incidents (A0)

An incident is a signal turned into a record with its evidence:

| Source | Fingerprint | Resolves when |
| --- | --- | --- |
| `job` failed | recipe + env + normalized first error line | a later run of the same recipe on the same env succeeds |
| `vercel` deploy failed | branch | a later deploy of that destination is ready |
| `ci` failed | repo + workflow | the latest run of that workflow passes |
| `convex` not responding (cloud environments only: a devbox that's off isn't an incident) | environment | it responds again |

- Same fingerprint → same incident: `count` and `lastSeen` go up; nothing new to notify.
- A resolved incident that comes back reopens (after 10 minutes resolved, it's a new occurrence).
- Evidence is redacted (`devtool_core::redact`) before it's stored: error, end of the log, commit,
  URL.
- Ignoring is a decision for the person: only the app can ignore or resolve (scope `JobsApprove`).

## 5. Providers and keys

- Providers that pi-ai supports: Anthropic, OpenAI, Google, OpenRouter, xAI, Groq, Mistral and any
  OpenAI-compatible endpoint (base URL, e.g. Ollama).
- The key is encrypted with **age** to the Mac's identity `agent-key` (in the Keychain, created on
  first use) and the ciphertext is stored in the daemon's SQLite (`agent_providers`). The app only
  sends it (write-only) and sees a hint (`…abcd`).
- **Running from the phone or elsewhere:** age encrypts to several recipients. A paired phone (or a
  remote runner) gets its own X25519 key; the daemon re-encrypts each provider key to it and the
  ciphertext travels through Condx Cloud. Condx Cloud never sees a key in plaintext. (Phase A6, §6.6.)
- The agent host gets the key in its environment when the daemon starts it, never on disk.

## 6. Autonomy

Per source (`job`, `vercel`, `ci`, `convex`), set in the app:

| Level | What happens |
| --- | --- |
| `off` | Not recorded |
| `notify` | Recorded and sent to the phone (default) |
| `diagnose` | An agent reads (repo, logs, MCP) and explains the cause |
| `fix` | It prepares a fix on a branch with a commit and asks for approval to deploy |

Plus: default provider and model, daily token budget, and how many agents can run at once. A fix is
always a proposal: it waits for your approval whatever the level.

## 6.1 The agent host (A2)

- `agent-host/` is a small Node program (TypeScript run with `--experimental-strip-types`, no build):
  a pi durable `Harness` on `~/.condx/daemon/agents.sqlite`, one conversation per run, the run id as
  the `requestId`. The daemon starts it when an agent is first needed and sends its config on stdin
  (the decrypted API keys travel there, never in the environment or on disk); it answers on
  127.0.0.1 with a token only the daemon knows. If the daemon goes away, its stdin closes and the
  host exits.
- Diagnose tools, all read-only and confined to the incident's repo: `read_file`, `list_files`,
  `search_code` (git grep), `git_log`, `git_show`; secret files (`.env*`, keys, `.git/`) are never
  read. `condx` reads from the daemon through the MCP with the **agent token**: a third token, kept
  in memory, `Read` scope, only accepted on `/mcp`, and only for operational tools (state, jobs,
  logs, deploys; never database rows or tab telemetry).
- The prompt says the evidence is untrusted data and asks for a fixed answer: Cause, Evidence, Fix,
  Confidence.
- The daemon follows each run every 3 s, adds its tokens to today's usage, stops it past the daily
  budget, and stores the answer in the incident (`agent`). Writing to the agent steers it while it
  works or continues the conversation.
- One provider per built-in kind (pi-ai registers providers by kind); OpenAI- and
  Anthropic-compatible ones (own base URL) can be several.
- **Import from pi** (Settings → Agents): the daemon reads `~/.pi/agent` (`models.json` custom
  providers with their URL and models, `auth.json` API keys, `settings.json` default) and stores each
  key encrypted. OAuth logins aren't copied (they rotate their tokens: a copy would break pi's
  session) and keys that come from a command (`!…`) aren't run.
- Tried with a real model (MiniMax M3, imported from pi) on 2026-10-25: on a one-line date bug it
  diagnosed the cause with the exact evidence (~40k tokens), and in fix mode reproduced the failure
  with `npm test` offline, changed the line, got the tests passing and left the commit (~30k tokens).

## 6.2 Fix mode (A3)

- The agent's write tools (`edit_file`, `write_file`) and `run_check` exist only in fix mode; a
  diagnose conversation doesn't have them.
- `run_check` runs `npm|pnpm|yarn|bun run <script>` for a script that was in the base commit's
  `package.json` (the agent can't add one and run it), under `sandbox-exec`: no network at all (not
  even localhost, where the daemon listens), writes only in the worktree and temp folders, no
  reading of `~/.condx`, `~/.ssh`, `~/.aws`, `gh`'s config or the Keychain service. Checks that
  need the network fail, and the agent says so.
- The worktree lives in `~/.condx/daemon/worktrees/<run>`; the branch is `condx/fix-<run>`. The
  daemon, not the agent, commits (author "Condx agent"), with the agent's "Cause" in the message.
  Nothing is pushed: deploying is running one of the project's enabled workflows with that commit,
  which asks for the approvals that workflow has. A frontend fix that Vercel must build needs the
  branch pushed: that's a later, approved step.

## 6.3 Templates (A5)

- The first enabled template whose sources and optional match text (title, fingerprint, error,
  recipe) fit the incident is used; the generic one, with no sources, catches the rest.
- Highest mode: a diagnose-only template turns an automatic fix into a diagnosis and refuses a
  manual "Prepare a fix".
- Tools: a template can only remove tools from its mode, never add one (the write tools exist only
  in fix mode). Instructions are added to the prompt (pi's per-conversation `instructions`).
- Limits per run: the daemon checks every 3 s and stops the run past its tokens or minutes, so a
  run can go a little over. The daily budget still applies on top.

## 6.4 Incidents on the phone (A0b)

- The daemon publishes open and ignored incidents (and those resolved in the last hour) to the
  `incidents:<daemon>` scope: title, source, environment, counts, the error and the end of the log
  (redacted, short), the agent's summary (status, mode, template, model, answer up to 3000
  characters, the fix's branch, files and stat — never the patch) and the project's enabled
  workflows.
- A phone can only patch the request fields of a row: `request` (diagnose, fix, ignore, resolve,
  deploy, discard, message), `requestArg`, `requestId`, `requestedAt`, `deviceKey`, `signature`.
  It signs `condx-incident:v1|daemon|incident|request|requestId|requestedAt|arg` with its key.
- The daemon verifies the signature against the phones this Mac confirmed, at most 10 minutes old,
  and applies the request with the same rules as the app (template, budget, enabled workflows and
  their approvals). It publishes `result` and `resultFor` (the request id); a request whose id is
  already in `resultFor` isn't applied again, also after a restart.
- A panic in a sync pass no longer stops the sync thread: it's reported and the session reopens.

## 6.5 Notifications (A1)

- Why ntfy: push inside the Condx app needs nsh's host channel (the native app around nsh gives the
  app its push token and the camera). It exists on nsh's `main` but not in the released 0.2.0, and
  only on the TSX side; and APNs and FCM need an Apple Developer account and a Firebase project.
  ntfy works today on both phones without either.
- Messages are short and carry no evidence (title, source, environment; the agent's first line of
  cause), redacted. On ntfy.sh the server sees them and whoever knows the topic can read them: the
  app suggests a random topic, and a self-hosted server is supported (https, or on this Mac).
- The daemon sends them (not the cloud): it's the one that detects incidents, so when the Mac is
  off there's nothing new to tell.

## 6.6 Keys on paired devices (A6)

- When a phone pairs it creates an age (X25519) identity next to its signing key (Keychain or
  Keystore) and sends the public `age1…` recipient with the claim. The Mac shows it with the device
  and trusts it only after the person confirms the pairing (`cloud-devices.json`, like the signing
  key).
- The daemon publishes scope `keys:<daemonId>`: one row per provider (name, kind, model, hint and
  the key armored with age to the Mac plus every confirmed device). It's read-only for devices; the
  cloud only stores ciphertext. Rows are cached and re-sealed only when providers or devices change.
- Unpairing a device re-seals every key without it. A copy it already had stays readable to it, so
  if a phone is lost: unpair it and rotate the provider key.
- Phones paired before A6 have no recipient: they must pair again to receive keys.
- A remote runner can pair the same way (its own identity, confirmed on the Mac); not built yet.

## 6.7 Remote runners (A7, not built)

A machine away from the Mac (a Linux box, a CI runner, a server) that runs agents when the Mac is
off or busy. Design only; nothing of it exists yet.

- **Pairing:** like a phone. `condx runner pair` shows a code; the Mac confirms the key's
  fingerprint and stores the runner in `cloud-devices.json` with its signing key, its age recipient
  and `kind: runner`. Unpairing it re-seals the keys without it (A6).
- **Work:** the daemon publishes a scope `runs:<daemonId>` with the runs it hands out (incident,
  mode, template, evidence redacted as for the phone). A runner takes one with a signed claim
  (`condx-run:v1|daemon|run|runner|at`); the daemon accepts the first and ignores the rest.
- **Keys:** the runner reads scope `keys` and opens only the provider its template names; the key
  lives in memory for that run.
- **Repo:** the runner clones from the remote (`git remote get-url origin`) at the incident's
  commit with its own deploy key (read-only); it never gets the Mac's checkout or `~/.condx`.
- **Tools:** no Condx MCP over the internet. The evidence travels with the run; tools that need
  the Mac (`get_environment`, `list_jobs`) are left out of the runner's tool list.
- **Result:** diagnosis as text; a fix as a patch (`git format-patch`) signed by the runner. The
  daemon checks the signature, applies it on `condx/fix-<run>` in its own worktree, runs the checks
  in the sandbox and only then shows "Fix ready". Deploying stays on the Mac (workflow + approvals).
- **Limits:** the Mac's budget and agents-at-once count runner runs too; a runner that doesn't
  report in the template's minutes loses the run.
- **Open questions:** where runners get Node and pi (container image?), whether a runner can run
  checks that need network, and how to rotate its deploy key.

## 7. Phases

| Phase | What | Done when |
| --- | --- | --- |
| A0 | Incidents in the daemon and the Mac app; providers with encrypted keys and autonomy in Settings | a failed job opens one incident that groups repeats and resolves on its own; a key is saved encrypted and never comes back |
| **A0b** ✅ | Incidents on the phone (Condx Cloud scope), ignore and assign from there (signed) | done: scope `incidents`; the phone diagnoses, prepares a fix, deploys it with an enabled workflow (slide to confirm), discards it, writes to the agent, ignores and resolves, each a signed request the daemon verifies and applies once (also across restarts, through `resultFor`) |
| **A1** ✅ (ntfy) | Notifications to the phone | done with **ntfy**: the daemon posts to the person's topic (ntfy.sh or their own server) on a new incident (at most every 30 min per incident), an agent that finishes and an approval that waits; Settings → Notifications with a random topic, an optional token (Keychain) and a test button. Push inside the Condx app and the QR camera wait for nsh's host channel (on `main`, not in 0.2.0, TSX side only) |
| **A2** ✅ | Agent host with pi durable, diagnose only; the conversation in the Mac app (the phone in A0b) | done: `agent-host/` (Node, pi durable 1.4.0, SQLite), `agent_host.rs`; a failed job opens an incident, the agent starts on its own (autonomy "diagnose"), reads the repo and Condx, and its diagnosis lands in the incident; runs survive a host restart; budget and agents-at-once enforced by the daemon |
| **A3** ✅ | Fix mode: worktree, checks offline, commit on `condx/fix-…` | done: the daemon makes a worktree from the incident's commit (sharing `node_modules`), the agent edits there and runs the project's checks inside `sandbox-exec` (no network, writes only in the worktree, no reading `~/.condx`, `~/.ssh`… or the Keychain; only the scripts of the base commit), and the daemon commits as "Condx agent" when it ends; the user's checkout never changes. "Discard" removes the branch and the worktree |
| A4 (Mac) ✅ | Run an enabled workflow with the fix's commit from the incident | done: the workflow asks for its own approvals; suggested workflows start turned off |
| **A4 (phone)** ✅ | Approve the fix on the phone → deploy workflow; steer from the chat | done with A0b |
| **A5** ✅ | Agent templates per incident kind ("agent factory"), budgets and limits | done: `templates.rs` with five built-in templates (frontend build, failing tests, Convex deploy, Convex not responding diagnose-only, generic); each sets when it applies, highest mode, provider/model, extra instructions, allowed tools and per-run limits (tokens, minutes). Edited in Settings → Agents → Templates |
| **A6** ✅ | Keys re-encrypted for paired devices and remote runners | done for phones: age identity at pairing, scope `keys` sealed to the Mac and confirmed devices, re-sealed on unpair; verified with two phones (one revoked) and a scan with no plaintext key in the cloud or daemon databases. Runners pair the same way when they exist |
| A7 | Remote runners (design in §6.7) | not started |
