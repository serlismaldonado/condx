// Correo como canal de respaldo (docs/design/cloud-site.md §9.1, fase S7) con AgentMail.
//
// - Entra: cada 30 s el daemon lee el buzón (`heskala@agentmail.to` u otro). Un correo de un
//   remitente permitido abre un chat de Condx (uno por hilo de correo); lo que se responda en ese
//   hilo sigue el mismo chat, y cuando el agente contesta, la respuesta sale por correo. AgentMail no
//   devuelve correos que fallan SPF/DKIM (salvo que se pidan), y uno de un remitente no permitido se
//   marca y se ignora sin contestar (sin rebotes a quien falsificó el remitente).
// - Sale: los avisos de ntfy también van por correo cuando ntfy está apagado o falla (`fallback`) o
//   siempre (`always`). Lo que llega en un minuto sale en un solo correo. Los avisos no piden respuesta:
//   con una key de Resend (`resend-key`) y un remitente (`resendFrom`) salen por Resend (AgentMail
//   tiene un cupo chico para enviar); responder igual llega al buzón (Reply-To) y el agente ve qué se
//   había mandado (cada envío queda en `meta:mail-sent`). Las respuestas de un chat van por AgentMail.
// - El correo pide, nunca aprueba: llega al agente del chat, que solo lee (sin aprobar ni desplegar).
//
// La API key es un secreto del daemon (`agentmail-key`); los ajustes, una fila de la base.

use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::state::Shared;

pub const KEY_ACCOUNT: &str = "agentmail-key";
pub const RESEND_ACCOUNT: &str = "resend-key";
const SENT_KEY: &str = "meta:mail-sent";
/// Los avisos que llegan en este rato salen juntos (una caída que abre cinco incidentes, un correo).
const DIGEST: Duration = Duration::from_secs(60);
static PENDING: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());
static FLUSHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
const SETTINGS_KEY: &str = "meta:mail";
const THREADS_KEY: &str = "meta:mail-threads";
const DONE: &str = "condx-done";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub enabled: bool,
    /// El buzón de AgentMail (su id es la dirección).
    pub inbox: String,
    /// Quién puede escribirle a los agentes (y a quién van los avisos: el primero).
    pub allow: Vec<String>,
    /// `fallback` (si ntfy está apagado o falla), `always` u `off`.
    pub notify: String,
    /// Desde cuándo se leen correos (al encenderlo): lo que ya estaba en el buzón no se toca.
    pub since: i64,
    /// El remitente de los avisos por Resend («Condx <condx@dominio-verificado>»); vacío: por AgentMail.
    pub resend_from: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self { enabled: false, inbox: "heskala@agentmail.to".into(), allow: vec![], notify: "fallback".into(), since: 0, resend_from: String::new() }
    }
}

