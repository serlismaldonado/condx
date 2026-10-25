// El host de agentes (agent-host/, Node con pi durable) y las corridas que el daemon le encarga
// (docs/design/agents.md, fase A2). El daemon decide y limita; el host ejecuta:
// - Lo arranca cuando hace falta: `node --experimental-strip-types agent-host/src/main.ts`, con su
//   configuración en una línea por stdin (las API keys descifradas viajan así: nunca en el entorno
//   ni en disco) y el puerto en la primera línea de stdout. Si el daemon se va, el host también
//   (se le cierra el stdin).
// - Le da un token de solo lectura (`auth::AGENT`) para que sus herramientas lean de Condx.
// - Una corrida diagnostica un incidente: arranca a mano (la app) o sola según la autonomía; el
//   daemon la sigue, guarda la respuesta en el incidente, suma el gasto del día y la detiene si
//   pasa el presupuesto. Cuántas a la vez también lo decide aquí.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use crate::agents::Level;
use crate::state::Shared;
use crate::store::now_ms;

const USAGE_KEY: &str = "meta:agent-usage";

struct Running {
    child: Child,
    _stdin: ChildStdin,
    port: u16,
    token: String,
}

#[derive(Default)]
pub struct AgentHost {
    running: Mutex<Option<Running>>,
    last_error: Mutex<Option<String>>,
}

/// La carpeta del host: `CONDX_AGENT_HOST_DIR`, la instalada (`~/.condx/daemon/agent-host`) o, en
/// desarrollo, la del checkout junto al binario.
pub fn host_dir() -> Option<PathBuf> {
    let has = |p: &PathBuf| p.join("src/main.ts").exists() && p.join("node_modules").exists();
    if let Ok(d) = std::env::var("CONDX_AGENT_HOST_DIR") {
        return Some(PathBuf::from(d)).filter(has);
    }
    let installed = crate::config::home().join("agent-host");
    if has(&installed) {
        return Some(installed);
    }
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().map(|a| a.join("agent-host")).find(has)
}

fn node() -> String {
    std::env::var("CONDX_NODE").unwrap_or_else(|_| "node".into())
}

impl AgentHost {
    pub fn status(&self) -> Value {
        let mut running = self.running.lock().unwrap();
        let alive = running.as_mut().is_some_and(|r| matches!(r.child.try_wait(), Ok(None)));
        json!({
            "available": host_dir().is_some(),
            "dir": host_dir().map(|d| d.display().to_string()),
            "running": alive,
            "error": self.last_error.lock().unwrap().clone(),
        })
    }

    /// El puerto y el token del host, arrancándolo si no está (bloqueante).
    fn ensure(&self, state: &Shared) -> Result<(u16, String)> {
        let mut running = self.running.lock().unwrap();
        if let Some(r) = running.as_mut() {
            if matches!(r.child.try_wait(), Ok(None)) {
                return Ok((r.port, r.token.clone()));
            }
        }
        *running = None;
        let started = spawn(state);
        *self.last_error.lock().unwrap() = started.as_ref().err().map(|e| format!("{e:#}"));
        let r = started?;
        let out = (r.port, r.token.clone());
        *running = Some(r);
        Ok(out)
    }

    /// Un pedido al host (bloqueante). Si no responde, lo arranca de nuevo una vez.
    pub fn call(&self, state: &Shared, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        for attempt in 0..2 {
            let (port, token) = self.ensure(state)?;
            let req = ureq::request(method, &format!("http://127.0.0.1:{port}{path}")).set("Authorization", &format!("Bearer {token}")).timeout(Duration::from_secs(30));
            let res = match &body {
                Some(b) => req.send_json(b.clone()),
                None => req.call(),
            };
            match res {
                Ok(r) => return r.into_json().map_err(|e| anyhow!("agent host: {e}")),
                Err(ureq::Error::Status(code, r)) => {
                    let v: Value = r.into_json().unwrap_or(Value::Null);
                    bail!("agent host ({code}): {}", v["error"].as_str().unwrap_or("error"));
                }
                Err(e) if attempt == 0 => {
                    tracing::warn!("agent host didn't answer ({e}); starting it again");
                    self.stop();
                }
                Err(e) => bail!("agent host: {e}"),
            }
        }
        unreachable!()
    }

