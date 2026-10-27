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
        // Los chequeos de un proyecto (lint, pruebas) tardan minutos: esperarlos no es que el host no
        // conteste, y reiniciarlo los cortaría.
        let slow = path == "/checks";
        for attempt in 0..2 {
            let (port, token) = self.ensure(state)?;
            let req = ureq::request(method, &format!("http://127.0.0.1:{port}{path}")).set("Authorization", &format!("Bearer {token}")).timeout(Duration::from_secs(if slow { 1800 } else { 30 }));
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
                Err(e) if attempt == 0 && !slow => {
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
        out.push(json!({ "id": id, "kind": p["kind"], "baseUrl": p["baseUrl"], "model": p["model"], "apiKey": key, "contextWindow": p["contextWindow"] }));
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
/// Una rama (`dev` en una tarea) es la de GitHub recién traída (`origin/dev`), no la copia local, que
/// puede estar vieja.
fn fix_base(repo: &std::path::Path, incident: &Value) -> String {
    let ev = &incident["evidence"];
    for candidate in [ev["commit"].as_str(), ev["branch"].as_str()].into_iter().flatten() {
        for r in [format!("refs/remotes/origin/{candidate}"), candidate.to_string()] {
            if let Ok(sha) = git(repo, &["rev-parse", "--verify", "--quiet", &format!("{r}^{{commit}}")]) {
                return sha;
            }
        }
    }
    git(repo, &["rev-parse", "HEAD"]).unwrap_or_else(|_| "HEAD".into())
}

/// Un worktree propio en la rama `condx/fix-…`, con los node_modules del repo (los chequeos corren
/// sin red: no se puede instalar nada).
/// Una copia de solo lectura detenida en `sha` (para quien revisa el cambio de otro agente).
fn review_worktree(repo: &std::path::Path, worktrees: &std::path::Path, run_id: &str, sha: &str) -> Result<PathBuf> {
    let dir = worktrees.join(format!("{run_id}-ro"));
    if dir.join(".git").exists() {
        return Ok(dir);
    }
    std::fs::create_dir_all(worktrees)?;
    // Las de revisiones de hace más de dos días ya no se usan (un mensaje después en ese hilo es raro).
    for e in std::fs::read_dir(worktrees)?.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|m| m.elapsed().ok()).is_some_and(|d| d.as_secs() > 2 * 86_400);
        if old && e.file_name().to_string_lossy().ends_with("-ro") {
            let _ = std::process::Command::new("git").arg("-C").arg(repo).args(["worktree", "remove", "--force"]).arg(e.path()).output();
        }
    }
    let out = std::process::Command::new("git").arg("-C").arg(repo).args(["worktree", "add", "--detach", "-f"]).arg(&dir).arg(sha).output()?;
    if !out.status.success() {
        bail!("couldn't check out {sha} to review it: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(dir)
}

fn prepare_worktree(repo: &std::path::Path, worktrees: &std::path::Path, run_id: &str, incident: &Value) -> Result<(PathBuf, String, String)> {
    git(repo, &["rev-parse", "--git-dir"]).context("fixes need the project to be a git repository")?;
    // Lo último de GitHub antes de elegir la base: sin eso, un agente trabajaría sobre lo que haya
    // quedado en esta copia (un `dev` de antes del último merge) sin avisar.
    crate::gitflow::fetch_origin(repo).map_err(|e| anyhow!("couldn't update from GitHub before starting, so it won't work from an old base: {e:#}"))?;
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
        // Todo lo que git no ignora; el enlace a node_modules (para los chequeos) nunca entra. Nombrarlo
        // en la orden falla si el .gitignore del repo ya lo ignora («paths are ignored»).
        git(&dir, &["add", "-A", "--", "."])?;
        git(&dir, &["rm", "--cached", "-q", "--ignore-unmatch", "--", "node_modules"])?;
        if git(&dir, &["diff", "--cached", "--name-only"])?.is_empty() {
            let head = git(&dir, &["rev-parse", "HEAD"])?;
            let changed = head != run["base"].as_str().unwrap_or_default();
            return Ok(json!({ "changes": changed, "commit": changed.then_some(head) }));
        }
        let cause = run["answer"].as_str().and_then(|a| a.split("## Cause").nth(1)).map(|s| s.split("##").next().unwrap_or("").trim().to_string()).unwrap_or_default();
        let message = if run["kind"] == "flow" {
            // Un agente de flujo no atiende un incidente: el commit lleva el título de su nodo y lo que hizo.
            let node = run["title"].as_str().unwrap_or("agent").split(" · ").next().unwrap_or("agent");
            let answer = run["answer"].as_str().unwrap_or_default();
            let done = Some(crate::workflows::first_line(answer.split("```json").next().unwrap_or(answer)))
                .filter(|l| !l.is_empty())
                .or_else(|| crate::roles::extract_json(answer).and_then(|v| v["change"].as_str().map(crate::workflows::first_line)))
                .unwrap_or_default();
            format!("fix: {node} (Condx agent)\n\n{done}")
        } else {
            format!("fix: {} (Condx agent)\n\n{cause}\n\nIncident: {}\nRun: {}", incident["title"].as_str().unwrap_or("incident"), incident["id"].as_str().unwrap_or("?"), run["id"].as_str().unwrap_or("?"))
        };
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

/// Borra el worktree y la rama de un arreglo y lo marca descartado. El repo es el del que salió el
/// worktree (el de un rol puede no ser el del proyecto).
fn remove_fix(state: &Shared, run: &mut Value, incident: &Value) {
    let dir = run["worktree"].as_str().map(PathBuf::from);
    let repo = dir
        .as_ref()
        .and_then(|d| git(d, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).ok())
        .and_then(|common| PathBuf::from(common).parent().map(PathBuf::from))
        .unwrap_or_else(|| repo_for(state, incident));
    if let Some(dir) = &dir {
        let _ = git(&repo, &["worktree", "remove", "--force", &dir.display().to_string()]);
    }
    if let Some(branch) = run["branch"].as_str() {
        let _ = git(&repo, &["branch", "-D", branch]);
    }
    run["fix"]["discarded"] = json!(true);
}

/// Descarta un arreglo: borra su worktree y su rama.
pub fn discard(state: &Shared, id: &str) -> Result<Value> {
    let mut run = state.store.agent_run(id)?.with_context(|| format!("there's no run “{id}”"))?;
    if run["status"] == "running" {
        bail!("stop the agent first");
    }
    let incident = state.store.incident(run["incident"].as_str().unwrap_or_default())?.unwrap_or(Value::Null);
    remove_fix(state, &mut run, &incident);
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
    let body = with_language(&st, body);
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
    let model = crate::agents::model_override().or_else(|| settings.default_model.clone()).filter(|m| !m.is_empty()).or_else(|| provider["model"].as_str().map(str::to_string)).filter(|m| !m.is_empty()).context("choose a model for the provider")?;
    if settings.daily_token_budget == 0 || usage_today(state) >= settings.daily_token_budget {
        bail!("today's token budget is used up ({} of {})", usage_today(state), settings.daily_token_budget);
    }
    let _slot = check_slot(state)?;
    let repo = crate::projects::get(state, project).map(|p| p.repo).unwrap_or_else(|_| state.config.workdir());
    let run_id = format!("chat-{:x}", now_ms());
    let title: String = text.lines().next().unwrap_or(text).chars().take(80).collect();
    let body = json!({ "runId": run_id, "mode": "chat", "providerId": provider_id, "model": model, "cwd": repo.display().to_string(), "text": text });
    let st = state.clone();
    let body = with_language(&st, body);
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
    // Un hilo nuevo sin proyecto: el que nombra el mensaje («¿en qué rama está serlismaldonado?»).
    let named = project.is_none().then(|| mentioned_project(state, text)).flatten();
    let mut run = start_chat(state, text, project.or(named.as_deref()), by).await?;
    run["replyNotice"] = json!(true);
    state.store.save_agent_run(&run)?;
    if !images.is_empty() {
        let id = run["id"].as_str().unwrap_or_default().to_string();
        message(state, &id, "These are the images I attached.", images).await?;
    }
    Ok(run)
}

/// El proyecto que nombra un texto (su nombre como palabra; el más largo si nombra varios).
pub fn mentioned_project(state: &Shared, text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_')).filter(|w| !w.is_empty()).collect();
    let mut found: Vec<String> = state.store.projects().unwrap_or_default().into_iter().map(|p| p.name).filter(|n| words.contains(&n.to_lowercase().as_str())).collect();
    found.sort_by_key(|n| std::cmp::Reverse(n.len()));
    found.into_iter().next()
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
    // El modelo elegido en el chat para este mensaje gana al del rol.
    let model = crate::agents::model_override()
        .or_else(|| role.model.clone())
        .filter(|m| !m.is_empty())
        .or_else(|| if role.provider.as_deref().is_some_and(|p| !p.is_empty()) { None } else { settings.default_model.clone() })
        .or_else(|| provider["model"].as_str().map(str::to_string))
        .filter(|m| !m.is_empty())
        .context("choose a model for the provider")?;
    if settings.daily_token_budget == 0 || usage_today(state) >= settings.daily_token_budget {
        bail!("today's token budget is used up ({} of {})", usage_today(state), settings.daily_token_budget);
    }
    let _slot = check_slot(state)?;
    // El repo del rol, si tiene uno para este pedido (Mayer trabaja en Condx); si no, el del proyecto.
    // Solo lo que pidió la persona (no la conversación que va de contexto) elige el repo.
    let ask = text.rsplit("The developer asks:").next().unwrap_or(text);
    let repo = crate::roles::repo_for(role, ask).filter(|p| p.join(".git").exists()).unwrap_or_else(|| crate::projects::get(state, project).map(|p| p.repo).unwrap_or_else(|_| state.config.workdir()));
    // Un agente que escribe trabaja en su worktree (rama condx/fix-…), como el modo arreglo.
    let write = role.mode == crate::roles::Mode::Write;
    let (cwd, branch, base) = if write {
        let (r, id) = (repo.clone(), run_id.to_string());
        let hint = json!({ "evidence": { "commit": base } });
        let worktrees = crate::config::home().join("worktrees");
        let (dir, branch, base) = tokio::task::spawn_blocking(move || prepare_worktree(&r, &worktrees, &id, &hint)).await.map_err(|e| anyhow!(e.to_string()))??;
        (dir, Some(branch), Some(base))
    } else if let Some(sha) = base.filter(|b| b.len() >= 7 && b.chars().all(|c| c.is_ascii_hexdigit())) {
        // Revisa un commit (el de otro agente): una copia detenida en ese commit, no la del proyecto.
        let (r, id, sha) = (repo.clone(), run_id.to_string(), sha.to_string());
        let worktrees = crate::config::home().join("worktrees");
        let dir = tokio::task::spawn_blocking(move || review_worktree(&r, &worktrees, &id, &sha)).await.map_err(|e| anyhow!(e.to_string()))??;
        (dir, None, base.map(str::to_string))
    } else {
        (repo.clone(), None, None)
    };
    // La rama de desarrollo de GitHub en ese momento: el reporte dice si la base coincide con ella.
    let dev_head = if write { crate::gitflow::dev_head(&repo).map(|(b, sha)| json!({ "branch": b, "commit": sha })) } else { None };
    let mode = if write { "fix" } else { "task" };
    let body = json!({ "runId": run_id, "mode": mode, "providerId": provider_id, "model": model, "cwd": cwd.display().to_string(), "text": text, "tools": role.tools });
    let st = state.clone();
    let body = with_language(&st, body);
    let res = tokio::task::spawn_blocking(move || st.agent_host.call(&st, "POST", "/runs", Some(body))).await.map_err(|e| anyhow!(e.to_string()))??;
    let run = json!({
        "id": run_id, "kind": "flow", "title": format!("{} · {flow}", role.name), "role": role.id, "incident": null, "project": project, "mode": mode,
        "provider": provider_id, "model": model, "by": format!("flow “{flow}”"),
        "status": "running", "startedAt": now_ms(), "endedAt": null, "answer": null, "error": null,
        "usage": { "total": 0 }, "conversationId": res["conversationId"],
        "worktree": write.then(|| cwd.display().to_string()), "branch": branch, "base": base, "devHead": dev_head, "fix": null,
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
        let fix = run["fix"].is_object().then(|| json!({ "branch": run["branch"], "commit": run["fix"]["commit"], "files": run["fix"]["files"], "stat": run["fix"]["stat"], "changes": run["fix"]["changes"], "discarded": run["fix"]["discarded"], "error": run["fix"]["error"], "pushed": run["fix"]["pushed"], "pushError": run["fix"]["pushError"], "pr": run["fix"]["pr"] }));
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
    if run["mode"] == "fix" && status == "failed" {
        // Falló: no queda nada que guardar, su worktree y su rama sobran.
        let incident = run["incident"].as_str().and_then(|i| state.store.incident(i).ok().flatten()).unwrap_or(Value::Null);
        remove_fix(state, &mut run, &incident);
    }
    if run["mode"] == "fix" && matches!(status, "done" | "stopped" | "aborted") {
        // Un agente de flujo no tiene incidente: el commit lleva el nombre de su flujo.
        let incident = run["incident"].as_str().and_then(|i| state.store.incident(i).ok().flatten()).unwrap_or_else(|| json!({ "title": run["title"], "id": run["id"] }));
        run["fix"] = commit_fix(&run, &incident);
        // Sin cambios: tampoco hay nada que conservar.
        if run["fix"]["changes"] == false {
            remove_fix(state, &mut run, &incident);
        }
        // Respaldo: la rama del arreglo sube a GitHub en cuanto queda el commit.
        if run["fix"]["changes"] == true {
            if let (Some(dir), Some(branch)) = (run["worktree"].as_str(), run["branch"].as_str()) {
                if let Value::Object(b) = crate::gitflow::backup(state, std::path::Path::new(dir), branch, run["continues"].as_str()) {
                    for (k, v) in b {
                        run["fix"][k] = v;
                    }
                }
            }
        }
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
    // Vino por correo: la respuesta sale en ese hilo de correo (una vez).
    if run["mailReply"].is_string() && status != "aborted" {
        crate::mail::reply_to_run(state, &run);
        let mut r = run.clone();
        r["mailReply"] = Value::Null;
        let _ = state.store.save_agent_run(&r);
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
        // Retomado en la Mac: su primer mensaje lleva lo del sitio como contexto; se muestra solo lo que
        // escribió la persona (lo del sitio ya va en orden con `siteItems`).
        if run["contextFirst"] == true {
            if let Some(first) = run["transcript"].as_array_mut().and_then(|t| t.iter_mut().find(|x| x["role"] == "user")) {
                let said = first["text"].as_str().unwrap_or_default().rsplit("The developer now says: ").next().unwrap_or_default().to_string();
                first["text"] = json!(said);
            }
        }
        // Lo que escribe ahora y las herramientas que corren (solo mientras trabaja).
        run["live"] = live["live"].clone();
        if run["status"] == "running" {
            run["answer"] = live["answer"].clone();
            run["usage"] = live["usage"].clone();
        }
    }
    // Lo que contestó el sitio en la nube con la Mac apagada (importado al volver): en orden con lo demás.
    if let Some(site) = run["siteItems"].as_array().cloned().filter(|s| !s.is_empty()) {
        let mut all: Vec<Value> = run["transcript"].as_array().cloned().unwrap_or_default();
        all.extend(site);
        all.sort_by_key(|x| x["at"].as_i64().unwrap_or(0));
        run["transcript"] = json!(all);
    }
    Ok(run)
}

/// Los mensajes del sitio en la nube que la Mac no tiene todavía: en un hilo del sitio (`site-…`) o en
/// uno suyo (`siteItems`). Los guarda en la corrida (un hilo del sitio se vuelve un chat de la Mac,
/// con el mismo id) y devuelve cuántos sumó. Sin repetir lo que ya tiene.
pub async fn import_site_items(state: &Shared, id: &str, title: &str, project: Option<&str>, items: &[Value]) -> Result<usize> {
    let existing = state.store.agent_run(id)?;
    if existing.is_none() && !id.starts_with("site-") {
        return Ok(0);
    }
    let have: std::collections::HashSet<(String, String)> = match &existing {
        Some(_) => get(state, id).await?["transcript"].as_array().cloned().unwrap_or_default().iter().map(|x| (x["role"].as_str().unwrap_or("").to_string(), x["text"].as_str().unwrap_or("").trim().chars().take(300).collect())).collect(),
        None => Default::default(),
    };
    let new: Vec<Value> = items
        .iter()
        .filter_map(|it| {
            let role = match it["kind"].as_str()? {
                "user" => "user",
                "reply" => "assistant",
                _ => return None,
            };
            let text = it["text"].as_str()?.trim().to_string();
            // Con lo que hizo el agente del sitio antes de responder (sus pasos y cuánto tardó).
            let mut x = json!({ "role": role, "text": text, "at": it["at"], "author": it["author"] });
            for k in ["steps", "took"] {
                if !it[k].is_null() {
                    x[k] = it[k].clone();
                }
            }
            (!text.is_empty()).then_some(x)
        })
        .collect();
    let key = |x: &Value| -> (String, String) { (x["role"].as_str().unwrap_or("").to_string(), x["text"].as_str().unwrap_or("").trim().chars().take(300).collect()) };
    // Los pasos que llegan de respuestas ya importadas sin ellos (importadas antes de que el sitio los mandara).
    let mut filled = false;
    if let Some(mut run) = existing.clone() {
        if let Some(site) = run["siteItems"].as_array_mut() {
            for old in site.iter_mut().filter(|o| o["steps"].is_null()) {
                if let Some(src) = new.iter().find(|n| !n["steps"].is_null() && key(n) == key(old)) {
                    old["steps"] = src["steps"].clone();
                    old["took"] = src["took"].clone();
                    filled = true;
                }
            }
        }
        if filled {
            state.store.save_agent_run(&run)?;
        }
    }
    let new: Vec<Value> = new.into_iter().filter(|x| !have.contains(&key(x))).collect();
    if new.is_empty() {
        return Ok(0);
    }
    let existing = if filled { state.store.agent_run(id)? } else { existing };
    let first_at = new.iter().filter_map(|x| x["at"].as_i64()).min().unwrap_or_else(now_ms);
    let last_at = new.iter().filter_map(|x| x["at"].as_i64()).max().unwrap_or_else(now_ms);
    let mut run = existing.unwrap_or_else(|| {
        let project = project.map(str::to_string).or_else(|| mentioned_project(state, title));
        json!({
            "id": id, "kind": "chat", "title": title, "incident": null, "project": project, "mode": "chat", "provider": null, "model": null, "by": "cloud site",
            "status": "done", "startedAt": first_at, "endedAt": last_at, "answer": null, "error": null, "usage": { "total": 0 }, "limits": { "maxTokens": 0, "maxMinutes": 0 },
        })
    });
    let mut site: Vec<Value> = run["siteItems"].as_array().cloned().unwrap_or_default();
    site.extend(new.iter().cloned());
    run["siteItems"] = json!(site);
    if run["status"] != "running" && run["endedAt"].as_i64().unwrap_or(0) < last_at {
        run["endedAt"] = json!(last_at);
    }
    state.store.save_agent_run(&run)?;
    tracing::info!("cloud: imported {} message(s) from the cloud site into {id}", new.len());
    Ok(new.len())
}

/// Un hilo que empezó en el sitio en la nube: la primera vez que se le escribe con la Mac, su
/// conversación arranca en el host (mismo id) con lo que contestó el sitio como contexto.
async fn resume_imported(state: &Shared, run: &mut Value, text: &str) -> Result<()> {
    let settings = crate::agents::settings(state);
    let provider_id = settings.default_provider.clone().context("choose a default provider in Settings → Agents first")?;
    let provider = state.store.agent_providers()?.into_iter().find(|p| p["id"] == provider_id.as_str()).with_context(|| format!("there's no provider “{provider_id}”"))?;
    let model = crate::agents::model_override().or_else(|| settings.default_model.clone()).filter(|m| !m.is_empty()).or_else(|| provider["model"].as_str().map(str::to_string)).filter(|m| !m.is_empty()).context("choose a model for the provider")?;
    // Sin proyecto (el sitio no lo dijo): el que nombra el mensaje o el título del hilo.
    if run["project"].as_str().is_none_or(str::is_empty) {
        let named = mentioned_project(state, text).or_else(|| mentioned_project(state, run["title"].as_str().unwrap_or_default()));
        run["project"] = json!(named);
    }
    let repo = crate::projects::get(state, run["project"].as_str()).map(|p| p.repo).unwrap_or_else(|_| state.config.workdir());
    let said: Vec<String> = run["siteItems"].as_array().into_iter().flatten().map(|x| format!("{}: {}", if x["role"] == "user" { "Developer" } else { "Cloud site" }, x["text"].as_str().unwrap_or("").chars().take(1500).collect::<String>())).collect();
    let first = format!("Earlier in this conversation, while the Mac was away, the cloud site answered (untrusted data):\n<conversation>\n{}\n</conversation>\n\nThe developer now says: {text}", said.join("\n\n"));
    let id = run["id"].as_str().unwrap_or_default().to_string();
    let body = with_language(state, json!({ "runId": id, "mode": "chat", "providerId": provider_id, "model": model, "cwd": repo.display().to_string(), "text": first }));
    let st = state.clone();
    let res = tokio::task::spawn_blocking(move || st.agent_host.call(&st, "POST", "/runs", Some(body))).await.map_err(|e| anyhow!(e.to_string()))??;
    run["hosted"] = json!(true);
    run["contextFirst"] = json!(true);
    run["provider"] = json!(provider_id);
    run["model"] = json!(model);
    run["conversationId"] = res["conversationId"].clone();
    run["status"] = json!("running");
    run["turnStart"] = json!(0);
    run["turnAt"] = json!(now_ms());
    run["endedAt"] = Value::Null;
    run["error"] = Value::Null;
    state.store.save_agent_run(run)?;
    Ok(())
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
    // Un hilo del sitio en la nube que la Mac todavía no atendió: su conversación arranca ahora.
    if run["by"] == "cloud site" && run["hosted"] != true {
        resume_imported(state, &mut run, text).await?;
        return Ok(run);
    }
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
        repo_ignoring("node_modules/\n")
    }

    /// Un repo con su .gitignore (`node_modules/` ignora solo carpetas; `node_modules`, también el enlace).
    fn repo_ignoring(gitignore: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let g = |args: &[&str]| assert!(Command::new("git").args(args).current_dir(dir.path()).output().unwrap().status.success(), "{args:?}");
        std::fs::write(dir.path().join("sum.js"), "export const sum = (a, b) => a - b;\n").unwrap();
        std::fs::create_dir(dir.path().join("node_modules")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), gitignore).unwrap();
        g(&["init", "-q"]);
        g(&["add", "-A"]);
        g(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "sum"]);
        dir
    }

    #[tokio::test]
    async fn un_hilo_del_sitio_se_importa_una_vez_y_sin_repetir() {
        let st = crate::state::test_state();
        let items = vec![
            json!({ "kind": "user", "at": 10, "text": "¿en qué rama está?" }),
            json!({ "kind": "reply", "author": "Cloud site", "at": 20, "text": "En main." }),
            json!({ "kind": "system", "at": 21, "text": "ignorado" }),
        ];
        assert_eq!(import_site_items(&st, "site-abc", "Rama", Some("serlismaldonado"), &items).await.unwrap(), 2);
        let run = st.store.agent_run("site-abc").unwrap().unwrap();
        assert_eq!(run["by"], "cloud site");
        assert_eq!(run["project"], "serlismaldonado");
        // Otra vez lo mismo más uno nuevo: solo entra el nuevo.
        let mut more = items.clone();
        more.push(json!({ "kind": "user", "at": 30, "text": "¿y el último commit?" }));
        assert_eq!(import_site_items(&st, "site-abc", "Rama", None, &more).await.unwrap(), 1);
        let t = get(&st, "site-abc").await.unwrap()["transcript"].as_array().cloned().unwrap();
        assert_eq!(t.iter().map(|x| x["text"].as_str().unwrap()).collect::<Vec<_>>(), vec!["¿en qué rama está?", "En main.", "¿y el último commit?"]);
        // Un hilo de la Mac que no es un chat suyo (un flujo): no se importa.
        assert_eq!(import_site_items(&st, "r123", "Flujo", None, &items).await.unwrap(), 0);
    }

    #[test]
    fn el_commit_no_falla_si_el_gitignore_ignora_el_enlace_a_node_modules() {
        // Como el de la landing: `node_modules` sin barra también ignora el enlace del worktree.
        let repo = repo_ignoring("node_modules\n");
        let worktrees = tempfile::tempdir().unwrap();
        let incident = json!({ "id": "inc-2", "title": "lang", "evidence": {} });
        let (dir, branch, base) = prepare_worktree(repo.path(), worktrees.path(), "run-inc-2-1", &incident).unwrap();
        std::fs::write(dir.join("sum.js"), "export const sum = (a, b) => a + b;\n").unwrap();
        let run = json!({ "id": "run-inc-2-1", "worktree": dir.display().to_string(), "base": base, "answer": "done" });
        let fix = commit_fix(&run, &incident);
        assert_eq!(fix["changes"], true, "{fix}");
        assert_eq!(git(repo.path(), &["show", "--name-only", "--format=", &branch]).unwrap(), "sum.js");
    }

    #[test]
    fn el_commit_de_un_agente_de_flujo_lleva_el_titulo_del_nodo_y_lo_que_hizo() {
        let repo = repo();
        let worktrees = tempfile::tempdir().unwrap();
        let (dir, branch, base) = prepare_worktree(repo.path(), worktrees.path(), "run-f-1", &json!({ "evidence": {} })).unwrap();
        std::fs::write(dir.join("sum.js"), "export const sum = (a, b) => a + b;\n").unwrap();
        let run = json!({ "id": "run-f-1", "kind": "flow", "title": "Fixer · Tarea", "worktree": dir.display().to_string(), "base": base, "answer": "Set lang to es.\n\n```json\n{}\n```" });
        let fix = commit_fix(&run, &json!({ "title": "Fixer · Tarea", "id": "run-f-1" }));
        assert_eq!(fix["changes"], true, "{fix}");
        let log = git(repo.path(), &["log", "-1", "--format=%s|%b", &branch]).unwrap();
        assert_eq!(log, "fix: Fixer (Condx agent)|Set lang to es.");
    }

    #[test]
    fn un_arreglo_sin_cambios_borra_su_worktree_y_su_rama() {
        let repo = repo();
        let worktrees = tempfile::tempdir().unwrap();
        let (dir, branch, _) = prepare_worktree(repo.path(), worktrees.path(), "run-f-2", &json!({ "evidence": {} })).unwrap();
        let st = crate::state::test_state();
        let mut run = json!({ "worktree": dir.display().to_string(), "branch": branch });
        remove_fix(&st, &mut run, &Value::Null);
        assert!(!dir.exists());
        assert!(git(repo.path(), &["branch", "--list", &branch]).unwrap().is_empty());
        assert_eq!(run["fix"]["discarded"], true);
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

/// El idioma de los agentes (configuración): va con cada corrida y el host lo suma a las instrucciones.
fn with_language(state: &Shared, mut body: Value) -> Value {
    let language = crate::agents::settings(state).language;
    if !language.trim().is_empty() {
        body["language"] = json!(language.trim());
    }
    body
}
