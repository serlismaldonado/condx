# Mantenimiento

| Tarea | Cómo |
| --- | --- |
| Actualizar el daemon tras cambios en `tools/devtool` | `cargo build --release` y `./target/release/condxd install` (en `tools/devtool`) |
| Actualizar la app | `tools/devtool/scripts/bundle-app.sh` |
| Cambiar una receta | Editar `tools/devtool/recipes/*.toml` y reinstalar el daemon (las recetas van compiladas) |
| Cambiar esta guía | Editar `tools/devtool/docs/guia/*.md` y volver a empaquetar la app |
| Deploy key de producción | `~/.condx/daemon/bin/condxd secret set prod-deploy-key` · ver con `secret status` |
| Respaldos | `~/.condx/backups/`; los de cloud se cifran y se borran a los 14 días |
| El ícono muestra ✕ «daemon» | Volver a correr `condxd install` |
| Logs crudos de los servicios locales | `~/.condx/daemon/services/<servicio>.log` (Vite y las acciones del devtool) |

Más detalle técnico: `tools/devtool/README.md` y `docs/diseno-devtool-escritorio.md` en el repo.
