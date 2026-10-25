# Day to day: devbox and throwaways

Almost nothing here asks for approval.

| I want to… | Recipe | Environment |
| --- | --- | --- |
| Run a function or a backfill | Run a function / Run a backfill | local or a throwaway |
| Back up | Backup (export) | any |
| Restore a backup | Restore a backup | local asks for approval; a throwaway doesn't |
| Restart the backend or rebuild the deployer | Restart the backend / Rebuild the deployer image | local |
| Try something without touching anything real | Create a throwaway backend → whatever you want → Destroy the throwaway backend | new |

> On the devbox, "Deploy a commit" doesn't last long: the deployer redeploys your checkout with the next file you save.
