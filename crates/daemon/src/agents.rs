// Configuración de los agentes (docs/design/agents.md §5-6), que solo cambia la app:
// - proveedores de modelos (los de pi-ai): tipo, nombre, URL base, modelo y la API key cifrada con
//   age a la identidad `agent-key` del Llavero; la clave cifrada vive en el SQLite del daemon y la
//   app solo ve una pista (`…abcd`). age admite varios destinatarios: un teléfono o un runner remoto
//   con su propia llave podrán recibir su copia (fase A6) sin que la nube vea la clave.
// - autonomía por fuente de incidentes (off, notify, diagnose, fix), proveedor y modelo por
//   defecto, presupuesto diario de tokens y cuántos agentes a la vez.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::str::FromStr;

use age::secrecy::ExposeSecret;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::state::Shared;

/// La identidad age que cifra las API keys (en el Llavero, servicio `condx`).
pub const AGENT_KEY: &str = "agent-key";
const SETTINGS_KEY: &str = "meta:agents";

/// Fuentes de incidentes (incidents.rs).
pub const SOURCES: [&str; 4] = ["job", "vercel", "ci", "convex"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Level {
    /// No se registra.
    Off,
    /// Se registra y se avisa.
    #[default]
    Notify,
    /// Un agente lee y explica la causa.
    Diagnose,
    /// Prepara un fix en una rama y pide aprobación para desplegar.
    Fix,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub autonomy: BTreeMap<String, Level>,
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    /// Tokens por día entre todos los agentes (0: sin agentes).
    pub daily_token_budget: u64,
    pub max_concurrent: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            autonomy: SOURCES.iter().map(|s| (s.to_string(), Level::Notify)).collect(),
            default_provider: None,
            default_model: None,
            daily_token_budget: 500_000,
            max_concurrent: 1,
        }
    }
}

impl Settings {
    pub fn level(&self, source: &str) -> Level {
        self.autonomy.get(source).copied().unwrap_or_default()
    }
}

pub fn settings(state: &Shared) -> Settings {
    let stored = state.store.probes().ok().and_then(|l| l.into_iter().find(|(k, _)| k == SETTINGS_KEY)).map(|(_, v)| v);
    let mut s = Settings::default();
    if let Some(v) = stored.and_then(|v| serde_json::from_value::<Settings>(v).ok()) {
        // Las fuentes que falten quedan en `notify`.
        let mut autonomy = s.autonomy.clone();
        autonomy.extend(v.autonomy);
        s = Settings { autonomy, ..v };
    }
    s
}

pub fn save_settings(state: &Shared, body: &Value) -> Result<Settings> {
    let given: Settings = serde_json::from_value(body.clone()).map_err(|e| anyhow!("invalid agent settings: {e}"))?;
    for k in given.autonomy.keys() {
        if !SOURCES.contains(&k.as_str()) {
            bail!("unknown incident source: {k}");
        }
    }
    if let Some(p) = &given.default_provider {
        if !state.store.agent_providers()?.iter().any(|v| v["id"] == p.as_str()) {
            bail!("there's no provider “{p}”");
        }
    }
    if given.max_concurrent == 0 || given.max_concurrent > 8 {
        bail!("agents at once: between 1 and 8");
    }
    let mut s = Settings::default();
    s.autonomy.extend(given.autonomy);
    let s = Settings { autonomy: s.autonomy, ..given };
    state.store.save_probe(SETTINGS_KEY, &serde_json::to_value(&s)?)?;
    tracing::info!("agent settings changed: {}", serde_json::to_string(&s).unwrap_or_default());
    Ok(s)
}

/// Un tipo de proveedor de pi-ai: id, nombre, si pide URL base y un modelo sugerido.
pub struct Kind {
    pub id: &'static str,
    pub label: &'static str,
    pub needs_base_url: bool,
    pub model: &'static str,
}