pub fn settings(state: &Shared) -> Settings {
    let stored = state.store.probes().ok().and_then(|l| l.into_iter().find(|(k, _)| k == SETTINGS_KEY)).map(|(_, v)| v);
    stored.and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

pub fn save(state: &Shared, s: &Settings) -> Result<()> {
    let mut s = s.clone();
    // Al encenderlo, desde ahora (un buzón compartido con otras pruebas no se toca hacia atrás); si
    // ya estaba encendido, se mantiene desde cuándo.
    let before = settings(state);
    if s.enabled {
        s.since = if before.enabled && before.since > 0 { before.since } else { crate::store::now_ms() };
    }
    let s = &s;
    if s.enabled && s.allow.is_empty() {
        bail!("say who can write to the agents (--allow you@example.com)");
    }
    if !matches!(s.notify.as_str(), "fallback" | "always" | "off") {
        bail!("notify is fallback, always or off");
    }
    state.store.save_probe(SETTINGS_KEY, &serde_json::to_value(s)?)?;
    Ok(())
}

fn api() -> String {
    std::env::var("CONDX_AGENTMAIL_API").unwrap_or_else(|_| "https://api.agentmail.to".into())
}

/// Un cliente de la API de AgentMail con su key.
struct Client {
    base: String,
    key: String,
    inbox: String,
}

impl Client {
    fn new(state: &Shared) -> Result<Self> {
        let key = crate::secrets::get(KEY_ACCOUNT)?.filter(|k| !k.trim().is_empty()).ok_or_else(|| anyhow!("there's no AgentMail key (condxd secret set agentmail-key)"))?;
        Ok(Self { base: api(), key: key.trim().to_string(), inbox: settings(state).inbox })
    }

    fn url(&self, path: &str) -> String {
        let enc = |s: &str| s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>();
        format!("{}/v0/inboxes/{}{path}", self.base, enc(&self.inbox))
    }

    fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let req = ureq::request(method, &self.url(path)).timeout(Duration::from_secs(20)).set("Authorization", &format!("Bearer {}", self.key));
        let reply = match body {
            Some(b) => req.send_json(b),
            None => req.call(),
        };
        match reply {
            Ok(r) => Ok(r.into_json().unwrap_or(Value::Null)),
            Err(ureq::Error::Status(code, r)) => bail!("AgentMail answered {code}: {}", devtool_core::redact(&r.into_string().unwrap_or_default()).chars().take(200).collect::<String>()),
            Err(e) => bail!("couldn't reach AgentMail: {e}"),
        }
    }

    fn recent(&self) -> Result<Vec<Value>> {
        Ok(self.call("GET", "/messages?limit=30", None)?["messages"].as_array().cloned().unwrap_or_default())
    }

    fn get(&self, id: &str) -> Result<Value> {
        self.call("GET", &format!("/messages/{id}"), None)
    }

    fn label(&self, id: &str, add: &str) -> Result<()> {
        self.call("PATCH", &format!("/messages/{id}"), Some(json!({ "add_labels": [add], "remove_labels": ["unread"] })))?;
        Ok(())
    }

    fn reply(&self, id: &str, body: &Body) -> Result<()> {
        self.call("POST", &format!("/messages/{id}/reply"), Some(body.json(json!({}))))?;
        Ok(())
    }

    fn send(&self, to: &str, subject: &str, body: &Body) -> Result<()> {
        self.call("POST", "/messages/send", Some(body.json(json!({ "to": [to], "subject": subject }))))?;
        Ok(())
    }
}

/// Lo que va en un correo: el texto siempre; el HTML (react-email) si el host pudo armarlo.
struct Body {
    text: String,
    html: Option<String>,
}

impl Body {
    fn json(&self, mut base: Value) -> Value {
        base["text"] = json!(self.text);
        if let Some(h) = &self.html {
            base["html"] = json!(h);
        }
        base
    }
}

/// El correo con el estilo de Condx (react-email, en el host de agentes, `POST /email`); si no se
/// puede, solo el texto.
fn compose(state: &Shared, markdown: &str, title: &str) -> Body {
    let footer = "Responde este correo para seguir la conversación. Aprobar y desplegar se hace en la app de Condx.";
    match state.agent_host.call(state, "POST", "/email", Some(json!({ "markdown": markdown, "title": title, "footer": footer }))) {
        Ok(v) if v["html"].is_string() => Body { text: v["text"].as_str().filter(|t| !t.trim().is_empty()).unwrap_or(markdown).to_string(), html: v["html"].as_str().map(str::to_string) },
        Ok(_) => Body { text: markdown.to_string(), html: None },
        Err(e) => {
            tracing::warn!("mail: sending without format (the agent host couldn't render it): {e:#}");
            Body { text: markdown.to_string(), html: None }
        }
    }
}

/// Cuándo llegó un correo, en ms (`timestamp` o `created_at`, RFC 3339); 0 si no se sabe.
fn received_at(m: &Value) -> i64 {
    let t = m["timestamp"].as_str().or_else(|| m["created_at"].as_str()).unwrap_or("");
    utc_ms(t).unwrap_or(0)
}

