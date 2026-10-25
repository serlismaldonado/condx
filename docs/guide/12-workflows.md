# Deploy workflows

A **workflow** is the recipe for how a project gets from a commit to an environment: which steps
run, in what order, how long it waits and where your approval is needed. They're built on the
**Workflows** page (sidebar, Project group), like in n8n: nodes joined by arrows.

heskala-erp starts with its usual promotion already built as a workflow ("Promotion to production":
backup → rehearsal → cloud dev → wait 30 min → promote). **Workflows → New → Create suggested**
adds, turned off, the suggested ones the project is missing: deploy to local, deploy to cloud dev
(CI green, cloud dev responds, your approval, deploy, notice), rehearsal with cloud dev data, roll
back production and cloud backups.

## The nodes

| Node | What it does | What you configure |
|---|---|---|
| **Recipe** | Requests a job from any recipe (backup, rehearsal, deploy, run a function…) and waits for it to finish. If the recipe asks for approval, the node waits for it. | Recipe, environment and one field per argument |
| **Wait** | Lets a few minutes pass (e.g. to see whether errors show up before going on). | Minutes |
| **Approval** | Stops until you approve it in the app. *Reinforced*: you type the environment's name; in production, with Touch ID. | Environment, level and message |
| **Check** | Checks something and fails if it isn't met: the environment responds, there's a recent backup or CI is green. | What to check, environment, hours |
| **Notice** | Leaves a message in the run. | Message |

Each node also has:

- **On/off switch:** turned off, it's skipped without deleting it.
- **On failure:** *stop the workflow* (default) or *continue with the next*.

In a recipe node, the **environment** only lists the environments where that recipe runs (it's
picked on its own if there's just one), and each argument has its field. The **Variables** block
explains what you can write in them, and a click copies one:

- `{run.commit}`: the commit or branch you choose when you start the run;
- `{node.<id>.<output>}`: what an earlier connected node left, for example
  `{node.backup.backup_id}` (the number of the backup made by the `backup` node).

## Building a workflow

- **New:** from a template, blank, importing a JSON, or **Create suggested**.
- **Add nodes:** with the palette at the top (Recipe, Wait, Approval, Check, Notice).
- **Move:** by dragging the node.
- **Connect:** pull the dot on a node's right side to another node.
- **Configure:** click the node: its panel opens on the right (you can also remove the node or its
  connections there).
- **Moving around the canvas:** two fingers to pan, pinch or ⌘ + wheel to zoom.
- **Save:** changes stay as a draft until you save (or *Discard*).
- **Export:** copies the workflow as JSON to the clipboard, to keep it in the project's repo or take
  it to another project with *Import*.
- **On/off:** the switch at the right end of the bar. A workflow that's off is kept with its runs
  but can't be run, not even by an agent through the MCP.

If the workflow is missing something (a recipe, an environment that doesn't exist, minutes at 0, a
cycle), it says so at the top and won't let you run it.

## Running and following a run

- Type the **commit or branch** (default `dev`) and tap **Run**.
- Nodes run one at a time, in arrow order. In the **Runs** list, picking one shows each node's
  status on the canvas (pending, running, waiting for approval, done, failed, skipped) and below,
  each one with its message and a link to its job.
- **Go to approve:** when a node waits for your approval, it takes you to the inbox (the "to
  approve" chip at the top tells you too).
- **Cancel:** stops the run and cancels the running job.
- **Retry:** on a run that failed or was cancelled, starts again from that node (the earlier ones
  aren't repeated).
- A run uses the workflow **as it was when it started**: editing it afterwards doesn't change it.
- If the daemon restarts, running workflows carry on by themselves.

## From an environment

Each environment has a **Workflows** tab with the workflows that touch it (some node points at that
environment): its last run with each step's status, **Run** (with the commit or branch at the top)
and **Open** to edit it. Below are that environment's approvals, so a workflow is followed and
approved without leaving the tab.

## Agents (MCP)

Through the MCP, an agent can **list** workflows (`list_workflows`), **run** one (`run_workflow`)
and see a run (`get_workflow_run`). Their approval nodes wait as always: **only the app approves**.
Approval nodes always ask, even when Settings has "No approval" for that kind of environment.