pub const KINDS: &[Kind] = &[
    Kind { id: "anthropic", label: "Anthropic", needs_base_url: false, model: "claude-sonnet-5-5" },
    Kind { id: "openai", label: "OpenAI", needs_base_url: false, model: "gpt-5" },
    Kind { id: "google", label: "Google Gemini", needs_base_url: false, model: "gemini-2.5-pro" },
    Kind { id: "openrouter", label: "OpenRouter", needs_base_url: false, model: "anthropic/claude-sonnet-5.5" },
    Kind { id: "xai", label: "xAI", needs_base_url: false, model: "grok-4" },
    Kind { id: "groq", label: "Groq", needs_base_url: false, model: "llama-3.3-70b-versatile" },
    Kind { id: "mistral", label: "Mistral", needs_base_url: false, model: "mistral-large-latest" },
    Kind { id: "openai-compatible", label: "OpenAI-compatible (Ollama, vLLM…)", needs_base_url: true, model: "" },
    Kind { id: "anthropic-compatible", label: "Anthropic-compatible (MiniMax…)", needs_base_url: true, model: "" },
];

fn kind(id: &str) -> Option<&'static Kind> {
    KINDS.iter().find(|k| k.id == id)
}

/// Cifra `plain` con age para estos destinatarios (texto armado, para guardarlo en SQLite).
pub fn seal(plain: &str, recipients: &[age::x25519::Recipient]) -> Result<String> {
    let encryptor = age::Encryptor::with_recipients(recipients.iter().map(|r| r as &dyn age::Recipient)).map_err(|e| anyhow!("age: {e}"))?;
    let mut out = vec![];
    let armor = age::armor::ArmoredWriter::wrap_output(&mut out, age::armor::Format::AsciiArmor)?;
    let mut writer = encryptor.wrap_output(armor)?;
    writer.write_all(plain.as_bytes())?;
    writer.finish()?.finish()?;
    Ok(String::from_utf8(out)?)
}

/// Descifra lo que `seal` cifró, con una identidad que esté entre sus destinatarios.
pub fn open(sealed: &str, identity: &age::x25519::Identity) -> Result<String> {
    let reader = age::armor::ArmoredReader::new(sealed.as_bytes());
    let decryptor = age::Decryptor::new(reader).map_err(|e| anyhow!("age: {e}"))?;
    let mut reader = decryptor.decrypt(std::iter::once(identity as &dyn age::Identity)).map_err(|e| anyhow!("couldn't decrypt the key: {e}"))?;
    let mut plain = String::new();
    reader.read_to_string(&mut plain)?;
    Ok(plain)
}

/// La marca del proveedor, para mostrar su logo en los hilos (app y teléfono): por su tipo y, en los
/// compatibles, por la URL o el nombre (Ollama, MiniMax…).
pub fn logo(provider: &Value) -> &'static str {
    let base = provider["baseUrl"].as_str().unwrap_or("").to_lowercase();
    let label = provider["label"].as_str().unwrap_or("").to_lowercase();
    let model = provider["model"].as_str().unwrap_or("").to_lowercase();
    let any = |w: &str| base.contains(w) || label.contains(w) || model.contains(w);
    match provider["kind"].as_str().unwrap_or("") {
        "anthropic" => "claude",
        "openai" => "openai",
        "google" => "gemini",
        "xai" => "x",
        "groq" => "groq",
        "mistral" => "mistral",
        "openrouter" => "openrouter",
        _ if any("minimax") => "minimax",
        _ if any("ollama") || base.contains(":11434") => "ollama",
        _ if any("deepseek") => "deepseek",
        _ if any("claude") || any("anthropic") => "claude",
        _ if any("gpt") || any("openai") => "openai",
        _ if any("gemini") => "gemini",
        _ => "other",
    }
}

/// La marca del proveedor con que corre un agente (el suyo o el predeterminado).
pub fn logo_for(state: &Shared, provider_id: Option<&str>) -> &'static str {
    let id = provider_id.filter(|p| !p.is_empty()).map(str::to_string).or_else(|| settings(state).default_provider);
    let Some(id) = id else { return "other" };
    state.store.agent_providers().ok().and_then(|ps| ps.into_iter().find(|p| p["id"] == id.as_str())).map(|p| logo(&p)).unwrap_or("other")
}

/// Descifra bytes cifrados con age para la Mac (los adjuntos que manda el teléfono).
pub fn open_bytes(sealed: &[u8]) -> Result<Vec<u8>> {
    let id = identity()?;
    let decryptor = age::Decryptor::new(sealed).map_err(|e| anyhow!("age: {e}"))?;
    let mut reader = decryptor.decrypt(std::iter::once(&id as &dyn age::Identity)).map_err(|e| anyhow!("couldn't decrypt the attachment: {e}"))?;
    let mut plain = vec![];
    reader.read_to_end(&mut plain)?;
    Ok(plain)
}

