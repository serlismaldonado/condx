# Other Convex projects

Any Convex repo of yours can have its local instance in Condx, with its backend, its deployer and
its frontend.

## Choosing the project

The **project selector** is in the title bar, next to the logo. The one you choose filters the
whole app (and is remembered next time):

- **Overview:** its environments, its git and its local deployer.
- **Environments** in the sidebar: only its own.
- **Jobs:** its jobs, and only the recipes that run on its environments.
- **Services** (in the local environment's Summary; "Local environment" if there are no
  environments) and **Terminal:** its instance and its folder.
- **Approvals** shows every project's: nothing waits unseen.

## Add or create: two different things

- **Add existing project:** registers a folder that already has `convex/` and `package.json`. It
  doesn't touch Convex: it only gives it ports and connects it.
- **Create project in Convex:** creates one from scratch in your Convex account (see below).

Both are in the title bar's project selector and in the "Project" fold of the local environment's
summary.

## Adding an existing project

**Add project** opens a **side panel** on the right:

- Type the repo's folder (`~/…` works) and tap **Add**, or
- tap **Add** on one of the repos **found** nearby (the ones with `convex/` and `package.json`).
  Each row shows its Convex project, if `.env.local` says so, and its frontend.

The panel closes when you choose and the new project is selected. If something fails, the error
goes to the notifications.

When adding it, Condx:

1. names it: the Convex project from `.env.local` (`# team: …, project: …`) or the folder;
2. gives it its own ports: backend from **3220** (in steps of 10; the site is the next one) and
   frontend from **3001**;
3. detects the frontend: **Vite** or **Next** from `package.json`. If there's neither, it only gets
   a backend and a deployer.

It also registers two environments:

- **`<project>-local`** (devbox): its native backend.
- **`<project>-dev`** (cloud dev): its deployment from `.env.local`, if it has one.

They're updated every time the daemon starts. Nothing is started yet.

## Creating a project in Convex

**Create project in Convex** opens a panel with the name (folder and project: lowercase, numbers and
hyphens), the folder where it's created, the **frontend template** in a dropdown (the official
`create-convex` ones: React + Vite, Next.js and TanStack Start, each with its shadcn/ui and login
variants — Convex Auth, Clerk, WorkOS AuthKit, **Better Auth** — or "Convex only", without a
frontend) and the Convex team. The Better Auth ones are the component's official examples: since
they come from a monorepo, the template step swaps their local dependencies for npm versions. After
that, set their variables (`BETTER_AUTH_SECRET`, `SITE_URL`) as their README says. It requests a
**project-create** job, which waits for your approval because it creates resources in your account:

1. generates the folder with `npm create convex@latest <name> -- -t <template>`;
2. installs dependencies (`npm install`);
3. creates the project and its development deployment with
   `npx convex dev --once --configure new`, with this Mac's Convex CLI session (if there isn't one,
   the step fails and asks for `npx convex login`);
4. adds it to Condx, like "Add existing project".

## Removing a project from Condx

In the project selector, the **×** on its row (or "Remove from Condx" in its fold). It asks for
confirmation. It stops appearing, its services stop and its environments are removed, but its
folder, its local data and its Convex project are **not** deleted: you can add it again (it shows
up among the found ones again).

## Starting its local instance

Choose the project at the top and use **Start all** (or each card):

| Service | What it does |
| --- | --- |
| **Convex backend (native)** | `convex-local-backend` on its port, with data in `~/.condx/daemon/native/<project>`. The first time it starts **empty**. |
| **Deployer (convex dev)** | Deploys the repo's functions to that backend every time you save. |
| **Frontend** | `vite` or `next dev` on its port, pointing at that backend. |

- The frontend gets the local backend's URL through environment variables (`VITE_CONVEX_URL`,
  `NEXT_PUBLIC_CONVEX_URL`, `CONVEX_URL` and their `…_SITE_URL`). Those variables win over
  `.env.local`, which is **not touched**: the repo keeps pointing at its cloud dev when you use it
  outside Condx.
- The deployer runs from a mirror folder and doesn't write to `.env.local`.
- Everything is native: no Docker.
- The processes keep running even if you close the app.

The **logs** below and the **Terminal** follow the selected project: a new terminal opens in its
folder.

## Bringing the cloud data

The first time, a project's local backend is empty. Its card (Services) has two buttons; both are
enabled if the repo's `.env.local` has `CONVEX_DEPLOYMENT=dev:<name>` (the card shows it as **Cloud
dev**):

| Button | Recipe | Approval |
| --- | --- | --- |
| **Bring data and variables from cloud dev** | `project-seed` | none: runs right away |
| **Copy production (with approval)** | `project-seed-prod` | in **Approvals**, typing the project's name |

Both do the same, each from its source:

1. **Export** the cloud data (with `files = yes`, file storage too) to a temporary 0600 zip.
2. **Read the environment variables** into a temporary 0600 file. If the project has a frontend,
   `SITE_URL` becomes its local URL so auth redirects come back to your Mac.
3. **Import** the zip into the local backend with `--replace-all`: **they replace its local data**.
   The deployer stops meanwhile. The zip is deleted at the end.
4. **Copy the variables** to the local backend (the value goes through stdin, never in arguments or
   the log).
5. **Start the deployer again**, which deploys the repo's functions.

The cloud is only read, with your CLI session (`npx convex login`). If the import fails, the zip
stays in `~/.condx/daemon/tmp/` for the retry; request the job again or delete it.

> The variables are the cloud's: test API keys, webhooks, etc. What the app does locally with them
> (emails, test payments) really goes out to those services.

## From the CLI and agents

```bash
condxd project discover            # Convex repos nearby
condxd project add ~/code/my-project
condxd project list
condxd project remove my-project
```

In the MCP: `list_projects`, `register_project`, and `list_services`, `service_action` and
`get_service_logs` with `project`.

From agents or the CLI: `request_job` with `recipe = project-seed` (or `project-seed-prod`),
`env = local` and `args = { "project": "<name>" }`. The production copy always waits for your
approval in the app.

## Recipes on its environments

A project's environments run the recipes marked `projects = true`: deploy a commit, deploy to cloud
dev, run a function or a backfill, backup (export), restore (import) and the two that bring data.
They run in the project's repo with its Convex CLI. Approvals are the same by kind of environment
(for example, deploying to cloud dev asks for a simple approval).
