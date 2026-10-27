// El sitio en la nube (docs/design/cloud-site.md, fase S1): el mismo `condxd` en Linux, en una VM
// chica, que sigue trabajando con la Mac apagada. Se empareja como un teléfono: su propia llave
// (ed25519) y su identidad de cifrado (age), un código de la Mac y la confirmación en la Mac. Con eso
// le llegan las claves de los proveedores cifradas solo para él (y para la Mac y los teléfonos), y el
// sitio las guarda como proveedores propios, cifrados con su `agent-key`. No tiene producción ni
// Llavero: los secretos van en `secrets.json` (0600) y la identidad en `site.json` (0600).
//
// `condxd site pair "<texto o enlace del QR>"` reclama el código; `condxd serve --site cloud`
// arranca el daemon en modo sitio y sincroniza las claves (con el timbre en vivo).
//
// En boat (`condxd site boat <sandbox>` y el secreto `boat-key`, una key solo de esa máquina con
// `sandbox.read,sandbox.stop,sandbox.resume`): el sitio se detiene solo cuando lleva un rato sin
// trabajo, o poco después de que la Mac vuelve a latir. Detenido no cobra; Condx Cloud lo despierta
// cuando llega trabajo y la Mac no está.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use ed25519_dalek::SigningKey;
use indexmap::IndexMap;
use localfirst_core::http::{ConvexHttpTransport, Doorbell};
use localfirst_core::{sqlite, sync_once, Engine, EngineConfig, OpDraft, Values};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::home;
use crate::state::Shared;
use crate::store::now_ms;

/// Cada cuánto suena por HTTP aunque el timbre en vivo ande, o sin él.
const SAFETY_RING_MS: i64 = 60_000;
const FALLBACK_RING_MS: i64 = 15_000;
/// Qué claves ya se aplicaron (por proveedor, su `updatedAt` en la Mac).
const APPLIED_KEY: &str = "meta:site-keys";
/// La key de boat del sitio (solo de su máquina: leer, detener, reanudar).
pub const BOAT_KEY: &str = "boat-key";
/// Sin trabajo este rato, el sitio se detiene (boat guarda el disco y deja de cobrar).
const IDLE_STOP_MS: i64 = 30 * 60_000;
/// Con la Mac latiendo (ella atiende), se detiene antes: este rato sin trabajo alcanza.
const HANDBACK_STOP_MS: i64 = 2 * 60_000;
/// Un latido de la Mac más viejo que esto: la Mac está apagada (late cada minuto; al irse en orden
/// avisa con 0).
const MAC_GONE_MS: i64 = 3 * 60_000;

/// Si este daemon corre como sitio en la nube (`serve --site cloud`).
pub fn is_cloud() -> bool {
    std::env::var("CONDX_SITE").is_ok_and(|s| s == "cloud")
}

/// Las URLs de boat de los frontends del sitio (`hosts.json`: `{"3002": "https://…-3002.on.boat.dev?_token=…"}`),
/// creadas con `boat host` (son fijas, privadas con token).
fn hosts() -> serde_json::Map<String, Value> {
    std::fs::read_to_string(crate::config::home().join("hosts.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default()
}

/// La URL de boat de un puerto, sin el token (el origen que ven el backend y Vite).
pub fn public_url(port: u16) -> Option<String> {
    if !is_cloud() {
        return None;
    }
    let url = hosts().get(&port.to_string())?.as_str()?.to_string();
    Some(url.split('?').next().unwrap_or_default().trim_end_matches('/').to_string())
}

/// La URL para abrir un puerto desde el teléfono (con su token si es privada).
pub fn open_url(port: u16) -> Option<String> {
    hosts().get(&port.to_string())?.as_str().map(str::to_string)
}

/// Lo que publica el sitio sobre sí mismo (sus servicios locales), para el teléfono con la Mac apagada.
pub const OVERVIEW_ROW: &str = "site-overview";

/// La última vez que alguien encendió servicios en el sitio (el teléfono, el MCP o la vista previa de
/// una tarea): mientras corren, cuentan como trabajo y el sitio no se apaga (hasta `SERVICES_KEEP_MS`).
static SERVICE_ASK: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

pub fn note_service_ask() {
    SERVICE_ASK.store(now_ms(), std::sync::atomic::Ordering::Relaxed);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub url: String,
    pub daemon_id: String,
    pub device_id: String,
    pub name: String,
    pub token: String,
    /// Semilla de la llave ed25519 (hex).
    pub seed: String,
    /// Identidad age (`AGE-SECRET-KEY-…`): abre las claves que la Mac le cifra.
    pub age: String,
    /// La máquina de boat donde corre (para detenerse sola), si corre en boat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boat_sandbox: Option<String>,
}

fn path() -> std::path::PathBuf {
    home().join("site.json")
}

pub fn load() -> Option<Identity> {
    std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str(&t).ok())
}

pub fn save(id: &Identity) -> Result<()> {
    crate::cloud::write_private(&path(), &serde_json::to_string_pretty(id)?)
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    (0..text.len()).step_by(2).map(|i| text.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok())).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// La huella corta de la llave (la misma que muestra la Mac al confirmar).
pub fn fingerprint(id: &Identity) -> String {
    let seed: Option<[u8; 32]> = unhex(&id.seed).and_then(|b| b.try_into().ok());
    let pk = seed.map(|s| hex(SigningKey::from_bytes(&s).verifying_key().as_bytes())).unwrap_or_default();
    pk.chars().take(12).collect::<Vec<_>>().chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join(" ")
}

fn call(url: &str, kind: &str, path: &str, args: Value) -> Result<Value> {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).build();
    let reply: Value = match agent.post(&format!("{}/api/{kind}", url.trim_end_matches('/'))).send_json(json!({ "path": path, "args": args, "format": "json" })) {
        Ok(r) => r.into_json()?,
        Err(ureq::Error::Status(_, r)) => r.into_json().unwrap_or(Value::Null),
        Err(e) => bail!("couldn't reach Condx Cloud: {e}"),
    };
    match reply["status"].as_str() {
        Some("success") => Ok(reply["value"].clone()),
        _ => bail!("{}", reply["errorMessage"].as_str().unwrap_or("Condx Cloud rejected the call")),
    }
}