/// La identidad de la Mac (se crea la primera vez que se guarda una clave). La que quedó en el
/// Llavero se lee una vez y pasa al archivo: con ella están cifradas las claves de los proveedores.
fn identity() -> Result<age::x25519::Identity> {
    if let Some(raw) = crate::secrets::get(AGENT_KEY)? {
        return age::x25519::Identity::from_str(raw.trim()).map_err(|e| anyhow!("invalid agent key in the Keychain: {e}"));
    }
    let id = age::x25519::Identity::generate();
    crate::secrets::set(AGENT_KEY, id.to_string().expose_secret())?;
    Ok(id)
}

/// Lo que la app y el MCP ven de un proveedor: sin la clave cifrada.
pub fn public(p: &Value) -> Value {
    let mut v = p.clone();
    if let Some(o) = v.as_object_mut() {
        o.remove("sealedKey");
        o.insert("logo".into(), json!(logo(p)));
    }
    v
}

fn hint(key: &str) -> String {
    let tail: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("…{tail}")
}

fn slug(text: &str) -> String {
    let s: String = text.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderBody {
    pub id: Option<String>,
    pub kind: String,
    pub label: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    /// Solo al crear o al cambiarla; nunca vuelve.
    pub api_key: Option<String>,
}

/// Crea o cambia un proveedor. `seal_to` cifra la clave (la identidad de la Mac fuera de las pruebas).
pub fn upsert_provider(state: &Shared, body: ProviderBody, seal_to: &dyn Fn(&str) -> Result<String>) -> Result<Value> {
    let k = kind(&body.kind).with_context(|| format!("unknown provider kind “{}”", body.kind))?;
    let base_url = body.base_url.map(|u| u.trim().trim_end_matches('/').to_string()).filter(|u| !u.is_empty());
    if k.needs_base_url && base_url.is_none() {
        bail!("{} needs a base URL", k.label);
    }
    if let Some(u) = &base_url {
        if !(u.starts_with("https://") || u.starts_with("http://")) {
            bail!("the base URL must start with https:// or http://");
        }
    }
    let label = body.label.map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).unwrap_or_else(|| k.label.to_string());
    let id = body.id.clone().filter(|i| !i.is_empty()).unwrap_or_else(|| slug(&label));
    let existing = state.store.agent_providers()?.into_iter().find(|p| p["id"] == id.as_str());
    if body.id.is_none() && existing.is_some() {
        bail!("there's already a provider called “{label}”");
    }
    let key = body.api_key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty());
    let (sealed, key_hint) = match (&key, &existing) {
        (Some(k), _) => (json!(seal_to(k)?), json!(hint(k))),
        (None, Some(e)) => (e["sealedKey"].clone(), e["keyHint"].clone()),
        // Un endpoint compatible local puede no pedir clave.
        (None, None) if k.needs_base_url => (Value::Null, Value::Null),
        (None, None) => bail!("{} needs an API key", k.label),
    };
    let now = crate::store::now_ms();
    let p = json!({
        "id": id,
        "kind": k.id,
        "label": label,
        "baseUrl": base_url,
        "model": body.model.map(|m| m.trim().to_string()).filter(|m| !m.is_empty()).unwrap_or_else(|| k.model.to_string()),
        "sealedKey": sealed,
        "keyHint": key_hint,
        "createdAt": existing.as_ref().map(|e| e["createdAt"].clone()).unwrap_or(json!(now)),
        "updatedAt": now,
    });
    state.store.save_agent_provider(&p)?;
    tracing::info!("agent provider saved: {} ({})", id, k.id);
    Ok(public(&p))
}

/// El destinatario de la Mac (para cifrar también a los teléfonos, fase A6).
pub fn mac_recipient() -> Result<age::x25519::Recipient> {
    Ok(identity()?.to_public())
}

pub fn seal_for_mac(plain: &str) -> Result<String> {
    seal(plain, &[identity()?.to_public()])
}

/// La API key de un proveedor, descifrada (para arrancar un agente; nunca va a la app).
pub fn api_key(state: &Shared, id: &str) -> Result<Option<String>> {
    let p = state.store.agent_providers()?.into_iter().find(|p| p["id"] == id).with_context(|| format!("there's no provider “{id}”"))?;
    match p["sealedKey"].as_str() {
        Some(s) => Ok(Some(open(s, &identity()?)?)),
        None => Ok(None),
    }
}

