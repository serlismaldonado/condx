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
/// El error de no haber lugar (ya trabajan tantos agentes como el límite): quien llama puede dejar
/// el pedido en cola en vez de perderlo.
pub const FULL: &str = "agents are busy";

/// Si el error es de no haber lugar.
pub fn is_full(e: &anyhow::Error) -> bool {
    e.to_string().starts_with(FULL)
}

/// Lugares tomados por pedidos que todavía no figuran como «trabajando» (entre ver que hay lugar y
/// guardar la corrida): sin esto, varios pedidos en el mismo instante pasaban todos el límite.
static RESERVED: Mutex<u32> = Mutex::new(0);

/// Un lugar tomado; se suelta al terminar el pedido (la corrida ya figura como «trabajando», o falló).
#[derive(Debug)]
pub struct Slot;

impl Drop for Slot {
    fn drop(&mut self) {
        let mut r = RESERVED.lock().unwrap_or_else(|e| e.into_inner());
        *r = r.saturating_sub(1);
    }
}

/// Toma un lugar si `running` (las que trabajan) más los tomados no llegan a `max`.
fn take_slot(running: u32, max: u32) -> std::result::Result<Slot, String> {
    let mut r = RESERVED.lock().unwrap_or_else(|e| e.into_inner());
    if running + *r >= max {
        return Err(format!("{FULL}: {} agent(s) working (the limit is {max})", running + *r));
    }
    *r += 1;
    Ok(Slot)
}

