# Plan: hilos de la Mac al nivel del teléfono, CLI `condx` y SDK listo para npm

Fecha: 2026-10-27. Para un agente que trabaja solo; lo revisa otro agente después. Tres partes,
en este orden: **A** (hilos de la Mac), **B** (CLI) y **C** (SDK). Cada fase termina con sus pruebas
en verde y un commit. Si una fase se traba, anota por qué en «Bitácora» (al final) y sigue con la
siguiente.

---

## 0. Cómo arrancar

### Carpeta y rama

- **Abre la sesión en `~/Documents/Software/condx`** (repo `serlismaldonado/condx`, público).
- El repo del teléfono está al lado, `~/Documents/Software/condx-mobile`: **solo léelo** (es la
  referencia de cómo se ve y se comporta cada cosa). No lo modifiques.
- Antes de tocar nada:

  ```bash
  cd ~/Documents/Software/condx
  git status            # debe estar limpio; si no, para y avisa
  git switch main && git pull --ff-only
  git switch -c feat/hilos-cli-sdk
  ```

- Un commit por fase (mensaje en inglés, como los del repo: `feat(app): …`, `feat(daemon): …`,
  `feat(cli): …`, `chore(sdk): …`), terminando con:

  ```
  Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
  ```

- Al final: `git push -u origin feat/hilos-cli-sdk` y abre un PR contra `main` con `gh pr create`
  (cuerpo con el resumen por fase y lo que no se pudo probar; termina con
  `🤖 Generated with [Claude Code](https://claude.com/claude-code)`). **No hagas merge.**

### Reglas que no se rompen

1. **No toques el daemon instalado** ni `~/.condx`. Para probar el daemon usa uno desechable (ver
   0.3). No corras `scripts/install-daemon.sh`, `scripts/release.sh`, `release-cloud.sh` ni
   `cloud-deploy.sh`. No pidas releases (`request_release`) ni trabajos al daemon real.
2. **No publiques nada**: ni `npm publish`, ni releases de GitHub, ni `vercel deploy`. La parte C
   deja el SDK *listo*; publicarlo lo hace el dueño.
3. **Aprobar sigue siendo solo de la app y del teléfono.** Ni la CLI ni el MCP ganan poder para
   aprobar trabajos, despliegues o producción.
4. **Repo público:** antes de cada push revisa que el diff no tenga secretos ni nombres reales de
   deployments, dominios privados o IDs de sandboxes:

   ```bash
   git diff origin/main...HEAD | grep -inE "prod:|dev:[a-z]|sk-|ghp_|github_pat|boat_[a-z0-9]{8}|bx_[a-z0-9]{6}|[a-z]+-[a-z]+-[0-9]{3}\.convex|heskala\.com|serlismaldonado\.com|BEGIN .*KEY" || echo limpio
   ```

5. No aceptes por el usuario diálogos de permisos de macOS (micrófono, Llavero, etc.). Si una
   prueba necesita uno, déjala anotada como «falta probar a mano».
6. Comentarios del código en español (como el resto del repo), textos de la interfaz en inglés con
   su traducción en `crates/app/locales/es.json` (cada `t!("…")` nuevo necesita su entrada; el JSON
   debe seguir siendo válido).

### 0.1 Contexto que tienes que leer primero (en este orden)