pub fn delete_provider(state: &Shared, id: &str) -> Result<()> {
    if !state.store.delete_agent_provider(id)? {
        bail!("there's no provider “{id}”");
    }
    // El que se borra deja de ser el de por defecto.
    let mut s = settings(state);
    if s.default_provider.as_deref() == Some(id) {
        s.default_provider = None;
        state.store.save_probe(SETTINGS_KEY, &serde_json::to_value(&s)?)?;
    }
    Ok(())
}

/// La carpeta de pi (el agente de código): `CONDX_PI_DIR` o `~/.pi/agent`.
pub fn pi_dir() -> std::path::PathBuf {
    std::env::var("CONDX_PI_DIR").map(std::path::PathBuf::from).unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".pi/agent"))
}

fn read_json(path: &std::path::Path) -> Value {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null)
}

/// Variables de `~/.pi/agent/.env` (`NAME=valor`, con o sin `export`).
fn pi_env(dir: &std::path::Path) -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(dir.join(".env"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().trim_start_matches("export ").trim().to_string(), v.trim().trim_matches('"').trim_matches('\'').to_string()))
        .collect()
}

/// La clave de un proveedor de pi: la de `models.json` (literal o `env:NOMBRE`, que se busca en
/// `auth.json`, en `.env` y en el entorno) o, si no, su API key de `auth.json`. Los comandos
/// (`!…`) no se ejecutan.
fn pi_key(dir: &std::path::Path, auth: &Value, provider: &str, model_key: Option<&str>) -> Option<String> {
    let from_auth = || auth[provider].as_object().filter(|o| o.get("type").and_then(Value::as_str) == Some("api_key")).and_then(|o| o.get("key")?.as_str().map(str::to_string));
    match model_key {
        Some(k) if k.starts_with('!') => from_auth(),
        Some(k) if k.starts_with("env:") => {
            let var = &k[4..];
            auth[format!("env:{var}")].as_str().or(auth[var].as_str()).map(str::to_string).or_else(|| pi_env(dir).get(var).cloned()).or_else(|| std::env::var(var).ok()).filter(|v| !v.is_empty()).or_else(from_auth)
        }
        Some(k) if !k.is_empty() => Some(k.to_string()),
        _ => from_auth(),
    }
}

