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

/// La identidad de la Mac (se crea la primera vez que se guarda una clave).
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

/// Todo lo que la app muestra en Configuración → Agentes.
pub fn json(state: &Shared) -> Value {
    let providers: Vec<Value> = state.store.agent_providers().unwrap_or_default().iter().map(public).collect();
    json!({
        "settings": settings(state),
        "providers": providers,
        "kinds": KINDS.iter().map(|k| json!({ "id": k.id, "label": k.label, "needsBaseUrl": k.needs_base_url, "model": k.model })).collect::<Vec<_>>(),
        "sources": SOURCES,
        "host": state.agent_host.status(),
        "usageToday": crate::agent_host::usage_today(state),
    })
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
}
