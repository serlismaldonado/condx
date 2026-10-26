// El sitio en la nube (docs/design/cloud-site.md, fase S1): el mismo `condxd` en Linux, en una VM
// chica, que sigue trabajando con la Mac apagada. Se empareja como un teléfono: su propia llave
// (ed25519) y su identidad de cifrado (age), un código de la Mac y la confirmación en la Mac. Con eso
// le llegan las claves de los proveedores cifradas solo para él (y para la Mac y los teléfonos), y el
// sitio las guarda como proveedores propios, cifrados con su `agent-key`. No tiene producción ni
// Llavero: los secretos van en `secrets.json` (0600) y la identidad en `site.json` (0600).
//
// `condxd site pair "<texto o enlace del QR>"` reclama el código; `condxd serve --site cloud`
// arranca el daemon en modo sitio y sincroniza las claves (con el timbre en vivo).

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use ed25519_dalek::SigningKey;
use indexmap::IndexMap;
use localfirst_core::http::{ConvexHttpTransport, Doorbell};
use localfirst_core::{sqlite, sync_once, Engine, EngineConfig};
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
}

fn path() -> std::path::PathBuf {
    home().join("site.json")
}

pub fn load() -> Option<Identity> {
    std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str(&t).ok())
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
    let v = call(&url, "mutation", "pairing:claim", json!({ "code": code, "publicKey": hex(key.verifying_key().as_bytes()), "token": token, "name": name, "encryptionKey": enc.to_public().to_string(), "kind": "site" }))?;
    let id = Identity {
        url: url.trim_end_matches('/').to_string(),
        daemon_id: v["daemonId"].as_str().context("unexpected answer")?.to_string(),
        device_id: v["deviceId"].as_str().context("unexpected answer")?.to_string(),
        name: name.to_string(),
        token,
        seed: hex(&seed),
        age: enc.to_string().expose_secret().to_string(),
    };
    std::fs::create_dir_all(home())?;
    crate::cloud::write_private(&path(), &serde_json::to_string_pretty(&id)?)?;
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
}

fn open(id: &Identity) -> Result<Session> {
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
    Ok(Session { engine, transport: ConvexHttpTransport::new(&id.url, &id.token), keys_scope, threads_scope, age, bell, rung_at: 0 })
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
    Ok(json!({ "liveBell": s.bell.is_live(), "keys": applied, "settingsFromMac": settings }))
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
                            match open(&id) {
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
    fn lee_el_qr_de_la_mac() {
        let (u, c) = parse_pairing("https://condx.example.com/pair#d=https%3A%2F%2Fx.convex.cloud&c=ABCD-1234", None).unwrap();
        assert_eq!((u.as_str(), c.as_str()), ("https://x.convex.cloud", "ABCD-1234"));
        let (u, c) = parse_pairing(r#"{"url":"https://x.convex.cloud","code":"Z9"}"#, None).unwrap();
        assert_eq!((u.as_str(), c.as_str()), ("https://x.convex.cloud", "Z9"));
        assert!(parse_pairing("Z9", None).is_err());
        assert_eq!(parse_pairing("Z9", Some("https://x.convex.cloud")).unwrap().1, "Z9");
    }
}