/// `AAAA-MM-DDTHH:MM:SS[.fff]Z` (UTC, como los da AgentMail) en ms desde 1970.
fn utc_ms(t: &str) -> Option<i64> {
    let (date, time) = t.trim().trim_end_matches('Z').split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, mo, day) = (d.next()??, d.next()??, d.next()??);
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut h = hms.split(':').map(|x| x.parse::<i64>().ok());
    let (hh, mm, ss) = (h.next()??, h.next()??, h.next()??);
    let ms = format!("{:0<3}", frac.chars().take(3).collect::<String>()).parse::<i64>().ok()?;
    // Días desde 1970-01-01 (algoritmo de Howard Hinnant).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + hh) * 60 + mm) * 60_000 + ss * 1000 + ms)
}

/// La dirección de un `From` («Nombre <a@b.com>» o «a@b.com»), en minúsculas.
pub fn address(from: &str) -> String {
    let from = from.trim();
    let inner = from.rsplit_once('<').and_then(|(_, r)| r.split_once('>')).map(|(a, _)| a).unwrap_or(from);
    inner.trim().to_lowercase()
}

fn allowed(s: &Settings, from: &str) -> bool {
    let a = address(from);
    !a.is_empty() && s.allow.iter().any(|x| x.trim().eq_ignore_ascii_case(&a))
}

