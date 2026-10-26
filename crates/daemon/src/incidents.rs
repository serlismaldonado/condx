// Incidentes (docs/design/agents.md §4): cada señal de error (un trabajo que falló, un despliegue
// de Vercel que falló, el CI en rojo, Convex que no responde) se vuelve un registro con su
// evidencia. La misma huella es el mismo incidente: se cuentan las repeticiones y no se vuelve a
// avisar; se resuelve solo cuando la señal desaparece. Ignorarlo o resolverlo a mano es de la app.
//
// Un bucle revisa cada 20 s lo que ya miran las sondas y el historial de trabajos; aquí no se
// llama a nada externo (salvo el log de un despliegue fallido, que frontend.rs guarda por URL).

use std::collections::HashSet;
use std::time::Duration;

use regex::Regex;
use serde_json::{json, Value};

use crate::agents::Level;
use crate::state::Shared;
use crate::store::now_ms;

/// Un incidente resuelto que vuelve antes de esto es el mismo episodio (no suma una repetición).
const REOPEN_GAP_MS: i64 = 10 * 60_000;
/// Trabajos fallidos que se miran.
const JOB_WINDOW_MS: i64 = 24 * 3_600_000;
/// Repeticiones distintas que se recuerdan (ids de trabajos, de corridas, URLs).
const KEEP_OCCURRENCES: usize = 20;

/// Una señal de error, ya con su huella.
#[derive(Debug, Clone)]
pub struct Signal {
    pub fingerprint: String,
    pub source: &'static str,
    pub project: Option<String>,
    pub env: Option<String>,
    pub title: String,
    pub evidence: Value,
    /// Lo que distingue una repetición (el id del trabajo, de la corrida, la URL); `None` en las
    /// señales de estado (Convex no responde), que solo vuelven tras resolverse.
    pub occurrence: Option<String>,
}

/// La primera línea de un error sin lo que cambia de una vez a otra (números, ids, rutas
/// temporales): dos fallos iguales dan la misma huella.
pub fn normalize(error: &str) -> String {
    static NOISE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = NOISE.get_or_init(|| Regex::new(r"[0-9a-f]{8,}|\d+").unwrap());
    let first = error.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let norm = re.replace_all(&first.to_lowercase(), "#").to_string();
    norm.chars().take(120).collect()
}

fn new_id() -> String {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("inc-{:x}{:03x}", now_ms(), n % 0x1000)
}

/// Lo que cambia en un incidente con una señal nueva (`None`: nada que guardar).
pub fn observe(prev: Option<&Value>, s: &Signal, now: i64) -> Option<Value> {
    let Some(prev) = prev else {
        return Some(json!({
            "id": new_id(),
            "fingerprint": s.fingerprint,
            "source": s.source,
            "project": s.project,
            "env": s.env,
            "title": s.title,
            "evidence": s.evidence,
            "status": "open",
            "count": 1,
            "occurrences": s.occurrence.iter().collect::<Vec<_>>(),
            "firstSeen": now,
            "lastSeen": now,
            "resolvedAt": null,
            "resolvedBy": null,
        }));
    };
    let mut v = prev.clone();
    let seen: Vec<String> = v["occurrences"].as_array().into_iter().flatten().filter_map(|o| o.as_str().map(str::to_string)).collect();
    let new_occurrence = s.occurrence.as_ref().is_some_and(|o| !seen.contains(o));
    let remember = |v: &mut Value| {
        if let Some(o) = &s.occurrence {
            let mut list = seen.clone();
            list.push(o.clone());
            let skip = list.len().saturating_sub(KEEP_OCCURRENCES);
            v["occurrences"] = json!(list[skip..]);
        }
    };
    match v["status"].as_str() {
        Some("resolved") => {
            let resolved_at = v["resolvedAt"].as_i64().unwrap_or(0);
            // Con repeticiones (trabajos, corridas), solo una nueva lo reabre.
            if s.occurrence.is_some() && !new_occurrence {
                return None;
            }
            if new_occurrence || now - resolved_at > REOPEN_GAP_MS {
                v["count"] = json!(v["count"].as_i64().unwrap_or(1) + 1);
            }
            remember(&mut v);
            v["status"] = json!("open");
            v["resolvedAt"] = Value::Null;
            v["resolvedBy"] = Value::Null;
        }
        // Abierto o ignorado: suma la repetición; un ignorado sigue ignorado.
        _ if new_occurrence => {
            v["count"] = json!(v["count"].as_i64().unwrap_or(1) + 1);
            remember(&mut v);
        }
        // La misma señal de estado: solo se refresca de vez en cuando.
        _ if now - v["lastSeen"].as_i64().unwrap_or(0) < 60_000 => return None,
        _ => {}
    }
    v["lastSeen"] = json!(now);
    v["title"] = json!(s.title);
    v["evidence"] = s.evidence.clone();
    Some(v)
}

