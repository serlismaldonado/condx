// Notificaciones al teléfono con ntfy (docs/design/agents.md, fase A1): el daemon publica en el
// tema de la persona (ntfy.sh o un servidor propio) cuando se abre un incidente, cuando un agente
// termina (diagnóstico o arreglo listo) y cuando algo espera su aprobación. La app gratuita de ntfy
// (iOS, Android) las muestra. Sin cuentas de Apple ni de Google.
//
// El mensaje es corto y sin evidencia (sin logs ni errores completos): en ntfy.sh el servidor lo ve,
// y cualquiera que sepa el tema puede leerlo, por eso la app sugiere uno al azar. Si el servidor pide
// un token, vive en el Llavero (`ntfy-token`), nunca en la base ni en la app.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::state::Shared;

const KEY: &str = "meta:notify";
pub const TOKEN_ACCOUNT: &str = "ntfy-token";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub enabled: bool,
    pub server: String,
    pub topic: String,
    pub incidents: bool,
    pub agents: bool,
    pub approvals: bool,
    /// Los avisos de los flujos (nodo «aviso»).
    pub flows: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { enabled: false, server: "https://ntfy.sh".into(), topic: String::new(), incidents: true, agents: true, approvals: true, flows: true }
    }
}

pub fn settings(state: &Shared) -> Settings {
    state.store.probes().ok().and_then(|l| l.into_iter().find(|(k, _)| k == KEY)).and_then(|(_, v)| serde_json::from_value(v).ok()).unwrap_or_default()
}

/// Un tema al azar (en ntfy los temas son públicos: el nombre es la clave).
pub fn random_topic() -> String {
    format!("condx-{}", crate::auth::random_token().unwrap_or_default().chars().take(20).collect::<String>())
}

/// Guarda la configuración (y el token, si llega: al Llavero; vacío lo borra).
pub fn save(state: &Shared, body: &Value) -> Result<Settings> {
    let mut s: Settings = serde_json::from_value(body.clone()).map_err(|e| anyhow::anyhow!("invalid notification settings: {e}"))?;
    s.server = s.server.trim().trim_end_matches('/').to_string();
    s.topic = s.topic.trim().to_string();
    if !(s.server.starts_with("https://") || s.server.starts_with("http://127.0.0.1") || s.server.starts_with("http://localhost")) {
        bail!("the server must use https (or be on this Mac)");
    }
    if s.enabled && !(4..=64).contains(&s.topic.len()) {
        bail!("choose a topic (4 to 64 characters)");
    }
    if !s.topic.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("the topic can only have letters, numbers, - and _");
    }
    if let Some(token) = body.get("token").and_then(Value::as_str) {
        let token = token.trim();
        if token.is_empty() {
            crate::secrets::delete(TOKEN_ACCOUNT)?;
        } else {
            crate::secrets::set(TOKEN_ACCOUNT, token)?;
        }
    }
    state.store.save_probe(KEY, &serde_json::to_value(&s)?)?;
    Ok(s)
}

pub fn json(state: &Shared) -> Value {
    let s = settings(state);
    json!({ "settings": s, "hasToken": crate::secrets::exists(TOKEN_ACCOUNT), "suggestedTopic": random_topic() })
}

/// Una notificación (bloqueante: llámese fuera del runtime o con spawn_blocking).
pub struct Message {
    pub title: String,
    pub body: String,
    pub tags: &'static str,
    pub priority: u8,
}

fn clip(text: &str, max: usize) -> String {
    let red = devtool_core::redact(text);
    let mut out: String = red.chars().take(max).collect();
    if red.chars().count() > max {
        out.push('…');
    }
    out
}