fn text_of(m: &Value) -> String {
    for k in ["extracted_text", "text"] {
        if let Some(t) = m[k].as_str().filter(|t| !t.trim().is_empty()) {
            return t.trim().to_string();
        }
    }
    // Solo HTML (reenvíos de Gmail u Outlook): sin etiquetas.
    let html = m["extracted_html"].as_str().or_else(|| m["html"].as_str()).unwrap_or("");
    let mut out = String::new();
    let mut tag = false;
    for c in html.chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            c if !tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn threads(state: &Shared) -> serde_json::Map<String, Value> {
    state.store.probes().ok().and_then(|l| l.into_iter().find(|(k, _)| k == THREADS_KEY)).and_then(|(_, v)| v.as_object().cloned()).unwrap_or_default()
}

/// Lo nuevo del buzón: cada correo permitido va a su chat; los demás se marcan y se ignoran.
pub async fn process_inbox(state: &Shared) -> Result<usize> {
    let s = settings(state);
    if !s.enabled {
        return Ok(0);
    }
    let st = state.clone();
    let client = tokio::task::spawn_blocking(move || Client::new(&st)).await??;
    let client = std::sync::Arc::new(client);
    let c = client.clone();
    let list = tokio::task::spawn_blocking(move || c.recent()).await??;
    let mut handled = 0;
    // Del más viejo al más nuevo: un hilo se abre antes de que llegue su respuesta.
    for m in list.into_iter().rev() {
        let labels: Vec<&str> = m["labels"].as_array().map(|l| l.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        if !labels.contains(&"received") || labels.contains(&DONE) || received_at(&m) < s.since {
            continue;
        }
        let id = m["message_id"].as_str().unwrap_or_default().to_string();
        let from = m["from"].as_str().unwrap_or_default().to_string();
        let c = client.clone();
        // De otro remitente: ni se contesta ni se toca (el buzón puede servir a otras cosas).
        if !allowed(&s, &from) {
            continue;
        }
        let id2 = id.clone();
        let full = tokio::task::spawn_blocking(move || c.get(&id2)).await??;
        let text = text_of(&full);
        let subject = full["subject"].as_str().unwrap_or("").to_string();
        let thread = full["thread_id"].as_str().unwrap_or(&id).to_string();
        // Si contesta un aviso de Condx, el agente ve qué se le había mandado.
        let context = answered_notice(state, &subject).map(|n| format!("\n\n(This answers a notice Condx sent by email:)\n{}", n["text"].as_str().unwrap_or(""))).unwrap_or_default();
        let ask = format!("Email from {} — {subject}\n\n{text}{context}\n\n(Sent by email: investigate and answer. Approvals and deploys are done in the Condx app.)", address(&from));
        let mut map = threads(state);
        let chat = match map.get(&thread).and_then(Value::as_str).map(str::to_string) {
            // El hilo de correo ya tiene su chat: sigue ahí (si no hay lugar, espera en la cola).
            Some(chat) => {
                crate::threads::message(state, &chat, &ask).await?;
                chat
            }
            None => match crate::agent_host::start_chat(state, &ask, None, &format!("email:{}", address(&from))).await {
                Ok(run) => {
                    let chat = run["id"].as_str().unwrap_or_default().to_string();
                    map.insert(thread.clone(), json!(chat));
                    let _ = state.store.save_probe(THREADS_KEY, &Value::Object(map));
                    chat
                }
                // Sin lugar para un agente: queda sin marcar y se intenta en la próxima vuelta.
                Err(e) if crate::agent_host::is_full(&e) => continue,
                Err(e) => return Err(e),
            },
        };
        // Cuando el agente conteste, la respuesta sale por correo en este hilo.
        if let Ok(Some(mut run)) = state.store.agent_run(&chat) {
            run["mailReply"] = json!(id);
            let _ = state.store.save_agent_run(&run);
        }
        let c = client.clone();
        tokio::task::spawn_blocking(move || c.label(&id, DONE)).await??;
        tracing::info!("mail: message from {} → chat {chat}", address(&from));
        handled += 1;
    }
    Ok(handled)
}

/// Lo que contestó el agente de un chat que vino por correo: sale en ese hilo de correo.
pub fn reply_to_run(state: &Shared, run: &Value) {
    let Some(id) = run["mailReply"].as_str().map(str::to_string) else { return };
    let text = match run["status"].as_str() {
        Some("done") => run["answer"].as_str().unwrap_or("").trim().to_string(),
        Some(st) => format!("The agent {st}: {}", run["error"].as_str().unwrap_or("no details")),
        None => return,
    };
    if text.is_empty() {
        return;
    }
    let who = run["role"].as_str().and_then(|r| crate::roles::get(state, r)).map(|r| r.name).unwrap_or_else(|| "Agent".to_string());
    let st = state.clone();
    std::thread::spawn(move || {
        let body = compose(&st, &text, &format!("{who} · respuesta a tu correo"));
        if let Err(e) = Client::new(&st).and_then(|c| c.reply(&id, &body)) {
            tracing::warn!("mail: couldn't reply by email: {e:#}");
        }
    });
}

/// Un aviso por correo al primero de la lista (si está encendido). Espera un minuto por si llegan más:
/// salen todos en un correo.
pub fn notify(state: &Shared, subject: &str, body: &str) {
    let s = settings(state);
    if !s.enabled || s.notify == "off" || s.allow.is_empty() {
        return;
    }
    PENDING.lock().unwrap().push((subject.to_string(), body.to_string()));
    if FLUSHING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let st = state.clone();
    std::thread::spawn(move || {
        std::thread::sleep(DIGEST);
        FLUSHING.store(false, std::sync::atomic::Ordering::SeqCst);
        let items = std::mem::take(&mut *PENDING.lock().unwrap());
        let Some((subject, body)) = digest(&items) else { return };
        let Some(to) = settings(&st).allow.first().cloned() else { return };
        if let Err(e) = send_notice(&st, &to, &subject, &body) {
            tracing::warn!("mail: notification not sent: {e:#}");
        }
    });
}

/// Varios avisos en uno: el asunto dice cuántos y el primero; el cuerpo, cada uno con su título.
fn digest(items: &[(String, String)]) -> Option<(String, String)> {
    match items {
        [] => None,
        [(subject, body)] => Some((subject.clone(), body.clone())),
        [(first, _), ..] => Some((
            format!("{} Condx notices: {first}", items.len()),
            items.iter().map(|(t, b)| if b.trim().is_empty() { format!("**{t}**") } else { format!("**{t}**\n\n{b}") }).collect::<Vec<_>>().join("\n\n---\n\n"),
        )),
    }
}

/// Un aviso (no pide respuesta): por Resend si hay key y remitente, si no por AgentMail. Queda anotado
/// para que el agente sepa qué se mandó si alguien contesta.
fn send_notice(state: &Shared, to: &str, subject: &str, markdown: &str) -> Result<()> {
    let s = settings(state);
    let mail = compose(state, markdown, subject);
    let resend = crate::secrets::get(RESEND_ACCOUNT).ok().flatten().filter(|k| !k.trim().is_empty() && !s.resend_from.trim().is_empty());
    let via = match resend {
        Some(key) => {
            let body = mail.json(json!({ "from": s.resend_from.trim(), "to": [to], "subject": subject, "reply_to": s.inbox }));
            match ureq::post(&format!("{}/emails", resend_api())).timeout(Duration::from_secs(20)).set("Authorization", &format!("Bearer {}", key.trim())).send_json(body) {
                Ok(_) => "resend",
                Err(ureq::Error::Status(code, r)) => bail!("Resend answered {code}: {}", devtool_core::redact(&r.into_string().unwrap_or_default()).chars().take(200).collect::<String>()),
                Err(e) => bail!("couldn't reach Resend: {e}"),
            }
        }
        None => {
            Client::new(state)?.send(to, subject, &mail)?;
            "agentmail"
        }
    };
    record_sent(state, json!({ "at": crate::store::now_ms(), "to": to, "subject": subject, "text": markdown.chars().take(4000).collect::<String>(), "via": via }));
    Ok(())
}

/// Un aviso de prueba ya (sin esperar a juntar otros): por dónde salió.
pub fn send_test(state: &Shared) -> Result<String> {
    let s = settings(state);
    let to = s.allow.first().cloned().ok_or_else(|| anyhow!("say who can write to the agents first"))?;
    send_notice(state, &to, "Condx: test notice", "Notices from this Mac arrive by email. Reply to this one and the agent gets it.")?;
    Ok(sent(state).first().and_then(|m| m["via"].as_str()).unwrap_or("?").to_string())
}

fn resend_api() -> String {
    std::env::var("CONDX_RESEND_API").unwrap_or_else(|_| "https://api.resend.com".into())
}

/// Los avisos mandados (los 50 últimos, el más nuevo primero).
pub fn sent(state: &Shared) -> Vec<Value> {
    state.store.probes().ok().and_then(|l| l.into_iter().find(|(k, _)| k == SENT_KEY)).and_then(|(_, v)| v.as_array().cloned()).unwrap_or_default()
}

fn record_sent(state: &Shared, item: Value) {
    let mut list = sent(state);
    list.insert(0, item);
    list.truncate(50);
    let _ = state.store.save_probe(SENT_KEY, &Value::Array(list));
}

/// El aviso que se está contestando (por el asunto, sin «Re:»), para dárselo al agente.
fn answered_notice(state: &Shared, subject: &str) -> Option<Value> {
    let mut s = subject.trim();
    loop {
        let lower = s.to_lowercase();
        match ["re:", "fwd:", "fw:", "rv:"].iter().find(|p| lower.starts_with(*p)) {
            Some(p) => s = s[p.len()..].trim_start(),
            None => break,
        }
    }
    sent(state).into_iter().find(|m| m["subject"].as_str() == Some(s))
}

/// Si los avisos van también por correo: siempre, o cuando ntfy no salió.
pub fn wants(state: &Shared, ntfy_sent: bool) -> bool {
    let s = settings(state);
    s.enabled && (s.notify == "always" || (s.notify == "fallback" && !ntfy_sent))
}

pub fn spawn(state: &Shared) {
    let state = state.clone();
    tokio::spawn(async move {
        let mut every = tokio::time::interval(Duration::from_secs(30));
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            if let Err(e) = process_inbox(&state).await {
                tracing::warn!("mail: {e:#}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_direccion_sale_del_from_y_se_compara_sin_mayusculas() {
        assert_eq!(address("Serlis <Serlis@Heskala.com>"), "serlis@heskala.com");
        assert_eq!(address("a@b.co"), "a@b.co");
        let s = Settings { enabled: true, allow: vec!["serlis@heskala.com".into()], ..Settings::default() };
        assert!(allowed(&s, "Serlis M <SERLIS@heskala.com>"));
        assert!(!allowed(&s, "Eve <serlis@heskala.com.evil.io>"));
        assert!(!allowed(&s, ""));
    }

    #[test]
    fn solo_cuenta_lo_que_llego_despues_de_encenderlo() {
        assert_eq!(received_at(&json!({ "timestamp": "2026-10-26T07:00:00.000Z" })), 1_792_998_000_000);
        assert_eq!(received_at(&json!({ "created_at": "2026-10-26T07:00:00Z" })), 1_792_998_000_000);
        assert_eq!(received_at(&json!({})), 0);
        assert_eq!(utc_ms("1970-01-01T00:00:01.5Z"), Some(1500));
        assert_eq!(utc_ms("2024-02-29T12:00:00Z"), Some(1_709_208_000_000));
    }

    #[test]
    fn el_texto_prefiere_lo_extraido_y_si_no_saca_las_etiquetas() {
        assert_eq!(text_of(&json!({ "extracted_text": "hola", "text": "hola\n> citado" })), "hola");
        assert_eq!(text_of(&json!({ "html": "<p>Revisa <b>esto</b></p>" })), "Revisa esto");
    }

    #[test]
    fn avisos_por_correo_segun_el_modo() {
        let modes = |notify: &str, enabled: bool, ntfy_sent: bool| {
            let s = Settings { enabled, notify: notify.into(), allow: vec!["a@b.co".into()], ..Settings::default() };
            s.enabled && (s.notify == "always" || (s.notify == "fallback" && !ntfy_sent))
        };
        assert!(modes("fallback", true, false));
        assert!(!modes("fallback", true, true));
        assert!(modes("always", true, true));
        assert!(!modes("off", true, false));
        assert!(!modes("always", false, false));
    }

    #[test]
    fn varios_avisos_salen_en_un_correo() {
        assert_eq!(digest(&[]), None);
        assert_eq!(digest(&[("A".into(), "a".into())]), Some(("A".into(), "a".into())));
        let (subject, body) = digest(&[("Incident: dev".into(), "convex".into()), ("Incident: prod".into(), String::new())]).unwrap();
        assert_eq!(subject, "2 Condx notices: Incident: dev");
        assert_eq!(body, "**Incident: dev**\n\nconvex\n\n---\n\n**Incident: prod**");
    }

    #[test]
    fn el_html_va_junto_al_texto_si_hay() {
        let b = Body { text: "hola".into(), html: Some("<p>hola</p>".into()) };
        assert_eq!(b.json(json!({ "to": ["a@b.co"] })), json!({ "to": ["a@b.co"], "text": "hola", "html": "<p>hola</p>" }));
        let t = Body { text: "hola".into(), html: None };
        assert_eq!(t.json(json!({})), json!({ "text": "hola" }));
    }

    #[test]
    fn el_cliente_pide_al_buzon_con_la_key() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let n = sock.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = r#"{"messages":[{"message_id":"m1","labels":["received","unread"],"from":"A <a@b.co>"}]}"#;
            let _ = write!(sock, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            req
        });
        let c = Client { base, key: "am_test".into(), inbox: "heskala@agentmail.to".into() };
        let list = c.recent().unwrap();
        assert_eq!(list[0]["message_id"], "m1");
        let req = server.join().unwrap();
        assert!(req.starts_with("GET /v0/inboxes/heskala%40agentmail.to/messages?limit=30"), "{req}");
        assert!(req.contains("Bearer am_test"), "{req}");
    }
}