    pub fn stop(&self) {
        if let Some(mut r) = self.running.lock().unwrap().take() {
            let _ = r.child.kill();
            let _ = r.child.wait();
        }
    }
}

/// Los proveedores con sus claves descifradas, para el host (bloqueante: lee el Llavero).
fn providers(state: &Shared) -> Result<Vec<Value>> {
    let mut out = vec![];
    for p in state.store.agent_providers()? {
        let id = p["id"].as_str().unwrap_or_default();
        let key = crate::agents::api_key(state, id)?;
        out.push(json!({ "id": id, "kind": p["kind"], "baseUrl": p["baseUrl"], "model": p["model"], "apiKey": key }));
    }
    Ok(out)
}

fn spawn(state: &Shared) -> Result<Running> {
    let dir = host_dir().context("the agent host isn't installed (agent-host/ with node_modules)")?;
    let mut child = Command::new(node())
        .args(["--experimental-strip-types", "--no-warnings", "src/main.ts"])
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("couldn't start {} (is Node installed?)", node()))?;
    let token = crate::auth::random_token()?;
    let faux = std::env::var("CONDX_AGENT_FAUX").ok().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let config = json!({
        "hostToken": token,
        "daemonUrl": format!("http://127.0.0.1:{}", state.config.port),
        "daemonToken": state.tokens.agent,
        "storage": crate::config::home().join("agents.sqlite").display().to_string(),
        "providers": if faux.is_some() { json!([]) } else { json!(providers(state)?) },
        "faux": faux,
    });
    let mut stdin = child.stdin.take().context("no stdin")?;
    writeln!(stdin, "{config}")?;
    stdin.flush()?;
    let stdout = child.stdout.take().context("no stdout")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let mut reader = BufReader::new(stdout);
        let _ = reader.read_line(&mut line);
        let _ = tx.send(line);
        // El resto de su salida se descarta (sus errores van por stderr al log del daemon).
        let mut sink = String::new();
        while reader.read_line(&mut sink).map(|n| n > 0).unwrap_or(false) {
            sink.clear();
        }
    });
    let line = match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(l) => l,
        Err(_) => {
            let _ = child.kill();
            bail!("the agent host didn't start in 30 s");
        }
    };
    let port = serde_json::from_str::<Value>(line.trim()).ok().and_then(|v| v["listening"].as_u64()).filter(|p| *p > 0).with_context(|| {
        let _ = child.kill();
        format!("the agent host didn't start: {}", line.trim())
    })? as u16;
    tracing::info!("agent host started on 127.0.0.1:{port} ({})", dir.display());
    Ok(Running { child, _stdin: stdin, port, token })
}

/// Los proveedores cambiaron: si el host corre, recibe la lista nueva.
pub async fn push_providers(state: &Shared) {
    let st = state.clone();
    let _ = tokio::task::spawn_blocking(move || {
        if !st.agent_host.status()["running"].as_bool().unwrap_or(false) {
            return;
        }
        match providers(&st) {
            Ok(list) => {
                if let Err(e) = st.agent_host.call(&st, "POST", "/providers", Some(json!({ "providers": list }))) {
                    tracing::warn!("couldn't update the agent host's providers: {e:#}");
                }
            }
            Err(e) => tracing::warn!("couldn't read the providers: {e:#}"),
        }
    })
    .await;
}

fn today() -> i64 {
    now_ms() / 86_400_000
}

/// Tokens gastados hoy (UTC) entre todas las corridas.
pub fn usage_today(state: &Shared) -> u64 {
    let v = state.store.probes().ok().and_then(|l| l.into_iter().find(|(k, _)| k == USAGE_KEY)).map(|(_, v)| v).unwrap_or(Value::Null);
    if v["day"].as_i64() == Some(today()) { v["tokens"].as_u64().unwrap_or(0) } else { 0 }
}

fn add_usage(state: &Shared, tokens: u64) {
    if tokens == 0 {
        return;
    }
    let total = usage_today(state) + tokens;
    let _ = state.store.save_probe(USAGE_KEY, &json!({ "day": today(), "tokens": total }));
}

fn repo_for(state: &Shared, incident: &Value) -> PathBuf {
    crate::projects::get(state, incident["project"].as_str()).map(|p| p.repo).unwrap_or_else(|_| state.config.workdir())
}