/// Trae los proveedores de pi: los de `models.json` (compatibles con OpenAI o con Anthropic, con
/// su URL y modelos) y las API keys de `auth.json` de los tipos integrados. Los inicios de sesión
/// OAuth no se copian (renuevan sus tokens: copiarlos rompería la sesión de pi). No pisa los que
/// ya hay. El proveedor y modelo por defecto de pi pasan a ser los de Condx si no hay uno.
pub fn import_from_pi(state: &Shared, dir: &std::path::Path, seal_to: &dyn Fn(&str) -> Result<String>) -> Result<Value> {
    let auth = read_json(&dir.join("auth.json"));
    let models = read_json(&dir.join("models.json"));
    let settings_pi = read_json(&dir.join("settings.json"));
    if auth.is_null() && models.is_null() {
        bail!("there's no pi configuration in {}", dir.display());
    }
    let existing: Vec<String> = state.store.agent_providers()?.iter().filter_map(|p| p["id"].as_str().map(str::to_string)).collect();
    let (pi_default, pi_model) = (settings_pi["defaultProvider"].as_str(), settings_pi["defaultModel"].as_str());
    let mut imported = vec![];
    let mut skipped = vec![];
    let mut covered = std::collections::HashSet::new();
    let add = |id: &str, body: ProviderBody, imported: &mut Vec<Value>, skipped: &mut Vec<Value>| {
        if existing.iter().any(|e| e == id) {
            skipped.push(json!({ "name": id, "reason": "already in Condx" }));
            return;
        }
        match upsert_provider(state, body, seal_to) {
            Ok(p) => imported.push(json!({ "id": p["id"], "kind": p["kind"], "model": p["model"] })),
            Err(e) => skipped.push(json!({ "name": id, "reason": format!("{e:#}") })),
        }
    };
    for (name, p) in models["providers"].as_object().cloned().unwrap_or_default() {
        covered.insert(name.clone());
        let kind = match p["api"].as_str() {
            Some("openai-completions") => "openai-compatible",
            Some("anthropic-messages") => "anthropic-compatible",
            other => {
                skipped.push(json!({ "name": name, "reason": format!("its API ({}) isn't supported yet", other.unwrap_or("?")) }));
                continue;
            }
        };
        let ids: Vec<&str> = p["models"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str()).collect();
        let model = pi_model.filter(|m| pi_default == Some(name.as_str()) && ids.contains(m)).or(ids.first().copied()).map(str::to_string);
        let key = pi_key(dir, &auth, &name, p["apiKey"].as_str());
        let url = p["baseUrl"].as_str().unwrap_or_default();
        let local = url.contains("://127.0.0.1") || url.contains("://localhost");
        if key.is_none() && !local {
            let why = if p["apiKey"].as_str().is_some_and(|k| k.starts_with('!')) { "its key comes from a command (Condx doesn't run it)" } else { "its key wasn't found" };
            skipped.push(json!({ "name": name, "reason": why }));
            continue;
        }
        let body = ProviderBody { id: None, kind: kind.into(), label: Some(name.clone()), base_url: p["baseUrl"].as_str().map(str::to_string), model, api_key: key };
        add(&slug(&name), body, &mut imported, &mut skipped);
    }
    for (name, cred) in auth.as_object().cloned().unwrap_or_default() {
        if covered.contains(&name) || !cred.is_object() {
            continue;
        }
        match cred["type"].as_str() {
            Some("oauth") => skipped.push(json!({ "name": name, "reason": "OAuth login: not copied (it would break pi's session); add an API key instead" })),
            Some("api_key") if kind(&name).is_some_and(|k| !k.needs_base_url) => {
                let body = ProviderBody { id: None, kind: name.clone(), label: None, base_url: None, model: None, api_key: cred["key"].as_str().map(str::to_string) };
                add(&slug(kind(&name).map(|k| k.label).unwrap_or(&name)), body, &mut imported, &mut skipped);
            }
            Some("api_key") => skipped.push(json!({ "name": name, "reason": "not a model provider Condx knows" })),
            _ => {}
        }
    }
    // El de pi por defecto, si se trajo y Condx no tiene uno.
    let mut default = Value::Null;
    if let Some(d) = pi_default.map(slug) {
        let mut s = settings(state);
        if s.default_provider.is_none() {
            if let Some(p) = imported.iter().find(|p| p["id"] == d.as_str()) {
                s.default_provider = Some(d.clone());
                s.default_model = p["model"].as_str().map(str::to_string);
                state.store.save_probe(SETTINGS_KEY, &serde_json::to_value(&s)?)?;
                default = json!({ "provider": d, "model": s.default_model });
            }
        }
    }
    tracing::info!("imported {} provider(s) from pi", imported.len());
    Ok(json!({ "imported": imported, "skipped": skipped, "default": default, "from": dir.display().to_string() }))
}

/// Todo lo que la app muestra en Configuración → Agentes.
pub fn json(state: &Shared) -> Value {
    let providers: Vec<Value> = state.store.agent_providers().unwrap_or_default().iter().map(public).collect();
    json!({
        "settings": settings(state),
        "providers": providers,
        "kinds": KINDS.iter().map(|k| json!({ "id": k.id, "label": k.label, "needsBaseUrl": k.needs_base_url, "model": k.model })).collect::<Vec<_>>(),
        "sources": SOURCES,
        "host": state.agent_host.status(),
        "templates": crate::templates::list(state),
        "tools": crate::templates::TOOLS,
        "usageToday": crate::agent_host::usage_today(state),
    })
}

#[cfg(test)]
mod logo_tests {
    use super::*;

