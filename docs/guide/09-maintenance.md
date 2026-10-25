# Maintenance

| Task | How |
| --- | --- |
| Update the daemon after changes | `cargo build --release` and `./target/release/condxd install` (in the Condx repo) |
| Update the app | `scripts/bundle-app.sh` |
| Change a recipe | Edit `recipes/*.toml` and reinstall the daemon (recipes are compiled in) |
| Change this guide | Edit `docs/guide/*.md` (English) or `docs/guia/*.md` (Spanish) and rebuild the app |
| Deploy keys | Each cloud environment's **Keys** tab, or `~/.condx/daemon/bin/condxd secret set prod-deploy-key` · check with `secret status` |
| Backups | `~/.condx/backups/`; cloud ones are encrypted and deleted after 14 days |
| The icon shows ✕ "daemon" | Run `condxd install` again |
| Raw logs of the local services | `~/.condx/daemon/services/<service>.log` |

More technical detail: the repo's `README.md`.