| Archivo | Para qué |
| --- | --- |
| `README.md` | Qué es Condx, cómo se compila |
| `docs/gpui-kit-guias.md` | Cómo escribir vistas GPUI sin re-renderizar de más (`Entity::cached`, entidades fuera de `render`, contar renders) |
| `crates/app/src/views/agents.rs` (654 líneas) | **La vista de hilos de la Mac**: `conversation`, `composer`, `thread_view`, `thread_item` |
| `crates/app/src/views/guide.rs` (línea ~87) | Ejemplo de `TextView::markdown(id, text).selectable(true)` que ya funciona en la app |
| `crates/daemon/src/http.rs` (rutas en ~1300-1420; handlers `post_thread_message` ~748, `post_agent_chat` ~767, `post_task` ~644) | La API que usa la app |
| `crates/daemon/src/threads.rs` (`message`, `message_with` ~648-700, `mention` ~640, `change_of` ~236) | Cómo se rutea un mensaje (prefijos `@agente` y `[model: …]`) y qué es `item["change"]` |
| `crates/daemon/src/attachments.rs` | Adjuntos del teléfono: `parse`, `receive`, `transcribe`, `transcribe_local` (whisper.cpp local) |
| `crates/daemon/src/cloud.rs` (~1420-1470) | Cómo entra un mensaje del teléfono con archivos (`receive` → `message_with` con imágenes) |
| `crates/daemon/src/auth.rs` | `Scope`, clientes `CLI` / `APP` / `AGENT` y sus tokens |
| `crates/daemon/src/security_tests.rs` | Pruebas de seguridad que hay que extender |
| `../condx-mobile/src/chat.rs` (1593 líneas) | **La referencia** de los hilos del teléfono: responder (líneas ~690-720, ~1080-1170), copiar (~713), bloque de cambios (`change_block`, ~430-480), reenviar (~1110-1125), Markdown (`md.rs`) |
| `packages/react`, `packages/convex` (README de cada uno) | El SDK |
| `docs/design/agent-flows.md` §2.8 | Diseño del SDK y de `condx init` |

### 0.2 Comandos de verificación (los mismos del CI, `.github/workflows/ci.yml`)

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTC_BOOTSTRAP=1 cargo check -p devtool-app --lib --target wasm32-unknown-unknown   # la demo web
cmp install.sh site/install.sh && bash -n install.sh
(cd agent-host && npm run check && npm test)        # solo si tocas agent-host
```

**La demo web compila la app para wasm.** Todo lo que agregues a `crates/app` y no exista en el
navegador (micrófono, diálogos de archivos del sistema, procesos) va detrás de
`#[cfg(not(target_arch = "wasm32"))]`, igual que lo que ya hay.

### 0.3 Daemon desechable para probar

`CONDX_HOME` es la carpeta del daemon (en lugar de `~/.condx/daemon`, ver
`crates/daemon/src/config.rs`) y `CONDX_PORT` cambia el puerto (el real es 4790):

```bash
export CONDX_HOME="$(mktemp -d)/daemon"      # nunca ~/.condx
export CONDX_PORT=4891
cargo build -p devtool-daemon
target/debug/condxd serve &                  # en segundo plano; mátalo al terminar
```

La app lee el mismo `CONDX_HOME` (`crates/app/src/client.rs`): ábrela con esas variables y habla con
el daemon desechable (`CONDX_HOME=… CONDX_PORT=… cargo run -p devtool-app`). Con `CONDX_HOME` puesto,
el daemon no instala ni toca el servicio de launchd real (`config.rs` ~38-41); compruébalo igual
antes de arrancarlo.

---

## Parte A — Los hilos de la Mac al nivel del teléfono

Hoy, en `crates/app/src/views/agents.rs`, los mensajes se pintan como texto plano que no se puede
seleccionar, y el compositor solo manda texto. El teléfono ya tiene todo esto; la Mac debe verse
como el resto de su app (tokens de `crate::theme`, `ui::…`), no como el teléfono.

Bloques ya disponibles (no los reescribas):

- `gpui_kit::component::text::TextView::markdown(id, text).selectable(true)` — Markdown con
  selección (ya se usa en la guía).
- La crate **`ely-chat`** (mismo repo y `rev` que las otras `ely-*` de `crates/app/Cargo.toml`:
  `1018af449569f629f25485b3dace216b57b31ce4`) trae `MessageActions` (copiar), `QuoteReply`,
  `Attachment`/`AttachmentChip`, `DragDropOverlay`, `VoiceInputButton`/`VoiceWaveform`,
  `StreamingMarkdown`, `CodeBlock`. Agrégala solo si la necesitas y **compila para wasm** (si no,
  detrás de `cfg`).
- `ely-git` (ya es dependencia) tiene filas de diff para mostrar un patch.
- `cx.write_to_clipboard(ClipboardItem::new_string(…))` para copiar.

### A1. Texto seleccionable y Markdown

