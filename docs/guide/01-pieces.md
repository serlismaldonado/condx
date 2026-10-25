# The pieces and the environments

| Piece | What it is | Where it lives |
| --- | --- | --- |
| **Daemon** (`condxd`) | The brain: always running in the background, it watches the environments and runs the jobs. | A macOS service that starts on its own; data in `~/.condx/daemon/` |
| **App** (Condx) | This window: look, request jobs and approve. If you close it, the daemon keeps going. | `~/Applications/Condx.app` and the "Condx ✓" icon in the menu bar |
| **MCP** | The door for AI agents. They can look and request jobs, but **never approve**. | Connected to Claude Code, Cursor, Codex or any MCP client |

## Environments

| Environment | Kind | What it is |
| --- | --- | --- |
| **local** | devbox | The Convex backend on this Mac |
| **dev** | cloud dev | Your cloud dev deployment (e.g. `happy-otter-123`) |
| **prod** | production | Your production deployment (e.g. `brave-falcon-456`), plus its frontend |
| throwaways | throwaway | Temporary backends for trying things; created and deleted |

## The general rule

The closer to production, the more protection:

| Environment | What you can do |
| --- | --- |
| devbox, throwaways | Almost everything, right away |
| cloud dev | Deploys and backfills, with your approval |
| production | Only promote, roll back or restore, with a strong approval |