pub fn resolve(prev: &Value, by: &str, now: i64) -> Value {
    let mut v = prev.clone();
    v["status"] = json!("resolved");
    v["resolvedAt"] = json!(now);
    v["resolvedBy"] = json!(by);
    v["lastSeen"] = json!(now.max(v["lastSeen"].as_i64().unwrap_or(0)));
    v
}

/// Lo que se miró en una vuelta: las señales activas y, por fuente, qué huellas se pudieron
/// revisar (las de una sonda que no respondió no se resuelven por falta de datos).
#[derive(Default)]
pub struct Scan {
    pub signals: Vec<Signal>,
    /// Prefijos de huella que se revisaron en esta vuelta.
    pub observed: Vec<String>,
}

fn project_of_env(state: &Shared, env: &str) -> Option<String> {
    state.store.environment(env).ok().flatten().and_then(|e| e.project)
}

fn redact(v: &Value) -> Value {
    match v.as_str() {
        Some(s) => json!(devtool_core::redact(s)),
        None => v.clone(),
    }
}

/// Trabajos que fallaron en las últimas 24 h y que nada arregló después (la misma receta en el
/// mismo entorno salió bien más tarde).
fn scan_jobs(state: &Shared, now: i64, scan: &mut Scan) {
    let mut last_ok: std::collections::HashMap<(String, String), i64> = Default::default();
    for j in state.store.jobs(&["succeeded"], 200).unwrap_or_default() {
        let key = (j["recipe"].as_str().unwrap_or("").to_string(), j["env"].as_str().unwrap_or("").to_string());
        let at = j["endedAt"].as_i64().unwrap_or(0);
        last_ok.entry(key).and_modify(|v| *v = (*v).max(at)).or_insert(at);
    }
    for job in state.store.jobs(&["failed"], 200).unwrap_or_default() {
        let ended = job["endedAt"].as_i64().unwrap_or(0);
        if now - ended > JOB_WINDOW_MS {
            continue;
        }
        let key = (job["recipe"].as_str().unwrap_or("").to_string(), job["env"].as_str().unwrap_or("").to_string());
        if last_ok.get(&key).is_some_and(|ok| *ok > ended) {
            continue;
        }
        let (recipe, env) = (job["recipe"].as_str().unwrap_or("?"), job["env"].as_str().unwrap_or("?"));
        let error = job["error"].as_str().unwrap_or("failed");
        scan.signals.push(Signal {
            fingerprint: format!("job:{recipe}:{env}:{}", normalize(error)),
            source: "job",
            project: project_of_env(state, env),
            env: Some(env.to_string()),
            title: format!("{} failed on {env}", job["title"].as_str().unwrap_or(recipe)),
            evidence: json!({ "jobId": job["id"], "recipe": recipe, "error": redact(&job["error"]), "commit": job["commit"], "endedAt": job["endedAt"], "log": job["id"].as_str().map(job_log_tail) }),
            occurrence: job["id"].as_str().map(str::to_string),
        });
    }
}

/// El final del log de un trabajo (ya redactado al escribirse): ahí está el comando, sus
/// argumentos y el error de verdad; sin esto el agente solo ve «exited with code 1».
fn job_log_tail(id: &str) -> String {
    tail_of(&crate::jobs::read_log(id, 0).unwrap_or_default().0)
}

