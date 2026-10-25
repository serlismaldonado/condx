# Local environment (Services) and terminal

## Local environment services

The local environment's **Summary** (under Environments, **local**) starts with its services: it
starts and stops what you need to test on this Mac. The project is chosen at the top: heskala-erp or
another Convex repo (see "Other Convex projects"). What follows describes heskala-erp.

### Docker mode or native mode

At the top, the **Local environment mode** card says how the backend and the deployer run:

| Mode | Backend | Deployer | Needs Docker/OrbStack? |
| --- | --- | --- | --- |
| **Docker** | `convex-backend` container | `deployer-dev` container | yes |
| **Native** | the `convex-local-backend` binary, data in `~/.condx/daemon/native/local` | `convex dev` as a daemon process | **no** |

- **Migrate to native mode** requests a job (with approval) that: backs up the Docker backend, reads
  its environment variables, stops it, starts the native one, imports the data, copies the
  variables and starts the native deployer. The Docker volume isn't touched.
- **Back to Docker / Use native mode** switches mode afterwards (recipe "Change the local
  environment mode"). Each mode keeps its data: going back to Docker returns to the data from before
  migrating.
- The backend stays on `localhost:3210`, so Vite and `.env.local` don't change. The native deployer
  runs from a mirror folder of the repo and doesn't touch your `.env.local`.
- **Throwaway backends** (rehearsals) are also native when the binary is there: they don't use
  Docker.

### Services

At the top, the **resource monitor**: CPU and memory of the running services (added up, and each
one's memory), with a mini chart of the last minute and a half.

Below, the services in rows. Each row has:

- the port, CPU and memory (or the phase, for the deployer);
- its status;
- the **start/stop**, **restart** and **view logs** buttons and, for the frontend, **open**.

| Service | What it is |
| --- | --- |
| **Convex backend** | Docker or native, depending on the mode (localhost:3210). |
| **Deployer (convex dev)** | Deploys the checkout's functions to the backend every time you save a file. |
| **Frontend (Vite)** | The web app at http://localhost:3000, pointing at the Convex in `.env.local` (the card shows which). |

- **Start all** starts backend → deployer → Vite in order; **Stop all**, the other way round.
- The daemon manages them: **they keep running even if you close the app**. If you reinstall the
  daemon, it watches the Vite that was running again.
- If port 3000 is used by another process, the row says so (with its pid) and doesn't start another
  one on top.
- **The project and the mode** (Docker or native) stay folded at the end of the page.

### Logs

Pick the service from the dropdown (or with its row's logs button). You see the last 400 lines,
without secrets or colors, updated every 2 seconds.

- Each line with its time, level and source (the Convex function or the module).
- **Minimum level** (TRACE…ERROR) and **search** filter without fetching again.
- The follow button scrolls down with each new line; scrolling up with the wheel releases it.
- For Backend and Deployer, Condx's own actions also show at the end (who started or stopped what).

## Terminal

The **Terminal** page opens a shell (your `$SHELL`, login) **in the selected project's repo**,
inside the app.

- **New** opens another tab; **Close** closes the current one (the shell ends).
- It works like a normal terminal: colors, `Tab` to complete, arrows for history, `ctrl-c`,
  `ctrl-l`, full-screen programs. **cmd-v** pastes.
- The **mouse wheel** scrolls up the screen's history; typing goes back to the bottom.
- **Shortcuts** types and runs frequent commands in the active tab: `git status`, types, lint,
  tests, Convex logs and Condx's status.
- Terminals live while the app is open (not in the daemon): if you close the app, they close.

> For what has to survive closing the app (Vite, the backend), use the local environment's Summary.
