# Design: agents that respond to incidents

Status: A0 in progress (2026-10-25). Builds on Condx Cloud (`cloud.md`, phone approvals).

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
  ciphertext travels through Condx Cloud. Condx Cloud never sees a key in plaintext. (Phase A6.)
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

## 7. Phases

| Phase | What | Done when |
| --- | --- | --- |
| A0 | Incidents in the daemon and the Mac app; providers with encrypted keys and autonomy in Settings | a failed job opens one incident that groups repeats and resolves on its own; a key is saved encrypted and never comes back |
| A0b | Incidents on the phone (Condx Cloud scope), ignore and assign from there (signed) | the phone sees an incident and the daemon applies its decision |
| A1 | Push notifications (with the QR camera: needs a native host app around nsh) | the phone rings for a new incident |
| A2 | Agent host with pi durable, diagnose only; the conversation in the Mac app and on the phone | an incident gets a diagnosis that survives restarting the daemon |
| A3 | Fix mode: worktree, tests on a throwaway environment, commit on `condx/fix-…` | "fix ready" with the diff summary and the test result |
| A4 | Approve the fix on the phone → deploy workflow (dev; production with Touch ID); steer from the chat | a fix approved from the phone reaches cloud dev |
| A5 | Agent templates per incident kind ("agent factory"), budgets and limits | each kind has its tools and limits |
| A6 | Keys re-encrypted for paired devices and remote runners | an agent runs elsewhere without the key in plaintext in the cloud |