/// El código del QR de la Mac: el enlace (`https://<sitio>/pair#d=<deployment>&c=<código>`), el JSON
/// del QR (`{"url", "code"}`) o el código suelto con `--url`.
pub fn parse_pairing(text: &str, url: Option<&str>) -> Result<(String, String)> {
    let text = text.trim();
    if let Some((_, frag)) = text.split_once("/pair#").filter(|_| text.starts_with("http")) {
        let get = |k: &str| frag.split('&').find_map(|kv| kv.strip_prefix(k).and_then(|v| v.strip_prefix('=')).map(percent_decode));
        return Ok((get("d").context("the link has no deployment URL")?, get("c").context("the link has no code")?));
    }
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        return Ok((v["url"].as_str().context("the QR has no deployment URL")?.to_string(), v["code"].as_str().context("the QR has no code")?.to_string()));
    }
    let url = url.filter(|u| !u.trim().is_empty()).context("pass the deployment URL too (--url)")?;
    Ok((url.trim().to_string(), text.to_string()))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], b.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok())) {
            (b'%', Some(v)) => {
                out.push(v);
                i += 3;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Reclama el código: genera la llave, el token y la identidad de cifrado, y queda pendiente de
/// confirmación en la Mac.
pub fn pair(text: &str, url: Option<&str>, name: &str) -> Result<Identity> {
    use age::secrecy::ExposeSecret;
    let (url, code) = parse_pairing(text, url)?;
    let seed: [u8; 32] = unhex(&crate::cloud::random_hex(32)?).and_then(|b| b.try_into().ok()).context("no randomness")?;
    let key = SigningKey::from_bytes(&seed);
    let token = crate::cloud::random_hex(32)?;
    let enc = age::x25519::Identity::generate();
    let mut args = json!({ "code": code, "publicKey": hex(key.verifying_key().as_bytes()), "token": token, "name": name, "encryptionKey": enc.to_public().to_string(), "kind": "site" });
    // Un Condx Cloud anterior no conoce `kind` (rechaza los argumentos antes de tocar el código): se
    // reclama como un teléfono y la Mac lo muestra entre ellos hasta que se despliegue el nuevo.
    let v = match call(&url, "mutation", "pairing:claim", args.clone()) {
        Ok(v) => v,
        Err(_) => {
            args.as_object_mut().unwrap().remove("kind");
            call(&url, "mutation", "pairing:claim", args)?
        }
    };
    let id = Identity {
        url: url.trim_end_matches('/').to_string(),
        daemon_id: v["daemonId"].as_str().context("unexpected answer")?.to_string(),
        device_id: v["deviceId"].as_str().context("unexpected answer")?.to_string(),
        name: name.to_string(),
        token,
        seed: hex(&seed),
        age: enc.to_string().expose_secret().to_string(),
        boat_sandbox: None,
    };
    std::fs::create_dir_all(home())?;
    save(&id)?;
    Ok(id)
}

/// `pending`, `active` o `revoked`.
pub fn status(id: &Identity) -> Result<String> {
    Ok(call(&id.url, "query", "pairing:me", json!({ "token": id.token }))?["status"].as_str().unwrap_or("unknown").to_string())
}

/// Lo que el sitio lee de Condx Cloud: las claves (`keys:`) y los hilos (`threads:`, donde la Mac
/// publica su configuración de agentes en la fila `_settings`).
struct Session {
    engine: crate::cloud::CloudEngine,
    transport: ConvexHttpTransport,
    keys_scope: String,
    threads_scope: String,
    age: age::x25519::Identity,
    bell: crate::bell::Bell,
    rung_at: i64,
    /// La última vez que tuvo trabajo (al abrir cuenta como trabajo: recién despertado).
    busy_at: i64,
    /// Ya le pidió a boat que lo detenga (una vez).
    stopping: bool,
    /// El runtime del daemon (arrancar un agente es asíncrono; este hilo no lo es).
    rt: tokio::runtime::Handle,
    /// Los mensajes de teléfono que está contestando: corrida del agente → dónde va la respuesta.
    answering: IndexMap<String, Answering>,
    /// Pedidos ya tomados (uno por fila y pedido).
    taken: std::collections::HashSet<String>,
    /// Proyectos que se están clonando o que fallaron (no se reintenta en cada vuelta).
    cloning: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, i64>>>,
    /// Cuándo publicó su resumen y cuándo un teléfono le pidió algo a un servicio.
    overview_at: i64,
    service_asked_at: i64,
    /// Las llamadas del MCP remoto que contesta con la Mac apagada (`remote_mcp.rs`).
    mcp: crate::remote_mcp::Calls,
}

/// Un mensaje de teléfono que el sitio está contestando: en qué fila va la respuesta (un hilo nuevo ya
/// es su `site-…`), lo que había antes en el hilo, el mensaje y lo último que se publicó.
struct Answering {
    row: String,
    base: Vec<Value>,
    user: Value,
    shown: Value,
}

fn open(id: &Identity, rt: tokio::runtime::Handle) -> Result<Session> {
    let (replica, outbox) = sqlite::open_in_memory().map_err(|e| anyhow!("{e}"))?;
    let mut n = 0u64;
    let mut engine = Engine::open(replica, outbox, EngineConfig { backoff_base_ms: 500, backoff_max_ms: 30_000, ..Default::default() }, move || {
        n += 1;
        format!("site-{}-{n}", now_ms())
    })
    .map_err(|e| anyhow!("{e}"))?;
    let keys_scope = format!("keys:{}", id.daemon_id);
    engine.enable_scope(&keys_scope, None).map_err(|e| anyhow!("{e}"))?;
    let threads_scope = format!("threads:{}", id.daemon_id);
    engine.enable_scope(&threads_scope, None).map_err(|e| anyhow!("{e}"))?;
    let age = id.age.parse::<age::x25519::Identity>().map_err(|e| anyhow!("bad encryption identity in site.json: {e}"))?;
    let bell = crate::bell::Bell::start(&id.url, &id.token, &[keys_scope.clone(), threads_scope.clone()], localfirst_core::PROTOCOL_VERSION);
    Ok(Session { engine, transport: ConvexHttpTransport::new(&id.url, &id.token), keys_scope, threads_scope, age, bell, rung_at: 0, busy_at: now_ms(), stopping: false, rt, answering: IndexMap::new(), taken: Default::default(), cloning: Default::default(), overview_at: 0, service_asked_at: 0, mcp: Default::default() })
}

/// Una vuelta: timbre (en vivo o por HTTP) y sincronizar; luego aplicar las claves que cambiaron.
fn tick(state: &Shared, s: &mut Session) -> Result<Value> {
    let now = now_ms();
    let due = now - s.rung_at >= if s.bell.is_live() { SAFETY_RING_MS } else { FALLBACK_RING_MS };
    let rung = if due {
        s.bell.take();
        s.rung_at = now;
        Some(s.transport.doorbell(&[s.keys_scope.clone(), s.threads_scope.clone()], localfirst_core::PROTOCOL_VERSION).map_err(|e| anyhow!("{e}"))?)
    } else {
        match s.bell.take() {
            Some(v) if v["status"] == "upgradeRequired" => Some(Doorbell::UpgradeRequired),
            Some(v) => Some(Doorbell::Versions(serde_json::from_value(v["versions"].clone())?)),
            None => None,
        }
    };
    match rung {
        Some(Doorbell::Versions(v)) => s.engine.on_doorbell(&v, now).map_err(|e| anyhow!("{e}"))?,
        Some(Doorbell::UpgradeRequired) => bail!("Condx Cloud needs a newer condxd"),
        None => {}
    }
    crate::cloud::block_on(sync_once(&mut s.engine, &s.transport, now_ms, 50)).map_err(|e| anyhow!("{e}"))?;
    // Primero las claves: la configuración nombra al proveedor por defecto.
    let applied = apply_keys(state, s)?;
    let settings = apply_settings(state, s)?;
    if let Err(e) = apply_flows(state, s) {
        tracing::warn!("site: couldn't copy the flows: {e:#}");
    }
    *SITE_LOGO_NOW.lock().unwrap() = crate::agents::logo_for(state, None);
    let mac_alive_at = s.engine.rows(&s.threads_scope).into_iter().find(|r| r.row.id == crate::cloud::MAC_ROW).and_then(|r| r.row.values.get("aliveAt").and_then(Value::as_i64));
    let mac_away = !mac_alive_at.is_some_and(|at| now - at < MAC_GONE_MS);
    clone_projects(state, s);
    answer_threads(state, s, mac_away);
    answer_mcp(state, s, mac_away);
    run_services(state, s, mac_away);
    publish_overview(state, s)?;
    // ¿Detenerse? Con trabajo (agentes o trabajos en marcha) no; sin trabajo un rato, sí. Los servicios
    // locales encendidos cuentan como trabajo hasta 3 h después del último pedido de un teléfono.
    let asked = s.service_asked_at.max(SERVICE_ASK.load(std::sync::atomic::Ordering::Relaxed));
    let services_on = now - asked < SERVICES_KEEP_MS && state.procs.any_running();
    let busy = services_on || !state.store.agent_runs(&["running"], None)?.is_empty() || !state.store.jobs(&["running", "queued"], 1)?.is_empty();
    if busy {
        s.busy_at = now;
    }
    let stop = should_stop(now, s.busy_at, mac_alive_at);
    if stop && !s.stopping {
        if let (Some(id), Some(key)) = (load().and_then(|i| i.boat_sandbox), crate::secrets::get(BOAT_KEY).ok().flatten()) {
            tracing::info!("site: no work for a while: asking boat to stop {id}");
            boat(&key, "POST", &format!("/sandboxes/{id}/stop"))?;
            s.stopping = true;
        }
    }
    Ok(json!({ "liveBell": s.bell.is_live(), "keys": applied, "settingsFromMac": settings, "idleFor": now - s.busy_at, "macAliveAt": mac_alive_at, "macAway": mac_away, "answering": s.answering.len(), "stopping": s.stopping }))
}

/// Cuánto mantienen encendida la máquina los servicios locales sin pedidos nuevos.
const SERVICES_KEEP_MS: i64 = 3 * 3_600_000;

/// Los pedidos de servicios (`_run` con `service`) con la Mac apagada: levantar, detener o reiniciar
/// el backend, el deployer o el frontend de un proyecto en esta máquina. Lo demás (recetas, flujos,
/// ajustes) espera a la Mac.
fn run_services(state: &Shared, s: &mut Session, mac_away: bool) {
    if !mac_away {
        return;
    }
    let rows = s.engine.rows(&s.threads_scope);
    let Some(r) = rows.iter().find(|r| r.row.id == crate::cloud::RUN_ROW) else { return };
    let v = &r.row.values;
    let Some(request_id) = v.get("requestId").and_then(Value::as_str).map(str::to_string) else { return };
    let answered = [v.get("resultFor"), v.get("siteFor")].iter().any(|f| f.and_then(|x| x.as_str()) == Some(request_id.as_str()));
    if r.pending || answered || !s.taken.insert(format!("_run:{request_id}")) {
        return;
    }
    let devices: Vec<crate::cloud::TrustedDevice> = rows
        .iter()
        .find(|r| r.row.id == crate::cloud::DEVICES_ROW)
        .and_then(|r| r.row.values.get("devices").and_then(Value::as_str))
        .and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or_default();
    let daemon_id = s.threads_scope.trim_start_matches("threads:").to_string();
    let result = match crate::cloud::verify_run(&daemon_id, v, &devices, now_ms()) {
        Err(e) => format!("rejected: {e}"),
        Ok(req) => {
            let ask: Value = serde_json::from_str(&req.arg).unwrap_or(Value::Null);
            let by = format!("phone:{}", req.device.name);
            match ask["service"].as_object() {
                // Pasar a producción por fusión: también con la Mac apagada (solo GitHub).
                None if ask["promote"].is_object() => match crate::projects::get(state, ask["promote"]["project"].as_str()).and_then(|p| crate::promote::merge(&p.repo, &by)) {
                    Ok(done) => format!("{done} (cloud site)"),
                    Err(e) => format!("rejected: {e:#}"),
                },
                // Subir a dev la rama de un agente: también con la Mac apagada (solo GitHub).
                None if ask["merge"].is_object() => match crate::projects::get(state, ask["merge"]["project"].as_str()).and_then(|p| crate::promote::merge_into_dev(&p.repo, ask["merge"]["branch"].as_str().unwrap_or_default(), &by)) {
                    Ok(done) => format!("{done} (cloud site)"),
                    Err(e) => format!("rejected: {e:#}"),
                },
                None => "rejected: the Mac is off; from the cloud site only services, merging into dev and passing to production can be asked".into(),
                Some(svc) => {
                    let name = svc.get("name").and_then(Value::as_str).unwrap_or_default();
                    let action = svc.get("action").and_then(Value::as_str).unwrap_or_default();
                    let _rt = s.rt.enter();
                    s.service_asked_at = now_ms();
                    s.overview_at = 0;
                    match crate::projects::get(state, svc.get("project").and_then(Value::as_str)) {
                        Err(e) => format!("rejected: {e:#}"),
                        Ok(p) => {
                            // Al levantar, el clon sigue la rama en la que trabajaba la Mac.
                            let branch = if action == "stop" {
                                Ok(String::new())
                            } else {
                                let mac_branch = svc.get("branch").and_then(Value::as_str).map(str::to_string).or_else(|| mac_branch(&rows, &p.name));
                                crate::services::cloud_checkout(state, &p, mac_branch.as_deref())
                            };
                            match branch.and_then(|branch| crate::services::action(state, &p, name, action).map(|_| branch)) {
                                Ok(branch) if branch.is_empty() => format!("{action} {name}: on its way (cloud site)"),
                                Ok(branch) => format!("{action} {name}: on its way (cloud site · {branch})"),
                                Err(e) => format!("rejected: {e:#}"),
                            }
                        }
                    }
                }
            }
        }
    };
    tracing::info!("site: phone run request: {result}");
    let mut set = Values::new();
    set.insert("siteResult".into(), Value::String(result));
    set.insert("siteFor".into(), Value::String(request_id));
    if let Err(e) = s.engine.commit(vec![OpDraft::patch(&s.threads_scope, crate::cloud::RUN_ROW, set)], now_ms()) {
        tracing::warn!("site: couldn't answer the run request: {e}");
    }
}

/// La rama en la que estaba la Mac en un proyecto (de su `_overview`).
fn mac_branch(rows: &[localfirst_core::ViewRow], project: &str) -> Option<String> {
    let text = rows.iter().find(|r| r.row.id == crate::cloud::OVERVIEW_ROW)?.row.values.get("overview")?.as_str()?.to_string();
    let v: Value = serde_json::from_str(&text).ok()?;
    v["projects"].as_array()?.iter().find(|p| p["name"] == project)?["git"]["branch"].as_str().map(str::to_string)
}

/// El resumen del sitio (sus proyectos clonados y sus servicios): cada 20 s, cada 3 s mientras cambia.
fn publish_overview(state: &Shared, s: &mut Session) -> Result<()> {
    // Hasta que se publiquen los frontends (scripts/boat-hosts.sh, después de desplegar Condx Cloud
    // con el campo `overview` permitido) no se publica: un envío rechazado trabaría los demás.
    if hosts().is_empty() {
        return Ok(());
    }
    let quiet = !state.services.busy_any() && now_ms() - s.service_asked_at > 60_000;
    if now_ms() - s.overview_at < if quiet { 20_000 } else { 3_000 } {
        return Ok(());
    }
    s.overview_at = now_ms();
    let overview = s.rt.block_on(crate::cloud::overview(state));
    let mut set = Values::new();
    set.insert("title".into(), json!("overview"));
    set.insert("kind".into(), json!("overview"));
    set.insert("overview".into(), Value::String(overview.to_string()));
    let exists = s.engine.rows(&s.threads_scope).iter().any(|r| r.row.id == OVERVIEW_ROW);
    let op = if exists { OpDraft::patch(&s.threads_scope, OVERVIEW_ROW, set) } else { OpDraft::create(&s.threads_scope, set).with_row(OVERVIEW_ROW) };
    s.engine.commit(vec![op], now_ms()).map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

/// Las llamadas del MCP remoto (Claude desde cualquier lugar) con la Mac apagada: las verifica con los
/// teléfonos de confianza que publica la Mac, las corre aparte y contesta en sus campos (`siteFor`,
/// `siteResult`), que Condx Cloud le deja escribir solo si la Mac no late.
fn answer_mcp(state: &Shared, s: &mut Session, mac_away: bool) {
    let all = s.engine.rows(&s.threads_scope);
    let devices: Vec<crate::cloud::TrustedDevice> = all.iter().find(|r| r.row.id == crate::cloud::DEVICES_ROW).and_then(|r| r.row.values.get("devices").and_then(Value::as_str)).and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default();
    let rows: Vec<(String, Values)> = if mac_away { all.into_iter().filter(|r| r.row.id.starts_with(crate::remote_mcp::ROW_PREFIX) && !r.pending).map(|r| (r.row.id.clone(), r.row.values.clone())).collect() } else { vec![] };
    let daemon_id = s.threads_scope.trim_start_matches("threads:").to_string();
    s.mcp.start(state, &s.rt, &daemon_id, &rows, &devices, Some(s.bell.waker()));
    let mut wrote = false;
    for (row, call, reply) in s.mcp.drain() {
        if !rows.iter().any(|(id, _)| id == &row) {
            continue;
        }
        let mut set = Values::new();
        set.insert("siteResult".into(), json!(reply));
        set.insert("siteFor".into(), json!(call));
        match s.engine.commit(vec![OpDraft::patch(&s.threads_scope, &row, set)], now_ms()) {
            Ok(_) => wrote = true,
            Err(e) => tracing::warn!("site: couldn't write the remote mcp answer on {row}: {e}"),
        }
    }
    if wrote {
        s.busy_at = now_ms();
        let _ = crate::cloud::block_on(sync_once(&mut s.engine, &s.transport, now_ms, 50));
    }
}

/// Los mensajes de los teléfonos que le tocan al sitio (S5): los de sus hilos (`site-…`) siempre; los
/// de los hilos de la Mac y los hilos nuevos, solo con la Mac apagada. Verifica la firma con los
/// teléfonos de confianza que publica la Mac (`_devices`), arranca un chat con la conversación como
/// contexto y, cuando el agente contesta, escribe la respuesta donde va.
fn answer_threads(state: &Shared, s: &mut Session, mac_away: bool) {
    let rows = s.engine.rows(&s.threads_scope);
    let devices: Vec<crate::cloud::TrustedDevice> = rows
        .iter()
        .find(|r| r.row.id == crate::cloud::DEVICES_ROW)
        .and_then(|r| r.row.values.get("devices").and_then(Value::as_str))
        .and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or_default();
    let daemon_id = s.threads_scope.trim_start_matches("threads:").to_string();
    for r in &rows {
        let v = &r.row.values;
        let id = r.row.id.as_str();
        let Some(request_id) = v.get("requestId").and_then(Value::as_str) else { continue };
        let answered = [v.get("resultFor"), v.get("siteFor")].iter().any(|f| f.and_then(|x| x.as_str()) == Some(request_id));
        let mine = id.starts_with("site-");
        // Con la Mac aquí contesta ella, también los hilos del sitio (los importa al volver).
        let _ = mine;
        if r.pending || answered || !mac_away || (id.starts_with('_') && id != crate::cloud::NEW_ROW) {
            continue;
        }
        if !s.taken.insert(format!("{id}:{request_id}")) {
            continue;
        }
        let req = match crate::cloud::verify_thread_request(&daemon_id, id, v, &devices, now_ms()) {
            Ok(req) => req,
            Err(e) => {
                tracing::warn!("site: phone message on {id} rejected: {e}");
                continue;
            }
        };
        // El proyecto y el modelo elegidos en el teléfono van al principio (`[project: …] [model: …]`).
        let (picked, said) = crate::agents::take_project(req.arg.trim());
        let picked = picked.filter(|n| state.store.projects().unwrap_or_default().iter().any(|p| &p.name == n));
        let (model, said) = crate::agents::take_model(&said);
        let said = said.trim().to_string();
        let mut text = said.clone();
        if req.files.is_some() {
            text.push_str("\n\n(The phone sent attachments; they're sealed for the Mac and can't be opened here.)");
        }
        let prompt = match transcript(v) {
            t if t.is_empty() => text.clone(),
            t => format!("(Earlier in this conversation:)\n{t}\n\n(New message from {}:)\n{text}", req.device.name),
        };
        // Un hilo nuevo: el proyecto que nombra el mensaje (la fila `_new` puede traer el de un hilo
        // anterior); los del sitio guardan el suyo.
        let row_project = v.get("project").and_then(Value::as_str).filter(|p| !p.is_empty()).map(str::to_string);
        let project = if id == crate::cloud::NEW_ROW { picked.or_else(|| crate::agent_host::mentioned_project(state, &text)).or(row_project) } else { row_project.or_else(|| crate::agent_host::mentioned_project(state, &text)) };
        let at = v.get("requestedAt").and_then(Value::as_i64).unwrap_or_else(now_ms);
        let user = json!({ "kind": "user", "author": null, "at": at, "text": said });
        tracing::info!("site: answering a phone message on {id}");
        // Primero, como la Mac: el mensaje recibido (✓✓ en el teléfono) y la respuesta pensando; el
        // agente arranca después (si el host está frío tarda unos segundos).
        let a = match take(s, id, request_id, user, v, project.as_deref()) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!("site: couldn't take the message on {id}: {e:#}");
                continue;
            }
        };
        // Sale ya (sin esperar a la próxima vuelta).
        let _ = crate::cloud::block_on(sync_once(&mut s.engine, &s.transport, now_ms, 50));
        let _rt = s.rt.enter();
        match s.rt.block_on(crate::agents::MODEL_OVERRIDE.scope(model, crate::agent_host::start_chat(state, &prompt, project.as_deref(), &format!("phone:{}", req.device.name)))) {
            Ok(run) => {
                s.answering.insert(run["id"].as_str().unwrap_or_default().to_string(), a);
            }
            Err(e) => {
                tracing::warn!("site: couldn't start an agent for {id}: {e:#}");
                let failed = json!({ "kind": "reply", "author": SITE_AUTHOR, "logo": site_logo(), "at": now_ms(), "text": format!("Couldn't start an agent on the cloud site: {e:#}") });
                let items: Vec<Value> = a.base.iter().cloned().chain([a.user.clone(), failed]).collect();
                let _ = publish(s, &a.row, items, true);
            }
        }
    }
    // Las corridas en marcha: su avance en vivo (texto a medias, qué hace); las que terminaron, la
    // respuesta final.
    let runs: Vec<String> = s.answering.keys().cloned().collect();
    for run_id in runs {
        // Con lo que tiene el host (transcripción y lo que hace ahora): los pasos, como en la Mac.
        let Ok(run) = s.rt.block_on(crate::agent_host::get(state, &run_id)) else { continue };
        let at = s.answering[&run_id].user["at"].as_i64().unwrap_or(0);
        let (reply, done) = match run["status"].as_str() {
            Some("running") => (crate::threads::live_item(&run, SITE_AUTHOR, site_logo(), at).unwrap_or_else(|| thinking(at)), false),
            Some("done") => {
                let mut reply = json!({ "kind": "reply", "author": SITE_AUTHOR, "logo": site_logo(), "at": now_ms(), "text": run["answer"].as_str().unwrap_or("").trim() });
                let (folded, _) = crate::threads::fold_steps(run["transcript"].as_array().map(Vec::as_slice).unwrap_or_default(), false);
                if let Some(last) = folded.last().filter(|t| t["role"] == "assistant") {
                    reply["steps"] = last["steps"].clone();
                    reply["took"] = last["took"].clone();
                }
                (reply, true)
            }
            Some(st) => (json!({ "kind": "reply", "author": SITE_AUTHOR, "logo": site_logo(), "at": now_ms(), "text": format!("The agent {st}: {}", run["error"].as_str().unwrap_or("no details")) }), true),
            None => continue,
        };
        if !done && s.answering[&run_id].shown == reply {
            continue;
        }
        let a = if done { s.answering.shift_remove(&run_id) } else { None };
        let row = a.as_ref().map(|a| a.row.clone()).unwrap_or_else(|| s.answering[&run_id].row.clone());
        let mut items = a.as_ref().map(|a| a.base.clone()).unwrap_or_else(|| s.answering[&run_id].base.clone());
        items.push(a.as_ref().map(|a| a.user.clone()).unwrap_or_else(|| s.answering[&run_id].user.clone()));
        items.push(reply.clone());
        let items = crate::cloud::slim_steps(items);
        if let Err(e) = publish(s, &row, items, done) {
            tracing::warn!("site: couldn't write the answer on {row}: {e:#}");
        }
        if let Some(a) = s.answering.get_mut(&run_id) {
            a.shown = reply;
        }
    }
}

const SITE_AUTHOR: &str = "Cloud site";
/// La marca con que se ve el sitio en los hilos: la de su proveedor predeterminado (se actualiza en
/// cada vuelta, cuando cambia la configuración que copia de la Mac).
static SITE_LOGO_NOW: std::sync::Mutex<&'static str> = std::sync::Mutex::new("other");

fn site_logo() -> &'static str {
    *SITE_LOGO_NOW.lock().unwrap()
}