/// Arranca una corrida de diagnóstico para un incidente. `by`: `app` o `auto`.
pub async fn start(state: &Shared, incident_id: &str, by: &str) -> Result<Value> {
    let incident = state.store.incident(incident_id)?.with_context(|| format!("there's no incident “{incident_id}”"))?;
    let settings = crate::agents::settings(state);
    let provider_id = settings.default_provider.clone().context("choose a default provider in Settings → Agents first")?;
    let provider = state.store.agent_providers()?.into_iter().find(|p| p["id"] == provider_id.as_str()).with_context(|| format!("there's no provider “{provider_id}”"))?;
    let model = settings.default_model.clone().or_else(|| provider["model"].as_str().map(str::to_string)).filter(|m| !m.is_empty()).context("choose a model for the provider")?;
    if settings.daily_token_budget == 0 || usage_today(state) >= settings.daily_token_budget {
        bail!("today's token budget is used up ({} of {})", usage_today(state), settings.daily_token_budget);
    }
    let running = state.store.agent_runs(&["running"], None)?;
    if running.iter().any(|r| r["incident"] == incident_id) {
        bail!("an agent is already working on this incident");
    }
    if running.len() as u32 >= settings.max_concurrent {
        bail!("{} agent(s) already running (the limit is {})", running.len(), settings.max_concurrent);
    }
    let n = state.store.agent_runs(&[], Some(incident_id))?.len() + 1;
    let run_id = format!("run-{incident_id}-{n}");
    let cwd = repo_for(state, &incident);
    let payload = json!({
        "title": incident["title"], "source": incident["source"], "project": incident["project"], "env": incident["env"],
        "count": incident["count"], "firstSeen": incident["firstSeen"], "lastSeen": incident["lastSeen"], "evidence": incident["evidence"],
    });
    let body = json!({ "runId": run_id, "mode": "diagnose", "providerId": provider_id, "model": model, "cwd": cwd.display().to_string(), "incident": payload });
    let st = state.clone();
    let res = tokio::task::spawn_blocking(move || st.agent_host.call(&st, "POST", "/runs", Some(body))).await.map_err(|e| anyhow!(e.to_string()))??;
    let run = json!({
        "id": run_id, "incident": incident_id, "mode": "diagnose", "provider": provider_id, "model": model, "by": by,
        "status": "running", "startedAt": now_ms(), "endedAt": null, "answer": null, "error": null,
        "usage": { "total": 0 }, "conversationId": res["conversationId"],
    });
    state.store.save_agent_run(&run)?;
    set_incident_agent(state, incident_id, &run);
    tracing::info!("agent {run_id} started on {incident_id} ({by})");
    Ok(run)
}

fn set_incident_agent(state: &Shared, incident_id: &str, run: &Value) {
    if let Ok(Some(mut inc)) = state.store.incident(incident_id) {
        let answer = run["answer"].as_str().map(|a| a.chars().take(4_000).collect::<String>());
        inc["agent"] = json!({ "runId": run["id"], "status": run["status"], "answer": answer, "at": now_ms(), "model": run["model"] });
        let _ = state.store.save_incident(&inc);
    }
}

/// Una vuelta de seguimiento de las corridas que siguen trabajando.
pub async fn poll(state: &Shared) {
    let settings = crate::agents::settings(state);
    for run in state.store.agent_runs(&["running"], None).unwrap_or_default() {
        let id = run["id"].as_str().unwrap_or_default().to_string();
        let st = state.clone();
        let path = format!("/runs/{id}");
        let live = tokio::task::spawn_blocking(move || st.agent_host.call(&st, "GET", &path, None)).await;
        let live = match live {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                // El host no conoce la corrida (otro almacén): se da por perdida.
                if e.to_string().contains("there's no run") {
                    finish(state, run, "failed", None, Some("the agent host lost this run".into()), 0);
                }
                continue;
            }
            Err(_) => continue,
        };
        let total = live["usage"]["total"].as_u64().unwrap_or(0);
        let before = run["usage"]["total"].as_u64().unwrap_or(0);
        let delta = total.saturating_sub(before);
        let status = live["status"].as_str().unwrap_or("running");
        if status == "running" {
            add_usage(state, delta);
            let mut r = run.clone();
            r["usage"] = live["usage"].clone();
            let _ = state.store.save_agent_run(&r);
            // Sobre el presupuesto: se detiene.
            if usage_today(state) >= settings.daily_token_budget {
                let st = state.clone();
                let path = format!("/runs/{id}/abort");
                let _ = tokio::task::spawn_blocking(move || st.agent_host.call(&st, "POST", &path, Some(json!({})))).await;
                finish(state, r, "stopped", live["answer"].as_str().map(str::to_string), Some("today's token budget ran out".into()), 0);
            }
            continue;
        }
        let mut r = run.clone();
        r["usage"] = live["usage"].clone();
        finish(state, r, status, live["answer"].as_str().map(str::to_string), live["reason"].as_str().map(str::to_string), delta);
    }
}

