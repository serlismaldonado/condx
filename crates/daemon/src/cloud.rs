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
const RECENT_JOBS: usize = 50;
/// Campos que el daemon escribe en cada alcance (los demás son de otros: el teléfono).
const ENV_FIELDS: &[&str] = &["name", "kind", "project", "deployment", "reachable", "checkedAt"];
const JOB_FIELDS: &[&str] = &["recipe", "title", "env", "envKind", "status", "approval", "requestedBy", "createdAt", "endedAt", "error", "commit"];
/// `phoneResult`: qué hizo el daemon con la decisión de un teléfono (aplicada, rechazada y por qué, o
/// a la espera de Touch ID en la Mac).
const APPROVAL_FIELDS: &[&str] = &["recipe", "title", "env", "envKind", "status", "approval", "requestedBy", "createdAt", "phoneResult"];

/// El vínculo con un deployment de Condx Cloud (`cloud.json`). El token va aparte.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    pub url: String,
    pub daemon_id: String,
    pub name: String,
    pub linked_at: i64,
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

fn random_hex(bytes: usize) -> Result<String> {
    use std::io::Read;
    let mut buf = vec![0u8; bytes];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn write_private(path: &std::path::Path, text: &str) -> Result<()> {
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
    let link = Link { url, daemon_id, name: name.to_string(), linked_at: now_ms() };
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

/// Filas por alcance a partir del estado del daemon. Puro dado lo leído: sin red ni reloj.
pub fn desired(daemon_id: &str, environments: &[devtool_core::Environment], probes: &dyn Fn(&str) -> Option<Value>, jobs: &[Value]) -> Desired {
    let mut out: Desired = IndexMap::new();
    let env: IndexMap<String, Values> = environments
        .iter()
        .map(|e| {
            let probe = probes(&format!("convex:{}", e.name));
            let reachable = probe.as_ref().map(|p| p["data"]["reachable"] == true || (p["ok"] == true && p["data"]["reachable"].is_null()));
            let kind = serde_json::to_value(e.kind).unwrap_or(Value::Null);
            (e.name.clone(), obj(json!({ "name": e.name, "kind": kind, "project": e.project, "deployment": e.convex_deployment, "reachable": reachable, "checkedAt": probe.as_ref().map(|p| p["at"].clone()) })))
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
            "createdAt": job["createdAt"], "endedAt": job["endedAt"], "error": short_error(&job["error"]), "commit": job["commit"],
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
                    let (now, before) = (values.get(*f), have.get(*f));
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
        if want.contains_key(id) {
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

/// Abre un código de vinculación. Lo que va en el QR: la URL del deployment y el código.
pub fn pair_start() -> Result<Value> {
    let link = load_link().context("not linked to Condx Cloud")?;
    let v = call("mutation", "pairing:open", json!({}))?;
    let code = v["code"].as_str().unwrap_or_default().to_string();
    let qr = json!({ "condx": 1, "url": link.url, "code": code }).to_string();
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
    call("mutation", "pairing:confirm", json!({ "deviceId": device_id }))?;
    let trusted = TrustedDevice { device_id: device_id.to_string(), name: device["name"].as_str().unwrap_or("Phone").to_string(), public_key, paired_at: now_ms() };
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

type CloudEngine = Engine<sqlite::SqliteReplicaStore, sqlite::SqliteOutboxStore>;

fn block_on<F: std::future::Future>(future: F) -> F::Output {
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
}

fn open_session(link: Link, token: &str) -> Result<Session> {
    let (replica, outbox) = sqlite::open(&home().join("cloud.sqlite3")).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut n = 0u64;
    let prefix = random_hex(4)?;
    let mut engine = Engine::open(replica, outbox, EngineConfig::default(), move || {
        n += 1;
        format!("{prefix}-{}-{n}", now_ms())
    })
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    let scopes: Vec<String> = ["env", "jobs", "approvals"].iter().map(|k| format!("{k}:{}", link.daemon_id)).collect();
    for s in &scopes {
        engine.enable_scope(s, None).map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    Ok(Session { transport: ConvexHttpTransport::new(&link.url, token), link, engine, scopes, results: IndexMap::new(), seen: Default::default() })
}

/// Una vuelta: confirmar las diferencias, timbre, sincronizar. Devuelve el estado para la sonda.
fn tick(state: &Shared, s: &mut Session) -> Result<Value> {
    let environments = state.store.environments().unwrap_or_default();
    let jobs = state.store.jobs(&[], RECENT_JOBS * 2).unwrap_or_default();
    let mut want = desired(&s.link.daemon_id, &environments, &|k| state.probe(k), &jobs);
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
    }
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
    match s.transport.doorbell(&s.scopes, localfirst_core::PROTOCOL_VERSION) {
        Ok(Doorbell::Versions(v)) => s.engine.on_doorbell(&v, now).map_err(|e| anyhow::anyhow!("{e}"))?,
        Ok(Doorbell::UpgradeRequired) => error = Some("Condx Cloud needs a newer Condx".to_string()),
        Err(e) => error = Some(e.to_string()),
    }
    let report = block_on(sync_once(&mut s.engine, &s.transport, now_ms, 200)).map_err(|e| anyhow::anyhow!("{e}"))?;
    process_decisions(state, s);
    let st = s.engine.sync_status();
    let proposals: Vec<Value> = s.results.iter().filter(|(_, r)| r.as_str() == NEEDS_MAC).map(|(j, _)| json!(j)).collect();
    Ok(json!({
        "url": s.link.url, "daemonId": s.link.daemon_id, "name": s.link.name,
        "pending": st.pending, "recovery": st.recovery, "paused": st.paused,
        "pushes": report.pushes, "pulls": report.pulls, "error": error, "phoneProposals": proposals,
        "scopes": s.scopes.iter().map(|sc| json!({ "scope": sc, "rows": s.engine.rows(sc).len(), "cursor": s.engine.scope_state(sc).and_then(|x| x.cursor) })).collect::<Vec<_>>(),
    }))
}

/// Arranca el hilo. Sin vínculo, solo mira cada tanto si apareció uno.
pub fn spawn(state: Shared) {
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
                            session = match open_session(link, &token) {
                                Ok(s) => Some(s),
                                Err(e) => {
                                    state.set_probe("cloud", false, Value::Null, Some(format!("{e:#}")));
                                    None
                                }
                            };
                        }
                        if let Some(s) = session.as_mut() {
                            match tick(&state, s) {
                                Ok(data) => {
                                    let ok = data["error"].is_null();
                                    let err = data["error"].as_str().map(str::to_string);
                                    state.set_probe("cloud", ok, data, err);
                                }
                                Err(e) => state.set_probe("cloud", false, Value::Null, Some(devtool_core::redact(&format!("{e:#}")))),
                            }
                        }
                    }
                    _ => {
                        if session.take().is_some() || state.probe("cloud").is_some_and(|p| !p["data"].is_null()) {
                            state.set_probe("cloud", true, Value::Null, None);
                        }
                    }
                }
                std::thread::sleep(TICK);
            }
        })
        .expect("couldn't start the cloud thread");
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
    }

    #[test]
    fn decisiones_del_telefono() {
        use ed25519_dalek::{Signer, SigningKey};
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let pk = key.verifying_key().as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>();
        let device = TrustedDevice { device_id: "phone-1".into(), name: "iPhone".into(), public_key: pk.clone(), paired_at: 0 };
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
    fn huella_corta() {
        assert_eq!(fingerprint("abcdef0123456789"), "abcd ef01 2345");
    }
}