    #[test]
    fn marca_por_tipo_y_por_url() {
        assert_eq!(logo(&json!({ "kind": "anthropic" })), "claude");
        assert_eq!(logo(&json!({ "kind": "anthropic-compatible", "baseUrl": "https://api.minimax.io/anthropic", "label": "MiniMax" })), "minimax");
        assert_eq!(logo(&json!({ "kind": "openai-compatible", "baseUrl": "http://localhost:11434/v1" })), "ollama");
        assert_eq!(logo(&json!({ "kind": "google" })), "gemini");
        assert_eq!(logo(&json!({ "kind": "openai-compatible", "baseUrl": "https://example.com" })), "other");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> Shared {
        crate::state::test_state()
    }

    #[test]
    fn la_clave_se_guarda_cifrada_y_no_vuelve() {
        let st = state();
        let mac = age::x25519::Identity::generate();
        let to = mac.to_public();
        let seal_to = move |k: &str| seal(k, std::slice::from_ref(&to));
        let p = upsert_provider(&st, ProviderBody { id: None, kind: "anthropic".into(), label: None, base_url: None, model: None, api_key: Some("sk-ant-secret-1234".into()) }, &seal_to).unwrap();
        assert_eq!(p["id"], "anthropic");
        assert_eq!(p["keyHint"], "…1234");
        assert_eq!(p["model"], "claude-sonnet-5-5");
        assert!(p.get("sealedKey").is_none(), "{p}");
        // En la base solo está cifrada; con la identidad de la Mac se abre.
        let stored = st.store.agent_providers().unwrap()[0].clone();
        let sealed = stored["sealedKey"].as_str().unwrap();
        assert!(!sealed.contains("sk-ant-secret"));
        assert_eq!(open(sealed, &mac).unwrap(), "sk-ant-secret-1234");
        // Otra identidad no puede.
        assert!(open(sealed, &age::x25519::Identity::generate()).is_err());
        // Lo que ve la app tampoco la trae.
        assert!(!json(&st).to_string().contains("sealedKey"));
    }

    #[test]
    fn cambiar_el_modelo_conserva_la_clave() {
        let st = state();
        let mac = age::x25519::Identity::generate();
        let to = mac.to_public();
        let seal_to = move |k: &str| seal(k, std::slice::from_ref(&to));
        upsert_provider(&st, ProviderBody { id: None, kind: "openai".into(), label: None, base_url: None, model: None, api_key: Some("sk-abc9999".into()) }, &seal_to).unwrap();
        let p = upsert_provider(&st, ProviderBody { id: Some("openai".into()), kind: "openai".into(), label: None, base_url: None, model: Some("gpt-5-mini".into()), api_key: None }, &seal_to).unwrap();
        assert_eq!(p["model"], "gpt-5-mini");
        assert_eq!(p["keyHint"], "…9999");
        let sealed = st.store.agent_providers().unwrap()[0]["sealedKey"].as_str().unwrap().to_string();
        assert_eq!(open(&sealed, &mac).unwrap(), "sk-abc9999");
    }

    #[test]
    fn varios_destinatarios_pueden_abrirla() {
        // Lo que permitirá la fase A6: la Mac y un teléfono (o un runner) con su copia.
        let (mac, phone) = (age::x25519::Identity::generate(), age::x25519::Identity::generate());
        let sealed = seal("sk-x", &[mac.to_public(), phone.to_public()]).unwrap();
        assert_eq!(open(&sealed, &mac).unwrap(), "sk-x");
        assert_eq!(open(&sealed, &phone).unwrap(), "sk-x");
    }

    #[test]
    fn valida_lo_que_llega() {
        let st = state();
        let seal_to = |k: &str| Ok(format!("sealed:{k}"));
        let err = |b: ProviderBody| upsert_provider(&st, b, &seal_to).unwrap_err().to_string();
        assert!(err(ProviderBody { id: None, kind: "nope".into(), label: None, base_url: None, model: None, api_key: Some("k".into()) }).contains("unknown"));
        assert!(err(ProviderBody { id: None, kind: "anthropic".into(), label: None, base_url: None, model: None, api_key: None }).contains("API key"));
        assert!(err(ProviderBody { id: None, kind: "openai-compatible".into(), label: None, base_url: None, model: None, api_key: None }).contains("base URL"));
        // Un endpoint local sin clave sí.
        let p = upsert_provider(&st, ProviderBody { id: None, kind: "openai-compatible".into(), label: Some("Ollama".into()), base_url: Some("http://127.0.0.1:11434/v1/".into()), model: Some("qwen3".into()), api_key: None }, &seal_to).unwrap();
        assert_eq!(p["id"], "ollama");
        assert_eq!(p["baseUrl"], "http://127.0.0.1:11434/v1");
        assert!(p["keyHint"].is_null());
    }

    #[test]
    fn la_autonomia_se_guarda_y_valida() {
        let st = state();
        assert_eq!(settings(&st).level("job"), Level::Notify);
        let s = save_settings(&st, &json!({ "autonomy": { "job": "fix", "ci": "off" }, "dailyTokenBudget": 1000, "maxConcurrent": 2 })).unwrap();
        assert_eq!(s.level("job"), Level::Fix);
        assert_eq!(s.level("ci"), Level::Off);
        // Las que no se mandan quedan en notify.
        assert_eq!(settings(&st).level("convex"), Level::Notify);
        assert_eq!(settings(&st).max_concurrent, 2);
        assert!(save_settings(&st, &json!({ "autonomy": { "mail": "fix" }, "maxConcurrent": 1 })).is_err());
        assert!(save_settings(&st, &json!({ "defaultProvider": "ghost", "maxConcurrent": 1 })).is_err());
        assert!(save_settings(&st, &json!({ "maxConcurrent": 0 })).is_err());
    }

    #[test]
    fn trae_los_proveedores_de_pi_sin_oauth_ni_comandos() {
        let st = state();
        let dir = tempfile::tempdir().unwrap();
        let w = |f: &str, v: Value| std::fs::write(dir.path().join(f), v.to_string()).unwrap();
        w("auth.json", json!({
            "anthropic": { "type": "oauth", "access": "a", "refresh": "r", "expires": 1 },
            "openrouter": { "type": "api_key", "key": "sk-or-test" },
            "nvidia": { "type": "api_key", "key": "nvapi-test" },
            "resend": { "type": "api_key", "key": "re_test" },
            "env:MOON_KEY": "sk-moon-from-auth",
            "EBAY_APP_ID": "x",
        }));
        w("models.json", json!({ "providers": {
            "minimax": { "api": "anthropic-messages", "baseUrl": "https://api.minimax.io/anthropic", "apiKey": "mm-literal", "models": [{ "id": "MiniMax-M2.7" }, { "id": "MiniMax-M3" }] },
            "nvidia": { "api": "openai-completions", "baseUrl": "https://integrate.api.nvidia.com/v1", "apiKey": "env:NVIDIA_API_KEY_UNSET_IN_TESTS", "models": [{ "id": "meta/llama" }] },
            "moonshot": { "api": "openai-completions", "baseUrl": "https://api.moonshot.ai/v1", "apiKey": "env:MOON_KEY", "models": [{ "id": "kimi" }] },
            "sneaky": { "api": "openai-completions", "baseUrl": "https://x.test/v1", "apiKey": "!cat ~/.ssh/id_rsa", "models": [{ "id": "m" }] },
            "bedrock": { "api": "bedrock-converse-stream", "models": [] },
        }}));
        w("settings.json", json!({ "defaultProvider": "minimax", "defaultModel": "MiniMax-M3" }));
        let mac = age::x25519::Identity::generate();
        let to = mac.to_public();
        let seal_to = move |k: &str| seal(k, std::slice::from_ref(&to));
        let out = import_from_pi(&st, dir.path(), &seal_to).unwrap();
        let mut ids: Vec<&str> = out["imported"].as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap()).collect();
        ids.sort();
        assert_eq!(ids, vec!["minimax", "moonshot", "nvidia", "openrouter"], "{out}");
        let key = |id: &str| {
            let p = st.store.agent_providers().unwrap().into_iter().find(|p| p["id"] == id).unwrap();
            (p["kind"].as_str().unwrap().to_string(), p["model"].as_str().unwrap_or("").to_string(), open(p["sealedKey"].as_str().unwrap(), &mac).unwrap())
        };
        assert_eq!(key("minimax"), ("anthropic-compatible".into(), "MiniMax-M3".into(), "mm-literal".into()));
        assert_eq!(key("moonshot").2, "sk-moon-from-auth");
        // Una variable que no existe: la API key de auth.json del mismo proveedor.
        assert_eq!(key("nvidia").2, "nvapi-test");
        assert_eq!(key("openrouter").0, "openrouter");
        let skipped = out["skipped"].to_string();
        assert!(skipped.contains("OAuth"), "{skipped}");
        assert!(skipped.contains("bedrock"));
        assert!(skipped.contains("resend"));
        // El comando no se ejecutó: sin clave en auth.json, no hay clave y no se importa.
        assert!(!ids.contains(&"sneaky"));
        assert!(skipped.contains("command (Condx doesn't run it)"), "{skipped}");
        assert_eq!(out["default"]["provider"], "minimax");
        assert_eq!(settings(&st).default_model.as_deref(), Some("MiniMax-M3"));
        // Otra vez: no pisa nada.
        let again = import_from_pi(&st, dir.path(), &seal_to).unwrap();
        assert!(again["imported"].as_array().unwrap().is_empty());
    }
}

