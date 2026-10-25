# Requesting a job

1. **Jobs → New job**.
2. Pick the **recipe** from the dropdown. Each one shows which kinds of environments it runs on.
3. Pick the **environment** from the dropdown. Only the environments where that recipe can run appear.
4. Check the **Approval** row: "no approval" starts on its own; "needs approval" waits for you.
5. Fill in the **arguments**. They come with sensible defaults.
6. **Request job**. Its detail opens.

## The list and the detail

- **Recent** is a table: status, recipe, details (the function, the commit or the project),
  environment, who requested it, when and the error. Click a header to sort. Click a row to open its
  detail below.
- **In the detail, each step is a block** with its status, duration and output (that step's part of
  the log), which folds. The running step and the failed one are open.
- **Arguments:** shown as a JSON tree with search.
- **Full log:** with a minimum-level filter.

## While it runs or after

- **Cancel** stops the process and cleans up what it created.
- **Retry from the failed step** picks up where it left off.
- **One job per environment at a time**, in the order they become ready. Different environments run in parallel.
- When a job finishes, fails or is cancelled, a notice appears and stays in the notifications (the bell).

## Recipes

| Recipe | What it does |
| --- | --- |
| Deploy a commit | Devbox or throwaway: a worktree of the commit + `convex dev --once` |
| Run a function / Run a backfill | `convex run` |
| Backup (export) | Exports to `~/.condx/backups/`; encrypted for cloud |
| Restore a backup | `import --replace-all` (replaces all the data) |
| Restart the backend / Rebuild the deployer | Devbox maintenance |
| Create / Destroy a throwaway backend | Temporary backends |
| Deploy to cloud dev | `convex dev --once` on cloud dev, with approval |
| Rehearsal with real data | See "Promotion to production" |
| Promote to production / Roll back production | See "Promotion to production" and "When something goes wrong" |
