// Condx Cloud (C3, docs/design/cloud.md): el daemon publica su estado (entornos, trabajos y
// aprobaciones) en un deployment de Convex propio del usuario, con el motor local-first de
// `localfirst-core`. Opcional: sin vincular no hace nada.
//
// - Un hilo propio es dueño del motor (SQLite en `cloud.sqlite3`): calcula lo que debería estar en
//   la nube, confirma solo las diferencias y sincroniza. Sin red, lo pendiente espera en la outbox.
// - Nada secreto sube: ni keys, ni tokens, ni argumentos de trabajos, ni valores de variables; los
//   errores pasan por `redact`.
// - El daemon solo toca los campos que son suyos: lo que escriba el teléfono (C4) no se pisa.
// - Vincular usa una admin key o deploy key UNA vez (registra el daemon) y no la guarda. El token
//   del daemon queda en `cloud-token` (0600), como los demás tokens.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use indexmap::IndexMap;
use localfirst_core::http::{ConvexHttpTransport, Doorbell};
use localfirst_core::{sqlite, sync_once, Engine, EngineConfig, OpDraft, OpKind, Values};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::home;
use crate::state::Shared;
use crate::store::now_ms;

const TICK: Duration = Duration::from_secs(3);
/// Después de un pedido de un teléfono, se mira más seguido un rato (alguien está usando la app).
const BUSY_TICK: Duration = Duration::from_secs(1);
const BUSY_FOR_MS: i64 = 30_000;
/// El timbre (¿cambió algo en la nube?) va en vivo: una suscripción de Convex que suena cuando un
/// teléfono escribe (`bell.rs`). Por HTTP, solo cada minuto por las dudas, o cada 15 s si el socket
/// no anda.
const SAFETY_RING_MS: i64 = 60_000;
const FALLBACK_RING_MS: i64 = 15_000;
/// Cada cuánto publica la Mac que sigue encendida (la nube, el sitio y el teléfono la dan por apagada
/// a los 3 min; al irse en orden avisa enseguida con `goodbye`).
const HEARTBEAT_MS: i64 = 60_000;

/// El daemon se va en orden (lo detienen o la Mac se apaga): Condx Cloud deja su latido en 0 y el sitio
/// en la nube toma los hilos enseguida. No en un reinicio (actualización, reinstalar): vuelve en segundos.
pub fn goodbye() -> Result<()> {
    let restarting = home().join("restarting");
    let fresh = std::fs::metadata(&restarting).and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|e| e.as_secs() < 120));
    let _ = std::fs::remove_file(&restarting);
    if fresh {
        return Ok(());
    }
    let (Some(link), Some(token)) = (load_link(), load_token()) else { return Ok(()) };
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(4)).build();
    agent.post(&format!("{}/api/mutation", link.url)).send_json(json!({ "path": "sync:goodbye", "args": { "token": token }, "format": "json" }))?;
    tracing::info!("cloud: said goodbye (the cloud site takes over)");
    Ok(())
}
const RECENT_JOBS: usize = 50;
/// Campos que el daemon escribe en cada alcance (los demás son de otros: el teléfono).
/// Sin la hora de la última revisión: cambiaba cada 30 s y nadie la lee (cuatro escrituras por vuelta).
const ENV_FIELDS: &[&str] = &["name", "kind", "project", "deployment", "reachable"];
const JOB_FIELDS: &[&str] = &["recipe", "title", "env", "envKind", "status", "approval", "requestedBy", "createdAt", "startedAt", "endedAt", "error", "commit", "log"];
/// `phoneResult`: qué hizo el daemon con la decisión de un teléfono (aplicada, rechazada y por qué, o
/// a la espera de Touch ID en la Mac).
const APPROVAL_FIELDS: &[&str] = &["recipe", "title", "env", "envKind", "status", "approval", "requestedBy", "createdAt", "phoneResult"];
/// Lo que el teléfono ve de un incidente (sin datos de los registros: el error y el final del log
/// redactados, y del agente su resumen y el arreglo). `result`/`resultFor`: qué hizo el daemon con
/// el último pedido del teléfono.
const INCIDENT_FIELDS: &[&str] = &[
    "title", "source", "project", "env", "status", "count", "firstSeen", "lastSeen", "error", "log", "commit", "branch", "url", "agent", "workflows", "result", "resultFor",
];
/// Lo que el teléfono ve de un hilo (threads.rs, compacto): sus mensajes en `items` (JSON, los
/// últimos 40, textos recortados), quiénes participan, si espera una respuesta, y el resultado del
/// último mensaje que mandó el teléfono.
const THREAD_FIELDS: &[&str] = &["title", "kind", "status", "updatedAt", "items", "participants", "waitingAnswer", "project", "env", "version", "versionCode", "url", "result", "resultFor", "recipient", "aliveAt", "settings", "devices", "projects", "catalog", "overview"];
/// Lo más largo de un mensaje de un hilo en el teléfono (antes 700: las respuestas se cortaban).
const ITEM_CHARS: usize = 12_000;
/// Lo que ocupan los mensajes de un hilo en su fila: si se pasa, se dejan afuera los más viejos.
const THREAD_CHARS: usize = 300_000;

/// Los mensajes que entran en la fila: los más nuevos, hasta `THREAD_CHARS`.
fn fit_items(items: Vec<Value>) -> Vec<Value> {
    let mut total = 0;
    let mut keep: Vec<Value> = vec![];
    for it in items.into_iter().rev() {
        total += it.to_string().len();
        if total > THREAD_CHARS && !keep.is_empty() {
            break;
        }
        keep.push(it);
    }
    keep.reverse();
    keep
}

/// La fila de los hilos que no es un hilo: la llave age de la Mac, para que el teléfono le cifre los adjuntos.
pub const MAC_ROW: &str = "_mac";
/// La fila donde el teléfono pide un hilo nuevo (un chat): su primer mensaje firmado va aquí.
pub const NEW_ROW: &str = "_new";
/// La fila con el catálogo de agentes (para mencionarlos desde el teléfono).
pub const AGENTS_ROW: &str = "_agents";
/// La última versión de la app del teléfono (versión, número y enlace de descarga).
pub const APK_ROW: &str = "_apk";
/// La configuración de los agentes de la Mac (proveedor y modelo por defecto, presupuesto, cuántos a
/// la vez, autonomía, política de Git; nada secreto): el sitio en la nube la copia.
pub const SETTINGS_ROW: &str = "_settings";
/// Los teléfonos de confianza de la Mac (id, nombre, llave pública): el sitio en la nube verifica con
/// ellos las firmas de los mensajes cuando contesta con la Mac apagada.
pub const DEVICES_ROW: &str = "_devices";
/// Donde un teléfono pide correr una receta o un flujo (firmado, como los pedidos de incidentes).
pub const RUN_ROW: &str = "_run";
/// Lo que un teléfono puede pedir: las recetas (con sus argumentos y en qué entornos) y los flujos.
pub const CATALOG_ROW: &str = "_catalog";

/// El resumen de cada proyecto para el teléfono (como la vista general de la Mac): git, deployer,
/// servicios locales con su estado, lo que pide atención y los detalles de cada entorno.
pub const OVERVIEW_ROW: &str = "_overview";

/// Texto corto y redactado (los errores pueden traer rutas o claves).
fn clip(v: &Value, max: usize) -> Value {
    short_text(v, max)
}

pub async fn overview(state: &Shared) -> Value {
    let mut projects = vec![];
    let cloud = crate::site::is_cloud();
    for p in state.store.projects().unwrap_or_default() {
        // En el sitio en la nube: solo su clon (rama) y a qué cloud dev van sus servicios.
        let o = if cloud {
            json!({ "git": crate::overview::git_light(&p.repo).await, "environments": [], "attention": [], "deployer": {} })
        } else {
            crate::overview::build_for(state, Some(&p.name)).await
        };
        let services: Vec<Value> = crate::services::list(state, &p)
            .await
            .iter()
            .map(|v| {
                json!({
                    "name": v["name"], "title": v["title"], "status": v["status"], "busy": v["busy"], "error": clip(&v["error"], 300),
                    "detail": clip(&v["detail"], 160), "healthy": v["healthy"], "phase": v["phase"], "port": v["port"], "mode": v["mode"],
                    "cpu": v["cpu"], "memory": v["memory"],
                    // En el sitio en la nube, el frontend se abre desde el teléfono (URL de boat con token).
                    "openUrl": if crate::site::is_cloud() && v["name"] != "backend" && v["name"] != "deployer" { v["port"].as_u64().and_then(|p| crate::site::open_url(p as u16)).map(Value::String).unwrap_or(Value::Null) } else { Value::Null },
                })
            })
            .collect();
        let envs: serde_json::Map<String, Value> = o["environments"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| {
                let name = e["name"].as_str()?.to_string();
                Some((name, json!({
                    "version": e["convex"]["version"], "latencyMs": e["convex"]["latencyMs"], "checkedAt": e["convex"]["checkedAt"],
                    "error": clip(&e["convex"]["error"], 200), "commit": e["deployed"]["commit"], "branch": e["deployed"]["branch"],
                    "behind": e["deployed"]["behindBranch"], "dirty": e["deployed"]["uncommittedChanges"],
                })))
            })
            .collect();
        let g = &o["git"];
        projects.push(json!({
            "name": p.name,
            "git": { "branch": g["branch"], "head": g["head"], "subject": clip(&g["subject"], 120), "ahead": g["ahead"], "behind": g["behind"], "changed": g["changed"], "untracked": g["untracked"] },
            "deployer": { "phase": o["deployer"]["phase"], "running": o["deployer"]["running"] },
            "attention": o["attention"].as_array().into_iter().flatten().take(6).map(|a| clip(a, 200)).collect::<Vec<_>>(),
            "services": services,
            "envs": envs,
            // Pasar a producción por fusión (dev → main en GitHub): qué entraría y su CI.
            "promote": crate::promote::preview_cached(&p.repo),
            "target": if cloud { crate::services::cloud_dev(state, &p).ok().map(|e| json!({ "env": e.name, "deployment": e.convex_deployment })).unwrap_or(Value::Null) } else { Value::Null },
        }));
    }
    json!({ "at": now_ms(), "projects": projects })
}

/// El catálogo para el teléfono: recetas y flujos encendidos de cada proyecto.
fn catalog(state: &Shared) -> Value {
    let recipes: Vec<Value> = crate::recipes::all()
        .iter()
        .map(|r| {
            let args: Vec<Value> = r.args.iter().map(|a| json!({ "name": a.name, "type": serde_json::to_value(a.kind).unwrap_or(Value::Null), "default": a.default, "description": a.description })).collect();
            json!({ "name": r.name, "title": r.title, "kinds": r.kinds, "args": args })
        })
        .collect();
    let mut flows = vec![];
    for p in state.store.projects().unwrap_or_default() {
        for w in crate::workflows::list(state, &p.name).unwrap_or_default().into_iter().filter(|w| w.enabled) {
            flows.push(json!({ "id": w.id, "name": w.name, "project": w.project }));
        }
    }
    json!({ "recipes": recipes, "flows": flows })
}

