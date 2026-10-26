// Eventos (docs/design/agent-flows.md, §2.1–2.2): lo que puede empezar un flujo, de todas las
// fuentes, en una sola forma. Llegan del SDK de las apps por Condx Cloud (errores del frontend, del
// backend y reportes de usuarios), de un agente externo por el MCP (`report_event`) y de los
// incidentes que abre el daemon. Cada flujo dice con sus disparadores cuáles le tocan.
//
// La misma huella no abre una segunda corrida mientras la primera sigue: se suma a ella («pasó 3
// veces más»). Si el flujo ya tiene otra corrida en curso (por otra huella), el evento espera su
// turno.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::state::Shared;
use crate::store::now_ms;

/// Las fuentes que un disparador puede elegir.
pub const SOURCES: &[&str] = &["frontend-error", "convex-error", "user-report", "mcp", "incident", "task"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Trigger {
    /// Una de `SOURCES`, o `any`.
    pub source: String,
    /// Texto que debe aparecer (sin distinguir mayúsculas) en el título, el mensaje o la función.
    #[serde(rename = "match")]
    pub match_text: Option<String>,
    /// Solo eventos de este entorno (si el evento lo dice).
    pub env: Option<String>,
}

impl Trigger {
    pub fn matches(&self, ev: &Value) -> bool {
        let source = ev["source"].as_str().unwrap_or_default();
        if self.source != "any" && self.source != source {
            return false;
        }
        if let Some(env) = self.env.as_deref().filter(|e| !e.is_empty()) {
            if ev["env"].as_str().is_some_and(|e| e != env) {
                return false;
            }
        }
        match self.match_text.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
            None => true,
            Some(m) => {
                let hay = format!("{} {} {} {}", ev["title"].as_str().unwrap_or_default(), ev["message"].as_str().unwrap_or_default(), ev["fn"].as_str().unwrap_or_default(), ev["reportText"].as_str().unwrap_or_default()).to_lowercase();
                hay.contains(&m.to_lowercase())
            }
        }
    }
}

pub fn check_trigger(t: &Trigger) -> Result<()> {
    if t.source != "any" && !SOURCES.contains(&t.source.as_str()) {
        bail!("unknown event source “{}”", t.source);
    }
    Ok(())
}

fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let mut out: String = line.chars().take(max).collect();
    if line.chars().count() > max {
        out.push('…');
    }
    out
}

/// Una fila del alcance `events:` de Condx Cloud (ingest.ts) → un evento.
pub fn from_cloud_row(row_id: &str, v: &Value) -> Value {
    let kind = v["type"].as_str().unwrap_or("error");
    let server = v["source"] == "server";
    let message = v["message"].as_str().unwrap_or_default();
    let (source, title) = match (kind, server) {
        ("report", _) => ("user-report", format!("User report: {}", first_line(v["reportText"].as_str().unwrap_or(message), 90))),
        (_, true) => ("convex-error", match v["fn"].as_str() {
            Some(f) => format!("{}: {}", f, first_line(message, 90)),
            None => first_line(message, 110),
        }),
        _ => ("frontend-error", first_line(message, 110)),
    };
    json!({
        "source": source,
        "project": v["project"],
        "title": title,
        "message": message,
        "stack": v["stack"],
        "url": v["url"],
        "fn": v["fn"],
        "app": v["app"],
        "release": v["release"],
        "breadcrumbs": v["breadcrumbs"],
        "reportText": v["reportText"],
        "reportEmail": v["reportEmail"],
        "hasScreenshot": v["screenshot"].is_string(),
        "count": v["count"].as_i64().unwrap_or(1),
        "firstAt": v["firstAt"],
        "fingerprint": if kind == "report" { format!("report:{row_id}") } else { format!("{source}:{}", v["fingerprint"].as_str().unwrap_or(row_id)) },
    })
}

/// Lo que un agente ve del evento: sin el correo de quien reportó (no lo necesita).
pub fn for_agents(ev: &Value) -> Value {
    let mut e = ev.clone();
    if let Some(o) = e.as_object_mut() {
        o.remove("reportEmail");
        o.remove("status");
        o.remove("runs");
    }
    e
}

