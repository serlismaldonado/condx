# When something goes wrong in production

| Situation | What to do |
| --- | --- |
| The new functions fail | Recipe **Roll back production (functions)**: by default it goes back to the previous recorded commit. Approval by typing "prod". Data isn't touched. |
| The frontend fails | In the terminal: `vercel rollback` (the recipe doesn't do it). |
| The data got damaged | **Restore a backup** on prod with a `.zip.age`. Approval with the name + Touch ID. **It loses everything written after the backup**: it's the last resort. |

## A job failed

1. Open the job in **Jobs**: the red step and the log say what happened.
2. If it was something temporary (network, a service down), **Retry from the failed step**.
3. If the problem is in the code, fix it, push the commit and start the chain again.