/// Un pedido de correr algo desde un teléfono (fila `_run`): verificado como los de incidentes, con
/// las mismas aprobaciones que si se pidiera en la Mac.
fn run_request(state: &Shared, s: &Session, row: &Values, devices: &[TrustedDevice]) -> String {
    let daemon = s.link.daemon_id.clone();
    let req = match verify_signed(row, devices, now_ms(), &["run"], |request, rid, at, arg| request_message(&daemon, RUN_ROW, request, rid, at, arg)) {
        Ok(r) => r,
        Err(e) => return format!("rejected: {e}"),
    };
    let ask: Value = match serde_json::from_str(&req.arg) {
        Ok(v) => v,
        Err(_) => return "rejected: the request isn't valid".into(),
    };
    let by = format!("phone:{}", req.device.name);
    // La configuración de los agentes que un teléfono puede cambiar: por ahora, el idioma.
    if let Some(set) = ask["settings"].as_object() {
        let mut current = serde_json::to_value(crate::agents::settings(state)).unwrap_or_default();
        if let Some(language) = set.get("language").and_then(Value::as_str) {
            current["language"] = json!(language.trim());
        }
        return match crate::agents::save_settings(state, &current) {
            Ok(_) => "settings saved".into(),
            Err(e) => format!("rejected: {e:#}"),
        };
    }
    let _rt = s.rt.enter();
    // Pasar a producción: fusionar dev en main por GitHub (el despliegue lo hace el repo o Vercel).
    if let Some(pr) = ask["promote"].as_object() {
        return match crate::projects::get(state, pr.get("project").and_then(Value::as_str)).and_then(|p| crate::promote::merge(&p.repo, &by)) {
            Ok(done) => done,
            Err(e) => format!("rejected: {e:#}"),
        };
    }
    // Subir la rama en la que trabaja la Mac (sus commits) para seguir en la nube desde ahí. Las
    // ramas principales no: esas se suben a mano o por un flujo.
    if let Some(h) = ask["handoff"].as_object() {
        let project = match crate::projects::get(state, h.get("project").and_then(Value::as_str)) {
            Ok(p) => p,
            Err(e) => return format!("rejected: {e:#}"),
        };
        let branch = std::process::Command::new("git").arg("-C").arg(&project.repo).args(["rev-parse", "--abbrev-ref", "HEAD"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        if branch.is_empty() || matches!(branch.as_str(), "main" | "master" | "dev" | "develop" | "HEAD") {
            return format!("rejected: the Mac is on {branch}; push it yourself (it's a main branch)");
        }
        return match crate::gitflow::push(&project.repo, &branch) {
            Ok(()) => format!("pushed {branch}: the cloud site continues from it"),
            Err(e) => format!("rejected: {e:#}"),
        };
    }
    // Levantar, detener o reiniciar un servicio local (o `all`), como el botón de la Mac.
    if let Some(svc) = ask["service"].as_object() {
        let name = svc.get("name").and_then(Value::as_str).unwrap_or_default();
        let action = svc.get("action").and_then(Value::as_str).unwrap_or_default();
        let project = svc.get("project").and_then(Value::as_str);
        return match crate::projects::get(state, project).and_then(|p| crate::services::action(state, &p, name, action)) {
            Ok(_) => format!("{action} {name}: on its way"),
            Err(e) => format!("rejected: {e:#}"),
        };
    }
    if let Some(flow) = ask["flow"].as_str() {
        return match crate::workflows::start(state, flow, ask["commit"].as_str().filter(|c| !c.is_empty()).unwrap_or("dev"), &by) {
            Ok(run) => format!("flow started ({})", run.id),
            Err(e) => format!("rejected: {e:#}"),
        };
    }
    let (Some(recipe), Some(env)) = (ask["recipe"].as_str(), ask["env"].as_str()) else { return "rejected: say which recipe and environment".into() };
    match s.rt.block_on(crate::jobs::request(state, recipe, env, &ask["args"], &by)) {
        Ok(job) => format!("job {} · {}", job.id, serde_json::to_value(job.status).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()),
        Err(e) => format!("rejected: {e:#}"),
    }
}

/// Los proyectos de la Mac con su repo de GitHub (sin credenciales) y su rama: el sitio en la nube los
/// clona para que sus agentes lean el código.
pub const PROJECTS_ROW: &str = "_projects";

/// Los proyectos que se publican: nombre, remoto `https://…` sin credenciales y rama por defecto.
pub fn published_projects(state: &Shared) -> Vec<Value> {
    let git = |repo: &std::path::Path, args: &[&str]| std::process::Command::new("git").arg("-C").arg(repo).args(args).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    state
        .store
        .projects()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| {
            let remote = git(&p.repo, &["remote", "get-url", "origin"])?;
            let url = github_https(&remote)?;
            let branch = git(&p.repo, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).and_then(|b| b.strip_prefix("origin/").map(str::to_string)).unwrap_or_else(|| "main".into());
            Some(json!({ "name": p.name, "url": url, "branch": branch }))
        })
        .collect()
}

/// `git@github.com:o/r.git` o `https://user:token@github.com/o/r` → `https://github.com/o/r.git`
/// (solo GitHub, sin credenciales).
fn github_https(remote: &str) -> Option<String> {
    let path = remote
        .strip_prefix("git@github.com:")
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))
        .or_else(|| remote.strip_prefix("https://").map(|r| r.rsplit_once('@').map(|(_, h)| h).unwrap_or(r)).and_then(|r| r.strip_prefix("github.com/")))?;
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    (path.split('/').count() == 2 && !path.is_empty()).then(|| format!("https://github.com/{path}.git"))
}
/// Cada cuánto se vuelven a armar los hilos (cada uno lee las conversaciones de sus agentes).
const THREADS_EVERY_MS: i64 = 8_000;
/// Cada cuánto se rearman los hilos mientras un agente trabaja (lo que escribe va llegando).
const LIVE_TICK_MS: i64 = 1_500;

/// Lo que el teléfono ve de un proveedor: sus datos y su clave cifrada para la Mac y los teléfonos de
/// confianza (`sealed`, age armado). Nunca la clave en claro.
const KEY_FIELDS: &[&str] = &["kind", "label", "baseUrl", "model", "keyHint", "sealed", "recipients", "updatedAt", "project"];

/// Los resueltos se ven un rato (para que el teléfono vea en qué terminó lo que pidió).
const RESOLVED_VISIBLE_MS: i64 = 3_600_000;

/// El vínculo con un deployment de Condx Cloud (`cloud.json`). El token va aparte.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    pub url: String,
    pub daemon_id: String,
    pub name: String,
    pub linked_at: i64,
    /// El sitio (HTTP actions: el QR, `/pair`, `/ingest`, `/files`) con un dominio propio, si se
    /// configuró (`condxd cloud site`); si no, el que corresponde a la URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
}

impl Link {
    /// Dónde responden las rutas HTTP de este deployment.
    pub fn site(&self) -> String {
        self.site.clone().unwrap_or_else(|| site_url(&self.url))
    }
}

/// Pone (o quita, con `None`) el dominio propio del sitio. Las rutas del sitio tienen que responder
/// ahí (un dominio propio del deployment o un proxy hacia `*.convex.site`).
pub fn set_site(site: Option<&str>) -> Result<Link> {
    let mut link = load_link().context("link Condx Cloud first (condxd cloud link)")?;
    link.site = match site.map(|s| s.trim().trim_end_matches('/')).filter(|s| !s.is_empty()) {
        Some(s) if s.starts_with("https://") => Some(s.to_string()),
        Some(_) => bail!("the site must be an https URL (e.g. https://condx.example.com)"),
        None => None,
    };
    write_private(&link_path(), &serde_json::to_string_pretty(&link)?)?;
    Ok(link)
}

fn link_path() -> PathBuf {
    home().join("cloud.json")
}

fn token_path() -> PathBuf {
    home().join("cloud-token")
}

pub fn load_link() -> Option<Link> {
    serde_json::from_str(&std::fs::read_to_string(link_path()).ok()?).ok()
}

fn load_token() -> Option<String> {
    std::fs::read_to_string(token_path()).ok().map(|t| t.trim().to_string()).filter(|t| t.len() >= 32)
}

pub(crate) fn random_hex(bytes: usize) -> Result<String> {
    use std::io::Read;
    let mut buf = vec![0u8; bytes];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

pub(crate) fn write_private(path: &std::path::Path, text: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(path)?;
    f.write_all(text.as_bytes())?;
    Ok(())
}

/// Registra este daemon en el deployment (`daemons:register`, interna: solo con admin o deploy
/// key) y guarda el vínculo y el token. La key se usa en esta llamada y se descarta.
pub fn link(url: &str, key: &str, name: &str) -> Result<Link> {
    let url = url.trim().trim_end_matches('/').to_string();
    let local = url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost");
    if !(url.starts_with("https://") || local) {
        bail!("the Condx Cloud URL must be https (or a local backend)");
    }
    if key.trim().len() < 16 {
        bail!("that doesn't look like an admin or deploy key");
    }
    let daemon_id = format!("mac-{}", random_hex(8)?);
    let token = random_hex(32)?;
    let body = json!({ "path": "daemons:register", "args": { "daemonId": daemon_id, "token": token, "name": name }, "format": "json" });
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).build();
    let reply: Value = match agent.post(&format!("{url}/api/mutation")).set("Authorization", &format!("Convex {}", key.trim())).send_json(body) {
        Ok(r) => r.into_json()?,
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            bail!("Condx Cloud answered {code}: {}", devtool_core::redact(&text).lines().take(2).collect::<Vec<_>>().join(" "))
        }
        Err(e) => bail!("couldn't reach {url}: {}", devtool_core::redact(&e.to_string())),
    };
    if reply["status"] != "success" {
        bail!("couldn't register the daemon: {}", devtool_core::redact(reply["errorMessage"].as_str().unwrap_or("no details")).lines().take(2).collect::<Vec<_>>().join(" "));
    }
    let link = Link { url, daemon_id, name: name.to_string(), linked_at: now_ms(), site: None };
    write_private(&token_path(), &token)?;
    write_private(&link_path(), &serde_json::to_string_pretty(&link)?)?;
    Ok(link)
}

/// Deja de sincronizar: borra el vínculo, el token y la copia local. Lo que quedó en el deployment
/// es del usuario (se borra desde su dashboard de Convex).
pub fn unlink() -> Result<()> {
    for path in [link_path(), token_path(), home().join("cloud.sqlite3"), home().join("cloud.sqlite3-wal"), home().join("cloud.sqlite3-shm")] {
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("couldn't remove {}", path.display()))?;
        }
    }
    Ok(())
}

// ── Lo que debería estar en la nube ─────────────────────────────────────

pub type Desired = IndexMap<String, IndexMap<String, Values>>;

fn obj(v: Value) -> Values {
    match v {
        Value::Object(m) => m.into_iter().filter(|(_, v)| !v.is_null()).collect(),
        _ => Values::new(),
    }
}

/// Un error de trabajo, corto y sin secretos.
fn short_error(v: &Value) -> Value {
    match v.as_str() {
        Some(e) => {
            let red = devtool_core::redact(e);
            Value::String(red.chars().take(300).collect())
        }
        None => Value::Null,
    }
}

fn short_text(v: &Value, max: usize) -> Value {
    match v.as_str() {
        Some(s) => Value::String(devtool_core::redact(s).chars().take(max).collect()),
        None => Value::Null,
    }
}

/// Una fila del alcance `incidents`: `workflows`, los flujos encendidos de su proyecto (para desplegar un arreglo).
pub fn incident_row(inc: &Value, workflows: &[(String, String)]) -> Values {
    let ev = &inc["evidence"];
    let log = ev["log"].as_str().map(|l| {
        let lines: Vec<&str> = l.lines().collect();
        lines[lines.len().saturating_sub(8)..].join("\n")
    });
    let agent = inc["agent"].as_object().map(|a| {
        let fix = &a.get("fix").cloned().unwrap_or(Value::Null);
        json!({
            "runId": a.get("runId"), "mode": a.get("mode"), "status": a.get("status"), "template": a.get("template"), "model": a.get("model"),
            "answer": short_text(a.get("answer").unwrap_or(&Value::Null), 3_000),
            "fix": if fix.is_object() { json!({
                "branch": fix["branch"], "commit": fix["commit"].as_str().map(|c| c[..c.len().min(12)].to_string()),
                "files": fix["files"].as_array().map(|f| f.iter().take(10).cloned().collect::<Vec<_>>()),
                "stat": short_text(&fix["stat"], 600), "changes": fix["changes"], "discarded": fix["discarded"], "error": short_text(&fix["error"], 300),
            }) } else { Value::Null },
        })
    });
    obj(json!({
        "title": inc["title"], "source": inc["source"], "project": inc["project"], "env": inc["env"], "status": inc["status"],
        "count": inc["count"], "firstSeen": inc["firstSeen"], "lastSeen": inc["lastSeen"],
        "error": short_error(&ev["error"]), "log": log.map(|l| short_text(&json!(l), 1_200)), "commit": ev["commit"].as_str().map(|c| c[..c.len().min(12)].to_string()),
        "branch": ev["branch"], "url": ev["url"], "agent": agent,
        "workflows": workflows.iter().map(|(id, name)| json!({ "id": id, "name": name })).collect::<Vec<_>>(),
    }))
}

/// Filas por alcance a partir del estado del daemon. Puro dado lo leído: sin red ni reloj.
pub fn desired(daemon_id: &str, environments: &[devtool_core::Environment], probes: &dyn Fn(&str) -> Option<Value>, jobs: &[Value]) -> Desired {
    let mut out: Desired = IndexMap::new();
    let env: IndexMap<String, Values> = environments
        .iter()
        .map(|e| {
            let probe = probes(&format!("convex:{}", e.name));
            let reachable = probe.as_ref().map(|p| p["data"]["reachable"] == true || (p["ok"] == true && p["data"]["reachable"].is_null()));
            let kind = serde_json::to_value(e.kind).unwrap_or(Value::Null);
            (e.name.clone(), obj(json!({ "name": e.name, "kind": kind, "project": e.project.as_deref().unwrap_or(crate::projects::DEFAULT), "deployment": e.convex_deployment, "reachable": reachable })))
        })
        .collect();
    out.insert(format!("env:{daemon_id}"), env);
    let mut recent: IndexMap<String, Values> = IndexMap::new();
    let mut approvals: IndexMap<String, Values> = IndexMap::new();
    for job in jobs.iter().take(RECENT_JOBS) {
        let Some(id) = job["id"].as_str() else { continue };
        // Sin argumentos (pueden traer datos de funciones): solo el commit, que no es secreto.
        let row = obj(json!({
            "recipe": job["recipe"], "title": job["title"], "env": job["env"], "envKind": job["envKind"], "status": job["status"],
            "approval": job["approval"]["level"].as_str().or(job["approval"].as_str()), "requestedBy": job["requestedBy"],
            "createdAt": job["createdAt"], "startedAt": job["startedAt"], "endedAt": job["endedAt"], "error": short_error(&job["error"]), "commit": job["commit"],
        }));
        if job["status"] == "waiting-approval" {
            approvals.insert(id.to_string(), APPROVAL_FIELDS.iter().filter_map(|f| row.get(*f).map(|v| (f.to_string(), v.clone()))).collect());
        }
        recent.insert(id.to_string(), row);
    }
    out.insert(format!("jobs:{daemon_id}"), recent);
    out.insert(format!("approvals:{daemon_id}"), approvals);
    out
}

fn owned(scope: &str) -> &'static [&'static str] {
    match scope.split(':').next() {
        Some("env") => ENV_FIELDS,
        Some("jobs") => JOB_FIELDS,
        Some("incidents") => INCIDENT_FIELDS,
        Some("keys") => KEY_FIELDS,
        Some("threads") => THREAD_FIELDS,
        _ => APPROVAL_FIELDS,
    }
}