/// Guarda un evento y lo despacha. Devuelve el evento guardado (con las corridas que empezó).
pub fn record(state: &Shared, mut ev: Value) -> Result<Value> {
    let now = now_ms();
    let fingerprint = ev["fingerprint"].as_str().map(str::to_string).filter(|f| !f.is_empty()).unwrap_or_else(|| format!("{}:{}", ev["source"].as_str().unwrap_or("event"), ev["title"].as_str().unwrap_or("")));
    let count = ev["count"].as_i64().unwrap_or(1).max(1);
    // Una huella que ya tiene evento abierto (con corrida en curso o esperando turno) suma.
    if let Some(mut prev) = state.store.event_by_fingerprint(&fingerprint)? {
        let active = matches!(prev["status"].as_str(), Some("queued" | "running"));
        if active {
            prev["count"] = json!(prev["count"].as_i64().unwrap_or(1) + count);
            prev["lastAt"] = json!(now);
            state.store.save_event(&prev)?;
            return Ok(prev);
        }
    }
    ev["id"] = json!(format!("ev{:x}{:04x}", now, (now as u64 ^ fingerprint.len() as u64) & 0xffff));
    ev["fingerprint"] = json!(fingerprint);
    ev["count"] = json!(count);
    ev["firstAt"] = json!(ev["firstAt"].as_i64().unwrap_or(now));
    ev["lastAt"] = json!(now);
    ev["status"] = json!("new");
    ev["runs"] = json!([]);
    state.store.save_event(&ev)?;
    dispatch(state, &mut ev)?;
    Ok(ev)
}

/// Empieza los flujos cuyos disparadores coinciden. Si alguno está ocupado, el evento espera.
pub fn dispatch(state: &Shared, ev: &mut Value) -> Result<()> {
    let project = ev["project"].as_str().map(str::to_string);
    let projects: Vec<String> = match project {
        Some(p) => vec![p],
        None => state.store.projects().unwrap_or_default().into_iter().map(|p| p.name).collect(),
    };
    let started: Vec<String> = ev["runs"].as_array().into_iter().flatten().filter_map(|r| r.as_str().map(str::to_string)).collect();
    let mut runs = started.clone();
    let mut waiting = false;
    let mut matched = false;
    for p in projects {
        for w in crate::workflows::list(state, &p).unwrap_or_default().into_iter().filter(|w| w.enabled && w.triggers.iter().any(|t| t.matches(ev))) {
            matched = true;
            if runs.iter().any(|r| crate::workflows::load_run(state, r).is_ok_and(|run| run.workflow_id == w.id)) {
                continue;
            }
            match crate::workflows::start_with(state, &w.id, "dev", &format!("event {}", ev["source"].as_str().unwrap_or("?")), Some(for_agents(ev))) {
                Ok(run) => {
                    tracing::info!("event {} started flow “{}” ({})", ev["id"], w.name, run.id);
                    runs.push(run.id);
                }
                Err(e) if e.to_string().contains("already has a run") => waiting = true,
                Err(e) => tracing::warn!("event {} couldn't start “{}”: {e:#}", ev["id"], w.name),
            }
        }
    }
    ev["runs"] = json!(runs);
    ev["status"] = json!(if waiting { "queued" } else if runs.is_empty() { if matched { "failed" } else { "unmatched" } } else { "running" });
    state.store.save_event(ev)?;
    Ok(())
}

/// Cada tanto: los eventos que esperaban turno, y los que terminaron (sus corridas acabaron).
pub fn tick(state: &Shared) {
    for mut ev in state.store.events(&["queued", "running"], 200).unwrap_or_default() {
        if ev["status"] == "queued" {
            let _ = dispatch(state, &mut ev);
            continue;
        }
        let open = ev["runs"].as_array().into_iter().flatten().filter_map(|r| r.as_str()).any(|r| {
            crate::workflows::load_run(state, r).is_ok_and(|run| matches!(run.status, crate::workflows::RunStatus::Running | crate::workflows::RunStatus::WaitingApproval | crate::workflows::RunStatus::WaitingAnswer))
        });
        if !open {
            ev["status"] = json!("done");
            let _ = state.store.save_event(&ev);
        }
    }
}

pub fn spawn(state: &Shared) {
    let state = state.clone();
    tokio::spawn(async move {
        let mut every = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            every.tick().await;
            let st = state.clone();
            let _ = tokio::task::spawn_blocking(move || tick(&st)).await;
        }
    });
}

/// Un evento que reporta un agente externo por el MCP.
pub fn from_mcp(args: &Value, client: &str) -> Result<Value> {
    let title = args["title"].as_str().map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| anyhow::anyhow!("give the event a title"))?;
    let details = devtool_core::redact(args["details"].as_str().unwrap_or_default());
    Ok(json!({
        "source": "mcp",
        "project": args["project"],
        "env": args["env"],
        "title": first_line(title, 140),
        "message": details.chars().take(4000).collect::<String>(),
        "severity": args["severity"].as_str().unwrap_or("unknown"),
        "reportedBy": client,
        "fingerprint": args["fingerprint"].as_str().map(|f| format!("mcp:{f}")).unwrap_or_else(|| format!("mcp:{}", title.to_lowercase())),
    }))
}