/// Hay lugar para un agente más (cuentan todos los que trabajan, empezados o retomados, y los que
/// están por empezar): lo toma hasta que la corrida figure como «trabajando».
fn check_slot(state: &Shared) -> Result<Slot> {
    let max = crate::agents::settings(state).max_concurrent;
    let running = state.store.agent_runs(&["running"], None)?.len() as u32;
    take_slot(running, max).map_err(|e| anyhow!(e))
}

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
    let faux_speed = std::env::var("CONDX_AGENT_FAUX_TPS").ok().and_then(|v| v.parse::<u64>().ok());
    let config = json!({
        "hostToken": token,
        "daemonUrl": format!("http://127.0.0.1:{}", state.config.port),
        "daemonToken": state.tokens.agent,
        "storage": crate::config::home().join("agents.sqlite").display().to_string(),
        "providers": if faux.is_some() { json!([]) } else { json!(providers(state)?) },
        "faux": faux,
        "fauxTokensPerSecond": faux_speed,
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

fn git(dir: &std::path::Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").args(args).current_dir(dir).env("GIT_TERMINAL_PROMPT", "0").output().context("couldn't run git")?;
    if !out.status.success() {
        bail!("git {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// El commit del que parte un arreglo: el del incidente si está en el repo, si no su rama, si no HEAD.
fn fix_base(repo: &std::path::Path, incident: &Value) -> String {
    let ev = &incident["evidence"];
    for candidate in [ev["commit"].as_str(), ev["branch"].as_str()].into_iter().flatten() {
        if let Ok(sha) = git(repo, &["rev-parse", "--verify", "--quiet", &format!("{candidate}^{{commit}}")]) {
            return sha;
        }
    }
    git(repo, &["rev-parse", "HEAD"]).unwrap_or_else(|_| "HEAD".into())
}

/// Un worktree propio en la rama `condx/fix-…`, con los node_modules del repo (los chequeos corren
/// sin red: no se puede instalar nada).
fn prepare_worktree(repo: &std::path::Path, worktrees: &std::path::Path, run_id: &str, incident: &Value) -> Result<(PathBuf, String, String)> {
    git(repo, &["rev-parse", "--git-dir"]).context("fixes need the project to be a git repository")?;
    let base = fix_base(repo, incident);
    let dir = worktrees.join(run_id);
    let branch = format!("condx/fix-{}", run_id.trim_start_matches("run-"));
    std::fs::create_dir_all(dir.parent().unwrap_or(&dir))?;
    git(repo, &["worktree", "add", "-q", "-b", &branch, &dir.display().to_string(), &base])?;
    let modules = repo.join("node_modules");
    if modules.is_dir() {
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink(&modules, dir.join("node_modules"));
    }
    Ok((dir, branch, base))
}

/// Al terminar un arreglo, el daemon (no el agente) hace el commit en su rama y guarda el diff.
fn commit_fix(run: &Value, incident: &Value) -> Value {
    let Some(dir) = run["worktree"].as_str().map(PathBuf::from) else { return Value::Null };
    if !dir.exists() {
        return json!({ "error": "the worktree is gone" });
    }
    let attempt = || -> Result<Value> {
        git(&dir, &["add", "-A", "--", ".", ":(exclude)node_modules"])?;
        if git(&dir, &["diff", "--cached", "--name-only"])?.is_empty() {
            let head = git(&dir, &["rev-parse", "HEAD"])?;
            let changed = head != run["base"].as_str().unwrap_or_default();
            return Ok(json!({ "changes": changed, "commit": changed.then_some(head) }));
        }
        let cause = run["answer"].as_str().and_then(|a| a.split("## Cause").nth(1)).map(|s| s.split("##").next().unwrap_or("").trim().to_string()).unwrap_or_default();
        let message = format!("fix: {} (Condx agent)\n\n{cause}\n\nIncident: {}\nRun: {}", incident["title"].as_str().unwrap_or("incident"), incident["id"].as_str().unwrap_or("?"), run["id"].as_str().unwrap_or("?"));
        git(&dir, &["-c", "user.name=Condx agent", "-c", "user.email=agent@condx.local", "-c", "commit.gpgsign=false", "commit", "-q", "-m", &message])?;
        let commit = git(&dir, &["rev-parse", "HEAD"])?;
        let base = run["base"].as_str().unwrap_or("HEAD~1");
        let stat = git(&dir, &["diff", "--stat", base, "HEAD"])?;
        let files: Vec<String> = git(&dir, &["diff", "--name-only", base, "HEAD"])?.lines().map(str::to_string).collect();
        let mut patch = devtool_core::redact(&git(&dir, &["diff", base, "HEAD"])?);
        if patch.len() > 80_000 {
            patch.truncate(80_000);
            patch.push_str("\n[… cut]");
        }
        Ok(json!({ "changes": true, "commit": commit, "files": files, "stat": stat, "patch": patch }))
    };
    attempt().unwrap_or_else(|e| json!({ "error": format!("{e:#}") }))
}

/// Descarta un arreglo: borra su worktree y su rama.
pub fn discard(state: &Shared, id: &str) -> Result<Value> {
    let mut run = state.store.agent_run(id)?.with_context(|| format!("there's no run “{id}”"))?;
    if run["status"] == "running" {
        bail!("stop the agent first");
    }
    let incident = state.store.incident(run["incident"].as_str().unwrap_or_default())?.unwrap_or(Value::Null);
    let repo = repo_for(state, &incident);
    if let Some(dir) = run["worktree"].as_str() {
        let _ = git(&repo, &["worktree", "remove", "--force", dir]);
    }
    if let Some(branch) = run["branch"].as_str() {
        let _ = git(&repo, &["branch", "-D", branch]);
    }
    run["fix"]["discarded"] = json!(true);
    state.store.save_agent_run(&run)?;
    if let Some(inc) = run["incident"].as_str() {
        set_incident_agent(state, inc, &run);
    }
    Ok(run)
}

/// Arranca una corrida para un incidente: `diagnose` (solo lee) o `fix` (en su worktree). `by`: `app` o `auto`.
pub async fn start(state: &Shared, incident_id: &str, by: &str, mode: &str) -> Result<Value> {
    if !matches!(mode, "diagnose" | "fix") {
        bail!("unknown mode “{mode}”");
    }
    let incident = state.store.incident(incident_id)?.with_context(|| format!("there's no incident “{incident_id}”"))?;
    let settings = crate::agents::settings(state);
    // La plantilla de este tipo de incidente: su modo máximo, su modelo, sus instrucciones y límites.
    let template = crate::templates::for_incident(&crate::templates::list(state), &incident).unwrap_or_default();
    let mut mode = mode;
    if mode == "fix" && template.max_mode < Level::Fix {
        if by == "auto" {
            mode = "diagnose";
        } else {
            bail!("“{}” incidents are diagnose-only (change it in Settings → Agents → Templates)", template.name);
        }
    }
    let template_provider = template.provider.clone().filter(|p| !p.is_empty());
    let provider_id = template_provider.clone().or_else(|| settings.default_provider.clone()).context("choose a default provider in Settings → Agents first")?;
    let provider = state.store.agent_providers()?.into_iter().find(|p| p["id"] == provider_id.as_str()).with_context(|| format!("there's no provider “{provider_id}”"))?;
    // El modelo: el de la plantilla; si la plantilla elige proveedor, el de ese proveedor; si no, el de por defecto.
    let model = template
        .model
        .clone()
        .filter(|m| !m.is_empty())
        .or_else(|| if template_provider.is_some() { None } else { settings.default_model.clone() })
        .or_else(|| provider["model"].as_str().map(str::to_string))
        .filter(|m| !m.is_empty())
        .context("choose a model for the provider")?;
    if settings.daily_token_budget == 0 || usage_today(state) >= settings.daily_token_budget {
        bail!("today's token budget is used up ({} of {})", usage_today(state), settings.daily_token_budget);
    }
    let running = state.store.agent_runs(&["running"], None)?;
    if running.iter().any(|r| r["incident"] == incident_id) {
        bail!("an agent is already working on this incident");
    }
    let _slot = check_slot(state)?;
    let n = state.store.agent_runs(&[], Some(incident_id))?.len() + 1;
    let run_id = format!("run-{incident_id}-{n}");
    let repo = repo_for(state, &incident);
    let (cwd, branch, base) = if mode == "fix" {
        let (r, id, inc) = (repo.clone(), run_id.clone(), incident.clone());
        let worktrees = crate::config::home().join("worktrees");
        let (dir, branch, base) = tokio::task::spawn_blocking(move || prepare_worktree(&r, &worktrees, &id, &inc)).await.map_err(|e| anyhow!(e.to_string()))??;
        (dir, Some(branch), Some(base))
    } else {
        (repo, None, None)
    };
    let payload = json!({
        "title": incident["title"], "source": incident["source"], "project": incident["project"], "env": incident["env"],
        "count": incident["count"], "firstSeen": incident["firstSeen"], "lastSeen": incident["lastSeen"], "evidence": incident["evidence"],
    });
    let body = json!({
        "runId": run_id, "mode": mode, "providerId": provider_id, "model": model, "cwd": cwd.display().to_string(), "incident": payload,
        "instructions": template.instructions, "tools": template.tools,
    });
    let st = state.clone();
    let res = tokio::task::spawn_blocking(move || st.agent_host.call(&st, "POST", "/runs", Some(body))).await.map_err(|e| anyhow!(e.to_string()))??;
    let run = json!({
        "id": run_id, "incident": incident_id, "mode": mode, "provider": provider_id, "model": model, "by": by,
        "status": "running", "startedAt": now_ms(), "endedAt": null, "answer": null, "error": null,
        "usage": { "total": 0 }, "conversationId": res["conversationId"],
        "worktree": (mode == "fix").then(|| cwd.display().to_string()), "branch": branch, "base": base, "fix": null,
        "template": { "id": template.id, "name": template.name },
        "limits": { "maxTokens": template.max_tokens, "maxMinutes": template.max_minutes },
    });
    state.store.save_agent_run(&run)?;
    set_incident_agent(state, incident_id, &run);
    tracing::info!("agent {run_id} started on {incident_id} ({by})");
    Ok(run)
}

/// Una conversación libre con el agente (la página Agentes de la app): sin incidente, solo lee
/// (el código del proyecto y Condx por el MCP). Proveedor y modelo, los de por defecto; el
/// presupuesto del día y los agentes a la vez cuentan igual.
pub async fn start_chat(state: &Shared, text: &str, project: Option<&str>, by: &str) -> Result<Value> {
    let text = text.trim();
    if text.is_empty() {
        bail!("write a message");
    }
    let settings = crate::agents::settings(state);
    let provider_id = settings.default_provider.clone().context("choose a default provider in Settings → Agents first")?;
    let provider = state.store.agent_providers()?.into_iter().find(|p| p["id"] == provider_id.as_str()).with_context(|| format!("there's no provider “{provider_id}”"))?;
    let model = settings.default_model.clone().filter(|m| !m.is_empty()).or_else(|| provider["model"].as_str().map(str::to_string)).filter(|m| !m.is_empty()).context("choose a model for the provider")?;
    if settings.daily_token_budget == 0 || usage_today(state) >= settings.daily_token_budget {
        bail!("today's token budget is used up ({} of {})", usage_today(state), settings.daily_token_budget);
    }
    let _slot = check_slot(state)?;
    let repo = crate::projects::get(state, project).map(|p| p.repo).unwrap_or_else(|_| state.config.workdir());
    let run_id = format!("chat-{:x}", now_ms());
    let title: String = text.lines().next().unwrap_or(text).chars().take(80).collect();
    let body = json!({ "runId": run_id, "mode": "chat", "providerId": provider_id, "model": model, "cwd": repo.display().to_string(), "text": text });
    let st = state.clone();
    let res = tokio::task::spawn_blocking(move || st.agent_host.call(&st, "POST", "/runs", Some(body))).await.map_err(|e| anyhow!(e.to_string()))??;
    let run = json!({
        "id": run_id, "kind": "chat", "title": title, "incident": null, "project": project, "mode": "chat", "provider": provider_id, "model": model, "by": by,
        "status": "running", "startedAt": now_ms(), "endedAt": null, "answer": null, "error": null,
        "usage": { "total": 0 }, "conversationId": res["conversationId"],
        "limits": { "maxTokens": 0, "maxMinutes": 0 },
    });
    state.store.save_agent_run(&run)?;
    tracing::info!("agent chat {run_id} started ({by})");
    Ok(run)
}

/// Un chat nuevo pedido desde el teléfono: con sus imágenes (van enseguida, como un mensaje) y aviso
/// al teléfono cuando el agente conteste.
pub async fn start_chat_with(state: &Shared, text: &str, project: Option<&str>, by: &str, images: &[Value]) -> Result<Value> {
    let mut run = start_chat(state, text, project, by).await?;
    run["replyNotice"] = json!(true);
    state.store.save_agent_run(&run)?;
    if !images.is_empty() {
        let id = run["id"].as_str().unwrap_or_default().to_string();
        message(state, &id, "These are the images I attached.", images).await?;
    }
    Ok(run)
}

/// El hilo de un incidente (desde el teléfono): la conversación del agente que ya trabajó en él;
/// si no hubo ninguno, el chat que se abrió antes sobre él, o uno nuevo que arranca con lo que se sabe
/// del incidente (`about`, no `incident`: no es la corrida del incidente y no cambia su tarjeta).
pub async fn thread_for_incident(state: &Shared, inc: &Value, by: &str) -> Result<String> {
    if let Some(id) = inc["agent"]["runId"].as_str() {
        return Ok(id.to_string());
    }
    let id = inc["id"].as_str().context("incident without id")?;
    if let Some(r) = state.store.agent_runs(&[], None)?.into_iter().find(|r| r["about"] == id) {
        return Ok(r["id"].as_str().unwrap_or_default().to_string());
    }
    let title = inc["title"].as_str().unwrap_or("an incident");
    let mut text = format!("Look at this incident and tell me briefly what is going on and what you would check first.\n\nIncident: {title}\nSource: {}", inc["source"].as_str().unwrap_or("?"));
    if let Some(env) = inc["env"].as_str().filter(|e| !e.is_empty()) {
        text.push_str(&format!("\nEnvironment: {env}"));
    }
    text.push_str(&format!("\nSeen {} time(s)", inc["count"].as_u64().unwrap_or(1)));
    if let Some(log) = inc["evidence"]["log"].as_str().filter(|l| !l.trim().is_empty()) {
        text.push_str(&format!("\n\nLog (end):\n{}", devtool_core::redact(log)));
    }
    let mut run = start_chat(state, &text, inc["project"].as_str(), by).await?;
    run["about"] = json!(id);
    run["replyNotice"] = json!(true);
    run["title"] = json!(format!("About: {}", title.chars().take(70).collect::<String>()));
    state.store.save_agent_run(&run)?;
    Ok(run["id"].as_str().unwrap_or_default().to_string())
}

/// Un agente de un rol para un nodo de un flujo (o un hilo): idempotente por `run_id` (si ya
/// existe, la devuelve). Error con «already running» si no hay lugar: quien llama espera.
pub async fn start_role(state: &Shared, role: &crate::roles::Role, run_id: &str, text: &str, project: Option<&str>, flow: &str, base: Option<&str>) -> Result<Value> {
    if let Some(r) = state.store.agent_run(run_id)? {
        return Ok(r);
    }
    if !role.enabled {
        bail!("the agent “{}” is turned off", role.name);
    }
    let settings = crate::agents::settings(state);
    let provider_id = role.provider.clone().filter(|p| !p.is_empty()).or_else(|| settings.default_provider.clone()).context("choose a default provider in Settings → Agents first")?;
    let provider = state.store.agent_providers()?.into_iter().find(|p| p["id"] == provider_id.as_str()).with_context(|| format!("there's no provider “{provider_id}”"))?;
    let model = role
        .model
        .clone()
        .filter(|m| !m.is_empty())
        .or_else(|| if role.provider.as_deref().is_some_and(|p| !p.is_empty()) { None } else { settings.default_model.clone() })
        .or_else(|| provider["model"].as_str().map(str::to_string))
        .filter(|m| !m.is_empty())
        .context("choose a model for the provider")?;
    if settings.daily_token_budget == 0 || usage_today(state) >= settings.daily_token_budget {
        bail!("today's token budget is used up ({} of {})", usage_today(state), settings.daily_token_budget);
    }
    let _slot = check_slot(state)?;
    let repo = crate::projects::get(state, project).map(|p| p.repo).unwrap_or_else(|_| state.config.workdir());
    // Un agente que escribe trabaja en su worktree (rama condx/fix-…), como el modo arreglo.
    let write = role.mode == crate::roles::Mode::Write;
    let (cwd, branch, base) = if write {
        let (r, id) = (repo.clone(), run_id.to_string());
        let hint = json!({ "evidence": { "commit": base } });
        let worktrees = crate::config::home().join("worktrees");
        let (dir, branch, base) = tokio::task::spawn_blocking(move || prepare_worktree(&r, &worktrees, &id, &hint)).await.map_err(|e| anyhow!(e.to_string()))??;
        (dir, Some(branch), Some(base))
    } else {
        (repo, None, None)
    };
    let mode = if write { "fix" } else { "task" };
    let body = json!({ "runId": run_id, "mode": mode, "providerId": provider_id, "model": model, "cwd": cwd.display().to_string(), "text": text, "tools": role.tools });
    let st = state.clone();
    let res = tokio::task::spawn_blocking(move || st.agent_host.call(&st, "POST", "/runs", Some(body))).await.map_err(|e| anyhow!(e.to_string()))??;
    let run = json!({
        "id": run_id, "kind": "flow", "title": format!("{} · {flow}", role.name), "role": role.id, "incident": null, "project": project, "mode": mode,
        "provider": provider_id, "model": model, "by": format!("flow “{flow}”"),
        "status": "running", "startedAt": now_ms(), "endedAt": null, "answer": null, "error": null,
        "usage": { "total": 0 }, "conversationId": res["conversationId"],
        "worktree": write.then(|| cwd.display().to_string()), "branch": branch, "base": base, "fix": null,
        "limits": { "maxTokens": role.max_tokens, "maxMinutes": role.max_minutes },
    });
    state.store.save_agent_run(&run)?;
    tracing::info!("agent {run_id} ({}) started for flow {flow}", role.id);
    Ok(run)
}

/// Las conversaciones para la lista de la página Agentes: chats y corridas sobre incidentes, la
/// más reciente primero, sin la transcripción.
pub fn list(state: &Shared) -> Result<Value> {
    let titles: std::collections::HashMap<String, String> = state.store.incidents(&[], 500).unwrap_or_default().into_iter().filter_map(|i| Some((i["id"].as_str()?.to_string(), i["title"].as_str()?.to_string()))).collect();
    let runs: Vec<Value> = state
        .store
        .agent_runs(&[], None)?
        .into_iter()
        .take(100)
        .map(|r| {
            let incident = r["incident"].as_str().filter(|i| !i.is_empty());
            let title = r["title"].as_str().map(str::to_string).or_else(|| incident.and_then(|i| titles.get(i).cloned())).unwrap_or_else(|| r["id"].as_str().unwrap_or("?").to_string());
            json!({
                "id": r["id"], "kind": r["kind"].as_str().unwrap_or(if incident.is_some() { "incident" } else { "chat" }), "title": title, "incident": incident, "mode": r["mode"],
                "status": r["status"], "model": r["model"], "startedAt": r["startedAt"], "endedAt": r["endedAt"], "usage": r["usage"]["total"], "error": r["error"],
            })
        })
        .collect();
    Ok(json!({ "runs": runs }))
}

fn set_incident_agent(state: &Shared, incident_id: &str, run: &Value) {
    if let Ok(Some(mut inc)) = state.store.incident(incident_id) {
        let answer = run["answer"].as_str().map(|a| a.chars().take(4_000).collect::<String>());
        // Del arreglo, lo que la tarjeta muestra (el parche entero está en la corrida).
        let fix = run["fix"].is_object().then(|| json!({ "branch": run["branch"], "commit": run["fix"]["commit"], "files": run["fix"]["files"], "stat": run["fix"]["stat"], "changes": run["fix"]["changes"], "discarded": run["fix"]["discarded"], "error": run["fix"]["error"] }));
        inc["agent"] = json!({ "runId": run["id"], "mode": run["mode"], "status": run["status"], "answer": answer, "error": run["error"], "at": now_ms(), "model": run["model"], "fix": fix, "template": run["template"]["name"] });
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
            // Sobre el presupuesto del día o los límites de su plantilla: se detiene.
            let max_tokens = r["limits"]["maxTokens"].as_u64().unwrap_or(0);
            let max_minutes = r["limits"]["maxMinutes"].as_u64().unwrap_or(0);
            // Desde el último mensaje de la persona (o desde que arrancó).
            let since = r["turnAt"].as_i64().or_else(|| r["startedAt"].as_i64()).unwrap_or(0);
            let minutes = (now_ms() - since).max(0) as u64 / 60_000;
            let spent = total.saturating_sub(r["turnStart"].as_u64().unwrap_or(0));
            // Cada vuelta del modelo reenvía toda la conversación: la siguiente gasta al menos lo
            // que la última. Se para antes de pasarse, no después.
            let next = delta;
            let why = if max_tokens > 0 && spent + next > max_tokens {
                Some(format!("it reached its template's limit of {max_tokens} tokens"))
            } else if usage_today(state) + next >= settings.daily_token_budget {
                Some("today's token budget ran out".to_string())
            } else if max_minutes > 0 && minutes >= max_minutes {
                Some(format!("it reached its template's limit of {max_minutes} minutes"))
            } else {
                None
            };
            if let Some(why) = why {
                let st = state.clone();
                let path = format!("/runs/{id}/abort");
                let _ = tokio::task::spawn_blocking(move || st.agent_host.call(&st, "POST", &path, Some(json!({})))).await;
                finish(state, r, "stopped", live["answer"].as_str().map(str::to_string), Some(why), 0);
            }
            continue;
        }
        let mut r = run.clone();
        r["usage"] = live["usage"].clone();
        finish(state, r, status, live["answer"].as_str().map(str::to_string), live["reason"].as_str().map(str::to_string), delta);
    }
    // Con lugares libres, lo que esperaba en cola sale.
    crate::threads::drain_queue(state).await;
}

/// Avisar al teléfono cuando el agente conteste lo que se le escribió en un hilo (una vez).
pub fn notice_on_reply(state: &Shared, run_id: &str) {
    if let Ok(Some(mut run)) = state.store.agent_run(run_id) {
        run["replyNotice"] = json!(true);
        let _ = state.store.save_agent_run(&run);
    }
}

fn finish(state: &Shared, mut run: Value, status: &str, answer: Option<String>, error: Option<String>, delta: u64) {
    add_usage(state, delta);
    // Le habían escrito en un hilo: lo que contestó va al teléfono (si los avisos están encendidos).
    let reply_notice = run["replyNotice"] == true;
    run["replyNotice"] = json!(false);
    run["status"] = json!(status);
    run["endedAt"] = json!(now_ms());
    run["answer"] = json!(answer);
    run["error"] = json!(error.filter(|_| status != "done"));
    if run["mode"] == "fix" && matches!(status, "done" | "stopped" | "aborted") {
        // Un agente de flujo no tiene incidente: el commit lleva el nombre de su flujo.
        let incident = run["incident"].as_str().and_then(|i| state.store.incident(i).ok().flatten()).unwrap_or_else(|| json!({ "title": run["title"], "id": run["id"] }));
        run["fix"] = commit_fix(&run, &incident);
    }
    let _ = state.store.save_agent_run(&run);
    if let Some(inc) = run["incident"].as_str() {
        set_incident_agent(state, inc, &run);
        // Detenido por la persona: no hace falta avisarle.
        if status != "aborted" {
            if let Ok(Some(incident)) = state.store.incident(inc) {
                crate::notify::agent_finished(state, &run, &incident);
            }
        }
    }
    if reply_notice && status != "aborted" {
        crate::notify::agent_replied(state, &run);
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
        // Lo que escribe ahora y las herramientas que corren (solo mientras trabaja).
        run["live"] = live["live"].clone();
        if run["status"] == "running" {
            run["answer"] = live["answer"].clone();
            run["usage"] = live["usage"].clone();
        }
    }
    Ok(run)
}

/// Un mensaje de la persona: lo redirige si trabaja, o sigue la conversación (y vuelve a correr).
pub async fn message(state: &Shared, id: &str, text: &str, images: &[Value]) -> Result<Value> {
    let mut run = state.store.agent_run(id)?.with_context(|| format!("there's no run “{id}”"))?;
    if text.trim().is_empty() {
        bail!("empty message");
    }
    let settings = crate::agents::settings(state);
    if usage_today(state) >= settings.daily_token_budget {
        bail!("today's token budget is used up");
    }
    // Retomar una conversación ocupa un lugar; a uno que ya trabaja se le suma el mensaje.
    let _slot = if run["status"] != "running" { Some(check_slot(state)?) } else { None };
    // Los límites de la plantilla valen por pedido: cada mensaje empieza su cuenta (si no, una
    // conversación larga quedaba detenida para siempre en cuanto pasaba el límite una vez).
    run["turnStart"] = json!(run["usage"]["total"].as_u64().unwrap_or(0));
    run["turnAt"] = json!(now_ms());
    let st = state.clone();
    let (path, body) = (format!("/runs/{id}/message"), json!({ "text": text, "images": images }));
    tokio::task::spawn_blocking(move || st.agent_host.call(&st, "POST", &path, Some(body))).await.map_err(|e| anyhow!(e.to_string()))??;
    let resumed = run["status"] != "running";
    if resumed {
        run["status"] = json!("running");
        run["endedAt"] = Value::Null;
        run["error"] = Value::Null;
    }
    state.store.save_agent_run(&run)?;
    if resumed {
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
        let mode = match settings.level(source) {
            Level::Fix => "fix",
            Level::Diagnose => "diagnose",
            _ => continue,
        };
        let id = inc["id"].as_str().unwrap_or_default();
        if let Err(e) = start(state, id, "auto", mode).await {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn los_pedidos_a_la_vez_no_pasan_el_limite() {
        // Cuatro pedidos en el mismo instante con lugar para dos (y ninguno figura aún como
        // «trabajando»): entran dos; al soltarse, entra otro.
        let a = take_slot(0, 2).expect("first");
        let b = take_slot(0, 2).expect("second");
        assert!(take_slot(0, 2).unwrap_err().starts_with(FULL));
        drop(a);
        let c = take_slot(0, 2).expect("after one is freed");
        drop((b, c));
        assert_eq!(*RESERVED.lock().unwrap(), 0);
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let g = |args: &[&str]| assert!(Command::new("git").args(args).current_dir(dir.path()).output().unwrap().status.success(), "{args:?}");
        std::fs::write(dir.path().join("sum.js"), "export const sum = (a, b) => a - b;\n").unwrap();
        std::fs::create_dir(dir.path().join("node_modules")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "node_modules/\n").unwrap();
        g(&["init", "-q"]);
        g(&["add", "-A"]);
        g(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "sum"]);
        dir
    }

    #[test]
    fn un_arreglo_trabaja_en_su_worktree_y_el_daemon_hace_el_commit() {
        let repo = repo();
        let worktrees = tempfile::tempdir().unwrap();
        let incident = json!({ "id": "inc-1", "title": "Tests fail", "evidence": { "commit": "doesnotexist" } });
        let (dir, branch, base) = prepare_worktree(repo.path(), worktrees.path(), "run-inc-1-1", &incident).unwrap();
        assert_eq!(branch, "condx/fix-inc-1-1");
        assert!(dir.join("sum.js").exists());
        assert!(dir.join("node_modules").exists(), "node_modules shared");
        // Lo que haría el agente:
        std::fs::write(dir.join("sum.js"), "export const sum = (a, b) => a + b;\n").unwrap();
        let run = json!({ "id": "run-inc-1-1", "worktree": dir.display().to_string(), "base": base, "answer": "## Cause\nsum subtracted.\n\n## Change\nsum.js" });
        let fix = commit_fix(&run, &incident);
        assert_eq!(fix["changes"], true, "{fix}");
        assert_eq!(fix["files"], json!(["sum.js"]));
        assert!(fix["patch"].as_str().unwrap().contains("+export const sum = (a, b) => a + b;"));
        // El commit está en la rama, con el autor del agente, y sin node_modules.
        let log = git(repo.path(), &["log", "-1", "--format=%an|%s|%b", &branch]).unwrap();
        assert!(log.starts_with("Condx agent|fix: Tests fail (Condx agent)|sum subtracted."), "{log}");
        assert_eq!(git(repo.path(), &["show", "--name-only", "--format=", &branch]).unwrap(), "sum.js");
        // El checkout del usuario no cambió.
        assert!(std::fs::read_to_string(repo.path().join("sum.js")).unwrap().contains("a - b"));
        // Sin cambios nuevos, no hay otro commit.
        let again = commit_fix(&json!({ "worktree": dir.display().to_string(), "base": base }), &incident);
        assert_eq!(again["changes"], true);
        assert_eq!(again["commit"], fix["commit"]);
    }
}


#[cfg(test)]
mod chat_tests {
    use super::*;

    #[tokio::test]
    async fn un_chat_pide_mensaje_y_proveedor_y_la_lista_junta_chats_e_incidentes() {
        let st = crate::state::test_state();
        assert!(start_chat(&st, "   ", None, "app").await.unwrap_err().to_string().contains("write a message"));
        assert!(start_chat(&st, "hola", None, "app").await.unwrap_err().to_string().contains("default provider"));
        // Una corrida de incidente y un chat ya guardados: la lista los junta, con su título.
        let mut inc = json!({ "id": "inc-1", "title": "Seed failed on dev", "status": "open", "source": "job", "fingerprint": "job:x", "firstSeen": 1, "lastSeen": 1, "count": 1 });
        inc["evidence"] = json!({});
        st.store.save_incident(&inc).unwrap();
        st.store.save_agent_run(&json!({ "id": "run-inc-1-1", "incident": "inc-1", "mode": "diagnose", "status": "done", "startedAt": 10, "usage": { "total": 5 } })).unwrap();
        st.store.save_agent_run(&json!({ "id": "chat-1", "kind": "chat", "title": "¿Qué hace el seed?", "incident": null, "mode": "chat", "status": "running", "startedAt": 20, "usage": { "total": 7 } })).unwrap();
        let runs = list(&st).unwrap()["runs"].as_array().cloned().unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!((runs[0]["id"].as_str(), runs[0]["kind"].as_str(), runs[0]["title"].as_str(), runs[0]["usage"].as_u64()), (Some("chat-1"), Some("chat"), Some("¿Qué hace el seed?"), Some(7)));
        assert_eq!((runs[1]["kind"].as_str(), runs[1]["title"].as_str()), (Some("incident"), Some("Seed failed on dev")));
        assert!(runs.iter().all(|r| r.get("transcript").is_none()));
    }
}