/// Los cambios que llevan lo que hay (`current`) a lo que debería haber (`want`), tocando solo los
/// campos del daemon. En aprobaciones una fila que ya no espera no se borra: pasa a `status` del
/// trabajo si sigue entre los recientes (el teléfono ve en qué terminó), y si no, se borra.
pub fn diff(scope: &str, current: &IndexMap<String, Values>, want: &IndexMap<String, Values>, jobs_status: &IndexMap<String, Value>) -> Vec<OpDraft> {
    let fields = owned(scope);
    let approvals = scope.starts_with("approvals:");
    let mut drafts = vec![];
    for (id, values) in want {
        match current.get(id) {
            None => drafts.push(OpDraft::create(scope, values.clone()).with_row(id)),
            Some(have) => {
                let mut set = Values::new();
                for f in fields {
                    // Sin valor y null son lo mismo (el motor no guarda los null): si no, se reescribe en cada vuelta.
                    let (now, before) = (values.get(*f).filter(|v| !v.is_null()), have.get(*f).filter(|v| !v.is_null()));
                    if now != before {
                        set.insert(f.to_string(), now.cloned().unwrap_or(Value::Null));
                    }
                }
                if !set.is_empty() {
                    drafts.push(OpDraft::patch(scope, id, set));
                }
            }
        }
    }
    for (id, have) in current {
        // Los hilos del sitio en la nube (`site-…`) son suyos: la Mac no los borra.
        if want.contains_key(id) || id.starts_with("site-") {
            continue;
        }
        match jobs_status.get(id) {
            Some(status) if approvals => {
                if have.get("status") != Some(status) {
                    let mut set = Values::new();
                    set.insert("status".into(), status.clone());
                    drafts.push(OpDraft::patch(scope, id, set));
                }
            }
            _ => drafts.push(OpDraft { scope: scope.into(), kind: Some(OpKind::Delete), row_id: Some(id.clone()), ..Default::default() }),
        }
    }
    drafts
}

// ── Teléfonos: vincular y decisiones firmadas ───────────────────────────

/// Una decisión de producción (o de aprobación reforzada) desde el teléfono: no se aplica; espera
/// a que la Mac confirme con Touch ID.
pub const NEEDS_MAC: &str = "needs-mac";
/// Una decisión vale 10 minutos: una aprobación hecha sin conexión que llega tarde no se aplica.
pub const DECISION_MAX_AGE_MS: i64 = 10 * 60 * 1000;

/// Un teléfono que la persona confirmó en esta Mac. La lista vive aquí (`cloud-devices.json`), no en
/// el backend: lo que el daemon acepta no depende de lo que diga la nube.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustedDevice {
    pub device_id: String,
    pub name: String,
    pub public_key: String,
    pub paired_at: i64,
    /// Su identidad de cifrado (age X25519, `age1…`), confirmada junto con la llave: el daemon le
    /// cifra las claves de los proveedores (fase A6). Los vinculados antes no la tienen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption_key: Option<String>,
    /// `site` para el sitio en la nube (la app lo muestra aparte); sin valor, un teléfono.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

fn devices_path() -> PathBuf {
    home().join("cloud-devices.json")
}

pub fn trusted_devices() -> Vec<TrustedDevice> {
    std::fs::read_to_string(devices_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_devices(list: &[TrustedDevice]) -> Result<()> {
    write_private(&devices_path(), &serde_json::to_string_pretty(list)?)
}

/// Huella corta de una llave para comparar a ojo en la Mac y en el teléfono.
pub fn fingerprint(public_key_hex: &str) -> String {
    public_key_hex.chars().take(12).collect::<Vec<_>>().chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join(" ")
}

/// Llama a una función de Condx Cloud con el token del daemon.
/// Sube una APK al storage de Condx Cloud (por una URL de subida: pesa más de lo que acepta una ruta
/// HTTP) y la publica. Devuelve el enlace de descarga (en el sitio, con la llave de esa versión).
pub fn publish_apk(path: &std::path::Path, version: &str, code: i64) -> Result<String> {
    use sha2::{Digest, Sha256};
    let link = load_link().context("not linked to Condx Cloud")?;
    let token = load_token().context("not linked to Condx Cloud")?;
    let api = site_url(&link.url);
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(300)).build();
    let auth = format!("Bearer {token}");
    let post = |route: &str, body: Value| -> Result<Value> {
        match agent.post(&format!("{api}{route}")).set("Authorization", &auth).send_json(body) {
            Ok(r) => Ok(r.into_json()?),
            Err(ureq::Error::Status(code, r)) => bail!("Condx Cloud answered {code} on {route}: {}", devtool_core::redact(&r.into_string().unwrap_or_default()).chars().take(200).collect::<String>()),
            Err(e) => bail!("couldn't reach Condx Cloud: {}", devtool_core::redact(&e.to_string())),
        }
    };
    let bytes = std::fs::read(path).with_context(|| format!("couldn't read {}", path.display()))?;
    let upload = post("/releases/upload-url", json!({}))?["url"].as_str().map(str::to_string).context("Condx Cloud gave no upload URL")?;
    let stored: Value = agent.post(&upload).set("Content-Type", "application/vnd.android.package-archive").send_bytes(&bytes).map_err(|e| anyhow::anyhow!("couldn't upload the APK: {}", devtool_core::redact(&e.to_string())))?.into_json()?;
    let storage_id = stored["storageId"].as_str().context("the upload gave no storage id")?;
    let sha = format!("{:x}", Sha256::digest(&bytes));
    let r = post("/releases/publish", json!({ "platform": "android", "version": version, "versionCode": code, "storageId": storage_id, "sha256": sha, "size": bytes.len() }))?;
    let route = r["path"].as_str().context("Condx Cloud gave no download path")?;
    Ok(format!("{}{route}", link.site()))
}

fn call(kind: &str, path: &str, mut args: Value) -> Result<Value> {
    let link = load_link().context("not linked to Condx Cloud")?;
    let token = load_token().context("not linked to Condx Cloud")?;
    args["token"] = json!(token);
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).build();
    let reply: Value = match agent.post(&format!("{}/api/{kind}", link.url)).send_json(json!({ "path": path, "args": args, "format": "json" })) {
        Ok(r) => r.into_json()?,
        Err(ureq::Error::Status(_, r)) => r.into_json().unwrap_or(Value::Null),
        Err(e) => bail!("couldn't reach Condx Cloud: {}", devtool_core::redact(&e.to_string())),
    };
    if reply["status"] == "success" {
        return Ok(localfirst_core::sets::js_normalize(&reply["value"]));
    }
    let message = reply["errorData"]["message"].as_str().map(str::to_string).unwrap_or_else(|| reply["errorMessage"].as_str().unwrap_or("error").lines().next().unwrap_or("error").to_string());
    bail!("{}", devtool_core::redact(&message))
}

/// El enlace del QR: la página `/pair` del sitio del deployment, con la URL y el código en el
/// fragmento (no viaja al servidor). La cámara de cualquier teléfono abre un enlace; la app de
/// Condx lo lee igual que el texto. Sitio: `.convex.cloud` → `.convex.site`; un dominio propio
/// sirve las dos cosas (el túnel manda `/pair` al sitio); un backend local, el puerto siguiente.
/// El sitio (HTTP actions) de un deployment: `.convex.cloud` → `.convex.site`; un dominio propio
/// sirve las dos cosas (el túnel manda las rutas del sitio); un backend local, el puerto siguiente.
pub fn site_url(url: &str) -> String {
    if let Some(base) = url.strip_suffix(".convex.cloud") {
        format!("{base}.convex.site")
    } else if let Some((host, port)) = url.strip_prefix("http://").and_then(|r| r.rsplit_once(':')).filter(|(h, _)| *h == "127.0.0.1" || *h == "localhost") {
        format!("http://{host}:{}", port.parse::<u16>().map(|p| p + 1).unwrap_or(3211))
    } else {
        url.to_string()
    }
}

/// Llaves del SDK de Condx (ingest.ts en la nube): con ellas una app manda sus errores y los
/// reportes de sus usuarios. La secreta se ve una sola vez (aquí no se guarda).
pub fn sdk_keys() -> Result<Value> {
    let link = load_link().context("link Condx Cloud first (Settings → Condx Cloud)")?;
    let keys = call("query", "ingest:listKeys", json!({}))?;
    Ok(json!({ "endpoint": link.site(), "keys": keys }))
}

pub fn sdk_create_key(project: &str) -> Result<Value> {
    let link = load_link().context("link Condx Cloud first (Settings → Condx Cloud)")?;
    let v = call("mutation", "ingest:createKey", json!({ "project": project }))?;
    Ok(json!({ "endpoint": link.site(), "project": project, "publicKey": v["publicKey"], "secret": v["secret"] }))
}

pub fn sdk_revoke_key(public_key: &str) -> Result<bool> {
    Ok(call("mutation", "ingest:revokeKey", json!({ "publicKey": public_key }))? == json!(true))
}

pub fn pair_link(site: &str, url: &str, code: &str) -> String {
    let enc = |s: &str| s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>();
    format!("{site}/pair#d={}&c={}", enc(url), enc(code))
}

/// Abre un código de vinculación. Lo que va en el QR: el enlace a la página de vinculación.
pub fn pair_start() -> Result<Value> {
    let link = load_link().context("not linked to Condx Cloud")?;
    let v = call("mutation", "pairing:open", json!({}))?;
    let code = v["code"].as_str().unwrap_or_default().to_string();
    let qr = pair_link(&link.site(), &link.url, &code);
    Ok(json!({ "code": code, "expiresAt": v["expiresAt"], "qr": qr, "url": link.url }))
}

/// Cómo va el código abierto; con el teléfono que lo reclamó y su huella, si ya hay uno.
pub fn pair_status() -> Result<Value> {
    let mut v = call("query", "pairing:status", json!({}))?;
    if let Some(key) = v["device"]["publicKey"].as_str().map(str::to_string) {
        v["device"]["fingerprint"] = json!(fingerprint(&key));
    }
    Ok(v)
}

/// La persona confirmó en la Mac: el teléfono queda activo y su llave, entre las de confianza.
pub fn pair_confirm(device_id: &str) -> Result<TrustedDevice> {
    let status = call("query", "pairing:status", json!({}))?;
    let device = &status["device"];
    if status["status"] != "claimed" || device["deviceId"] != device_id {
        bail!("that phone isn't waiting for confirmation anymore");
    }
    let public_key = device["publicKey"].as_str().filter(|k| k.len() == 64).context("the phone's key is missing")?.to_string();
    let encryption_key = device["encryptionKey"].as_str().filter(|k| k.parse::<age::x25519::Recipient>().is_ok()).map(str::to_string);
    call("mutation", "pairing:confirm", json!({ "deviceId": device_id }))?;
    let trusted = TrustedDevice { device_id: device_id.to_string(), name: device["name"].as_str().unwrap_or("Phone").to_string(), public_key, paired_at: now_ms(), encryption_key, kind: device["kind"].as_str().map(str::to_string) };
    let mut list = trusted_devices();
    list.retain(|d| d.device_id != device_id);
    list.push(trusted.clone());
    save_devices(&list)?;
    Ok(trusted)
}

pub fn pair_cancel() -> Result<()> {
    call("mutation", "pairing:cancel", json!({})).map(|_| ())
}

/// Desvincula un teléfono: deja de valer aquí (de inmediato) y en el backend.
pub fn revoke_device(device_id: &str) -> Result<()> {
    let mut list = trusted_devices();
    list.retain(|d| d.device_id != device_id);
    save_devices(&list)?;
    // Si la nube no responde, igual quedó fuera de lo que el daemon acepta.
    if let Err(e) = call("mutation", "pairing:revoke", json!({ "deviceId": device_id })) {
        tracing::warn!("cloud: couldn't revoke {device_id} in Condx Cloud: {e:#}");
    }
    Ok(())
}

/// Lo que firma el teléfono (mismo texto en `condx-mobile`).
pub fn decision_message(daemon_id: &str, job_id: &str, decision: &str, decided_at: i64) -> String {
    format!("condx-approval:v1|{daemon_id}|{job_id}|{decision}|{decided_at}")
}

/// Una decisión válida de un teléfono de confianza: la decisión y el teléfono.
#[derive(Debug, Clone, PartialEq)]
pub struct PhoneDecision {
    pub approve: bool,
    pub device: TrustedDevice,
    pub confirm: Option<String>,
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

/// Verifica una fila de aprobación: firma de un teléfono de confianza, para este daemon y trabajo,
/// y reciente. Puro (para las pruebas).
pub fn verify_decision(daemon_id: &str, job_id: &str, job_created_at: i64, row: &Values, devices: &[TrustedDevice], now: i64) -> Result<PhoneDecision, String> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let decision = row.get("decision").and_then(Value::as_str).ok_or("no decision")?;
    if decision != "approve" && decision != "reject" {
        return Err("unknown decision".into());
    }
    let at = row.get("decidedAt").and_then(Value::as_i64).ok_or("no time")?;
    let key = row.get("deviceKey").and_then(Value::as_str).ok_or("no key")?;
    let device = devices.iter().find(|d| d.public_key == key).ok_or("unknown phone (not paired with this Mac)")?;
    if now - at > DECISION_MAX_AGE_MS || at > now + 60_000 || at < job_created_at {
        return Err("stale decision".into());
    }
    let sig = row.get("signature").and_then(Value::as_str).and_then(unhex).ok_or("no signature")?;
    let sig = Signature::from_slice(&sig).map_err(|_| "bad signature")?;
    let bytes: [u8; 32] = unhex(key).and_then(|b| b.try_into().ok()).ok_or("bad key")?;
    let vk = VerifyingKey::from_bytes(&bytes).map_err(|_| "bad key")?;
    vk.verify(decision_message(daemon_id, job_id, decision, at).as_bytes(), &sig).map_err(|_| "signature doesn't match".to_string())?;
    Ok(PhoneDecision { approve: decision == "approve", device: device.clone(), confirm: row.get("confirmText").and_then(Value::as_str).map(str::to_string) })
}

// ── Claves para los teléfonos (A6) ──────────────────────────────────────