/// Las últimas 40 líneas, como mucho 4000 bytes.
fn tail_of(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut tail = lines[lines.len().saturating_sub(40)..].join("\n");
    if tail.len() > 4_000 {
        let cut = tail.len() - 4_000;
        let at = (cut..tail.len()).find(|i| tail.is_char_boundary(*i)).unwrap_or(tail.len());
        tail = format!("…{}", &tail[at..]);
    }
    tail
}

/// Si el error es de la red de esta Mac (sin DNS, sin conexión) y no del deployment.
fn local_network_error(error: &str) -> bool {
    let e = error.to_lowercase();
    ["dns", "lookup", "resolve", "nodename nor servname", "name or service not known", "no address associated", "network is unreachable", "network is down", "internet connection appears to be offline"]
        .iter()
        .any(|k| e.contains(k))
}

/// Convex de cada entorno de la nube que no responde. Si la Mac se quedó sin red (un error de DNS, o
/// todos los de la nube fallan a la vez) no es un incidente de Convex: no se abre ninguno.
fn scan_convex(state: &Shared, scan: &mut Scan) {
    let cloud: Vec<(devtool_core::Environment, Value)> = state
        .store
        .environments()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.convex_url.is_some() && matches!(e.kind, devtool_core::EnvKind::CloudDev | devtool_core::EnvKind::CloudProd))
        .filter_map(|e| state.probe(&format!("convex:{}", e.name)).map(|p| (e, p)))
        .collect();
    let failing = cloud.iter().filter(|(_, p)| p["ok"] != true).count();
    let all_down = cloud.len() >= 2 && failing == cloud.len();
    for env in state.store.environments().unwrap_or_default() {
        let Some(p) = state.probe(&format!("convex:{}", env.name)) else { continue };
        let fp = format!("convex:{}", env.name);
        // Sin red en la Mac no se sabe nada del deployment: ni se abre ni se cierra su incidente.
        if p["ok"] != true && (all_down || local_network_error(p["error"].as_str().unwrap_or(""))) {
            continue;
        }
        scan.observed.push(fp.clone());
        // Un devbox o un desechable apagado no es un incidente: solo los de la nube.
        if p["ok"] == true || env.convex_url.is_none() || !matches!(env.kind, devtool_core::EnvKind::CloudDev | devtool_core::EnvKind::CloudProd) {
            continue;
        }
        scan.signals.push(Signal {
            fingerprint: fp,
            source: "convex",
            project: env.project.clone(),
            env: Some(env.name.clone()),
            title: format!("Convex for {} isn't responding", env.name),
            evidence: json!({ "error": redact(&p["error"]), "url": env.convex_url, "checkedAt": p["at"] }),
            occurrence: None,
        });
    }
}

/// La última corrida terminada de cada workflow de CI, si falló.
fn scan_ci(state: &Shared, scan: &mut Scan) {
    for p in state.probes().into_iter().filter(|p| p["key"].as_str().is_some_and(|k| k == "ci" || k.starts_with("ci:"))) {
        if p["ok"] != true {
            continue;
        }
        let project = p["key"].as_str().and_then(|k| k.strip_prefix("ci:")).map(str::to_string);
        for repo in p["data"]["repos"].as_array().into_iter().flatten() {
            let slug = repo["repo"].as_str().unwrap_or("?");
            scan.observed.push(format!("ci:{slug}:"));
            let mut done = HashSet::new();
            for run in repo["runs"].as_array().into_iter().flatten().filter(|r| r["status"] == "completed") {
                let wf = run["workflowName"].as_str().unwrap_or("?").to_string();
                if !done.insert(wf.clone()) || run["conclusion"] != "failure" {
                    continue;
                }
                scan.signals.push(Signal {
                    fingerprint: format!("ci:{slug}:{wf}"),
                    source: "ci",
                    project: project.clone(),
                    env: None,
                    title: format!("CI “{wf}” failed on {}", run["headBranch"].as_str().unwrap_or("?")),
                    evidence: json!({ "repo": slug, "workflow": wf, "branch": run["headBranch"], "commit": run["headSha"], "title": run["displayTitle"], "url": run["url"] }),
                    occurrence: run["databaseId"].as_i64().map(|i| i.to_string()),
                });
            }
        }
    }
}