fn finish(state: &Shared, mut run: Value, status: &str, answer: Option<String>, error: Option<String>, delta: u64) {
    add_usage(state, delta);
    run["status"] = json!(status);
    run["endedAt"] = json!(now_ms());
    run["answer"] = json!(answer);
    run["error"] = json!(error.filter(|_| status != "done"));
    let _ = state.store.save_agent_run(&run);
    if let Some(inc) = run["incident"].as_str() {
        set_incident_agent(state, inc, &run);
    }
    tracing::info!("agent {} {status}", run["id"].as_str().unwrap_or("?"));
}

/// Lo que la app ve de una corrida: lo guardado y, si el host la tiene, la transcripción en vivo.
pub async fn get(state: &Shared, id: &str) -> Result<Value> {
    let mut run = state.store.agent_run(id)?.with_context(|| format!("there's no run “{id}”"))?;
    let st = state.clone();
    let path = format!("/runs/{id}");
    if let Ok(Ok(live)) = tokio::task::spawn_blocking(move || st.agent_host.call(&st, "GET", &path, None)).await {
        run["transcript"] = live["transcript"].clone();
        if run["status"] == "running" {
            run["answer"] = live["answer"].clone();
            run["usage"] = live["usage"].clone();
        }
    }
    Ok(run)
}

/// Un mensaje de la persona: lo redirige si trabaja, o sigue la conversación (y vuelve a correr).
pub async fn message(state: &Shared, id: &str, text: &str) -> Result<Value> {
    let mut run = state.store.agent_run(id)?.with_context(|| format!("there's no run “{id}”"))?;
    if text.trim().is_empty() {
        bail!("empty message");
    }
    let settings = crate::agents::settings(state);
    if usage_today(state) >= settings.daily_token_budget {
        bail!("today's token budget is used up");
    }
    let st = state.clone();
    let (path, body) = (format!("/runs/{id}/message"), json!({ "text": text }));
    tokio::task::spawn_blocking(move || st.agent_host.call(&st, "POST", &path, Some(body))).await.map_err(|e| anyhow!(e.to_string()))??;
    if run["status"] != "running" {
        run["status"] = json!("running");
        run["endedAt"] = Value::Null;
        state.store.save_agent_run(&run)?;
        if let Some(inc) = run["incident"].as_str() {
            set_incident_agent(state, inc, &run);
        }
    }
    Ok(run)
}

pub async fn abort(state: &Shared, id: &str) -> Result<Value> {
    let run = state.store.agent_run(id)?.with_context(|| format!("there's no run “{id}”"))?;
    let st = state.clone();
    let path = format!("/runs/{id}/abort");
    tokio::task::spawn_blocking(move || st.agent_host.call(&st, "POST", &path, Some(json!({})))).await.map_err(|e| anyhow!(e.to_string()))??;
    finish(state, run.clone(), "aborted", run["answer"].as_str().map(str::to_string), Some("stopped by the person".into()), 0);
    state.store.agent_run(id)?.context("gone")
}

/// Los incidentes recién abiertos cuya fuente pide diagnóstico (o más) arrancan un agente.
pub async fn auto_start(state: &Shared, opened: &[Value]) {
    let settings = crate::agents::settings(state);
    for inc in opened {
        let source = inc["source"].as_str().unwrap_or_default();
        if settings.level(source) < Level::Diagnose {
            continue;
        }
        let id = inc["id"].as_str().unwrap_or_default();
        if let Err(e) = start(state, id, "auto").await {
            tracing::info!("no agent for {id}: {e:#}");
        }
    }
}

pub fn spawn_poller(state: &Shared) {
    let state = state.clone();
    tokio::spawn(async move {
        let mut every = tokio::time::interval(Duration::from_secs(3));
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            poll(&state).await;
        }
    });
}