/// Los destinatarios de las claves: la Mac y los teléfonos de confianza con identidad de cifrado.
pub fn key_recipients(mac: &age::x25519::Recipient, devices: &[TrustedDevice]) -> Vec<age::x25519::Recipient> {
    let mut out = vec![mac.clone()];
    out.extend(devices.iter().filter_map(|d| d.encryption_key.as_deref()?.parse().ok()));
    out
}

/// Una fila del alcance `keys` por proveedor, cifrando de nuevo solo si cambió el proveedor o
/// quiénes la reciben. Sin teléfonos que la puedan abrir no sube nada.
fn key_rows(state: &Shared, devices: &[TrustedDevice], cache: &mut IndexMap<String, (String, String)>) -> IndexMap<String, Values> {
    let mut rows = IndexMap::new();
    if !devices.iter().any(|d| d.encryption_key.is_some()) {
        return rows;
    }
    let Ok(mac) = crate::agents::mac_recipient() else { return rows };
    let recipients = key_recipients(&mac, devices);
    let names: Vec<String> = recipients.iter().map(|r| r.to_string()).collect();
    for p in state.store.agent_providers().unwrap_or_default() {
        let Some(id) = p["id"].as_str().map(str::to_string) else { continue };
        let print = format!("{}|{}", p["updatedAt"], names.join(","));
        let sealed = match cache.get(&id) {
            Some((f, s)) if *f == print => s.clone(),
            _ => match crate::agents::api_key(state, &id).and_then(|k| crate::agents::seal(&k.unwrap_or_default(), &recipients)) {
                Ok(s) => {
                    cache.insert(id.clone(), (print, s.clone()));
                    s
                }
                Err(e) => {
                    tracing::warn!("cloud: couldn't seal the key of {id}: {e:#}");
                    continue;
                }
            },
        };
        rows.insert(id, obj(json!({ "kind": p["kind"], "label": p["label"], "baseUrl": p["baseUrl"], "model": p["model"], "keyHint": p["keyHint"], "sealed": sealed, "recipients": recipients.len(), "updatedAt": p["updatedAt"] })));
    }
    rows.extend(deploy_key_rows(state, devices, cache));
    rows.extend(app_env_rows(state, devices, cache));
    rows
}

/// Si una variable es de Convex (el sitio pone las suyas, apuntando al cloud dev): esas no viajan.
fn is_convex_var(name: &str) -> bool {
    name.to_ascii_uppercase().contains("CONVEX")
}

/// Las variables de la app de cada proyecto (su `.env.local`, sin las de Convex), cifradas solo para
/// el sitio en la nube: con la Mac apagada, su frontend las necesita (claves de servicios, secretos
/// de autenticación…). Una fila `appenv:<proyecto>`; ninguna si el proyecto no tiene variables.
fn app_env_rows(state: &Shared, devices: &[TrustedDevice], cache: &mut IndexMap<String, (String, String)>) -> IndexMap<String, Values> {
    use std::hash::{Hash, Hasher};
    let mut rows = IndexMap::new();
    let site: Vec<age::x25519::Recipient> = devices.iter().filter(|d| d.kind.as_deref() == Some("site")).filter_map(|d| d.encryption_key.as_deref()?.parse().ok()).collect();
    if site.is_empty() {
        return rows;
    }
    let names: Vec<String> = site.iter().map(|r| r.to_string()).collect();
    for p in state.store.projects().unwrap_or_default() {
        let vars: serde_json::Map<String, Value> = crate::cmd::env_file_values(&p.repo.join(".env.local")).into_iter().filter(|(k, _)| !is_convex_var(k)).map(|(k, v)| (k, Value::String(v))).collect();
        if vars.is_empty() {
            continue;
        }
        let text = Value::Object(vars.clone()).to_string();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (text.as_str(), names.join(",")).hash(&mut h);
        let print = format!("{:x}", h.finish());
        let id = format!("appenv:{}", p.name);
        let sealed = match cache.get(&id) {
            Some((f, s)) if *f == print => s.clone(),
            _ => match crate::agents::seal(&text, &site) {
                Ok(s) => {
                    cache.insert(id.clone(), (print.clone(), s.clone()));
                    s
                }
                Err(e) => {
                    tracing::warn!("cloud: couldn't seal the app variables of {}: {e:#}", p.name);
                    continue;
                }
            },
        };
        // Solo los nombres a la vista (nunca los valores): para saber qué llegó.
        let hint = vars.keys().cloned().collect::<Vec<_>>().join(", ");
        rows.insert(id, obj(json!({ "kind": "app-env", "label": p.name, "project": p.name, "keyHint": hint, "sealed": sealed, "recipients": site.len(), "updatedAt": print })));
    }
    rows
}

/// Las deploy keys de cloud dev (nunca producción), cifradas solo para el sitio en la nube: con la
/// Mac apagada, sus servicios despliegan al cloud dev de cada proyecto. Una fila `deploy:<entorno>`.
fn deploy_key_rows(state: &Shared, devices: &[TrustedDevice], cache: &mut IndexMap<String, (String, String)>) -> IndexMap<String, Values> {
    use std::hash::{Hash, Hasher};
    let mut rows = IndexMap::new();
    let site: Vec<age::x25519::Recipient> = devices.iter().filter(|d| d.kind.as_deref() == Some("site")).filter_map(|d| d.encryption_key.as_deref()?.parse().ok()).collect();
    if site.is_empty() {
        return rows;
    }
    let names: Vec<String> = site.iter().map(|r| r.to_string()).collect();
    for env in state.store.environments().unwrap_or_default().into_iter().filter(|e| e.kind == devtool_core::EnvKind::CloudDev) {
        let Ok(Some((_, key))) = crate::secrets::deploy_key(&env) else { continue };
        if crate::secrets::check_deploy_key(&env, &key).is_err() {
            continue;
        }
        // Un sello de la key (no la key): cambia si la key o el sitio cambian.
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (key.as_str(), names.join(",")).hash(&mut h);
        let print = format!("{:x}", h.finish());
        let id = format!("deploy:{}", env.name);
        let sealed = match cache.get(&id) {
            Some((f, s)) if *f == print => s.clone(),
            _ => match crate::agents::seal(&key, &site) {
                Ok(s) => {
                    cache.insert(id.clone(), (print.clone(), s.clone()));
                    s
                }
                Err(e) => {
                    tracing::warn!("cloud: couldn't seal the deploy key of {}: {e:#}", env.name);
                    continue;
                }
            },
        };
        rows.insert(id, obj(json!({
            "kind": "convex-deploy", "label": env.name, "baseUrl": env.convex_url, "model": env.convex_deployment,
            "project": env.project.as_deref().unwrap_or(crate::projects::DEFAULT), "sealed": sealed, "recipients": site.len(), "updatedAt": print,
        })));
    }
    rows
}

// ── Pedidos de los teléfonos sobre incidentes ───────────────────────────

/// Lo que firma el teléfono para pedir algo sobre un incidente (mismo texto en `condx-mobile`). El
/// argumento va al final: lo anterior no tiene `|`, así que no hay ambigüedad.
pub fn request_message(daemon_id: &str, incident_id: &str, request: &str, request_id: &str, at: i64, arg: &str) -> String {
    format!("condx-incident:v1|{daemon_id}|{incident_id}|{request}|{request_id}|{at}|{arg}")
}

const REQUESTS: &[&str] = &["diagnose", "fix", "ignore", "resolve", "deploy", "discard", "message", "thread"];

#[derive(Debug, Clone, PartialEq)]
pub struct PhoneRequest {
    pub request: String,
    pub arg: String,
    pub request_id: String,
    pub device: TrustedDevice,
    /// Los adjuntos de un mensaje en un hilo (JSON de `attachments::FileRef`), firmados con el texto.
    pub files: Option<String>,
}

/// Verifica el pedido de una fila de incidente: firma de un teléfono de confianza, para este daemon
/// e incidente, y reciente. Puro (para las pruebas).
pub fn verify_request(daemon_id: &str, incident_id: &str, row: &Values, devices: &[TrustedDevice], now: i64) -> Result<PhoneRequest, String> {
    verify_signed(row, devices, now, REQUESTS, |request, request_id, at, arg| request_message(daemon_id, incident_id, request, request_id, at, arg))
}

/// Lo que firma el teléfono para escribir en un hilo (otro prefijo: una firma de incidente no
/// sirve en un hilo ni al revés).
pub fn thread_request_message(daemon_id: &str, thread_id: &str, request_id: &str, at: i64, arg: &str) -> String {
    format!("condx-thread:v1|{daemon_id}|{thread_id}|message|{request_id}|{at}|{arg}")
}

pub fn verify_thread_request(daemon_id: &str, thread_id: &str, row: &Values, devices: &[TrustedDevice], now: i64) -> Result<PhoneRequest, String> {
    // Los adjuntos (`requestFiles`) van firmados junto con el texto: «texto␟archivos».
    let files = row.get("requestFiles").and_then(Value::as_str).filter(|f| !f.is_empty()).map(str::to_string);
    let mut r = verify_signed(row, devices, now, &["message"], |_, request_id, at, arg| {
        let arg = match &files {
            Some(f) => format!("{arg}\u{1f}{f}"),
            None => arg.to_string(),
        };
        thread_request_message(daemon_id, thread_id, request_id, at, &arg)
    })?;
    r.files = files;
    Ok(r)
}

/// Verifica un pedido de correr algo (fila `_run`) firmado por un teléfono de confianza.
pub fn verify_run(daemon_id: &str, row: &Values, devices: &[TrustedDevice], now: i64) -> Result<PhoneRequest, String> {
    verify_signed(row, devices, now, &["run"], |request, rid, at, arg| request_message(daemon_id, RUN_ROW, request, rid, at, arg))
}

fn verify_signed(row: &Values, devices: &[TrustedDevice], now: i64, allowed: &[&str], message: impl Fn(&str, &str, i64, &str) -> String) -> Result<PhoneRequest, String> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let request = row.get("request").and_then(Value::as_str).ok_or("no request")?;
    if !allowed.contains(&request) {
        return Err("unknown request".into());
    }
    let request_id = row.get("requestId").and_then(Value::as_str).ok_or("no request id")?;
    let at = row.get("requestedAt").and_then(Value::as_i64).ok_or("no time")?;
    let arg = row.get("requestArg").and_then(Value::as_str).unwrap_or("");
    let key = row.get("deviceKey").and_then(Value::as_str).ok_or("no key")?;
    let device = devices.iter().find(|d| d.public_key == key).ok_or("unknown phone (not paired with this Mac)")?;
    if now - at > DECISION_MAX_AGE_MS || at > now + 60_000 {
        return Err("stale request".into());
    }
    let sig = row.get("signature").and_then(Value::as_str).and_then(unhex).ok_or("no signature")?;
    let sig = Signature::from_slice(&sig).map_err(|_| "bad signature")?;
    let bytes: [u8; 32] = unhex(key).and_then(|b| b.try_into().ok()).ok_or("bad key")?;
    let vk = VerifyingKey::from_bytes(&bytes).map_err(|_| "bad key")?;
    vk.verify(message(request, request_id, at, arg).as_bytes(), &sig).map_err(|_| "signature doesn't match".to_string())?;
    Ok(PhoneRequest { request: request.into(), arg: arg.into(), request_id: request_id.into(), device: device.clone(), files: None })
}

/// Hace lo que pidió el teléfono, con las mismas reglas que la app (la plantilla, el presupuesto,
/// los flujos encendidos y sus aprobaciones). Devuelve el resultado que ve el teléfono.
fn apply_request(state: &Shared, rt: &tokio::runtime::Handle, incident_id: &str, r: &PhoneRequest) -> String {
    let by = format!("phone:{}", r.device.name);
    let inc = match state.store.incident(incident_id) {
        Ok(Some(i)) => i,
        _ => return "rejected: incident not found".into(),
    };
    let run_id = inc["agent"]["runId"].as_str().map(str::to_string);
    let res: anyhow::Result<String> = match r.request.as_str() {
        "diagnose" | "fix" => rt.block_on(crate::agent_host::start(state, incident_id, &by, &r.request)).map(|run| format!("started {}", run["id"].as_str().unwrap_or("?"))),
        "ignore" | "resolve" => crate::incidents::act(state, incident_id, &r.request).map(|_| r.request.clone() + "d"),
        "message" => match &run_id {
            Some(id) => rt.block_on(crate::agent_host::message(state, id, &r.arg, &[])).map(|_| "sent to the agent".into()),
            None => Err(anyhow::anyhow!("no agent has worked on this incident")),
        },
        // El hilo del incidente: el teléfono lo abre con el id que vuelve («thread <id>»).
        "thread" => rt.block_on(crate::agent_host::thread_for_incident(state, &inc, &by)).map(|id| format!("thread {id}")),
        "discard" => match &run_id {
            Some(id) => crate::agent_host::discard(state, id).map(|_| "fix discarded".into()),
            None => Err(anyhow::anyhow!("there's no fix")),
        },
        "deploy" => (|| {
            let fix = &inc["agent"]["fix"];
            let commit = fix["commit"].as_str().filter(|_| fix["changes"] == true && fix["discarded"] != true).ok_or_else(|| anyhow::anyhow!("there's no fix ready"))?;
            // «<flujo>» o «<flujo>|direct» (el pedido explícito de desplegar sin PR).
            let (wid, direct) = match r.arg.split_once('|') {
                Some((w, "direct")) => (w.to_string(), true),
                _ => (r.arg.clone(), false),
            };
            let w = crate::workflows::get(state, &wid)?;
            let project = inc["project"].as_str().unwrap_or(crate::projects::DEFAULT);
            if w.project != project {
                anyhow::bail!("that workflow belongs to another project");
            }
            // Antes de desplegar: la rama sube y queda su PR (si no se pidió directo).
            let pr = match inc["agent"]["runId"].as_str().and_then(|id| state.store.agent_run(id).ok().flatten()) {
                Some(mut agent_run) => {
                    let pr = crate::gitflow::before_deploy(state, &agent_run, direct)?;
                    if let Some(url) = &pr {
                        agent_run["fix"]["pr"] = json!(url);
                        let _ = state.store.save_agent_run(&agent_run);
                    }
                    pr
                }
                None => None,
            };
            // Arrancar un flujo usa tokio (este hilo no es del runtime).
            let _rt = rt.enter();
            let run = crate::workflows::start(state, &wid, commit, &by)?;
            Ok(match pr {
                Some(url) => format!("PR {url} · workflow “{}” started ({})", w.name, run.id),
                None => format!("workflow “{}” started ({})", w.name, run.id),
            })
        })(),
        other => Err(anyhow::anyhow!("unknown request {other}")),
    };
    match res {
        Ok(s) => s,
        Err(e) => format!("rejected: {e:#}"),
    }
}