/// La respuesta mientras el agente todavía no escribió nada.
fn thinking(at: i64) -> Value {
    json!({ "at": at + 1, "kind": "reply", "live": true, "author": SITE_AUTHOR, "logo": site_logo(), "activity": "Thinking…", "text": "" })
}

/// Toma un mensaje: lo marca recibido y publica el mensaje con la respuesta pensando. Un hilo pedido
/// con «New thread» se crea ya como `site-…` (el teléfono lo abre enseguida).
fn take(s: &mut Session, id: &str, request_id: &str, user: Value, v: &Values, project: Option<&str>) -> Result<Answering> {
    let now = now_ms();
    let list = |k: &str| v.get(k).and_then(Value::as_str).and_then(|t| serde_json::from_str::<Vec<Value>>(t).ok()).unwrap_or_default();
    let at = user["at"].as_i64().unwrap_or(now);
    let items = |base: &[Value]| json!(Value::Array(base.iter().cloned().chain([user.clone(), thinking(at)]).collect()).to_string());
    let mut set = Values::new();
    let (row, base, drafts) = if id == crate::cloud::NEW_ROW {
        let row = format!("site-{}", crate::cloud::random_hex(6)?);
        let title: String = user["text"].as_str().unwrap_or("Chat").chars().take(60).collect();
        let participants = json!([{ "node": "agent", "name": SITE_AUTHOR, "logo": site_logo() }]).to_string();
        let mut new = Values::new();
        for (k, val) in [("title", json!(title)), ("kind", json!("chat")), ("status", json!("running")), ("updatedAt", json!(now)), ("items", items(&[])), ("participants", json!(participants)), ("waitingAnswer", json!(false))] {
            new.insert(k.into(), val);
        }
        if let Some(p) = project {
            new.insert("project".into(), json!(p));
        }
        set.insert("siteFor".into(), json!(request_id));
        set.insert("siteResult".into(), json!(format!("thread {row}")));
        let drafts = vec![OpDraft::create(&s.threads_scope, new).with_row(&row), OpDraft::patch(&s.threads_scope, crate::cloud::NEW_ROW, set)];
        (row, vec![], drafts)
    } else if id.starts_with("site-") {
        let base = list("items");
        set.insert("items".into(), items(&base));
        set.insert("status".into(), json!("running"));
        set.insert("updatedAt".into(), json!(now));
        set.insert("result".into(), json!("sent to Cloud site"));
        set.insert("resultFor".into(), json!(request_id));
        (id.to_string(), base, vec![OpDraft::patch(&s.threads_scope, id, set)])
    } else {
        let base = list("siteItems");
        set.insert("siteItems".into(), items(&base));
        set.insert("siteFor".into(), json!(request_id));
        (id.to_string(), base, vec![OpDraft::patch(&s.threads_scope, id, set)])
    };
    s.engine.commit(drafts, now).map_err(|e| anyhow!("{e}"))?;
    Ok(Answering { row, base, user: user.clone(), shown: thinking(at) })
}