/// Una tarea que la persona pide desde el chat («agrega el campo X a Y»): cada una, su propio evento.
pub fn from_task(text: &str, project: Option<&str>) -> Result<Value> {
    let t = text.trim();
    if t.is_empty() {
        bail!("describe the task");
    }
    Ok(json!({ "source": "task", "project": project, "title": first_line(t, 120), "message": t.chars().take(4000).collect::<String>(), "fingerprint": format!("task:{}", now_ms()) }))
}

/// Un incidente recién abierto → evento (los flujos con disparador `incident`).
pub fn from_incident(inc: &Value) -> Value {
    json!({
        "source": "incident",
        "project": inc["project"],
        "env": inc["env"],
        "title": inc["title"],
        "message": inc["evidence"]["error"],
        "incident": inc["id"],
        "kind": inc["source"],
        "evidence": inc["evidence"],
        "fingerprint": format!("incident:{}", inc["fingerprint"].as_str().unwrap_or_default()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filas_de_la_nube_a_eventos() {
        let e = from_cloud_row("ev-1", &json!({ "type": "error", "source": "browser", "message": "TypeError: x is undefined\n at a", "fingerprint": "te", "count": 3, "project": "shop" }));
        assert_eq!((e["source"].as_str(), e["title"].as_str(), e["fingerprint"].as_str()), (Some("frontend-error"), Some("TypeError: x is undefined"), Some("frontend-error:te")));
        let s = from_cloud_row("ev-2", &json!({ "type": "error", "source": "server", "message": "Error: Card declined", "fn": "payments:charge", "fingerprint": "cd" }));
        assert_eq!(s["source"], "convex-error");
        assert_eq!(s["title"], "payments:charge: Error: Card declined");
        let r = from_cloud_row("ev-3", &json!({ "type": "report", "source": "browser", "message": "User report: x", "reportText": "The pay button does nothing", "reportEmail": "ana@x.com", "screenshot": "data:image/png;base64,AA" }));
        assert_eq!((r["source"].as_str(), r["title"].as_str(), r["hasScreenshot"].as_bool()), (Some("user-report"), Some("User report: The pay button does nothing"), Some(true)));
        assert!(for_agents(&r).get("reportEmail").is_none(), "agents don't get the reporter's email");
    }

    #[test]
    fn disparadores() {
        let ev = json!({ "source": "convex-error", "title": "payments:charge: Error: Card declined", "env": "prod" });
        assert!(Trigger { source: "convex-error".into(), ..Default::default() }.matches(&ev));
        assert!(Trigger { source: "any".into(), match_text: Some("CHARGE".into()), ..Default::default() }.matches(&ev));
        assert!(!Trigger { source: "frontend-error".into(), ..Default::default() }.matches(&ev));
        assert!(!Trigger { source: "any".into(), env: Some("dev".into()), ..Default::default() }.matches(&ev));
        assert!(check_trigger(&Trigger { source: "email".into(), ..Default::default() }).is_err());
        let m = from_mcp(&json!({ "title": "Checkout test fails on prod data", "details": "token sk_live_abcdefghijklmnopqrstu", "severity": "high" }), "claude-code").unwrap();
        assert_eq!((m["source"].as_str(), m["reportedBy"].as_str()), (Some("mcp"), Some("claude-code")));
        assert!(!m["message"].as_str().unwrap().contains("abcdefghijklmnop"));
        assert!(from_mcp(&json!({ "title": " " }), "x").is_err());
    }

    #[test]
    fn la_misma_huella_suma_y_no_abre_otra() {
        let st = crate::state::test_state();
        let ev = json!({ "source": "mcp", "title": "boom", "fingerprint": "mcp:boom" });
        let first = record(&st, ev.clone()).unwrap();
        // Sin flujos que lo escuchen: queda sin dueño, y uno nuevo con esa huella es otro evento.
        assert_eq!(first["status"], "unmatched");
        let mut queued = first.clone();
        queued["status"] = json!("queued");
        st.store.save_event(&queued).unwrap();
        let again = record(&st, ev).unwrap();
        assert_eq!((again["id"].clone(), again["count"].as_i64()), (first["id"].clone(), Some(2)));
    }
}
