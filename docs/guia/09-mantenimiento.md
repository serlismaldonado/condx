# Mantenimiento

| Tarea | Cómo |
| --- | --- |
| Actualizar el daemon tras cambios | `cargo build --release` y `./target/release/condxd install` (en el repo de Condx) |
| Actualizar la app | `scripts/bundle-app.sh` |
| Cambiar una receta | Editar `recipes/*.toml` y reinstalar el daemon (las recetas van compiladas) |
| Cambiar esta guía | Editar `docs/guia/*.md` (español) o `docs/guide/*.md` (inglés) y volver a empaquetar la app |
| Deploy key de producción | `~/.condx/daemon/bin/condxd secret set prod-deploy-key` · ver con `secret status` |
| Respaldos | `~/.condx/backups/`; los de cloud se cifran y se borran a los 14 días |
| El ícono muestra ✕ «daemon» | Volver a correr `condxd install` |
| Logs crudos de los servicios locales | `~/.condx/daemon/services/<servicio>.log` (Vite y las acciones del devtool) |

Más detalle técnico: el `README.md` del repo.