/// Publica el hilo con la respuesta (en vivo o final): en un hilo del sitio, como la Mac en los suyos;
/// en uno de la Mac, aparte (`siteItems`).
fn publish(s: &mut Session, row: &str, items: Vec<Value>, done: bool) -> Result<()> {
    let now = now_ms();
    let mut set = Values::new();
    if row.starts_with("site-") {
        set.insert("items".into(), json!(Value::Array(items).to_string()));
        set.insert("status".into(), json!(if done { "done" } else { "running" }));
        set.insert("updatedAt".into(), json!(now));
    } else {
        set.insert("siteItems".into(), json!(Value::Array(items).to_string()));
    }
    s.engine.commit(vec![OpDraft::patch(&s.threads_scope, row, set)], now).map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

/// Los proyectos de la Mac (`_projects`) que el sitio todavía no tiene: los clona en segundo plano
/// (con el token de GitHub si hay: los repos privados) y los registra, para que sus agentes lean el
/// código. Un clon que falla se reintenta a los 10 min.
fn clone_projects(state: &Shared, s: &mut Session) {
    let wanted: Vec<Value> = s
        .engine
        .rows(&s.threads_scope)
        .into_iter()
        .find(|r| r.row.id == crate::cloud::PROJECTS_ROW)
        .and_then(|r| r.row.values.get("projects").and_then(Value::as_str).and_then(|t| serde_json::from_str(t).ok()))
        .unwrap_or_default();
    let have: Vec<String> = state.store.projects().unwrap_or_default().into_iter().map(|p| p.name).collect();
    for p in wanted {
        let (Some(name), Some(url)) = (p["name"].as_str().map(str::to_string), p["url"].as_str().map(str::to_string)) else { continue };
        if have.contains(&name) {
            // Ya clonado: que tenga sus submódulos y dependencias (una vez por arranque; los clones de antes no los tenían).
            let mut busy = s.cloning.lock().unwrap();
            if busy.insert(format!("ready:{name}"), now_ms()).is_none() {
                let dir = home().join("projects").join(&name);
                std::thread::spawn(move || {
                    if let Err(e) = make_ready(&dir) {
                        tracing::warn!("site: {name} isn't ready to work on: {e:#}");
                    }
                });
            }
            continue;
        }
        if !url.starts_with("https://github.com/") {
            continue;
        }
        {
            let mut busy = s.cloning.lock().unwrap();
            if busy.get(&name).is_some_and(|at| now_ms() - at < 10 * 60_000) {
                continue;
            }
            busy.insert(name.clone(), now_ms());
        }
        let branch = p["branch"].as_str().unwrap_or("main").to_string();
        let st = state.clone();
        std::thread::spawn(move || match clone_project(&st, &name, &url, &branch) {
            Ok(()) => tracing::info!("site: project {name} cloned and registered"),
            Err(e) => tracing::warn!("site: couldn't clone {name}: {e:#}"),
        });
    }
}

fn app_env_path(project: &str) -> std::path::PathBuf {
    home().join("app-env").join(format!("{project}.json"))
}

/// Las variables de la app de un proyecto que mandó la Mac (sin las de Convex), para su frontend.
pub fn app_env(project: &str) -> Vec<(String, String)> {
    if !is_cloud() {
        return vec![];
    }
    std::fs::read_to_string(app_env_path(project))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Map<String, Value>>(&t).ok())
        .map(|m| m.into_iter().filter_map(|(k, v)| v.as_str().map(|v| (k, v.to_string()))).collect())
        .unwrap_or_default()
}