- En `thread_item` (y donde `conversation` pinta los mensajes del chat), cambia el texto de cada
  mensaje por `TextView::markdown(id, texto).selectable(true)`.
  - `id` estable por mensaje: `format!("msg-{thread}-{i}")` (o el `at` del item). Si el id cambia en
    cada render, la selección se pierde.
  - Los mensajes del usuario también, sin Markdown pesado (pueden ir como texto plano seleccionable
    con `SelectableText` de `ely_core::typography` o el mismo `TextView`).
  - Mientras el agente escribe (item `live`), el texto cambia: que no salte la selección ni el
    scroll. Si `TextView` re-parsea todo en cada frame y se nota, cachea el elemento por
    `(id, texto)` siguiendo `docs/gpui-kit-guias.md`.
- Bloques de código con fondo y botón de copiar; citas (`> …`) con borde a la izquierda (es el bug
  que se arregló en el teléfono: las citas no deben cortarse).
- **Pruebas:**
  - Unitarias (en `agents.rs`, `#[cfg(test)]`): una función pura `message_markdown(item: &Value) -> String`
    que arma lo que se pinta (autor, texto sin la nota final de `threads::without_note`, etc.), con
    casos: texto con citas, con código, con la nota «(Condx note: …)».
  - A mano: abrir la app contra el daemon desechable, crear un chat, seleccionar con el mouse un
    pedazo de una respuesta, ⌘C y pegarlo en otro lado. Captura de pantalla en la Bitácora.
- **Hecho cuando:** se puede seleccionar cualquier parte de cualquier mensaje y copiarla, y el
  Markdown (títulos, listas, código, citas, tablas) se ve bien en claro y oscuro.

### A2. Acciones por mensaje: copiar y responder

- Al pasar el mouse sobre un mensaje: **Copy** (todo el mensaje, en Markdown) y, en mensajes de
  agentes, **Reply**.
- **Reply** guarda en el estado de la app `chat_reply: Option<(handle, extracto)>` y muestra arriba
  del compositor una cita corta con «×» para quitarla.
- Al mandar: si hay `chat_reply` y el texto **no** empieza ya con `@`, se antepone `@handle `
  (exactamente como el teléfono, `chat.rs` ~1153). Así el mensaje le llega a quien lo escribió
  (`threads::mention` ya lo rutea).
- **Pruebas:** función pura `compose(text, reply: Option<&str>, model: Option<&str>) -> String` con
  casos: sin nada; con respuesta; texto que ya trae `@otro` (gana el del texto); con modelo
  (`[model: x] @handle texto`, en ese orden, igual que el teléfono ~1153-1168); espacios y saltos de
  línea al principio.
- **Hecho cuando:** responder a un mensaje de Fixer hace que conteste Fixer, y escribir `@risk …`
  igual contesta Risk.

### A3. `@` con autocompletado

- Al escribir `@` en el compositor aparece una lista: primero los participantes del hilo
  (`r["participants"]`, ya se usan en `thread_view`), después los roles del catálogo
  (`GET /api/agents/roles`). Filtra mientras escribes; ↑/↓ para moverte, Enter o Tab para elegir,
  Esc para cerrar. Al elegir se reemplaza `@parcial` por `@handle `.
- Los botones de participantes que ya existen («· ask it») siguen funcionando.
- **Pruebas:** función pura `mention_query(text, cursor) -> Option<(start, parcial)>` y
  `candidates(parcial, participantes, roles) -> Vec<handle>` (orden, sin repetidos, sin distinguir
  mayúsculas).

### A4. Selector de modelo que no se resetea

- Un selector en el compositor con los modelos de los proveedores configurados (`GET /api/agents`:
  mira qué devuelve para los proveedores y sus modelos). Opción «Default».
- La elección se guarda **por hilo** en el estado de la app (no se pierde al recargar el hilo ni al
  cambiar de pestaña; ese fue el bug del teléfono). Se manda como prefijo `[model: …]` vía `compose`.
- **Pruebas:** la de `compose` (A2) y una de que el estado por hilo sobrevive a `load_chat`.

### A5. Adjuntos: imágenes y archivos

**Daemon** (`crates/daemon`):

- Refactor de `attachments.rs`: separa **bajar** (`receive`, que descarga de Condx Cloud) de
  **preparar**: una función `prepare(state, thread_id, files: Vec<(FileRef, Vec<u8>)>) -> Result<Prepared>`
  con la lógica de hoy (imágenes → `images` en base64, texto chico → inline, audio → transcribir,
  lo demás → guardado en `attachments/<thread>/` y mencionado por ruta). `receive` pasa a llamar a
  `prepare`. El comportamiento del teléfono no cambia.
