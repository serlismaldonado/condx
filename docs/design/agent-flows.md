# Flows with agents, events and threads

Status: design (2026-10-25). Builds on the workflow engine (`workflows.rs`), the agent host
(`agent_host.rs`, `agent-host/` with pi durable), incidents, notifications and the phone.

## 1. Goal

Several agents, each with its own responsibility, working **inside a flow**. An event (an error in
production, say) starts the flow: one agent looks at the error and writes a quick report; another
takes the report, rates the risk (low to critical), says what to check and whether it needs an
immediate fix; the developer gets a notice on the phone saying the error arrived **and was already
reviewed**. Other flows go further: an agent fixes it, another reviews the fix, the checks run, the
developer is notified again and approves (or, if the flow says so, it all goes on its own).

Every flow run is a **thread**: a chat with a reason (a fix, a task, an error) where one or more
agents and the developer talk in natural language. Everything a flow does can be followed,
questioned and steered from there. The agents run **durably** (pi durable): a run that waits hours
for an approval, or a daemon that restarts, doesn't lose or repeat work.

## 2. Concepts

### 2.1 Events

What can start a flow, normalized into one shape: `{ id, source, kind, project, env, title,
fingerprint, severity, payload, at }` (payload redacted, size-capped, untrusted).

| Source | What it is | Today |
| --- | --- | --- |
| `incident` | An incident opened or reopened (job, Vercel, CI, Convex down) | exists (incidents.rs) |
| `convex-error` | Errors of Convex functions on cloud dev / prod (the deployment's log stream) | new |
| `app-error` | Errors of the running app (console, promises, Convex calls) from Condx telemetry | exists as data (`get_errors`), not as events |
| `webhook` | Any JSON a service posts to Condx Cloud (Sentry, an uptime check, another tool), signed with a per-source secret | new |
| `task` | The developer opens a task from the chat ("add field X to Y") | new |
| `schedule` | Every N hours / a cron | new |
| `manual` | Run it from the app or the phone | exists |

Webhooks arrive at Condx Cloud (the Mac may be behind NAT) and reach the daemon by sync, like the
phone's requests; the daemon verifies the source's HMAC.

### 2.2 Triggers

A flow lists its triggers: `{ source, match, env, minSeverity }` (`match` is text that must appear in
the title, fingerprint or error, as in templates). The same fingerprint doesn't start a second run
while one is open: the new occurrence is added to the open thread ("happened 3 more times").

### 2.3 Agents (a catalog of roles)

An agent is a reusable definition, edited in Settings → Agents (today's templates grow into this):

- name and **role** (the instructions: "you triage errors", "you rate risk", "you fix", "you review");
- provider and model (or the defaults);
- **mode**: `read` (code and Condx through the MCP) or `write` (its own worktree, sandboxed checks);
- tools (a subset), per-run limits (tokens, minutes);
- **output**: free text or a structured result validated against a schema.

Built-in agents:

| Agent | Mode | Output |
| --- | --- | --- |
| Triage | read | `{ summary, where: [file:line], likely_cause, confidence }` |
| Risk | read | `{ risk: low\|medium\|high\|critical, impact, check: [..], immediate_fix: bool, reason }` |
| Fixer | write | branch + commit + `{ change, checks }` (what A3 does today) |
| Reviewer | read | `{ verdict: approve\|changes, findings: [..] }` on the fixer's diff |
| Explainer | read | plain text: answers questions in the thread |

Each agent sees the event, the outputs of earlier nodes it's given, and the thread's messages
addressed to it.

### 2.4 New node kinds and conditions

- `agent` `{ agent, input: [node ids whose outputs it reads], instructions? }`: runs an agent; its
  structured output becomes the node's outputs.
- **Conditions on edges**: an edge can carry `when`, e.g. `risk.risk >= high`,
  `risk.immediate_fix == true`, `review.verdict == approve`, `checks == passed`. A node runs when at
  least one incoming edge is satisfied; the others are skipped (and say why).
- `notify` grows: to the phone (push when nsh allows, ntfy today) and always into the thread, with a
  short summary built from outputs ("Error in payments.charge · risk high · fix recommended").
- `approval` (exists) can be answered on the phone; `ask`: an agent asks the developer something in
  the thread and the run waits for the answer.
- `deploy` stays a `recipe` node; production keeps Touch ID on the Mac, whatever the flow says.

### 2.5 Threads (the chat as the interface)

Every flow run opens a thread; tasks and free chats are threads too. A thread has:

- a **reason** (`error`, `fix`, `task`, `chat`) and its event;
- **participants**: the developer and the agents of the flow (each with its name and role);
- **messages**: from agents (text, plus a card for structured outputs: the risk report, the diff, the
  checks), from the flow (node started, waiting approval, skipped) and from the developer.

The developer writes in the thread: to the agent that's working (it steers it), or to one by name
(`@reviewer why did you reject it?`), which wakes it with the thread's context. Approve / reject buttons
appear in the thread when the flow waits. The Agents page becomes the thread list (today's chats and
incident runs are threads already); the phone shows the same threads.

### 2.6 Durability (why pi durable)

- Each agent node is a pi durable conversation bound to its thread; its first message carries
  `requestId = run:node`, so a retry or a restart never starts it twice.
- The flow run is already durable (SQLite, resumes after a restart). A node waiting for an agent, an
  answer or an approval can wait hours; nothing runs meanwhile.
- A developer message is a durable input to that conversation (steer or follow-up), so a reply sent
  from the phone while the Mac sleeps is applied when it wakes, once.
- Later: an agent may hand work to another inside the same run (sub-agent), still durable.

### 2.7 Autonomy, configurable per flow

Per flow: how far it may go on its own, e.g. "fix and open a branch without asking when risk ≤
medium; above that, wait for my OK", "deploy to dev automatically when review and checks pass",
"never deploy". Invariants no flow can change:

- agents never approve; production needs the developer (Touch ID on the Mac);
- write agents only touch their worktree, checks run offline in the sandbox;
- event payloads and logs are data, never instructions;
- a budget per run and per day (today's), and agents-at-once.

## 3. Example: "Error in production"

```
[event: convex-error on prod]
        │
   ┌────▼─────┐      ┌──────────┐
   │ Triage   │─────▶│  Risk    │
   └──────────┘      └────┬─────┘
                          │ always
                  ┌───────▼────────┐
                  │ notify phone   │  "payments.charge failing · high · fix recommended"
                  └───────┬────────┘
            risk ≥ high & │ immediate_fix
                  ┌───────▼────────┐
                  │ Fixer (write)  │  branch condx/fix-…
                  └───────┬────────┘
                  ┌───────▼────────┐
                  │ Reviewer       │
                  └───────┬────────┘
         verdict=approve  │
                  ┌───────▼────────┐
                  │ checks (CI)    │
                  └───────┬────────┘
                  ┌───────▼────────┐
                  │ notify + approval (phone) │
                  └───────┬────────┘
                  ┌───────▼────────┐
                  │ deploy dev → prod (Touch ID) │
                  └────────────────┘
```

All of it happens in one thread: the triage report, the risk card, the notice, the diff, the review,
the checks, the approval buttons, and anything the developer asks along the way.

## 4. What exists and what's new

| Piece | Exists | New |
| --- | --- | --- |
| Flow engine (graph, durable runs, approvals, notify) | yes | `agent` and `ask` nodes, edge conditions, triggers |
| Agent host with pi durable | yes (diagnose, fix, chat) | agent definitions with roles and structured output; runs bound to a thread and a node |
| Incidents as signals | yes | as flow events (plus convex-error, app-error, webhook, task, schedule) |
| Chat | yes (one agent per conversation) | threads with several agents, mentions, flow messages and cards |
| Phone | incidents, approvals, keys | threads, notices with the summary |

## 5. Phases

| Phase | What | Done when |
| --- | --- | --- |
| F1 | Agent catalog (roles, mode, output schema) and the `agent` node; outputs flow to the next nodes | a manual flow Triage → Risk produces a validated risk report in the run |
| F2 | Edge conditions; `notify` with a summary to the phone and the thread | the same flow notifies only when risk ≥ the threshold, and skips the rest with a reason |
| F3 | Threads: each run is a thread with its agents, flow messages, cards and mentions; the Agents page lists threads; `ask` node | the developer asks `@risk` something in the run's thread and gets the answer in context |
| F4 | Events and triggers: incident, convex-error, app-error, webhook (via Condx Cloud, HMAC), task, schedule; dedupe by fingerprint | a Convex error on dev starts the flow once and later occurrences join its thread |
| F5 | Fix pipeline in flows: Fixer → Reviewer → checks → approval or auto → deploy; autonomy per flow; built-in flows ("Error in production", "CI failing", "Task from chat") | a real error goes from event to a reviewed, tested fix waiting for approval on the phone |
| F6 | Threads on the phone (read, write, approve) | the whole loop can be driven from the phone |
| F7 | Outcomes per agent and flow (accepted, discarded, reverted, cost) | Settings shows which agents and flows pay off |

Incidents keep working as today; their autonomy settings become the default flow for each source,
so nothing changes until the developer edits it.

## 6. Open questions

- Error sources first: Convex function errors from the deployment's logs, app telemetry, or an
  external service (Sentry or similar) by webhook?
- A webhook payload can be large: keep only a capped, redacted excerpt plus a link?
- Can one flow run several agents in parallel (triage and a log search at once), or keep it one node
  at a time as today?