/// `git fetch origin` en un clon del sitio, probando cada token de GitHub (como al clonar).
pub fn fetch(dir: &std::path::Path) -> bool {
    let mut tokens: Vec<Option<String>> = crate::secrets::accounts_with_prefix("github-token").iter().filter_map(|a| crate::secrets::get(a).ok().flatten()).map(Some).collect();
    tokens.push(None);
    tokens.into_iter().any(|token| {
        let mut cmd = std::process::Command::new("git");
        if let Some(token) = &token {
            use base64::Engine as _;
            let basic = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{}", token.trim()));
            cmd.args(["-c", &format!("http.extraHeader=Authorization: Basic {basic}")]);
        }
        cmd.env("GIT_TERMINAL_PROMPT", "0").arg("-C").arg(dir).args(["fetch", "-q", "origin"]).output().is_ok_and(|o| o.status.success())
    })
}

fn clone_project(state: &Shared, name: &str, url: &str, branch: &str) -> Result<()> {
    let dir = home().join("projects").join(name);
    // Un clon cortado a medias (el contenedor se reinició) no tiene ningún commit: se empieza de nuevo.
    let complete = std::process::Command::new("git").arg("-C").arg(&dir).args(["rev-parse", "--verify", "HEAD"]).output().is_ok_and(|o| o.status.success());
    if dir.exists() && !complete {
        let _ = std::fs::remove_dir_all(&dir);
    }
    if !dir.join(".git").exists() {
        std::fs::create_dir_all(dir.parent().unwrap())?;
        // Primero el token del dueño del repo (`github-token:<dueño>`), después los demás (un repo
        // movido a una organización redirige y necesita el de la organización) y al final sin token
        // (los públicos).
        let owner = url.trim_start_matches("https://github.com/").split('/').next().unwrap_or_default().to_string();
        let first = format!("github-token:{owner}");
        let mut accounts = crate::secrets::accounts_with_prefix("github-token");
        accounts.retain(|a| *a != first);
        accounts.insert(0, first);
        let mut tokens: Vec<Option<String>> = accounts.iter().filter_map(|a| crate::secrets::get(a).ok().flatten()).map(Some).collect();
        tokens.push(None);
        let mut last = String::new();
        // El nombre actual del repo (uno movido a una organización redirige; la API sigue la
        // redirección, `git clone` con el token de la organización no).
        let url = tokens.iter().find_map(|t| canonical_url(url, t.as_deref())).unwrap_or_else(|| url.to_string());
        let url = url.as_str();
        for token in tokens {
            let mut cmd = std::process::Command::new("git");
            // El token va en un encabezado de esta orden, no en el remoto guardado.
            if let Some(token) = &token {
                use base64::Engine as _;
                let basic = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{}", token.trim()));
                cmd.args(["-c", &format!("http.extraHeader=Authorization: Basic {basic}")]);
            }
            let out = cmd.env("GIT_TERMINAL_PROMPT", "0").args(["clone", "--quiet", "--branch", branch, url]).arg(&dir).output().context("couldn't run git")?;
            if out.status.success() {
                last.clear();
                break;
            }
            let _ = std::fs::remove_dir_all(&dir);
            last = devtool_core::redact(&String::from_utf8_lossy(&out.stderr)).chars().take(300).collect();
        }
        if !last.is_empty() {
            bail!("git clone failed with every GitHub token (they need Contents: read on the repo): {last}");
        }
    }
    crate::projects::register(state, &dir, Some(name))?;
    make_ready(&dir)
}