- `POST /api/threads/{id}/message` y `POST /api/agent-chats` aceptan además
  `files: [{ name, mime, data }]` (`data` en base64). Límites: 10 archivos, 10 MB cada uno, 25 MB en
  total; nombres saneados (sin `/`, sin `..`); `mime` vacío → se infiere de la extensión. Llaman a
  `prepare` y luego a `threads::message_with(…, &p.images)` / `start_chat_with(…, images)` con el
  texto de `p.text` agregado al mensaje (igual que `cloud.rs` ~1436).
- **Pruebas** (en `attachments.rs` y en los tests de `http.rs`):
  - `prepare`: una imagen PNG chica → va en `images`; un `.ts` chico → inline; un archivo grande de
    texto → guardado y mencionado; un binario → guardado; nombre con `../` → saneado.
  - El handler: base64 inválido → 400; más de 10 archivos → 400; 26 MB → 400 (o 413); sin `files` →
    igual que hoy.

**App:**

- Botón de clip en el compositor (diálogo del sistema: `cx.prompt_for_paths`), **arrastrar y soltar**
  archivos sobre la conversación (`ExternalPaths` de GPUI) y **pegar** una imagen del portapapeles
  (⌘V con imagen: `ClipboardEntry::Image`). Todo detrás de `cfg(not(wasm32))`.
- Chips de lo adjuntado (nombre, tamaño, «×»), antes de mandar. La app lee los archivos y los manda
  en base64 en `files`.
- **Pruebas:** función pura que arma el cuerpo JSON (`text`, `files`) y rechaza lo que pasa los
  límites antes de mandar (mismo tope que el daemon).
- **Hecho cuando:** una captura arrastrada a un hilo llega al agente como imagen (un agente que ve
  imágenes la describe) y un `.log` llega como texto.

### A6. Notas de voz

- **Daemon:** `POST /api/transcribe` con `{ name, mime, data }` (base64, tope 15 MB) que devuelve
  `{ text }` usando `attachments::transcribe` (proveedor si hay; si no, whisper.cpp local). Mismo
  permiso que escribir en un hilo (ver B1: `Scope::Chat`). Prueba: con un WAV corto de prueba en
  `crates/daemon/tests/fixtures/` y whisper ausente, error claro («install whisper-cli»), no un panic;
  marca la prueba que necesita whisper real con `#[ignore]`.
- **App:** botón de micrófono en el compositor. Mientras graba: forma de onda (`VoiceWaveform`) y
  botón para parar. Al parar, manda el audio a `/api/transcribe` y **pone el texto en el
  compositor** para que la persona lo revise antes de mandar (en la Mac no hace falta mandar el
  audio al agente).
  - Captura de audio con la crate `cpal` (agrégala solo para `cfg(target_os = "macos")`), mono,
    16 kHz o el que dé el dispositivo, a WAV en memoria.
  - El bundle necesita `NSMicrophoneUsageDescription` en el `Info.plist` que arma
    `scripts/bundle-app.sh` (texto: «Condx records voice notes for your agents. The audio is
    transcribed on this Mac.»).
  - **No aceptes el permiso del micrófono por el usuario**: la prueba con micrófono real queda
    «falta probar a mano».
- **Pruebas:** la codificación a WAV (cabecera correcta, 16 bits, mono) en una función pura.

### A7. «See changes»: el diff del agente

- Los items de un hilo pueden traer `item["change"] = { commit, branch, stat, files, patch }`
  (`threads::change_of` / `attach_change`; `patch` hasta 12 000 caracteres).
- Debajo del mensaje: una fila «See changes · N files · +a −b · branch» que se despliega en el diff
  por archivo (con `ely-git` si sirve; si no, líneas `+`/`−` coloreadas con fuente mono), con botón
  para copiar el patch y otro para copiar el nombre de la rama.
- **Pruebas:** función pura que parte un `patch` unificado en archivos y cuenta `+`/`−` (casos:
  varios archivos, archivo nuevo, archivo borrado, patch cortado a la mitad por el tope).

### A8. Mensajes que fallan y cola