/// Envía `m` al tema configurado. `token`: el del Llavero (aparte para probar sin él).
pub fn send_to(s: &Settings, token: Option<&str>, m: &Message) -> Result<()> {
    if s.topic.is_empty() {
        bail!("no topic");
    }
    // Publicar en JSON (a la raíz del servidor): admite UTF-8 en el título (las cabeceras, solo ASCII).
    let mut req = ureq::post(&format!("{}/", s.server)).timeout(Duration::from_secs(10));
    if let Some(t) = token {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    let body = serde_json::json!({ "topic": s.topic, "title": clip(&m.title, 120), "message": clip(&m.body, 400), "tags": [m.tags], "priority": m.priority });
    match req.send_json(body) {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(code, r)) => bail!("ntfy answered {code}: {}", r.into_string().unwrap_or_default().chars().take(200).collect::<String>()),
        Err(e) => bail!("couldn't reach {}: {e}", s.server),
    }
}

/// Envía si está encendido y el tipo de aviso lo está; los errores van al log (no frenan nada).
pub fn send(state: &Shared, kind: &str, m: Message) {
    let s = settings(state);
    let wanted = match kind {
        "incident" => s.incidents,
        "agent" => s.agents,
        "approval" => s.approvals,
        "flow" => s.flows,
        _ => true,
    };
    if !wanted {
        return;
    }
    // Por correo también: siempre, o cuando ntfy está apagado o falla (`mail.rs`).
    if !s.enabled {
        if crate::mail::wants(state, false) {
            crate::mail::notify(state, &m.title, &m.body);
        }
        return;
    }
    let st = state.clone();
    std::thread::spawn(move || {
        let token = crate::secrets::get(TOKEN_ACCOUNT).ok().flatten();
        let sent = match send_to(&s, token.as_deref(), &m) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("notification not sent: {e:#}");
                false
            }
        };
        if crate::mail::wants(&st, sent) {
            crate::mail::notify(&st, &m.title, &m.body);
        }
    });
}

pub fn incident_opened(state: &Shared, inc: &Value) {
    let count = inc["count"].as_i64().unwrap_or(1);
    let body = [inc["source"].as_str().map(str::to_string), inc["env"].as_str().map(|e| format!("on {e}")), (count > 1).then(|| format!("{count} times"))]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    send(state, "incident", Message { title: format!("Incident: {}", inc["title"].as_str().unwrap_or("?")), body, tags: "rotating_light", priority: 4 });
}

/// Un agente terminó: la primera línea de su causa (o por qué paró).
pub fn agent_finished(state: &Shared, run: &Value, incident: &Value) {
    let status = run["status"].as_str().unwrap_or("");
    let cause = run["answer"].as_str().and_then(|a| a.split("## Cause").nth(1)).and_then(|s| s.lines().map(str::trim).find(|l| !l.is_empty())).unwrap_or("").to_string();
    let fix_ready = run["fix"]["changes"] == true;
    let (title, body) = match (status, run["mode"].as_str()) {
        ("done", Some("fix")) if fix_ready => (format!("Fix ready: {}", incident["title"].as_str().unwrap_or("?")), format!("{cause}\nOn {}. Review it and deploy from Condx.", run["branch"].as_str().unwrap_or("its branch"))),
        ("done", _) => (format!("Diagnosis: {}", incident["title"].as_str().unwrap_or("?")), cause),
        (_, _) => (format!("Agent {status}: {}", incident["title"].as_str().unwrap_or("?")), run["error"].as_str().unwrap_or("").to_string()),
    };
    send(state, "agent", Message { title, body, tags: if status == "done" { "robot" } else { "warning" }, priority: 3 });
}

/// Un agente contestó en un hilo: quién, de qué hilo y el comienzo de lo que dijo.
pub fn agent_replied(state: &Shared, run: &Value) {
    let who = run["role"].as_str().and_then(|r| crate::roles::get(state, r)).map(|r| r.name).unwrap_or_else(|| "The agent".to_string());
    // Los agentes de un flujo se titulan «Risk · flujo»: sin repetir quién.
    let title = run["title"].as_str().unwrap_or("a thread");
    let about = title.strip_prefix(&format!("{who} · ")).unwrap_or(title);
    let (title, body) = match run["status"].as_str() {
        Some("done") => {
            let text = run["answer"].as_str().unwrap_or("");
            let first = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("```")).unwrap_or("").to_string();
            (format!("{who} replied · {about}"), first)
        }
        Some(st) => (format!("{who} {st} · {about}"), run["error"].as_str().unwrap_or("").to_string()),
        None => return,
    };
    send(state, "agent", Message { title, body, tags: "speech_balloon", priority: 3 });
}