/// Lo que mandó el SDK de las apps: la nube agrupa por huella (una fila por error, que suma), así
/// que el daemon recuerda cuántas veces vio cada fila y solo cuenta lo nuevo (borrar al procesar
/// perdería una repetición que llega en el medio). Las filas sin cambios hace una semana se borran.
const EVENTS_SEEN: &str = "meta:cloud-events-seen";
const EVENT_ROW_TTL_MS: i64 = 7 * 24 * 3600 * 1000;

fn process_events(state: &Shared, s: &mut Session) {
    let rows = s.engine.rows(&s.events_scope);
    if rows.is_empty() {
        return;
    }
    let now = now_ms();
    // De la base (sobrevive a un reinicio: si no, todo volvería a contarse).
    let mut seen: serde_json::Map<String, Value> = state.store.probes().ok().and_then(|l| l.into_iter().find(|(k, _)| k == EVENTS_SEEN)).and_then(|(_, v)| v["data"].as_object().cloned()).unwrap_or_default();
    let before = seen.clone();
    // Un evento puede empezar un flujo (tokio::spawn): este hilo no es del runtime.
    let _rt = s.rt.enter();
    let mut live = std::collections::HashSet::new();
    for r in rows {
        live.insert(r.row.id.clone());
        let v = Value::Object(r.row.values.clone().into_iter().collect());
        let count = v["count"].as_i64().unwrap_or(1);
        let had = seen.get(&r.row.id).and_then(Value::as_i64).unwrap_or(0);
        if count > had {
            let mut ev = crate::events::from_cloud_row(&r.row.id, &v);
            ev["count"] = json!(count - had);
            if let Err(e) = crate::events::record(state, ev) {
                tracing::warn!("cloud: event {}: {e:#}", r.row.id);
                continue;
            }
            seen.insert(r.row.id.clone(), json!(count));
        }
        if now - v["lastAt"].as_i64().unwrap_or(now) > EVENT_ROW_TTL_MS {
            let d = OpDraft { scope: s.events_scope.clone(), kind: Some(OpKind::Delete), row_id: Some(r.row.id.clone()), ..Default::default() };
            if s.engine.commit(vec![d], now).is_ok() {
                seen.remove(&r.row.id);
            }
        }
    }
    seen.retain(|k, _| live.contains(k));
    if seen != before {
        state.set_probe(EVENTS_SEEN, true, Value::Object(seen), None);
    }
}

/// Los pedidos nuevos de los teléfonos sobre incidentes: verificar y hacer (cada uno una vez).
fn process_requests(state: &Shared, s: &mut Session) {
    let scope = format!("incidents:{}", s.link.daemon_id);
    let devices = trusted_devices();
    for row in s.engine.rows(&scope) {
        let Some(request_id) = row.row.values.get("requestId").and_then(Value::as_str).map(str::to_string) else { continue };
        // Ya respondido (también por un proceso anterior: `resultFor` vive en la nube): no se repite.
        let answered = row.row.values.get("resultFor").and_then(Value::as_str) == Some(request_id.as_str());
        if row.pending || answered || !s.seen.insert(format!("{}:{request_id}", row.row.id)) {
            continue;
        }
        let id = row.row.id.clone();
        let result = match verify_request(&s.link.daemon_id, &id, &row.row.values, &devices, now_ms()) {
            Ok(r) => apply_request(state, &s.rt, &id, &r),
            Err(e) => format!("rejected: {e}"),
        };
        tracing::info!("cloud: phone request on {id}: {result}");
        s.incident_results.insert(id, (request_id, result));
    }
}

/// Los hilos recientes, compactos para el teléfono: los últimos 40 mensajes con textos recortados.
async fn thread_rows(state: &Shared) -> IndexMap<String, Values> {
    let mut out = IndexMap::new();
    let list = crate::threads::list(state).ok().and_then(|v| v["runs"].as_array().cloned()).unwrap_or_default();
    for t in list.into_iter().take(15) {
        let Some(id) = t["id"].as_str().map(str::to_string) else { continue };
        let th = match crate::threads::get(state, &id).await {
            Ok(th) => th,
            Err(e) => {
                tracing::warn!("cloud: couldn't build thread {id}: {e:#}");
                continue;
            }
        };
        let cut = |s: &str, n: usize| -> String { if s.chars().count() > n { format!("{}…", s.chars().take(n).collect::<String>()) } else { s.to_string() } };
        let items: Vec<Value> = if th["thread"] == true {
            th["items"].as_array().cloned().unwrap_or_default().iter().rev().take(40).rev().map(|it| {
                let mut text = devtool_core::redact(it["text"].as_str().unwrap_or_default());
                if let Some(ev) = it["event"].as_object() {
                    let title = ev.get("title").and_then(Value::as_str).unwrap_or("");
                    text = match ev.get("count").and_then(Value::as_i64).filter(|c| *c > 1) {
                        Some(c) => format!("{title} · {c}×"),
                        None => title.to_string(),
                    };
                }
                if let Some(res) = it["result"].as_object() {
                    let fields: Vec<String> = res.iter().map(|(k, v)| format!("{k}: {}", v.as_str().unwrap_or_default())).collect();
                    text = format!("{text}\n{}", fields.join("\n"));
                }
                json!({ "kind": it["kind"], "author": it["author"], "logo": it["logo"], "to": it["to"], "at": it["at"], "status": it["status"], "job": it["job"], "source": it["event"]["source"], "answered": it["answered"], "live": it["live"], "activity": it["activity"], "text": cut(text.trim(), ITEM_CHARS) })
            }).collect()
        } else {
            let msgs: Vec<Value> = th["transcript"].as_array().cloned().unwrap_or_default().into_iter().filter(|x| x["role"] != "tool").collect();
            // Un chat abierto sobre un incidente: su primer mensaje es el contexto que armó el daemon.
            let about = state.store.agent_run(&id).ok().flatten().is_some_and(|r| r["about"].is_string());
            let skip = msgs.len().saturating_sub(30);
            msgs[skip..].iter().enumerate().filter_map(|(ix, x)| {
                let text: String = x["text"].as_str()?.lines().filter(|l| !l.starts_with("→ ")).collect::<Vec<_>>().join("\n");
                let kind = match x["role"].as_str() {
                    Some("user") if about && skip + ix == 0 => "context",
                    Some("user") => "user",
                    _ => "reply",
                };
                (!text.trim().is_empty()).then(|| json!({ "kind": kind, "author": "Agent", "at": x["at"], "text": cut(text.trim(), ITEM_CHARS) }))
            }).collect::<Vec<Value>>()
            .into_iter()
            .chain(crate::threads::live_item(&th, "Agent", th["logo"].as_str().unwrap_or(""), msgs.iter().filter_map(|x| x["at"].as_i64()).max().unwrap_or(0)))
            .chain(crate::threads::stopped_item(&th, "Agent", msgs.iter().filter_map(|x| x["at"].as_i64()).max().unwrap_or(0)))
            .collect()
        };
        // Lo que espera un agente libre, al final y marcado «en cola».
        let mut items = items;
        items.extend(crate::threads::queued_items(state, &id).into_iter().map(|q| json!({ "kind": "user", "status": "queued", "at": q["at"], "text": cut(q["text"].as_str().unwrap_or(""), ITEM_CHARS) })));
        let items = fit_items(items);
        let mut participants: Vec<Value> = th["participants"].as_array().cloned().unwrap_or_default().iter().map(|p| json!({ "node": p["node"], "name": p["name"], "logo": p["logo"] })).collect();
        // Un chat de un solo agente: él es el participante (con la marca de su proveedor).
        if participants.is_empty() && th["thread"] != true {
            participants.push(json!({ "node": "agent", "name": "Agent", "logo": th["logo"] }));
        }
        // En qué proyecto y entorno trabaja: el del flujo o el del incidente; un chat, su proyecto (o el
        // de siempre del daemon).
        let (project, env) = match crate::workflows::load_run(state, &id) {
            Ok(run) => (Some(run.project.clone()), run.event.as_ref().and_then(|e| e["env"].as_str()).map(str::to_string)),
            Err(_) => {
                let run = state.store.agent_run(&id).ok().flatten().unwrap_or_default();
                match run["incident"].as_str().and_then(|i| state.store.incident(i).ok().flatten()) {
                    // Sin proyecto (el CI de otro repo): el nombre del repo.
                    Some(inc) => (inc["project"].as_str().map(str::to_string).or_else(|| inc["evidence"]["repo"].as_str().and_then(|r| r.rsplit('/').next()).map(str::to_string)), inc["env"].as_str().map(str::to_string)),
                    None => (run["project"].as_str().map(str::to_string), None),
                }
            }
        };
        let project = project.filter(|p| !p.is_empty()).or_else(|| state.config.workdir().file_name().map(|n| n.to_string_lossy().into_owned()));
        let mut row = Values::new();
        row.insert("project".into(), json!(project));
        row.insert("env".into(), json!(env));
        row.insert("title".into(), json!(t["title"]));
        row.insert("kind".into(), json!(t["kind"]));
        row.insert("status".into(), json!(th["status"]));
        row.insert("updatedAt".into(), json!(items.last().and_then(|i| i["at"].as_i64()).unwrap_or(t["startedAt"].as_i64().unwrap_or(0))));
        row.insert("items".into(), json!(serde_json::to_string(&items).unwrap_or_default()));
        row.insert("participants".into(), json!(serde_json::to_string(&participants).unwrap_or_default()));
        row.insert("waitingAnswer".into(), json!(th["waitingAnswer"] == true));
        out.insert(id, row);
    }
    out
}

/// Los mensajes nuevos de los teléfonos en los hilos: verificar la firma y mandarlos (cada uno una vez).
fn process_thread_requests(state: &Shared, s: &mut Session) {
    let scope = format!("threads:{}", s.link.daemon_id);
    let devices = trusted_devices();
    for row in s.engine.rows(&scope) {
        let Some(request_id) = row.row.values.get("requestId").and_then(Value::as_str).map(str::to_string) else { continue };
        let answered = row.row.values.get("resultFor").and_then(Value::as_str) == Some(request_id.as_str());
        // Lo contestó el sitio en la nube con la Mac apagada (`siteFor`): no se contesta dos veces.
        let by_site = row.row.values.get("siteFor").and_then(Value::as_str) == Some(request_id.as_str());
        if row.pending || answered || by_site || row.row.id.starts_with("site-") || !s.seen.insert(format!("{}:{request_id}", row.row.id)) {
            continue;
        }
        let id = row.row.id.clone();
        if id == RUN_ROW {
            let result = run_request(state, s, &row.row.values, &devices);
            tracing::info!("cloud: phone run request: {result}");
            s.thread_results.insert(id, (request_id, result));
            s.threads_built.0 = 0;
            // Un servicio pedido: el resumen se refresca enseguida y seguido mientras cambia.
            s.service_asked_at = now_ms();
            s.overview_built.0 = 0;
            continue;
        }
        // Lo que contestó el sitio en este hilo mientras la Mac no estaba: va como contexto al agente.
        let site_context = site_context(&row.row.values, s.site_handed.get(&id).copied().unwrap_or(0));
        let result = match verify_thread_request(&s.link.daemon_id, &id, &row.row.values, &devices, now_ms()) {
            Ok(r) => {
                // Los adjuntos: bajarlos de la nube (cifrados para esta Mac) y prepararlos para el agente.
                let refs = crate::attachments::parse(r.files.as_deref());
                let prepared = if refs.is_empty() {
                    Ok(Default::default())
                } else {
                    let token = load_token().unwrap_or_default();
                    crate::attachments::receive(state, &s.link.site(), &token, &id, &refs)
                };
                match prepared {
                    Err(e) => format!("rejected: couldn't receive the attachments: {e:#}"),
                    Ok(p) => {
                        let text = [site_context.as_ref().map(|(t, _)| t.as_str()).unwrap_or(""), r.arg.trim(), p.text.as_str()].iter().filter(|t| !t.is_empty()).cloned().collect::<Vec<_>>().join("\n\n");
                        if let Some((_, n)) = &site_context {
                            s.site_handed.insert(id.clone(), *n);
                        }
                        let _rt = s.rt.enter();
                        if id == NEW_ROW {
                            // Un hilo nuevo: un chat con el agente, que arranca con este mensaje.
                            let by = format!("phone:{}", r.device.name);
                            match s.rt.block_on(crate::agent_host::start_chat_with(state, &text, None, &by, &p.images)) {
                                Ok(run) => format!("thread {}", run["id"].as_str().unwrap_or("?")),
                                Err(e) => format!("rejected: {e:#}"),
                            }
                        } else {
                            match s.rt.block_on(crate::threads::message_with(state, &id, &text, &p.images)) {
                                Ok(v) => format!("sent to {}", v["to"].as_str().unwrap_or("the thread")),
                                Err(e) => format!("rejected: {e:#}"),
                            }
                        }
                    }
                }
            }
            Err(e) => format!("rejected: {e}"),
        };
        tracing::info!("cloud: phone message on thread {id}: {result}");
        s.thread_results.insert(id, (request_id, result));
        s.threads_built.0 = 0;
    }
}