- Si `POST …/message` falla (red, 4xx/5xx) o el resultado es `rejected…`, el mensaje queda en el
  hilo marcado «Not sent» con **Retry** y **Discard**, y su texto vuelve al compositor si se
  descarta. No se pierde nada de lo escrito.
- Los mensajes en cola del daemon ya llegan como items `status: "queued"` con `position`
  (`threads::queued_items`): muéstralos como «Queued · #n».
- **Pruebas:** la lógica del «outbox» (agregar, reintentar, descartar) como funciones puras sobre el
  estado.

### A9. Repaso de la Parte A

- Recorre el hilo completo contra el daemon desechable con un proveedor de prueba (si no hay
  proveedor, usa el modelo guionado de `agent-host`, mira sus tests) y deja capturas en la Bitácora:
  seleccionar y copiar, responder, `@`, modelo, adjuntar imagen y archivo, fallo y reintento,
  cambios. Claro y oscuro.
- `cargo clippy …`, `cargo test --workspace` y el check de wasm en verde.

---

## Parte B — La CLI `condx`

Hoy solo existe `condxd` (el daemon), en `~/.condx/daemon/bin`, fuera del PATH y con comandos de
mantenimiento. La CLI es para el día a día de una persona en la terminal y para scripts.

### B0. Forma

- **El mismo binario**: si el ejecutable se llama `condx` (`argv[0]`), `main.rs` entra al modo CLI
  (módulo nuevo `crates/daemon/src/cli.rs`); `condxd` sigue igual. Así no cambia el empaquetado ni
  el actualizador (`update.rs` reemplaza `condxd` y el enlace sigue apuntando a él).
- `install.sh` (y `site/install.sh`, **deben quedar idénticos**: el CI los compara) crea el enlace
  `~/.local/bin/condx → ~/.condx/daemon/bin/condxd` y, si `~/.local/bin` no está en el PATH, imprime
  la línea para agregarlo (sin editar archivos del usuario). `condxd install` hace lo mismo.
- La CLI habla con el daemon **por HTTP** (`http://127.0.0.1:<port>`, puerto de `config.json`,
  4790 por defecto) con el token de `~/.condx/daemon/token` (cliente `CLI` de `auth.rs`). Respeta
  `CONDX_HOME`. Si el daemon no responde: «The Condx daemon isn't running. Start it with: condxd install».
- Toda salida tiene `--json` (lo que devuelve la API, tal cual) para scripts; sin `--json`, texto
  corto y alineado. Códigos de salida: 0 bien, 1 error de ejecución, 2 uso incorrecto.
- `condx --help` y `condx <comando> --help`. Sin dependencias nuevas pesadas: el daemon ya tiene
  `ureq` y `serde_json`; el parseo de argumentos hazlo a mano como `main.rs` (o justifica `clap` en
  la Bitácora).

### B1. Permiso para conversar (sin poder aprobar)

- Nuevo `Scope::Chat` en `auth.rs`: empezar chats, escribir en cualquier hilo como la persona,
  crear tareas y transcribir. **No** incluye `JobsApprove`.
- `CLI` y `APP` ganan `Chat`. `post_thread_message`, `post_agent_chat`, `post_task` y el nuevo
  `/api/transcribe` piden `Scope::Chat` en vez de `JobsApprove`.
- **Pruebas** en `security_tests.rs` y `auth.rs`: el token de la CLI puede `Chat`, `JobsRequest`,
  `Read`, y **no** puede aprobar (`POST /api/approvals/{id}` → 403), ni cambiar ajustes de agentes,
  ni claves; el token `AGENT` no puede `Chat`; la app sigue pudiendo todo lo de antes.

### B2. Comandos

