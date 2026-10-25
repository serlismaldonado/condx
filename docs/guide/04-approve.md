# Approve

What waits for your go-ahead shows up in **Approvals** (with a notice, if an agent requested it). Each card shows who requested it, the commit, the arguments and, for promotions, the preconditions and their status.

## Levels

| Level | What you do | Used for |
| --- | --- | --- |
| Simple | **Approve** | Deploying or backfilling on cloud dev; restoring on the devbox |
| Type the name | Type the environment's name, **Approve** and confirm | Promoting and rolling back production; restoring on cloud dev |
| Touch ID | Type the name + **Approve with Touch ID** | Restoring a backup in production |

- An approval **expires after 30 minutes**.
- **Reject** discards it.
- Only this app can approve: agents and the CLI use a token without that permission.