/// El último despliegue de Vercel que falló, si nada salió bien en producción después.
async fn scan_vercel(state: &Shared, scan: &mut Scan) {
    let Some(data) = state.probe_data("vercel") else { return };
    scan.observed.push("vercel:".into());
    let failed = &data["lastFailed"];
    if !failed.is_object() {
        return;
    }
    let after_prod = failed["createdAt"].as_i64().unwrap_or(0) > data["production"]["createdAt"].as_i64().unwrap_or(0);
    if !after_prod {
        return;
    }
    let branch = failed["branch"].as_str().unwrap_or("?");
    let url = failed["url"].as_str().unwrap_or_default().to_string();
    let log = match state.config.repo.clone() {
        Some(repo) if !url.is_empty() => crate::frontend::failed_log(&repo, &url).await,
        _ => Value::Null,
    };
    scan.signals.push(Signal {
        fingerprint: format!("vercel:{branch}"),
        source: "vercel",
        project: None,
        env: None,
        title: format!("Vercel deploy of {branch} failed"),
        evidence: json!({ "url": url, "branch": branch, "commit": failed["commit"], "target": failed["target"], "log": log["tail"] }),
        occurrence: Some(url),
    });
}

/// Aplica una vuelta: abre, cuenta y resuelve. Devuelve los incidentes nuevos o reabiertos.
pub fn apply(state: &Shared, scan: &Scan, settings: &crate::agents::Settings, now: i64) -> Vec<Value> {
    let mut opened = vec![];
    let mut active = HashSet::new();
    for s in &scan.signals {
        if settings.level(s.source) == Level::Off {
            continue;
        }
        active.insert(s.fingerprint.clone());
        let prev = state.store.incident_by_fingerprint(&s.fingerprint).ok().flatten();
        let was_open = prev.as_ref().is_some_and(|p| p["status"] != "resolved");
        if let Some(v) = observe(prev.as_ref(), s, now) {
            if let Err(e) = state.store.save_incident(&v) {
                tracing::warn!("couldn't save incident: {e}");
                continue;
            }
            if !was_open && v["status"] == "open" {
                tracing::info!("incident {}: {}", v["id"].as_str().unwrap_or("?"), v["title"].as_str().unwrap_or(""));
                opened.push(v);
            }
        }
    }
    for inc in state.store.incidents(&["open", "ignored"], 500).unwrap_or_default() {
        let fp = inc["fingerprint"].as_str().unwrap_or_default();
        if active.contains(fp) {
            continue;
        }
        let gone = if inc["source"] == "job" {
            // Un trabajo se arregla cuando la misma receta sale bien después en el mismo entorno.
            later_success(state, &inc)
        } else {
            scan.observed.iter().any(|p| fp.starts_with(p.as_str()))
        };
        if gone {
            let _ = state.store.save_incident(&resolve(&inc, "auto", now));
        }
    }
    opened
}

fn later_success(state: &Shared, inc: &Value) -> bool {
    // Contra cuándo falló el último trabajo (no cuándo se detectó): un éxito entre medio también cuenta.
    let (recipe, env) = (&inc["evidence"]["recipe"], &inc["env"]);
    let failed_at = inc["evidence"]["endedAt"].as_i64().unwrap_or_else(|| inc["lastSeen"].as_i64().unwrap_or(0));
    state.store.jobs(&["succeeded"], 200).unwrap_or_default().iter().any(|j| &j["recipe"] == recipe && &j["env"] == env && j["endedAt"].as_i64().unwrap_or(0) > failed_at)
}

pub async fn tick(state: &Shared) -> Vec<Value> {
    let now = now_ms();
    let mut scan = Scan::default();
    scan_jobs(state, now, &mut scan);
    scan_convex(state, &mut scan);
    scan_ci(state, &mut scan);
    scan_vercel(state, &mut scan).await;
    let settings = crate::agents::settings(state);
    apply(state, &scan, &settings, now)
}