/// Los tokens de GitHub del sitio como encabezados de git (y al final sin ninguno, para lo público).
fn git_auth_headers() -> Vec<Option<String>> {
    use base64::Engine as _;
    let mut out: Vec<Option<String>> = crate::secrets::accounts_with_prefix("github-token")
        .iter()
        .filter_map(|a| crate::secrets::get(a).ok().flatten())
        .map(|t| Some(format!("http.extraHeader=Authorization: Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{}", t.trim())))))
        .collect();
    out.push(None);
    out
}

/// Un clon listo para que trabajen los agentes: los submódulos de sus workspaces (`packages/*`,
/// `apps/*`: el código los importa; otros, como herramientas, no hacen falta) y las dependencias
/// instaladas (los chequeos corren sin red: no podrían instalarlas).
pub fn make_ready(dir: &std::path::Path) -> Result<()> {
    let git = |args: &[&str], header: Option<&str>| {
        let mut cmd = std::process::Command::new("git");
        if let Some(h) = header {
            cmd.args(["-c", h]);
        }
        cmd.env("GIT_TERMINAL_PROMPT", "0").arg("-C").arg(dir).args(args).output()
    };
    if dir.join(".gitmodules").exists() {
        let listed = git(&["config", "-f", ".gitmodules", "--get-regexp", "path"], None)?;
        let paths: Vec<String> = String::from_utf8_lossy(&listed.stdout).lines().filter_map(|l| l.split_whitespace().nth(1)).filter(|p| p.starts_with("packages/") || p.starts_with("apps/")).filter(|p| !dir.join(p).join(".git").exists()).map(str::to_string).collect();
        if !paths.is_empty() {
            let mut last = String::new();
            let ok = git_auth_headers().iter().any(|h| {
                let mut args = vec!["submodule", "update", "--init", "--"];
                args.extend(paths.iter().map(String::as_str));
                match git(&args, h.as_deref()) {
                    Ok(o) if o.status.success() => true,
                    Ok(o) => {
                        last = devtool_core::redact(&String::from_utf8_lossy(&o.stderr)).chars().take(300).collect();
                        false
                    }
                    Err(_) => false,
                }
            });
            if !ok {
                bail!("couldn't fetch the submodules {} (a GitHub token needs Contents: read on them): {last}", paths.join(", "));
            }
            tracing::info!("site: submodules {} ready in {}", paths.join(", "), dir.display());
        }
    }
    // Las dependencias: sin instalar, o con otro lockfile que el de la última vez (otra rama trae otras).
    let lock = ["bun.lock", "bun.lockb", "package-lock.json"].iter().find_map(|f| std::fs::read(dir.join(f)).ok()).unwrap_or_default();
    let print = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        lock.hash(&mut h);
        format!("{:x}", h.finish())
    };
    let stamp = dir.join("node_modules/.condx-deps");
    let current = std::fs::read_to_string(&stamp).ok().is_some_and(|s| s.trim() == print);
    if dir.join("package.json").exists() && (!dir.join("node_modules").is_dir() || !current) {
        let bun = dir.join("bun.lock").exists() || dir.join("bun.lockb").exists();
        let (program, args): (&str, &[&str]) = if bun { ("bun", &["install"]) } else if dir.join("package-lock.json").exists() { ("npm", &["ci"]) } else { ("npm", &["install"]) };
        let out = std::process::Command::new(program).args(args).current_dir(dir).output().with_context(|| format!("couldn't run {program}"))?;
        if !out.status.success() {
            bail!("{program} {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).lines().rev().take(3).collect::<Vec<_>>().join(" | "));
        }
        let _ = std::fs::write(&stamp, &print);
        tracing::info!("site: dependencies installed in {}", dir.display());
    }
    Ok(())
}

