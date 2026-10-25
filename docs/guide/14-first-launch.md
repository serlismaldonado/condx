# First launch, the palette and pre-deploy checks

## First launch

With no projects yet, **Overview** turns into a three-step guide:

1. **Check this Mac.** Node.js 18+, the Convex CLI session (`npx convex login`), the local
   Convex backend, git, and optionally the GitHub CLI (for CI) and Docker (native mode doesn't
   need it). Each missing piece shows the command that fixes it, with a copy button.
   - **Download** fetches `convex-local-backend` for this Mac from Convex's GitHub releases into
     `~/.condx/daemon/bin`. If `npx convex dev --local` already downloaded one, Condx reuses it.
   - Downloading an executable is app-only: an agent through the MCP can't do it.
2. **Add your Convex project.** Pick a folder with `convex/` and `package.json`, or one of the
   Convex repos Condx found in your projects folder. **Create in Convex** makes a new one.
3. **Start it.** Local environment → **Start all**: backend, deployer (`convex dev`) and frontend.

## Command palette

**⌘K** (Ctrl+K on the web demo) opens a palette with fuzzy search:

- **Go to** any page (Overview, Workflows, Git & CI, Jobs, Approvals, Settings…).
- **Environments**: an environment, or straight to one of its tabs (`prod › Data`,
  `local › Logs`…).
- **Tables**: any table of an environment whose Data tab you opened, e.g. `comments · local`.
- **Projects**: switch project.
- **Actions**: start or stop the local services, add or create a project, notifications, theme.

The last commands you ran lead the list.

## CI for every project

Condx reads the GitHub Actions runs of each project's `origin` (`gh run list`, so `gh` must be
logged in). They show in **Git & CI** and in the overview, and the workflow check **CI is green**
uses the project's own CI.

## Compare schemas before deploying

In an environment's **Schema** tab, **Compare local → cloud** reads the schema deployed in both
(the cloud one with its deploy key, from the Keys tab) and lists:

- tables, fields and indexes that come, go or change;
- **risks** that would make `convex deploy` fail or leave documents invalid: a new required
  field, a field that changes type or becomes required, a field that leaves a typed table;
- **environment variables** the local backend has and the cloud doesn't (names only, never
  values): the code reads them and they'd be missing there.

Nothing is deployed. The same comparison is a workflow check (**The local schema is safe to
deploy here**, which also fails on missing variables) and an MCP tool (`get_schema_diff`), so an agent can look before it asks.