/// Las últimas 60 líneas del registro de un trabajo (redactadas; 6000 caracteres como mucho).
fn log_tail(id: &str) -> String {
    let (text, _) = crate::jobs::read_log(id, 0).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let tail = lines[lines.len().saturating_sub(60)..].join("\n");
    let tail = devtool_core::redact(&tail);
    let start = tail.len().saturating_sub(6000);
    let start = (start..tail.len()).find(|i| tail.is_char_boundary(*i)).unwrap_or(tail.len());
    tail[start..].to_string()
}

/// Las respuestas del sitio en la nube (`siteItems`) que la Mac todavía no le pasó a su agente, como
/// texto, y cuántas hay en total.
fn site_context(row: &Values, handed: usize) -> Option<(String, usize)> {
    let items: Vec<Value> = row.get("siteItems").and_then(Value::as_str).and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default();
    if items.len() <= handed {
        return None;
    }
    let lines: Vec<String> = items[handed..]
        .iter()
        .map(|i| format!("{}: {}", if i["kind"] == "user" { "User" } else { "Cloud site" }, i["text"].as_str().unwrap_or("")))
        .collect();
    Some((format!("(While this Mac was away, this happened in the thread on the cloud site:)\n{}", lines.join("\n")), items.len()))
}

/// Las decisiones nuevas de los teléfonos: verificar y, si corresponde, decidir el trabajo.
fn process_decisions(state: &Shared, s: &mut Session) {
    let scope = format!("approvals:{}", s.link.daemon_id);
    let devices = trusted_devices();
    for row in s.engine.rows(&scope) {
        let Some(sig) = row.row.values.get("signature").and_then(Value::as_str) else { continue };
        if row.pending || !s.seen.insert(format!("{}:{sig}", row.row.id)) {
            continue;
        }
        let job_id = row.row.id.clone();
        let result = match state.store.job(&job_id) {
            Ok(Some(job)) => decide_from_phone(state, &s.link.daemon_id, &job_id, &job, &row.row.values, &devices),
            _ => "rejected: job not found".to_string(),
        };
        tracing::info!("cloud: phone decision on {job_id}: {result}");
        s.results.insert(job_id, result);
    }
}

fn decide_from_phone(state: &Shared, daemon_id: &str, job_id: &str, job: &Value, row: &Values, devices: &[TrustedDevice]) -> String {
    let created = job["createdAt"].as_i64().unwrap_or(0);
    let d = match verify_decision(daemon_id, job_id, created, row, devices, now_ms()) {
        Ok(d) => d,
        Err(e) => return format!("rejected: {e}"),
    };
    let by = format!("phone:{}", d.device.name);
    if !d.approve {
        return match crate::jobs::decide(state, job_id, false, None, false, &by) {
            Ok(_) => "rejected the job".into(),
            Err(e) => format!("rejected: {e}"),
        };
    }
    // Producción o aprobación reforzada: el teléfono solo propone; confirma la Mac con Touch ID.
    let level = job["approval"].as_str().unwrap_or_default();
    if level == "reinforced" || job["envKind"] == "cloud-prod" {
        crate::jobs::log_line(job_id, &format!("Approval proposed from {} (confirm in the app with Touch ID)", d.device.name));
        return NEEDS_MAC.into();
    }
    match crate::jobs::decide(state, job_id, true, d.confirm.as_deref(), false, &by) {
        Ok(_) => "approved".into(),
        Err(e) => format!("rejected: {e}"),
    }
}

// ── El hilo de sincronización ───────────────────────────────────────────

pub(crate) type CloudEngine = Engine<sqlite::SqliteReplicaStore, sqlite::SqliteOutboxStore>;

