# Condx

**A local devtool for Convex.** Run your Convex projects locally, see every environment at a
glance, browse their data, and deploy through approvals — from one macOS app, with an MCP server
so your AI agents can help without being able to ship on their own.

> **Status: early (v0.1).** macOS only, built from source. A signed app and Homebrew come later.

## What it does

- **Every environment in one place.** Your local backend, throwaway backends, cloud dev and
  production: status, commit, logs, schema, environment variable names and recent deploys.
- **Local backend without Docker.** Runs Convex's local backend, its deployer and your frontend
  (Vite or Next) as managed processes, on their own ports, without touching your `.env.local`.
- **Data browser.** Tables with record counts, paging, filters, sorting, relation ids
  (`field → table`) and dates, plus a JSON panel. Read-only, and production stays hidden unless
  you allow it.
- **Jobs from recipes.** Deploy a commit, back up (encrypted for cloud), rehearse a deploy on a
  copy of real data in a throwaway backend, run a function, import, roll back.
- **Approvals.** What changes an environment waits for you in the app — a click, typing the
  environment's name, or Touch ID, depending on the recipe and your rules.
- **Workflows.** Chain checks, approvals, waits and recipes (e.g. CI green → approve → deploy to
  cloud dev → notify). Turn them on and off without deleting them.
- **Rules per kind of environment.** Who can read records and how much a change needs, set in the
  app. Queries that only read don't ask for approval; mutations do.
- **Keys in the Keychain.** Each cloud environment's deploy key is saved from the app into the
  macOS Keychain and never shown again.
- **MCP for agents.** Claude Code, Cursor, Codex or any MCP client can look at everything, request
  jobs and start workflows — but they can't approve, change the rules or touch the keys.
- **English and Spanish.**

## How it works

Two pieces, both on your Mac:

- **`condxd`**, a daemon listening only on `127.0.0.1:4790`. It knows your projects and
  environments, runs jobs, keeps the history in SQLite and serves the MCP. Data lives in
  `~/.condx/`.
- **Condx**, the desktop app (GPUI). It's a client of the daemon and the only place where
  approvals, rules and keys are decided.

No accounts and no telemetry: everything stays on your machine.

## Requirements

- macOS 15 or later.
- [Rust](https://rustup.rs) (the toolchain is pinned in `rust-toolchain.toml` and installs itself).
- Node.js and the Convex CLI in your projects (`npx convex`), logged in for cloud environments.

## Install from source

```bash
git clone https://github.com/serlismaldonado/condx.git
cd condx
cargo build --release

# The daemon, as a launchd service (dev.condx.daemon)
./target/release/condxd install

# The app, into ~/Applications
scripts/bundle-app.sh
open ~/Applications/Condx.app
```

`install` copies the daemon to `~/.condx/daemon/bin/condxd`. If macOS asks to let `condxd` access
your Documents folder, accept it: it needs that to read git and run the Convex CLI in your
projects. To update, pull, build and run `install` again.

## First steps

```bash
# Add a Convex project (a folder with convex/ and package.json)
~/.condx/daemon/bin/condxd project add ~/code/my-app

# Register cloud environments (names from your Convex dashboard)
~/.condx/daemon/bin/condxd env add my-app-dev --kind cloud-dev --deployment happy-otter-123
~/.condx/daemon/bin/condxd env add my-app-prod --kind cloud-prod --deployment brave-falcon-456
```

Then, in the app:

1. Start the local backend from the project's local environment.
2. Save each cloud environment's deploy key in its **Keys** tab (Convex dashboard → Settings →
   Deploy keys). With it, Data shows record counts and functions run with the key's permissions.
3. Create the suggested workflows (**Workflows → New → Create suggested**); they start turned off.

## Connect an agent

Settings → **Connect an agent** has the commands ready to copy. For Claude Code:

```bash
claude mcp add condx -- ~/.condx/daemon/bin/condxd mcp
```

Any stdio MCP client runs `condxd mcp`. Over HTTP: `POST http://127.0.0.1:4790/mcp` with
`Authorization: Bearer $(cat ~/.condx/daemon/token)`. Start with `get_overview`.

## Safety model

- The daemon listens only on `127.0.0.1`; tokens are files with mode `0600`.
- Agents use a token without the approve scope: approvals, rules and keys are only changed from the
  app.
- Secrets live in the macOS Keychain; logs and errors redact them.
- Production only changes through its own recipes (promote, roll back, import with Touch ID).

## Website and web demo

The landing page lives in [`site/`](site). Its live demo is Condx itself compiled to WebAssembly
(GPUI's web platform) with a sample daemon (`crates/app/src/web/`): no real backend is touched.

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129   # the version in Cargo.lock
scripts/build-web-demo.sh                          # writes site/demo/pkg
```

## Documentation

The in-app guide follows the app's language: English in [`docs/guide`](docs/guide), Spanish in [`docs/guia`](docs/guia).

## Acknowledgements

Built with [GPUI](https://www.gpui.rs), [GPUI Kit](https://gpui-kit.com) and
[Ely](https://elygpui.com). Condx is an independent project and isn't affiliated with or endorsed
by Convex.

## Support

Condx is free and open source. If it helps you, you can support it:

[![Buy me a coffee](https://img.shields.io/badge/Buy%20me%20a%20coffee-FFDD00?logo=buymeacoffee&logoColor=000)](https://buymeacoffee.com/serlismaldonado)

## License

[MIT](LICENSE)