| Comando | API | Qué hace |
| --- | --- | --- |
| `condx status` | `GET /api/overview` (o la que use `condxd status`; mira `main.rs`) | Entornos, CI, deployer, trabajos y aprobaciones pendientes, en pocas líneas |
| `condx projects` | `GET /api/projects` | Proyectos |
| `condx envs [--project P]` | `GET /api/environments` | Entornos con estado |
| `condx jobs [--limit N] [--watch]` | `GET /api/jobs` | Trabajos recientes; `--watch` refresca cada 2 s hasta Ctrl+C |
| `condx job <id> [--log] [--follow]` | `GET /api/jobs/{id}` y su log | Un trabajo; `--follow` sigue el log hasta que termina |
| `condx recipes` | la ruta de recetas | Lo que se puede pedir |
| `condx run <recipe> [--env E] [--commit C] [key=value…]` | `POST /api/jobs` | Pide un trabajo; imprime «Waiting for your approval in the app» si lo necesita. `--wait` espera a que termine y sale con 0/1 |
| `condx approvals` | `GET /api/approval…` | **Solo lista** lo que espera aprobación y dice «Approve it in the Condx app or on your phone» |
| `condx incidents` | `GET /api/incidents` | Incidentes abiertos |
| `condx threads` | `GET /api/threads` | Hilos recientes |
| `condx thread <id> [--follow]` | `GET /api/threads/{id}` | Mensajes del hilo; `--follow` muestra lo nuevo mientras el agente trabaja |
| `condx chat "texto" [--project P] [--model M]` | `POST /api/agent-chats` | Chat nuevo (acepta `@agente` al principio); imprime el id y, por defecto, sigue la respuesta hasta que termina |
| `condx say <thread> "texto" [--model M] [--file F…]` | `POST /api/threads/{id}/message` | Escribe en un hilo, con adjuntos (A5) |
| `condx task "texto" [--project P]` | `POST /api/tasks` | Una tarea para los flujos |
| `condx logs <servicio> [--follow]` | `GET /api/services/{name}/logs` | Logs de un servicio local |
| `condx doctor` | `GET /api/doctor` | Lo que falta en esta Mac |
| `condx mcp` | — | Igual que `condxd mcp` (para configurar Claude Code con `condx mcp`) |
| `condx open [thread\|job <id>]` | — | Abre la app (`open -a Condx`; con id, el deep link si existe; si no, solo la app) |

Confirma cada ruta en `http.rs` antes de usarla (algunas de la tabla pueden llamarse distinto); si
falta una que hace falta, agrégala con el permiso que corresponda y su prueba.

### B3. `condx init` (el SDK en un proyecto)

- En la carpeta de un proyecto de Convex (tiene `convex/` y `package.json`):
  1. Muestra lo que va a hacer y **no escribe nada sin `--write`**.
  2. Con `--write`: `npm install @condx/react @condx/convex` (o el gestor que use el proyecto según
     su lockfile: bun, pnpm, yarn, npm); agrega `app.use(condx)` en `convex/convex.config.ts` (lo
     crea si no existe, con el formato de los componentes de Convex); pide al daemon una clave de
     ingesta para el proyecto (`POST /api/sdk/keys`; hoy pide `JobsApprove`: cámbiala a `Chat` o
     deja que `init` solo imprima dónde crearla en la app, decide y anótalo) y muestra el fragmento
     de `<CondxProvider ingestKey="…">` para pegar (no edita el código React del usuario).
  3. Registra el proyecto en Condx si no lo está (`POST /api/projects`).
- **Pruebas:** contra una copia temporal de `packages/convex/example` (o un proyecto mínimo en
  `crates/daemon/tests/fixtures/convex-app/`): sin `--write` no cambia ningún archivo; con `--write`
  el `convex.config.ts` queda con `app.use(condx)` una sola vez aunque se corra dos veces; detecta el
  gestor por el lockfile. La instalación con npm real márcala `#[ignore]` (necesita red).

### B4. Pruebas de la CLI

- El cliente HTTP de la CLI se prueba contra el router real (`http::router`) con tokens fijos
  (`auth::Tokens::fixed`, como en `security_tests.rs`) en un puerto efímero: `status`, `jobs`,
  `run` (crea un trabajo que queda esperando aprobación), `chat` y `say` con el token de la CLI, y
  que `approve` **no existe** (`condx approve` → uso incorrecto, código 2).
- El formateo de texto (tablas, tiempos relativos) como funciones puras con sus tests.
- A mano, con el daemon desechable: `ln -s target/debug/condxd /tmp/condx && CONDX_HOME=… /tmp/condx status`.

### B5. Documentación de la CLI

- `docs/guide/17-cli.md` y `docs/guia/17-cli.md` (formato de las otras guías: tablas cortas, sin
  relleno), registradas en `crates/app/src/views/guide.rs` (y su título en `es.json`), y una sección
  corta «Command line» en el `README.md`.