pub(crate) fn block_on<F: std::future::Future>(future: F) -> F::Output {
    // El transporte HTTP es bloqueante (ureq): sus futures terminan en la primera vuelta.
    let mut future = std::pin::pin!(future);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(v) = future.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

struct Session {
    link: Link,
    engine: CloudEngine,
    transport: ConvexHttpTransport,
    scopes: Vec<String>,
    /// Resultado de cada decisión de teléfono, por trabajo (sube como `phoneResult`).
    results: IndexMap<String, String>,
    /// Firmas ya procesadas (una decisión se aplica una vez).
    seen: std::collections::HashSet<String>,
    /// Resultado del último pedido de teléfono por incidente: (requestId, resultado).
    incident_results: IndexMap<String, (String, String)>,
    /// El runtime de tokio del daemon (arrancar un agente es asíncrono; este hilo no lo es).
    rt: tokio::runtime::Handle,
    /// Claves ya cifradas por proveedor: (huella de proveedor y destinatarios, texto cifrado). age
    /// cifra distinto cada vez: sin esto cada vuelta subiría un cambio.
    sealed: IndexMap<String, (String, String)>,
    /// El alcance de eventos del SDK (`events:<daemonId>`): lo escribe la nube (ingest.ts), el
    /// daemon lo lee y borra lo que ya convirtió en evento. Va aparte del diff de los demás.
    events_scope: String,
    /// Los hilos ya armados (cuándo, y sus filas) y el resultado del último mensaje del teléfono en cada uno.
    threads_built: (i64, IndexMap<String, Values>),
    /// Otra vuelta enseguida (se atendió un pedido) y hasta cuándo mirar seguido.
    again: bool,
    busy_until: i64,
    thread_results: IndexMap<String, (String, String)>,
    /// El timbre en vivo, el token para abrirlo y cuándo sonó por HTTP.
    bell: Option<crate::bell::Bell>,
    token: String,
    rung_at: i64,
    /// El último latido publicado (`aliveAt` en la fila `_mac`).
    alive_at: i64,
    /// Cuántas respuestas del sitio en la nube ya se le pasaron al agente, por hilo.
    site_handed: std::collections::HashMap<String, usize>,
    /// Los proyectos publicados (se recalculan cada 5 min: correr git en cada vuelta sobra).
    projects_built: (i64, Vec<Value>),
    /// La cola del registro publicada por trabajo: cuándo se leyó y el texto.
    logs: std::collections::HashMap<String, (i64, String)>,
    /// El catálogo publicado (recetas y flujos; se recalcula cada minuto).
    catalog_built: (i64, Value),
    overview_built: (i64, Value),
    service_asked_at: i64,
}

fn open_session(link: Link, token: &str, rt: tokio::runtime::Handle) -> Result<Session> {
    let (replica, outbox) = sqlite::open(&home().join("cloud.sqlite3")).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut n = 0u64;
    let prefix = random_hex(4)?;
    let mut engine = Engine::open(replica, outbox, EngineConfig::default(), move || {
        n += 1;
        format!("{prefix}-{}-{n}", now_ms())
    })
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    let scopes: Vec<String> = ["env", "jobs", "approvals", "incidents", "keys", "threads"].iter().map(|k| format!("{k}:{}", link.daemon_id)).collect();
    for s in &scopes {
        engine.enable_scope(s, None).map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    let events_scope = format!("events:{}", link.daemon_id);
    engine.enable_scope(&events_scope, None).map_err(|e| anyhow::anyhow!("{e}"))?;
    // Los resultados ya publicados (de un proceso anterior) se conservan: así un pedido respondido
    // no se borra ni se vuelve a hacer.
    let incident_results: IndexMap<String, (String, String)> = engine
        .rows(&format!("incidents:{}", link.daemon_id))
        .into_iter()
        .filter_map(|r| Some((r.row.id.clone(), (r.row.values.get("resultFor")?.as_str()?.to_string(), r.row.values.get("result")?.as_str()?.to_string()))))
        .collect();
    let thread_results: IndexMap<String, (String, String)> = engine
        .rows(&format!("threads:{}", link.daemon_id))
        .into_iter()
        .filter_map(|r| Some((r.row.id.clone(), (r.row.values.get("resultFor")?.as_str()?.to_string(), r.row.values.get("result")?.as_str()?.to_string()))))
        .collect();
    Ok(Session { transport: ConvexHttpTransport::new(&link.url, token), link, engine, scopes, results: IndexMap::new(), seen: Default::default(), incident_results, rt, sealed: IndexMap::new(), events_scope, threads_built: (0, IndexMap::new()), thread_results, again: false, busy_until: 0, bell: None, token: token.to_string(), rung_at: 0, alive_at: 0, site_handed: Default::default(), projects_built: (0, vec![]), logs: Default::default(), catalog_built: (0, Value::Null), overview_built: (0, Value::Null), service_asked_at: 0 })
}

/// Una vuelta: confirmar las diferencias, timbre, sincronizar. Devuelve el estado para la sonda.
fn tick(state: &Shared, s: &mut Session) -> Result<Value> {
    let environments = state.store.environments().unwrap_or_default();
    let jobs = state.store.jobs(&[], RECENT_JOBS * 2).unwrap_or_default();
    let mut want = desired(&s.link.daemon_id, &environments, &|k| state.probe(k), &jobs);
    // La cola del registro de los trabajos que corren o fallaron (los 8 más nuevos): el teléfono la
    // muestra en el detalle. Uno que corre se refresca cada 10 s como mucho (no un cambio por vuelta).
    if let Some(rows) = want.get_mut(&format!("jobs:{}", s.link.daemon_id)) {
        let now = now_ms();
        let mut keep = std::collections::HashMap::new();
        for (id, row) in rows.iter_mut().filter(|(_, r)| matches!(r.get("status").and_then(Value::as_str), Some("running" | "failed"))).take(8) {
            let running = row.get("status").and_then(Value::as_str) == Some("running");
            let log = match s.logs.get(id) {
                Some((at, text)) if !running || now - at < 10_000 => text.clone(),
                _ => log_tail(id),
            };
            let at = s.logs.get(id).filter(|(_, t)| *t == log).map(|(a, _)| *a).unwrap_or(now);
            row.insert("log".into(), Value::String(log.clone()));
            keep.insert(id.clone(), (at, log));
        }
        s.logs = keep;
    }
    // Las decisiones de teléfono con su resultado: la fila sigue (con el estado real del trabajo)
    // aunque ya no espere aprobación, para que el teléfono vea qué pasó.
    let recent = want.get(&format!("jobs:{}", s.link.daemon_id)).cloned().unwrap_or_default();
    if let Some(rows) = want.get_mut(&format!("approvals:{}", s.link.daemon_id)) {
        for (job, result) in &s.results {
            if !rows.contains_key(job) {
                if let Some(r) = recent.get(job) {
                    rows.insert(job.clone(), APPROVAL_FIELDS.iter().filter_map(|f| r.get(*f).map(|v| (f.to_string(), v.clone()))).collect());
                }
            }
            if let Some(row) = rows.get_mut(job) {
                row.insert("phoneResult".into(), Value::String(result.clone()));
            }
        }
        // Una aprobación que vuelve a esperar (un reintento: `createdAt` nuevo) no arrastra la
        // decisión ni el resultado de la vez anterior: el teléfono vuelve a mostrar los botones (el
        // daemon igual rechaza una firma anterior a `createdAt`).
        let scope = format!("approvals:{}", s.link.daemon_id);
        let stale: Vec<String> = s
            .engine
            .rows(&scope)
            .into_iter()
            .filter(|r| {
                let decided = r.row.values.get("decidedAt").and_then(Value::as_i64);
                let waiting = rows.get(&r.row.id).filter(|w| w.get("status").and_then(Value::as_str) == Some("waiting-approval")).and_then(|w| w.get("createdAt")).and_then(Value::as_i64);
                matches!((decided, waiting), (Some(d), Some(c)) if d < c)
            })
            .map(|r| r.row.id.clone())
            .collect();
        for job in stale {
            s.results.shift_remove(&job);
            if let Some(row) = rows.get_mut(&job) {
                for f in ["decision", "decidedAt", "deviceKey", "signature", "confirmText", "phoneResult"] {
                    row.insert(f.into(), Value::Null);
                }
            }
        }
    }
    // Incidentes abiertos e ignorados (y los resueltos hace poco), con los flujos de su proyecto.
    let now_inc = now_ms();
    let mut flows: std::collections::HashMap<String, Vec<(String, String)>> = Default::default();
    let mut rows: IndexMap<String, Values> = IndexMap::new();
    for inc in state.store.incidents(&[], 100).unwrap_or_default() {
        let recent = inc["status"] != "resolved" || now_inc - inc["resolvedAt"].as_i64().unwrap_or(0) < RESOLVED_VISIBLE_MS;
        let Some(id) = inc["id"].as_str().filter(|_| recent) else { continue };
        let project = inc["project"].as_str().unwrap_or(crate::projects::DEFAULT).to_string();
        let wf = flows.entry(project.clone()).or_insert_with(|| {
            crate::workflows::list(state, &project).unwrap_or_default().into_iter().filter(|w| w.enabled).map(|w| (w.id, w.name)).take(6).collect()
        });
        let mut row = incident_row(&inc, wf);
        if let Some((rid, result)) = s.incident_results.get(id) {
            row.insert("result".into(), Value::String(result.clone()));
            row.insert("resultFor".into(), Value::String(rid.clone()));
        }
        rows.insert(id.to_string(), row);
        if rows.len() >= 40 {
            break;
        }
    }
    want.insert(format!("incidents:{}", s.link.daemon_id), rows);
    want.insert(format!("keys:{}", s.link.daemon_id), key_rows(state, &trusted_devices(), &mut s.sealed));
    // Los hilos (cada tanto: armarlos lee las conversaciones de los agentes).
    // Con un agente trabajando, más seguido: lo que escribe llega al teléfono mientras escribe.
    let working = state.store.agent_runs(&["running"], None).map(|r| !r.is_empty()).unwrap_or(false);
    if working {
        s.busy_until = s.busy_until.max(now_ms() + LIVE_TICK_MS * 3);
    }
    let every = if working { LIVE_TICK_MS } else { THREADS_EVERY_MS };
    if now_ms() - s.threads_built.0 > every {
        let _rt = s.rt.enter();
        s.threads_built = (now_ms(), s.rt.block_on(thread_rows(state)));
    }
    let mut trows = s.threads_built.1.clone();
    // La Mac: su llave (para cifrar lo que le manda un teléfono) y su latido, cada 5 min: el teléfono
    // muestra «Mac offline» si el último es viejo.
    let now = now_ms();
    if now - s.alive_at >= HEARTBEAT_MS {
        s.alive_at = now;
    }
    let mut mac_row = obj(json!({ "aliveAt": s.alive_at }));
    if let Ok(mac) = crate::agents::mac_recipient() {
        mac_row.insert("recipient".into(), Value::String(mac.to_string()));
    }
    trows.insert(MAC_ROW.into(), mac_row);
    // El catálogo de agentes (encendidos), para llamarlos a cualquier hilo con @ desde el teléfono.
    let mut agents: Vec<Value> = crate::roles::list(state).into_iter().filter(|r| r.enabled).map(|r| json!({ "id": r.id, "name": r.name, "logo": crate::agents::logo_for(state, r.provider.as_deref()) })).collect();
    // Y los agentes externos conectados por el MCP (Claude Code, Codex…): también se los menciona.
    agents.extend(crate::connections::list().iter().map(|c| json!({ "id": c.handle(), "name": c.name, "logo": c.logo(), "external": true })));
    trows.insert(AGENTS_ROW.into(), obj(json!({ "title": "agents", "participants": serde_json::to_string(&agents).unwrap_or_default() })));
    trows.insert(SETTINGS_ROW.into(), obj(json!({ "title": "settings", "settings": serde_json::to_string(&crate::agents::settings(state)).unwrap_or_default() })));
    trows.insert(DEVICES_ROW.into(), obj(json!({ "title": "devices", "devices": serde_json::to_string(&trusted_devices()).unwrap_or_default() })));
    if now_ms() - s.catalog_built.0 > 60_000 {
        s.catalog_built = (now_ms(), catalog(state));
    }
    trows.insert(CATALOG_ROW.into(), obj(json!({ "title": "catalog", "catalog": s.catalog_built.1.to_string() })));
    // El resumen: cada 20 s; cada 3 s mientras un servicio está ocupado o justo después de un pedido.
    let quiet = !state.services.busy_any() && now_ms() - s.service_asked_at > 60_000;
    if now_ms() - s.overview_built.0 > if quiet { 20_000 } else { 3_000 } {
        s.overview_built = (now_ms(), s.rt.block_on(overview(state)));
    }
    trows.insert(OVERVIEW_ROW.into(), obj(json!({ "title": "overview", "overview": s.overview_built.1.to_string() })));
    let mut run_row = obj(json!({ "title": "run" }));
    if let Some((rid, result)) = s.thread_results.get(RUN_ROW) {
        run_row.insert("result".into(), Value::String(result.clone()));
        run_row.insert("resultFor".into(), Value::String(rid.clone()));
    }
    trows.insert(RUN_ROW.into(), run_row);
    if now_ms() - s.projects_built.0 > 5 * 60_000 {
        s.projects_built = (now_ms(), published_projects(state));
    }
    trows.insert(PROJECTS_ROW.into(), obj(json!({ "title": "projects", "projects": serde_json::to_string(&s.projects_built.1).unwrap_or_default() })));
    if let Some(apk) = crate::update::latest_apk(state) {
        trows.insert(APK_ROW.into(), obj(json!({ "title": "apk", "version": apk["version"], "versionCode": apk["versionCode"], "url": apk["url"] })));
    }
    let mut new_row = obj(json!({ "title": "new" }));
    if let Some((rid, result)) = s.thread_results.get(NEW_ROW) {
        new_row.insert("result".into(), Value::String(result.clone()));
        new_row.insert("resultFor".into(), Value::String(rid.clone()));
    }
    trows.insert(NEW_ROW.into(), new_row);
    for (id, (rid, result)) in &s.thread_results {
        if let Some(row) = trows.get_mut(id) {
            row.insert("result".into(), Value::String(result.clone()));
            row.insert("resultFor".into(), Value::String(rid.clone()));
        }
    }
    want.insert(format!("threads:{}", s.link.daemon_id), trows);
    let jobs_status: IndexMap<String, Value> = jobs.iter().filter_map(|j| Some((j["id"].as_str()?.to_string(), j["status"].clone()))).collect();
    let now = now_ms();
    for scope in &s.scopes {
        let current: IndexMap<String, Values> = s.engine.rows(scope).into_iter().map(|r| (r.row.id, r.row.values)).collect();
        let drafts = diff(scope, &current, want.get(scope).unwrap_or(&IndexMap::new()), &jobs_status);
        // De a uno: un rechazo (p. ej. una fila que otro borró) no arrastra a los demás.
        for d in drafts {
            if let Err(e) = s.engine.commit(vec![d], now) {
                tracing::warn!("cloud: couldn't queue a change: {e}");
            }
        }
    }
    let mut error = None;
    let ring: Vec<String> = s.scopes.iter().cloned().chain(std::iter::once(s.events_scope.clone())).collect();
    let bell = s.bell.get_or_insert_with(|| crate::bell::Bell::start(&s.link.url, &s.token, &ring, localfirst_core::PROTOCOL_VERSION));
    let rung = if ring_due(now, s.rung_at, bell.is_live()) {
        bell.take();
        s.rung_at = now;
        Some(s.transport.doorbell(&ring, localfirst_core::PROTOCOL_VERSION).map_err(|e| e.to_string()))
    } else {
        bell.take().map(|v| match v["status"].as_str() {
            Some("upgradeRequired") => Ok(Doorbell::UpgradeRequired),
            _ => serde_json::from_value(v["versions"].clone()).map(Doorbell::Versions).map_err(|e| format!("unexpected doorbell: {e}")),
        })
    };
    match rung {
        Some(Ok(Doorbell::Versions(v))) => s.engine.on_doorbell(&v, now).map_err(|e| anyhow::anyhow!("{e}"))?,
        Some(Ok(Doorbell::UpgradeRequired)) => error = Some("Condx Cloud needs a newer Condx".to_string()),
        Some(Err(e)) => error = Some(e),
        None => {}
    }
    let report = block_on(sync_once(&mut s.engine, &s.transport, now_ms, 200)).map_err(|e| anyhow::anyhow!("{e}"))?;
    let seen = s.seen.len();
    process_decisions(state, s);
    process_requests(state, s);
    process_thread_requests(state, s);
    // Se hizo algo que pidió un teléfono: la próxima vuelta va enseguida (publica el resultado) y,
    // por un rato, más seguido.
    s.again = s.seen.len() > seen;
    if s.again {
        s.busy_until = now_ms() + BUSY_FOR_MS;
    }
    process_events(state, s);
    let st = s.engine.sync_status();
    let proposals: Vec<Value> = s.results.iter().filter(|(_, r)| r.as_str() == NEEDS_MAC).map(|(j, _)| json!(j)).collect();
    Ok(json!({
        "url": s.link.url, "daemonId": s.link.daemon_id, "name": s.link.name,
        "pending": st.pending, "recovery": st.recovery, "paused": st.paused,
        "pushes": report.pushes, "pulls": report.pulls, "liveBell": s.bell.as_ref().is_some_and(|b| b.is_live()), "error": error, "phoneProposals": proposals,
        "scopes": s.scopes.iter().map(|sc| json!({ "scope": sc, "rows": s.engine.rows(sc).len(), "cursor": s.engine.scope_state(sc).and_then(|x| x.cursor) })).collect::<Vec<_>>(),
    }))
}

/// Si toca el timbre por HTTP en esta vuelta: con el timbre en vivo andando, cada minuto (un socket
/// caído que no se notó); sin él, cada 15 s.
fn ring_due(now: i64, rung_at: i64, live: bool) -> bool {
    now - rung_at >= if live { SAFETY_RING_MS } else { FALLBACK_RING_MS }
}

/// Arranca el hilo. Sin vínculo, solo mira cada tanto si apareció uno.
pub fn spawn(state: Shared) {
    let rt = tokio::runtime::Handle::current();
    std::thread::Builder::new()
        .name("condx-cloud".into())
        .spawn(move || {
            let mut session: Option<Session> = None;
            loop {
                let link = load_link();
                let token = load_token();
                match (link, token) {
                    (Some(link), Some(token)) => {
                        if session.as_ref().map(|s| &s.link) != Some(&link) {
                            session = match open_session(link, &token, rt.clone()) {
                                Ok(s) => Some(s),
                                Err(e) => {
                                    state.set_probe("cloud", false, Value::Null, Some(format!("{e:#}")));
                                    None
                                }
                            };
                        }
                        if let Some(s) = session.as_mut() {
                            // Un pánico en una vuelta no mata la sincronización: se informa y la sesión se abre de nuevo.
                            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tick(&state, s))) {
                                Ok(Ok(data)) => {
                                    let ok = data["error"].is_null();
                                    let err = data["error"].as_str().map(str::to_string);
                                    state.set_probe("cloud", ok, data, err);
                                }
                                Ok(Err(e)) => state.set_probe("cloud", false, Value::Null, Some(devtool_core::redact(&format!("{e:#}")))),
                                Err(p) => {
                                    let why = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "unknown".into());
                                    tracing::error!("cloud: a sync pass panicked: {why}");
                                    state.set_probe("cloud", false, Value::Null, Some(format!("internal error in the sync: {why}")));
                                    session = None;
                                }
                            }
                        }
                    }
                    _ => {
                        if session.take().is_some() || state.probe("cloud").is_some_and(|p| !p["data"].is_null()) {
                            state.set_probe("cloud", true, Value::Null, None);
                        }
                    }
                }
                let wait = match session.as_mut().map(|s| (std::mem::take(&mut s.again), s.busy_until)) {
                    Some((true, _)) => Duration::ZERO,
                    Some((false, until)) if now_ms() < until => BUSY_TICK,
                    _ => TICK,
                };
                // Lo que mande un teléfono despierta enseguida (el timbre en vivo); si no, la espera.
                match session.as_ref().and_then(|s| s.bell.as_ref()) {
                    Some(bell) if !wait.is_zero() => {
                        bell.wait(wait);
                    }
                    _ => std::thread::sleep(wait),
                }
            }
        })
        .expect("couldn't start the cloud thread");
}

#[cfg(test)]
mod fit_tests {
    use super::*;