/// Un incidente que vuelve no avisa otra vez antes de esto (uno que va y viene no llena el teléfono).
const NOTIFY_AGAIN_MS: i64 = 30 * 60_000;

/// Avisa de los recién abiertos, salvo los que avisaron hace poco; guarda cuándo (`notifiedAt`).
/// Devuelve los que avisó.
pub fn notify_opened(state: &Shared, opened: &[Value], now: i64) -> Vec<String> {
    let mut sent = vec![];
    for inc in opened {
        if inc["notifiedAt"].as_i64().is_some_and(|at| now - at < NOTIFY_AGAIN_MS) {
            continue;
        }
        crate::notify::incident_opened(state, inc);
        let mut v = inc.clone();
        v["notifiedAt"] = json!(now);
        let _ = state.store.save_incident(&v);
        sent.extend(inc["id"].as_str().map(str::to_string));
    }
    sent
}

pub fn spawn(state: &Shared) {
    let state = state.clone();
    tokio::spawn(async move {
        let mut every = tokio::time::interval(Duration::from_secs(20));
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            let opened = tick(&state).await;
            notify_opened(&state, &opened, now_ms());
            // Los flujos con disparador «incident» también lo ven.
            for inc in &opened {
                if let Err(e) = crate::events::record(&state, crate::events::from_incident(inc)) {
                    tracing::warn!("incident event: {e:#}");
                }
            }
            crate::agent_host::auto_start(&state, &opened).await;
        }
    });
}

/// Lo que la persona decide en la app: ignorar, resolver o reabrir.
pub fn act(state: &Shared, id: &str, action: &str) -> anyhow::Result<Value> {
    let inc = state.store.incident(id)?.ok_or_else(|| anyhow::anyhow!("there's no incident “{id}”"))?;
    let now = now_ms();
    let v = match action {
        "ignore" => {
            let mut v = inc;
            v["status"] = json!("ignored");
            v
        }
        "resolve" => resolve(&inc, "app", now),
        "reopen" => {
            let mut v = inc;
            v["status"] = json!("open");
            v["resolvedAt"] = Value::Null;
            v["resolvedBy"] = Value::Null;
            v
        }
        other => anyhow::bail!("unknown action “{other}”: ignore, resolve or reopen"),
    };
    state.store.save_incident(&v)?;
    Ok(v)
}