- En la landing (`site/index.html`), en la caja de instalación, una línea debajo: `condx status ·
  condx chat "…" · condx jobs --watch` con enlace a la guía. Sigue el estilo de la página (mira las
  clases `.term`, `.actions`, `.link`). No cambies nada más de la landing.

---

## Parte C — El SDK listo para publicar

`packages/react` (`@condx/react`) y `packages/convex` (`@condx/convex`) están en 0.1.0, con tests,
pero no están en npm, no están en el CI y nadie los menciona.

### C1. Paquetes publicables

- En los dos `package.json`: `repository` (`{ "type": "git", "url": "git+https://github.com/serlismaldonado/condx.git", "directory": "packages/react" }` y el de convex), `homepage` (`https://condx.app`), `bugs`, `keywords`, `description`, `sideEffects: false` (react), `engines`.
- `@condx/convex` hoy exporta `src/*.ts` sin compilar. Sigue la forma de los componentes publicados
  de Convex (mira cómo exporta `@convex-dev/*`, p. ej. `node_modules/@convex-dev` si hay alguno en
  `packages/convex/example`, o la plantilla oficial de componentes): build a `dist/` con tipos,
  `exports` apuntando a `dist`, y el `convex.config` exportado como lo espera `app.use`.
- `prepublishOnly` que corre `check`, `test` y `build`.
- **Verificación sin publicar:** en cada paquete, `npm pack --dry-run` lista solo `dist`, `README.md`,
  `LICENSE` y `package.json`; y una instalación del `.tgz` (`npm pack` y `npm install ./condx-*.tgz`)
  en una copia temporal de `packages/convex/example` compila (`npx tsc --noEmit`) y sus tests pasan.

### C2. CI

- Un job nuevo `sdk` en `.github/workflows/ci.yml` (Ubuntu, Node 22) con matriz
  `packages/react` y `packages/convex`: `npm ci`, `npm run check`, `npm test`, `npm run build` (si
  aplica), `npm pack --dry-run`. Copia el estilo del job `agent-host`.

### C3. Documentación del SDK

- `packages/react/README.md` y `packages/convex/README.md`: instalación, uso mínimo (provider,
  `<ReportProblem>`, `reportError`, `withCondx`), privacidad por defecto, `beforeSend`.
- Guía `docs/guide/18-sdk.md` / `docs/guia/18-sdk.md` (registrada en la guía de la app): qué captura,
  cómo conectarlo (`condx init` o a mano), dónde aparecen los eventos (incidentes y flujos), y cómo
  rotar la clave de ingesta (vista SDK de la app, `crates/app/src/views/sdk.rs`).
- En la landing, si encaja sin rediseñar: una tarjeta o línea «Catch errors in any Convex app»
  con enlace a la guía. Si no encaja limpio, no la agregues y anótalo.

### C4. Lo que hace el dueño (no tú)

Deja al final del PR los comandos exactos para publicar:

```bash
npm login
cd packages/react && npm publish --access public
cd ../convex && npm publish --access public
```

(y que el scope `@condx` tiene que existir en su cuenta de npm).

---

## Criterios de terminado (para quien revisa)

- [ ] `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` y el check
      de wasm en verde; `cmp install.sh site/install.sh` igual.
- [ ] Parte A: capturas de cada función en claro y oscuro; pruebas unitarias nuevas para `compose`,
      menciones, diff, outbox, cuerpo de adjuntos y WAV; `prepare` y los handlers con sus casos.
- [ ] Parte B: `Scope::Chat` con pruebas de seguridad; la CLI probada contra el router; `condx
      approve` no existe; guía y README.
- [ ] Parte C: `npm pack --dry-run` limpio en los dos; instalación del `.tgz` en el ejemplo; job de
      CI; guías.
- [ ] Ningún secreto ni nombre real en el diff; nada publicado; `~/.condx` sin tocar.
- [ ] PR abierto contra `main` con resumen por fase y «falta probar a mano» (micrófono, etc.).

## Bitácora

(El agente anota aquí, por fase: qué hizo, qué decidió distinto a este plan y por qué, capturas, y
lo que quedó sin probar.)
