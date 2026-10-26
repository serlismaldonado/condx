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
/// Un latido de la Mac más viejo que esto: la Mac está apagada (late cada 5 min).
const MAC_GONE_MS: i64 = 12 * 60_000;

/// Si este daemon corre como sitio en la nube (`serve --site cloud`).
pub fn is_cloud() -> bool {
    std::env::var("CONDX_SITE").is_ok_and(|s| s == "cloud")
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
}

/// Un mensaje de teléfono que el sitio está contestando.
struct Answering {
    row: String,
    request_id: String,
    user: Value,
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
    Ok(Session { engine, transport: ConvexHttpTransport::new(&id.url, &id.token), keys_scope, threads_scope, age, bell, rung_at: 0, busy_at: now_ms(), stopping: false, rt, answering: IndexMap::new(), taken: Default::default(), cloning: Default::default() })
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
    let mac_alive_at = s.engine.rows(&s.threads_scope).into_iter().find(|r| r.row.id == crate::cloud::MAC_ROW).and_then(|r| r.row.values.get("aliveAt").and_then(Value::as_i64));
    let mac_away = !mac_alive_at.is_some_and(|at| now - at < MAC_GONE_MS);
    clone_projects(state, s);
    answer_threads(state, s, mac_away);
    // ¿Detenerse? Con trabajo (agentes o trabajos en marcha) no; sin trabajo un rato, sí.
    let busy = !state.store.agent_runs(&["running"], None)?.is_empty() || !state.store.jobs(&["running", "queued"], 1)?.is_empty();
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
        if r.pending || answered || (!mine && !mac_away) || (id.starts_with('_') && id != crate::cloud::NEW_ROW) {
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
        let mut text = req.arg.trim().to_string();
        if req.files.is_some() {
            text.push_str("\n\n(The phone sent attachments; they're sealed for the Mac and can't be opened here.)");
        }
        let prompt = match transcript(v) {
            t if t.is_empty() => text.clone(),
            t => format!("(Earlier in this conversation:)\n{t}\n\n(New message from {}:)\n{text}", req.device.name),
        };
        let project = v.get("project").and_then(Value::as_str).map(str::to_string);
        let _rt = s.rt.enter();
        match s.rt.block_on(crate::agent_host::start_chat(state, &prompt, project.as_deref(), &format!("phone:{}", req.device.name))) {
            Ok(run) => {
                let at = v.get("requestedAt").and_then(Value::as_i64).unwrap_or_else(now_ms);
                let user = json!({ "kind": "user", "author": null, "at": at, "text": req.arg.trim() });
                tracing::info!("site: answering a phone message on {id}");
                s.answering.insert(run["id"].as_str().unwrap_or_default().to_string(), Answering { row: id.to_string(), request_id: request_id.to_string(), user });
            }
            Err(e) => tracing::warn!("site: couldn't start an agent for {id}: {e:#}"),
        }
    }
    // Las corridas que terminaron: la respuesta a su hilo.
    let done: Vec<(String, Value)> = s.answering.keys().filter_map(|run_id| state.store.agent_run(run_id).ok().flatten().filter(|r| r["status"] != "running").map(|r| (run_id.clone(), r))).collect();
    for (run_id, run) in done {
        let Some(a) = s.answering.shift_remove(&run_id) else { continue };
        let text = match run["status"].as_str() {
            Some("done") => run["answer"].as_str().unwrap_or("").trim().to_string(),
            Some(st) => format!("The agent {st}: {}", run["error"].as_str().unwrap_or("no details")),
            None => continue,
        };
        let reply = json!({ "kind": "reply", "author": "Cloud site", "at": now_ms(), "text": text });
        if let Err(e) = write_answer(s, &a, reply) {
            tracing::warn!("site: couldn't write the answer on {}: {e:#}", a.row);
        }
    }
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
        if have.contains(&name) || !url.starts_with("https://github.com/") {
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

fn clone_project(state: &Shared, name: &str, url: &str, branch: &str) -> Result<()> {
    let dir = home().join("projects").join(name);
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

/// Escribe la respuesta: en un hilo del sitio, como la Mac en los suyos; en un hilo de la Mac, aparte
/// (`siteItems`, `siteFor`); un hilo nuevo se crea como `site-…` y `_new` dice cuál.
fn write_answer(s: &mut Session, a: &Answering, reply: Value) -> Result<()> {
    let now = now_ms();
    let rows = s.engine.rows(&s.threads_scope);
    let row = rows.iter().find(|r| r.row.id == a.row).map(|r| r.row.values.clone()).unwrap_or_default();
    let list = |k: &str| row.get(k).and_then(Value::as_str).and_then(|t| serde_json::from_str::<Vec<Value>>(t).ok()).unwrap_or_default();
    let mut set = Values::new();
    let drafts = if a.row == crate::cloud::NEW_ROW {
        let id = format!("site-{}", crate::cloud::random_hex(6)?);
        let title: String = a.user["text"].as_str().unwrap_or("Chat").chars().take(60).collect();
        let participants = json!([{ "node": "agent", "name": "Cloud site", "logo": "minimax" }]).to_string();
        let mut new = Values::new();
        for (k, v) in [("title", json!(title)), ("kind", json!("chat")), ("status", json!("done")), ("updatedAt", json!(now)), ("items", json!(json!([a.user, reply]).to_string())), ("participants", json!(participants)), ("waitingAnswer", json!(false))] {
            new.insert(k.into(), v);
        }
        set.insert("siteFor".into(), json!(a.request_id));
        set.insert("siteResult".into(), json!(format!("thread {id}")));
        vec![OpDraft::create(&s.threads_scope, new).with_row(&id), OpDraft::patch(&s.threads_scope, crate::cloud::NEW_ROW, set)]
    } else if a.row.starts_with("site-") {
        let mut items = list("items");
        items.extend([a.user.clone(), reply]);
        set.insert("items".into(), json!(Value::Array(items).to_string()));
        set.insert("status".into(), json!("done"));
        set.insert("updatedAt".into(), json!(now));
        set.insert("result".into(), json!("sent to Cloud site"));
        set.insert("resultFor".into(), json!(a.request_id));
        vec![OpDraft::patch(&s.threads_scope, &a.row, set)]
    } else {
        let mut items = list("siteItems");
        items.extend([a.user.clone(), reply]);
        set.insert("siteItems".into(), json!(Value::Array(items).to_string()));
        set.insert("siteFor".into(), json!(a.request_id));
        vec![OpDraft::patch(&s.threads_scope, &a.row, set)]
    };
    s.engine.commit(drafts, now).map_err(|e| anyhow!("{e}"))?;
    Ok(())
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
        let body = crate::agents::ProviderBody { id: Some(r.row.id.clone()), kind: text("kind").unwrap_or_default(), label: text("label"), base_url: text("baseUrl"), model: text("model"), api_key: Some(key).filter(|k| !k.is_empty()) };
        crate::agents::upsert_provider(state, body, &crate::agents::seal_for_mac)?;
        applied.insert(r.row.id.clone(), stamp);
        changed = true;
        tracing::info!("site: provider key received ({})", r.row.id);
    }
    let gone: Vec<String> = applied.keys().filter(|id| !rows.iter().any(|r| &r.row.id == *id)).cloned().collect();
    for id in gone {
        let _ = crate::agents::delete_provider(state, &id);
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
    let wanted: Value = serde_json::from_str(text)?;
    if serde_json::to_value(crate::agents::settings(state))? != wanted {
        crate::agents::save_settings(state, &wanted)?;
        tracing::info!("site: agent settings copied from the Mac");
    }
    Ok(true)
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
                        // Lo que cambie en la nube despierta enseguida (el timbre en vivo).
                        s.bell.wait(Duration::from_secs(30));
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
        // La Mac late (hace 3 min) y lleva 2 min sin trabajo: se detiene.
        assert!(should_stop(now, now - HANDBACK_STOP_MS, Some(now - 3 * 60_000)));
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