/// `https://github.com/<dueño>/<repo>.git` tal como lo nombra hoy GitHub (`full_name`), si el token
/// (o ninguno, para los públicos) lo ve.
fn canonical_url(url: &str, token: Option<&str>) -> Option<String> {
    let path = url.trim_start_matches("https://github.com/").trim_end_matches(".git");
    let mut req = ureq::get(&format!("https://api.github.com/repos/{path}")).timeout(Duration::from_secs(15)).set("Accept", "application/vnd.github+json").set("User-Agent", "condx");
    if let Some(t) = token {
        req = req.set("Authorization", &format!("Bearer {}", t.trim()));
    }
    let v: Value = req.call().ok()?.into_json().ok()?;
    v["full_name"].as_str().map(|n| format!("https://github.com/{n}.git"))
}

/// La conversación hasta ahora (lo de la Mac y lo del sitio), como texto para el agente.
fn transcript(v: &Values) -> String {
    let list = |k: &str| v.get(k).and_then(Value::as_str).and_then(|t| serde_json::from_str::<Vec<Value>>(t).ok()).unwrap_or_default();
    let mut items = list("items");
    items.extend(list("siteItems"));
    items.sort_by_key(|i| i["at"].as_i64().unwrap_or(0));
    let start = items.len().saturating_sub(30);
    items[start..]
        .iter()
        .map(|i| format!("{}: {}", if i["kind"] == "user" { "User" } else { i["author"].as_str().unwrap_or("Agent") }, i["text"].as_str().unwrap_or("").chars().take(2000).collect::<String>()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Si el sitio se detiene: sin trabajo media hora, o dos minutos si la Mac está latiendo (ella atiende).
fn should_stop(now: i64, busy_at: i64, mac_alive_at: Option<i64>) -> bool {
    let idle = now - busy_at;
    let mac_here = mac_alive_at.is_some_and(|at| now - at < MAC_GONE_MS);
    idle >= IDLE_STOP_MS || (mac_here && idle >= HANDBACK_STOP_MS)
}

fn boat_api() -> String {
    std::env::var("CONDX_BOAT_API").unwrap_or_else(|_| "https://boat.dev/api/v1".into())
}

/// Una llamada a la API de boat con la key del sitio.
pub fn boat(key: &str, method: &str, path: &str) -> Result<Value> {
    let req = ureq::request(method, &format!("{}{path}", boat_api())).timeout(Duration::from_secs(20)).set("Authorization", &format!("Bearer {}", key.trim()));
    let reply = if method == "GET" { req.call() } else { req.send_json(json!({})) };
    match reply {
        Ok(r) => Ok(r.into_json().unwrap_or(Value::Null)),
        Err(ureq::Error::Status(code, r)) => bail!("boat answered {code}: {}", devtool_core::redact(&r.into_string().unwrap_or_default()).chars().take(200).collect::<String>()),
        Err(e) => bail!("couldn't reach boat: {e}"),
    }
}

/// Las claves que llegaron cifradas para el sitio, como proveedores propios (solo las que cambiaron);
/// las que la Mac quitó se borran.
fn apply_keys(state: &Shared, s: &Session) -> Result<usize> {
    use std::io::Read;
    let mut applied: IndexMap<String, Value> = state.store.probes()?.into_iter().find(|(k, _)| k == APPLIED_KEY).and_then(|(_, v)| serde_json::from_value(v).ok()).unwrap_or_default();
    let rows = s.engine.rows(&s.keys_scope);
    let mut changed = false;
    for r in &rows {
        let v = &r.row.values;
        let stamp = v.get("updatedAt").cloned().unwrap_or(Value::Null);
        if applied.get(&r.row.id) == Some(&stamp) {
            continue;
        }
        let Some(sealed) = v.get("sealed").and_then(Value::as_str) else { continue };
        let decryptor = age::Decryptor::new(age::armor::ArmoredReader::new(sealed.as_bytes())).map_err(|e| anyhow!("{e}"))?;
        let mut key = String::new();
        match decryptor.decrypt(std::iter::once(&s.age as &dyn age::Identity)) {
            Ok(mut reader) => reader.read_to_string(&mut key)?,
            // Todavía sellada sin el sitio (la Mac no lo confirmó o no volvió a cifrar): otra vuelta.
            Err(_) => continue,
        };
        let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        // Las variables de la app de un proyecto: a un archivo propio (solo el dueño lo lee).
        if text("kind").as_deref() == Some("app-env") {
            let project = text("project").unwrap_or_default();
            if !project.is_empty() && !project.contains('/') {
                let path = app_env_path(&project);
                std::fs::create_dir_all(path.parent().unwrap())?;
                std::fs::write(&path, &key)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
                }
                applied.insert(r.row.id.clone(), stamp);
                changed = true;
                tracing::info!("site: app variables received ({project})");
            }
            continue;
        }
        if text("kind").as_deref() == Some("convex-deploy") {
            let name = text("label").unwrap_or_default();
            let url = text("baseUrl");
            crate::secrets::set(&format!("deploy-key:{name}"), &key)?;
            state.store.upsert_environment(&devtool_core::Environment {
                name: name.clone(),
                kind: devtool_core::EnvKind::CloudDev,
                label: Some(format!("{name} · cloud dev")),
                convex_site_url: url.as_ref().map(|u| u.replace(".convex.cloud", ".convex.site")),
                convex_url: url,
                convex_deployment: text("model"),
                frontend_url: None,
                // La rama que va a ese cloud dev (la del entorno en la Mac): con ella levanta el sitio.
                branch: text("branch"),
                credential: None,
                created_at: now_ms(),
                project: text("project"),
            })?;
            applied.insert(r.row.id.clone(), stamp);
            changed = true;
            tracing::info!("site: cloud dev deploy key received ({name})");
            continue;
        }
        let body = crate::agents::ProviderBody { id: Some(r.row.id.clone()), kind: text("kind").unwrap_or_default(), label: text("label"), base_url: text("baseUrl"), model: text("model"), api_key: Some(key).filter(|k| !k.is_empty()), context_window: None };
        crate::agents::upsert_provider(state, body, &crate::agents::seal_for_mac)?;
        applied.insert(r.row.id.clone(), stamp);
        changed = true;
        tracing::info!("site: provider key received ({})", r.row.id);
    }
    let gone: Vec<String> = applied.keys().filter(|id| !rows.iter().any(|r| &r.row.id == *id)).cloned().collect();
    for id in gone {
        match id.strip_prefix("deploy:") {
            Some(env) => {
                let _ = crate::secrets::delete(&format!("deploy-key:{env}"));
            }
            None if id.starts_with("appenv:") => {
                let _ = std::fs::remove_file(app_env_path(id.trim_start_matches("appenv:")));
            }
            None => {
                let _ = crate::agents::delete_provider(state, &id);
            }
        }
        applied.shift_remove(&id);
        changed = true;
    }
    if changed {
        state.store.save_probe(APPLIED_KEY, &serde_json::to_value(&applied)?)?;
    }
    Ok(applied.len())
}

/// La configuración de agentes de la Mac (fila `_settings`), si cambió. Devuelve si hay una.
fn apply_settings(state: &Shared, s: &Session) -> Result<bool> {
    let Some(row) = s.engine.rows(&s.threads_scope).into_iter().find(|r| r.row.id == crate::cloud::SETTINGS_ROW) else { return Ok(false) };
    let Some(text) = row.row.values.get("settings").and_then(Value::as_str) else { return Ok(false) };
    let mut wanted: Value = serde_json::from_str(text)?;
    // Lo que la Mac suma para los teléfonos (los modelos del proveedor predeterminado) no es configuración
    // de los agentes; los avisos (ntfy) se copian aparte.
    let notify = wanted.as_object_mut().and_then(|o| {
        o.remove("defaultModels");
        o.remove("notify")
    });
    if let Some(n) = notify.filter(|n| n.is_object()) {
        if serde_json::to_value(crate::notify::settings(state))? != n {
            crate::notify::save(state, &n)?;
            tracing::info!("site: notification settings copied from the Mac");
        }
    }
    if serde_json::to_value(crate::agents::settings(state))? != wanted {
        crate::agents::save_settings(state, &wanted)?;
        tracing::info!("site: agent settings copied from the Mac");
    }
    Ok(true)
}

/// Los flujos de la Mac (fila `_flows`): los mismos en el sitio, con el mismo id. Los de un proyecto
/// que la Mac ya no tiene se quitan (incluidos los que el sitio sembró por su cuenta al clonar).
fn apply_flows(state: &Shared, s: &Session) -> Result<()> {
    let Some(text) = s.engine.rows(&s.threads_scope).into_iter().find(|r| r.row.id == crate::cloud::FLOWS_ROW).and_then(|r| r.row.values.get("flows").and_then(Value::as_str).map(str::to_string)) else { return Ok(()) };
    static LAST: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
    if *LAST.lock().unwrap() == text {
        return Ok(());
    }
    let flows: Vec<Value> = serde_json::from_str(&text)?;
    let mut by_project: std::collections::HashMap<String, Vec<String>> = Default::default();
    for w in &flows {
        let (Some(id), Some(project)) = (w["id"].as_str(), w["project"].as_str()) else { continue };
        if state.store.workflow(id)?.as_ref() != Some(w) {
            state.store.save_workflow(id, project, w)?;
        }
        by_project.entry(project.to_string()).or_default().push(id.to_string());
    }
    for (project, ids) in &by_project {
        for old in state.store.workflows(project)? {
            if let Some(id) = old["id"].as_str().filter(|id| !ids.iter().any(|x| x == id)) {
                state.store.remove_workflow(id)?;
            }
        }
    }
    tracing::info!("site: {} flows copied from the Mac", flows.len());
    *LAST.lock().unwrap() = text;
    Ok(())
}

/// El hilo del sitio: espera la confirmación de la Mac y después sincroniza las claves.
pub fn spawn(state: Shared) {
    let rt = tokio::runtime::Handle::current();
    std::thread::Builder::new()
        .name("condx-site".into())
        .spawn(move || {
            let mut session: Option<Session> = None;
            loop {
                let wait = match load() {
                    None => {
                        state.set_probe("site", false, Value::Null, Some("not paired: condxd site pair \"<the QR's text>\"".into()));
                        Duration::from_secs(10)
                    }
                    Some(id) if session.is_none() => match status(&id) {
                        Ok(st) if st == "active" => {
                            match open(&id, rt.clone()) {
                                Ok(s) => session = Some(s),
                                Err(e) => state.set_probe("site", false, Value::Null, Some(format!("{e:#}"))),
                            }
                            Duration::ZERO
                        }
                        Ok(st) => {
                            state.set_probe("site", false, json!({ "status": st, "fingerprint": fingerprint(&id) }), Some(format!("waiting for the Mac: confirm “{}” ({}) in Condx → Settings → Condx Cloud", id.name, fingerprint(&id))));
                            Duration::from_secs(5)
                        }
                        Err(e) => {
                            state.set_probe("site", false, Value::Null, Some(format!("{e:#}")));
                            Duration::from_secs(15)
                        }
                    },
                    Some(_) => {
                        let s = session.as_mut().unwrap();
                        match tick(&state, s) {
                            Ok(data) => state.set_probe("site", true, data, None),
                            Err(e) => state.set_probe("site", false, Value::Null, Some(format!("{e:#}"))),
                        }
                        // Lo que cambie en la nube despierta enseguida (el timbre en vivo); contestando, el
                        // avance del agente se publica cada segundo y medio.
                        // Un servicio que se levanta o se acaba de pedir: cada 3 s, para que el teléfono lo vea.
                        let services_moving = state.services.busy_any() || now_ms() - s.service_asked_at < 90_000;
                        s.bell.wait(if !s.answering.is_empty() { Duration::from_millis(1500) } else if services_moving { Duration::from_secs(3) } else { Duration::from_secs(30) });
                        Duration::ZERO
                    }
                };
                if !wait.is_zero() {
                    std::thread::sleep(wait);
                }
            }
        })
        .expect("couldn't start the site thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_conversacion_junta_lo_de_la_mac_y_lo_del_sitio_en_orden() {
        let mut v = Values::new();
        v.insert("items".into(), json!(json!([{ "kind": "user", "at": 1, "text": "hola" }, { "kind": "reply", "author": "Agent", "at": 2, "text": "qué tal" }]).to_string()));
        v.insert("siteItems".into(), json!(json!([{ "kind": "user", "at": 3, "text": "sigues?" }, { "kind": "reply", "author": "Cloud site", "at": 4, "text": "sí" }]).to_string()));
        assert_eq!(transcript(&v), "User: hola\nAgent: qué tal\nUser: sigues?\nCloud site: sí");
        assert_eq!(transcript(&Values::new()), "");
    }

    #[test]
    fn se_detiene_sin_trabajo_o_cuando_la_mac_volvio() {
        let now = 100 * 60_000;
        // Con trabajo reciente y sin Mac: sigue.
        assert!(!should_stop(now, now - 60_000, None));
        // Media hora sin trabajo: se detiene.
        assert!(should_stop(now, now - IDLE_STOP_MS, None));
        // La Mac late (hace 1 min) y lleva 2 min sin trabajo: se detiene.
        assert!(should_stop(now, now - HANDBACK_STOP_MS, Some(now - 60_000)));
        // El latido es viejo (Mac apagada): espera la media hora.
        assert!(!should_stop(now, now - HANDBACK_STOP_MS, Some(now - MAC_GONE_MS)));
    }

    #[test]
    fn lee_el_qr_de_la_mac() {
        let (u, c) = parse_pairing("https://condx.example.com/pair#d=https%3A%2F%2Fx.convex.cloud&c=ABCD-1234", None).unwrap();
        assert_eq!((u.as_str(), c.as_str()), ("https://x.convex.cloud", "ABCD-1234"));
        let (u, c) = parse_pairing(r#"{"url":"https://x.convex.cloud","code":"Z9"}"#, None).unwrap();
        assert_eq!((u.as_str(), c.as_str()), ("https://x.convex.cloud", "Z9"));
        assert!(parse_pairing("Z9", None).is_err());
        assert_eq!(parse_pairing("Z9", Some("https://x.convex.cloud")).unwrap().1, "Z9");
    }
}
