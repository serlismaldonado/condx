# Promotion to production

It's the **"Promotion to production"** workflow (see "Deploy workflows"): in the production
environment's **Workflows** tab (under Environments, **prod**) you run it, see which step it's on
and approve. The steps are the usual ones:

```
1. Prod backup ─► 2. Rehearsal ─► 3. Cloud dev ─► 4. Wait 30 min ─► 5. Promote
```

## 1. Production backup

- Recipe "Backup (export)" on prod. No approval: it only reads.
- It stays encrypted on this Mac (`prod-<date>.zip.age`) with a number, for example #1.
- **It's valid for 2 hours** for promoting.

## 2. Rehearsal

- Uses the backup from step 1 and the commit being promoted.
- In a throwaway backend on this Mac: it imports the backup, deploys the commit, runs the
  backfills, checks there are no errors, records the result and deletes the throwaway. **Production
  isn't touched.**
- If the new schema doesn't accept the old data, request it from production's Jobs tab with
  `base_commit` (the commit to deploy before the import).

## 3. Cloud dev

- "Deploy to cloud dev" and, after it, the backfills.
- In **Approvals**, approve **the deploy first**; if you approve a backfill before, it runs on the
  old code.

## 4. Wait 30 minutes

Cloud dev has to work for 30 minutes **since the deploy** without function errors. The page shows
how long is left. Meanwhile, it's a good idea to try the app against cloud dev.

## 5. Promote

1. In **Approvals**, type **"prod"**, **Approve** and confirm.
2. The job checks everything again, deploys Convex with the production deploy key, runs the
   backfills, watches for errors for 5 minutes and deploys the frontend. If something fails, it
   stops there: follow the log in the Jobs tab.

## The conditions

| Condition | How it's met |
| --- | --- |
| CI green | The repo and its submodules' commits (a repo with workflows turned off counts as "no CI configured") |
| Same commit on cloud dev | Deployed by Condx (step 3) |
| 30 min on cloud dev without errors | Step 4 |
| Production backup under 2 h old | Step 1 |
| Rehearsal with that backup and that commit | Step 2 |

If the backup expired (over 2 h), repeat steps 1 and 2: they take a couple of minutes.