/// Aprobaciones nuevas: las que ya esperaban al arrancar no avisan (sin una ola de avisos).
static SEEN: Mutex<Option<HashSet<String>>> = Mutex::new(None);

pub fn check_approvals(state: &Shared) {
    let waiting = state.store.jobs(&["waiting-approval"], 100).unwrap_or_default();
    let mut seen = SEEN.lock().unwrap();
    let first = seen.is_none();
    let set = seen.get_or_insert_with(HashSet::new);
    for job in waiting {
        let Some(id) = job["id"].as_str() else { continue };
        if set.insert(id.to_string()) && !first && job["requestedBy"] != "app" {
            let title = format!("Waiting for your approval: {}", job["title"].as_str().unwrap_or("?"));
            let body = format!("{} · requested by {}", job["env"].as_str().unwrap_or("?"), job["requestedBy"].as_str().unwrap_or("?"));
            send(state, "approval", Message { title, body, tags: "raised_hand", priority: 4 });
        }
    }
}

pub fn spawn(state: &Shared) {
    let state = state.clone();
    tokio::spawn(async move {
        let mut every = tokio::time::interval(Duration::from_secs(10));
        loop {
            every.tick().await;
            check_approvals(&state);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn envia_al_tema_con_titulo_corto_sin_secretos_y_con_token() {
        use axum::{http::HeaderMap, routing::post, Json, Router};
        let got = std::sync::Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let g = got.clone();
        let app = Router::new().route(
            "/",
            post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let g = g.clone();
                async move {
                    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                    g.lock().unwrap().push((auth, body));
                    "ok"
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let s = Settings { enabled: true, server, topic: "condx-test".into(), ..Default::default() };
        // Con caracteres fuera de ASCII en el título (por cabecera, ntfy los rechazaba).
        let m = Message { title: "Risk replied · Triage, ask, notify".into(), body: format!("CONVEX_DEPLOY_KEY=prod:abc123secretvalue {}", "x".repeat(600)), tags: "speech_balloon", priority: 4 };
        tokio::task::spawn_blocking(move || send_to(&s, Some("tk_test"), &m)).await.unwrap().unwrap();
        let got = got.lock().unwrap();
        let (auth, body) = &got[0];
        assert_eq!(body["topic"], "condx-test");
        assert_eq!(body["title"], "Risk replied · Triage, ask, notify");
        assert_eq!(body["tags"], json!(["speech_balloon"]));
        assert_eq!(body["priority"], 4);
        assert_eq!(auth, "Bearer tk_test");
        let msg = body["message"].as_str().unwrap();
        assert!(!msg.contains("abc123secretvalue"), "{msg}");
        assert!(msg.chars().count() <= 401);
    }

    #[test]
    fn valida_la_configuracion() {
        let st = crate::state::test_state();
        assert!(save(&st, &json!({ "enabled": true, "server": "http://example.com", "topic": "condx-abcdef" })).is_err(), "plain http to another host");
        assert!(save(&st, &json!({ "enabled": true, "server": "https://ntfy.sh", "topic": "ab" })).is_err());
        assert!(save(&st, &json!({ "enabled": true, "server": "https://ntfy.sh", "topic": "has spaces" })).is_err());
        let s = save(&st, &json!({ "enabled": true, "server": "https://ntfy.example.com/", "topic": "condx-abcdef", "approvals": false })).unwrap();
        assert_eq!(s.server, "https://ntfy.example.com");
        assert!(!settings(&st).approvals);
        assert!(random_topic().starts_with("condx-") && random_topic().len() == 26);
    }
}