/// Los incidentes para la app y el MCP: `status` = open (abiertos e ignorados), resolved o all.
pub fn list(state: &Shared, status: Option<&str>, project: Option<&str>, limit: usize) -> Vec<Value> {
    let statuses: &[&str] = match status.unwrap_or("open") {
        "open" => &["open", "ignored"],
        "resolved" => &["resolved"],
        _ => &[],
    };
    let default = crate::projects::DEFAULT;
    state
        .store
        .incidents(statuses, 500)
        .unwrap_or_default()
        .into_iter()
        .filter(|i| match project {
            None | Some("") => true,
            Some(p) => i["project"].as_str().unwrap_or(default) == p,
        })
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn sin_red_en_la_mac_no_es_un_incidente_de_convex() {
        assert!(local_network_error("error sending request: dns error: failed to lookup address information: nodename nor servname provided"));
        assert!(local_network_error("No address associated with hostname"));
        assert!(!local_network_error("HTTP 503 from the deployment"));
    }

    use super::*;

    fn sig(fp: &str, occ: Option<&str>) -> Signal {
        Signal { fingerprint: fp.into(), source: "job", project: None, env: Some("dev".into()), title: "Deploy failed on dev".into(), evidence: json!({ "recipe": "deploy" }), occurrence: occ.map(str::to_string) }
    }

    #[test]
    fn la_huella_ignora_numeros_e_ids() {
        assert_eq!(normalize("Error: timeout after 3021 ms (job 7f3a9c21b0)\nstack…"), normalize("Error: timeout after 15 ms (job 0011aabbcc)"));
        assert_ne!(normalize("Error: timeout"), normalize("Error: schema invalid"));
    }

    #[test]
    fn la_misma_huella_cuenta_repeticiones_sin_reabrir() {
        let a = observe(None, &sig("job:x", Some("j1")), 1_000).unwrap();
        assert_eq!(a["count"], 1);
        assert_eq!(a["status"], "open");
        // El mismo trabajo otra vez: nada que guardar.
        assert!(observe(Some(&a), &sig("job:x", Some("j1")), 2_000).is_none());
        let b = observe(Some(&a), &sig("job:x", Some("j2")), 3_000).unwrap();
        assert_eq!(b["count"], 2);
        assert_eq!(b["id"], a["id"]);
        assert_eq!(b["occurrences"], json!(["j1", "j2"]));
    }

    #[test]
    fn un_ignorado_sigue_ignorado_y_un_resuelto_vuelve() {
        let mut a = observe(None, &sig("job:x", Some("j1")), 0).unwrap();
        a["status"] = json!("ignored");
        let b = observe(Some(&a), &sig("job:x", Some("j2")), 10).unwrap();
        assert_eq!(b["status"], "ignored");
        let r = resolve(&b, "auto", 20);
        // El mismo trabajo de antes no lo reabre; uno nuevo sí.
        assert!(observe(Some(&r), &sig("job:x", Some("j2")), 30).is_none());
        let c = observe(Some(&r), &sig("job:x", Some("j3")), 40).unwrap();
        assert_eq!(c["status"], "open");
        assert_eq!(c["count"], 3);
    }

    #[test]
    fn una_senal_de_estado_que_vuelve_pronto_es_el_mismo_episodio() {
        let s = Signal { occurrence: None, source: "convex", ..sig("convex:dev", None) };
        let a = observe(None, &s, 0).unwrap();
        let r = resolve(&a, "auto", 1_000);
        let back = observe(Some(&r), &s, 1_000 + 60_000).unwrap();
        assert_eq!((back["status"].as_str(), back["count"].as_i64()), (Some("open"), Some(1)));
        let r2 = resolve(&back, "auto", 200_000);
        let later = observe(Some(&r2), &s, 200_000 + REOPEN_GAP_MS + 1).unwrap();
        assert_eq!(later["count"], 2);
    }

    #[test]
    fn la_vuelta_abre_resuelve_y_respeta_off() {
        let st = crate::state::test_state();
        let settings = crate::agents::Settings::default();
        let convex = |fp: &str| Signal { occurrence: None, source: "convex", ..sig(fp, None) };
        let scan = Scan { signals: vec![convex("convex:dev"), sig("job:deploy:dev:boom", Some("j1"))], observed: vec!["convex:dev".into(), "convex:prod".into()] };
        let opened = apply(&st, &scan, &settings, 1_000);
        assert_eq!(opened.len(), 2);
        // La siguiente vuelta: Convex responde (observado y sin señal) → se resuelve; el trabajo sigue.
        let scan2 = Scan { signals: vec![sig("job:deploy:dev:boom", Some("j1"))], observed: vec!["convex:dev".into()] };
        assert!(apply(&st, &scan2, &settings, 2_000).is_empty());
        assert_eq!(st.store.incident_by_fingerprint("convex:dev").unwrap().unwrap()["status"], "resolved");
        assert_eq!(st.store.incident_by_fingerprint("job:deploy:dev:boom").unwrap().unwrap()["status"], "open");
        // Una fuente apagada no registra nada nuevo.
        let mut off = settings.clone();
        off.autonomy.insert("ci".into(), Level::Off);
        let ci = Signal { source: "ci", ..sig("ci:o/r:Tests", Some("9")) };
        assert!(apply(&st, &Scan { signals: vec![ci], observed: vec![] }, &off, 3_000).is_empty());
        assert!(st.store.incident_by_fingerprint("ci:o/r:Tests").unwrap().is_none());
        // Sin datos de una fuente (sonda caída), no se resuelve por falta de datos.
        let s3 = Scan { signals: vec![convex("convex:dev")], observed: vec![] };
        apply(&st, &s3, &settings, 4_000);
        assert!(apply(&st, &Scan::default(), &settings, 5_000).is_empty());
        assert_eq!(st.store.incident_by_fingerprint("convex:dev").unwrap().unwrap()["status"], "open");
    }

    #[test]
    fn la_app_ignora_resuelve_y_reabre() {
        let st = crate::state::test_state();
        let opened = apply(&st, &Scan { signals: vec![sig("job:a", Some("j1"))], observed: vec![] }, &crate::agents::Settings::default(), 0);
        let id = opened[0]["id"].as_str().unwrap();
        assert_eq!(act(&st, id, "ignore").unwrap()["status"], "ignored");
        assert_eq!(list(&st, None, None, 10).len(), 1, "ignored still shows among the open ones");
        assert_eq!(act(&st, id, "resolve").unwrap()["resolvedBy"], "app");
        assert!(list(&st, None, None, 10).is_empty());
        assert_eq!(list(&st, Some("resolved"), None, 10).len(), 1);
        assert_eq!(act(&st, id, "reopen").unwrap()["status"], "open");
        assert!(act(&st, id, "delete").is_err());
    }

    #[test]
    fn un_trabajo_que_despues_sale_bien_resuelve_su_incidente() {
        let st = crate::state::test_state();
        let job = |id: &str, status: &str, ended: i64, error: Value| json!({ "id": id, "recipe": "seed", "title": "Seed data", "env": "dev", "status": status, "error": error, "endedAt": ended });
        let now = 10_000_000;
        st.store.save_job("f1", now - 5_000, "failed", &job("f1", "failed", now - 5_000, json!("Uncaught Error: boom 12"))).unwrap();
        let mut scan = Scan::default();
        scan_jobs(&st, now, &mut scan);
        let settings = crate::agents::Settings::default();
        assert_eq!(apply(&st, &scan, &settings, now).len(), 1);
        // Sale bien después: la falla ya no es una señal y el incidente se resuelve.
        st.store.save_job("ok1", now - 1_000, "succeeded", &job("ok1", "succeeded", now - 1_000, Value::Null)).unwrap();
        let mut scan = Scan::default();
        scan_jobs(&st, now + 20_000, &mut scan);
        assert!(scan.signals.is_empty());
        apply(&st, &scan, &settings, now + 20_000);
        let inc = &list(&st, Some("resolved"), None, 10)[0];
        assert_eq!(inc["resolvedBy"], "auto");
    }

    #[test]
    fn un_incidente_que_va_y_viene_no_avisa_cada_vez() {
        let st = crate::state::test_state();
        let convex = Signal { occurrence: None, source: "convex", ..sig("convex:dev", None) };
        let settings = crate::agents::Settings::default();
        let opened = apply(&st, &Scan { signals: vec![convex.clone()], observed: vec!["convex:dev".into()] }, &settings, 0);
        assert_eq!(notify_opened(&st, &opened, 0).len(), 1);
        // Se resuelve y vuelve a los 2 minutos: es el mismo episodio, sin otro aviso.
        apply(&st, &Scan { signals: vec![], observed: vec!["convex:dev".into()] }, &settings, 60_000);
        let back = apply(&st, &Scan { signals: vec![convex.clone()], observed: vec!["convex:dev".into()] }, &settings, 120_000);
        assert_eq!(back.len(), 1, "reopened");
        assert!(notify_opened(&st, &back, 120_000).is_empty());
        // Pasada la pausa, sí.
        apply(&st, &Scan { signals: vec![], observed: vec!["convex:dev".into()] }, &settings, 200_000);
        let later = apply(&st, &Scan { signals: vec![convex], observed: vec!["convex:dev".into()] }, &settings, 200_000 + NOTIFY_AGAIN_MS);
        assert_eq!(notify_opened(&st, &later, 200_000 + NOTIFY_AGAIN_MS).len(), 1);
    }

    #[test]
    fn el_final_del_log_va_en_la_evidencia() {
        let log = (1..=100).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n") + "\nArgumentValidationError: Object contains extra field `_id`";
        let tail = tail_of(&log);
        assert!(tail.starts_with("line 62") && tail.ends_with("extra field `_id`"), "{tail}");
        let long = "é".repeat(5_000);
        let t = tail_of(&long);
        assert!(t.starts_with('…') && t.len() <= 4_004);
        assert_eq!(tail_of(""), "");
    }

}