    #[test]
    fn deja_los_mensajes_mas_nuevos_que_entran() {
        let big = "x".repeat(ITEM_CHARS);
        let items: Vec<Value> = (0..60).map(|i| json!({ "at": i, "text": big })).collect();
        let kept = fit_items(items);
        assert!(kept.len() < 60 && kept.len() >= 20, "{}", kept.len());
        assert_eq!(kept.last().unwrap()["at"], 59);
        assert_eq!(fit_items(vec![json!({ "at": 1, "text": "x".repeat(400_000) })]).len(), 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devtool_core::{EnvKind, Environment};

    fn env(name: &str, kind: EnvKind) -> Environment {
        Environment { name: name.into(), kind, label: None, convex_url: Some("http://127.0.0.1:3210".into()), convex_site_url: None, convex_deployment: Some("happy-otter-123".into()), frontend_url: None, branch: None, credential: None, created_at: 0, project: Some("my-app".into()) }
    }

    fn jobs() -> Vec<Value> {
        vec![
            json!({ "id": "j1", "recipe": "deploy-cloud", "title": "Deploy", "env": "dev", "status": "waiting-approval", "approval": { "level": "typed" }, "requestedBy": "mcp", "createdAt": 10, "args": { "payload": "SECRET-ARG" } }),
            json!({ "id": "j2", "recipe": "backup", "env": "prod", "status": "failed", "createdAt": 5, "error": "boom CONVEX_DEPLOY_KEY=prod:brave-falcon-456|eyJ2MiI6IjEyMzQ1Njc4OTAxMjM0NTY3ODkwIn0= end" }),
        ]
    }

    #[test]
    fn lo_que_sube_no_trae_secretos_ni_argumentos() {
        let probes = |k: &str| (k == "convex:dev").then(|| json!({ "ok": true, "at": 99, "data": { "reachable": true } }));
        let want = desired("mac-1", &[env("dev", EnvKind::CloudDev)], &probes, &jobs());
        let text = serde_json::to_string(&want).unwrap();
        assert!(!text.contains("SECRET-ARG"), "{text}");
        assert!(!text.contains("eyJ2MiI6"), "{text}");
        assert!(!text.contains("3210"), "URLs don't go up: {text}");
        assert_eq!(want["env:mac-1"]["dev"]["reachable"], json!(true));
        assert_eq!(want["env:mac-1"]["dev"]["kind"], json!("cloud-dev"));
        assert_eq!(want["approvals:mac-1"].keys().collect::<Vec<_>>(), vec!["j1"]);
        assert_eq!(want["approvals:mac-1"]["j1"]["approval"], json!("typed"));
        assert_eq!(want["jobs:mac-1"].len(), 2);
    }

    #[test]
    fn diff_toca_solo_los_campos_del_daemon() {
        let want: IndexMap<String, Values> = [("j1".to_string(), obj(json!({ "status": "waiting-approval", "recipe": "deploy-cloud" })))].into_iter().collect();
        // El teléfono ya escribió su decisión: no se pisa.
        let current: IndexMap<String, Values> = [("j1".to_string(), obj(json!({ "status": "waiting-approval", "recipe": "deploy-cloud", "decision": "approve" })))].into_iter().collect();
        assert!(diff("approvals:mac-1", &current, &want, &IndexMap::new()).is_empty());
        // Un cambio del daemon: solo ese campo.
        let want2: IndexMap<String, Values> = [("j1".to_string(), obj(json!({ "status": "waiting-approval", "recipe": "deploy" })))].into_iter().collect();
        let d = diff("approvals:mac-1", &current, &want2, &IndexMap::new());
        assert_eq!(d.len(), 1);
        assert_eq!(Value::Object(d[0].set.clone().unwrap()), json!({ "recipe": "deploy" }));
    }

    #[test]
    fn una_aprobacion_resuelta_pasa_a_su_estado_y_luego_se_borra() {
        let current: IndexMap<String, Values> = [("j1".to_string(), obj(json!({ "status": "waiting-approval" })))].into_iter().collect();
        let empty = IndexMap::new();
        let status: IndexMap<String, Value> = [("j1".to_string(), json!("running"))].into_iter().collect();
        let d = diff("approvals:mac-1", &current, &empty, &status);
        assert_eq!(d[0].kind, Some(OpKind::Patch));
        assert_eq!(Value::Object(d[0].set.clone().unwrap()), json!({ "status": "running" }));
        // Ya no está entre los recientes: se borra.
        let d = diff("approvals:mac-1", &current, &empty, &IndexMap::new());
        assert_eq!(d[0].kind, Some(OpKind::Delete));
        // En trabajos, lo que ya no está se borra siempre.
        let d = diff("jobs:mac-1", &current, &empty, &status);
        assert_eq!(d[0].kind, Some(OpKind::Delete));
    }

    #[test]
    fn el_timbre_por_http_solo_de_respaldo() {
        let now = 10_000_000;
        // Con el timbre en vivo: cada minuto.
        assert!(!ring_due(now, now - 30_000, true));
        assert!(ring_due(now, now - SAFETY_RING_MS, true));
        // Sin él: cada 15 s.
        assert!(!ring_due(now, now - 3_000, false));
        assert!(ring_due(now, now - FALLBACK_RING_MS, false));
    }

    #[test]
    fn el_remoto_de_github_sin_credenciales() {
        assert_eq!(github_https("git@github.com:o/r.git").as_deref(), Some("https://github.com/o/r.git"));
        assert_eq!(github_https("https://x-access-token:SECRET@github.com/o/r").as_deref(), Some("https://github.com/o/r.git"));
        assert_eq!(github_https("https://github.com/o/r.git/").as_deref(), Some("https://github.com/o/r.git"));
        assert_eq!(github_https("https://gitlab.com/o/r.git"), None);
    }

    #[test]
    fn lo_que_contesto_el_sitio_va_una_vez_como_contexto() {
        let mut row = Values::new();
        row.insert("siteItems".into(), json!(json!([{ "kind": "user", "text": "hola" }, { "kind": "reply", "text": "aquí el sitio" }]).to_string()));
        let (text, n) = site_context(&row, 0).unwrap();
        assert!(text.contains("User: hola") && text.contains("Cloud site: aquí el sitio"), "{text}");
        assert_eq!(n, 2);
        assert!(site_context(&row, 2).is_none());
        assert!(site_context(&Values::new(), 0).is_none());
    }

    #[test]
    fn null_y_sin_valor_no_se_reescriben() {
        // El motor no guarda los null: un campo null del daemon no es un cambio en cada vuelta.
        let current: IndexMap<String, Values> = [("t".to_string(), obj(json!({ "title": "x" })))].into_iter().collect();
        let want: IndexMap<String, Values> = [("t".to_string(), obj(json!({ "title": "x", "env": null })))].into_iter().collect();
        assert!(diff("threads:mac-1", &current, &want, &IndexMap::new()).is_empty());
    }

    #[test]
    fn crear_y_campo_que_desaparece() {
        let current: IndexMap<String, Values> = [("e".to_string(), obj(json!({ "name": "dev", "reachable": true })))].into_iter().collect();
        let want: IndexMap<String, Values> = [("e".to_string(), obj(json!({ "name": "dev" }))), ("n".to_string(), obj(json!({ "name": "new" })))].into_iter().collect();
        let d = diff("env:mac-1", &current, &want, &IndexMap::new());
        assert_eq!(d.len(), 2);
        assert_eq!(Value::Object(d[0].set.clone().unwrap()), json!({ "reachable": null }));
        assert_eq!(d[1].kind, Some(OpKind::Create));
    }

    #[test]
    fn vincular_exige_https_o_un_backend_local() {
        assert!(link("http://evil.example", "x".repeat(40).as_str(), "mac").is_err());
        assert!(link("https://ok.convex.cloud", "short", "mac").is_err());
        let pl = |url: &str, code: &str| pair_link(&site_url(url), url, code);
        assert_eq!(pl("https://happy-otter-123.convex.cloud", "K7Q2-M9XD"), "https://happy-otter-123.convex.site/pair#d=https%3A%2F%2Fhappy-otter-123.convex.cloud&c=K7Q2-M9XD");
        assert_eq!(pl("https://condx.example.com", "AB-CD"), "https://condx.example.com/pair#d=https%3A%2F%2Fcondx.example.com&c=AB-CD");
        assert!(pl("http://127.0.0.1:3390", "X").starts_with("http://127.0.0.1:3391/pair#"));
        // Un dominio propio para el sitio: el QR abre ahí y el teléfono sigue usando la API del deployment.
        let link = Link { url: "https://happy-otter-123.convex.cloud".into(), daemon_id: "mac-1".into(), name: "Mac".into(), linked_at: 0, site: Some("https://condx.example.com".into()) };
        assert_eq!(pair_link(&link.site(), &link.url, "AB"), "https://condx.example.com/pair#d=https%3A%2F%2Fhappy-otter-123.convex.cloud&c=AB");
        let old: Link = serde_json::from_str(r#"{"url":"https://a.convex.cloud","daemonId":"mac-1","name":"Mac","linkedAt":0}"#).unwrap();
        assert_eq!(old.site(), "https://a.convex.site");
    }

    #[test]
    fn decisiones_del_telefono() {
        use ed25519_dalek::{Signer, SigningKey};
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let pk = key.verifying_key().as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>();
        let device = TrustedDevice { device_id: "phone-1".into(), name: "iPhone".into(), public_key: pk.clone(), paired_at: 0, encryption_key: None, kind: None };
        let now = 10_000_000;
        let row = |decision: &str, at: i64, job: &str, key_hex: &str| -> Values {
            let sig = key.sign(decision_message("mac-1", job, decision, at).as_bytes());
            obj(json!({ "decision": decision, "decidedAt": at, "deviceKey": key_hex, "signature": sig.to_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>() }))
        };
        let ok = verify_decision("mac-1", "j1", 0, &row("approve", now, "j1", &pk), std::slice::from_ref(&device), now + 5_000).unwrap();
        assert!(ok.approve && ok.device.device_id == "phone-1");
        // Firmada para otro trabajo, otro daemon, o vieja: no.
        assert!(verify_decision("mac-1", "j1", 0, &row("approve", now, "j2", &pk), std::slice::from_ref(&device), now).is_err());
        assert!(verify_decision("mac-2", "j1", 0, &row("approve", now, "j1", &pk), std::slice::from_ref(&device), now).is_err());
        assert_eq!(verify_decision("mac-1", "j1", 0, &row("approve", now, "j1", &pk), std::slice::from_ref(&device), now + DECISION_MAX_AGE_MS + 1), Err("stale decision".into()));
        // Anterior al trabajo (una firma vieja reusada): no.
        assert!(verify_decision("mac-1", "j1", now + 1, &row("approve", now, "j1", &pk), std::slice::from_ref(&device), now + 2).is_err());
        // Un teléfono que esta Mac no confirmó, aunque el backend lo tenga: no.
        assert_eq!(verify_decision("mac-1", "j1", 0, &row("approve", now, "j1", &pk), &[], now), Err("unknown phone (not paired with this Mac)".into()));
        // Cambiar la decisión firmada: no.
        let mut tampered = row("reject", now, "j1", &pk);
        tampered.insert("decision".into(), json!("approve"));
        assert_eq!(verify_decision("mac-1", "j1", 0, &tampered, std::slice::from_ref(&device), now), Err("signature doesn't match".into()));
    }

    #[test]
    fn pedidos_del_telefono_sobre_incidentes() {
        use ed25519_dalek::{Signer, SigningKey};
        let key = SigningKey::from_bytes(&[9u8; 32]);
        let pk = key.verifying_key().as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>();
        let device = TrustedDevice { device_id: "phone-1".into(), name: "iPhone".into(), public_key: pk.clone(), paired_at: 0, encryption_key: None, kind: None };
        let now = 10_000_000;
        let row = |request: &str, arg: &str, at: i64, incident: &str| -> Values {
            let sig = key.sign(request_message("mac-1", incident, request, "rq0123456789abcd", at, arg).as_bytes());
            obj(json!({ "request": request, "requestArg": arg, "requestId": "rq0123456789abcd", "requestedAt": at, "deviceKey": pk, "signature": sig.to_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>() }))
        };
        let devices = std::slice::from_ref(&device);
        let ok = verify_request("mac-1", "inc-1", &row("message", "look at lib/dates.js | and tests", now, "inc-1"), devices, now).unwrap();
        assert_eq!((ok.request.as_str(), ok.arg.as_str()), ("message", "look at lib/dates.js | and tests"));
        // Para otro incidente, viejo, de un teléfono que esta Mac no confirmó o pedido desconocido: no.
        assert!(verify_request("mac-1", "inc-1", &row("diagnose", "", now, "inc-2"), devices, now).is_err());
        assert_eq!(verify_request("mac-1", "inc-1", &row("fix", "", now, "inc-1"), devices, now + DECISION_MAX_AGE_MS + 1), Err("stale request".into()));
        assert!(verify_request("mac-1", "inc-1", &row("fix", "", now, "inc-1"), &[], now).is_err());
        assert_eq!(verify_request("mac-1", "inc-1", &row("approve", "", now, "inc-1"), devices, now), Err("unknown request".into()));
        // Cambiar el flujo a desplegar (el argumento firmado): no.
        let mut tampered = row("deploy", "w-dev", now, "inc-1");
        tampered.insert("requestArg".into(), json!("w-prod"));
        assert_eq!(verify_request("mac-1", "inc-1", &tampered, devices, now), Err("signature doesn't match".into()));
        // Una firma de incidente no sirve en un hilo (otro prefijo), y en un hilo solo hay mensajes.
        let as_thread = row("message", "look at lib/dates.js | and tests", now, "inc-1");
        assert_eq!(verify_thread_request("mac-1", "inc-1", &as_thread, devices, now), Err("signature doesn't match".into()));
    }

    #[test]
    fn la_fila_de_un_incidente_va_redactada_y_corta() {
        let log: String = (0..30).map(|i| format!("line {i}\n")).collect::<String>() + "CONVEX_DEPLOY_KEY=prod:abc123secretvalue failed";
        let inc = json!({
            "id": "inc-1", "title": "Deploy failed", "source": "job", "status": "open", "count": 2,
            "evidence": { "error": "boom with CONVEX_DEPLOY_KEY=prod:abc123secretvalue", "log": log, "commit": "0123456789abcdef0123" },
            "agent": { "runId": "run-1", "mode": "fix", "status": "done", "answer": "## Cause\nx", "fix": { "branch": "condx/fix-1", "commit": "fedcba9876543210", "files": ["a", "b"], "stat": "a | 2 +-", "changes": true, "patch": "SHOULD NOT GO UP" } },
        });
        let row = incident_row(&inc, &[("w1".into(), "Deploy to dev".into())]);
        let text = serde_json::to_string(&row).unwrap();
        assert!(!text.contains("abc123secretvalue"), "{text}");
        assert!(!text.contains("SHOULD NOT GO UP"), "the patch stays on the Mac");
        assert_eq!(row["log"].as_str().unwrap().lines().count(), 8);
        assert_eq!(row["commit"], "0123456789ab");
        assert_eq!(row["agent"]["fix"]["commit"], "fedcba987654");
        assert_eq!(row["workflows"][0]["name"], "Deploy to dev");
    }

    #[test]
    fn las_claves_se_cifran_para_la_mac_y_los_telefonos_de_confianza() {
        let mac = age::x25519::Identity::generate();
        let phone = age::x25519::Identity::generate();
        let old_phone = age::x25519::Identity::generate();
        let device = |id: &str, key: Option<String>| TrustedDevice { device_id: id.into(), name: id.into(), public_key: "0".repeat(64), paired_at: 0, encryption_key: key, kind: None };
        let devices = vec![device("phone-1", Some(phone.to_public().to_string())), device("phone-legacy", None), device("phone-old", Some(old_phone.to_public().to_string()))];
        let recipients = key_recipients(&mac.to_public(), &devices);
        assert_eq!(recipients.len(), 3, "the Mac and the two phones with an identity");
        let sealed = crate::agents::seal("sk-test-1234", &recipients).unwrap();
        assert_eq!(crate::agents::open(&sealed, &phone).unwrap(), "sk-test-1234");
        assert_eq!(crate::agents::open(&sealed, &mac).unwrap(), "sk-test-1234");
        // Quitado el teléfono viejo, lo que se cifra después ya no lo abre.
        let after = key_recipients(&mac.to_public(), &devices[..2]);
        let resealed = crate::agents::seal("sk-test-1234", &after).unwrap();
        assert!(crate::agents::open(&resealed, &old_phone).is_err());
        assert_eq!(crate::agents::open(&resealed, &phone).unwrap(), "sk-test-1234");
    }

    #[test]
    fn huella_corta() {
        assert_eq!(fingerprint("abcdef0123456789"), "abcd ef01 2345");
    }
}
