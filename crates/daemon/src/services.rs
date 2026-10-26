// Entorno local de desarrollo de cada proyecto: levantar, detener y ver los
// logs del backend, el deployer (convex dev en modo watch) y el frontend. En
// heskala-erp el frontend se llama `vite` y, en modo Docker, backend y
// deployer son servicios del compose del devbox; en el resto de los casos
// todo son procesos del daemon (ver native.rs).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use devtool_core::redact;
use serde_json::{json, Value};

use crate::cmd::{env_file_value, run};
use crate::native::{self, Mode};
use crate::probes::docker_container;
use crate::procs::{self, append, http_ok, log_file, Spec};
use crate::projects::Project;
use crate::state::Shared;
use crate::store::now_ms;

const COMPOSE_FILE: &str = "docker/onprem-dev/compose.yaml";
const COMPOSE_PROJECT: &str = "heskala";
pub const VITE_PORT: u16 = 3000;
pub const NAMES: [&str; 3] = ["backend", "deployer", "vite"];

#[derive(Default)]
pub struct Services {
    last_error: Mutex<HashMap<String, String>>,
    busy: Mutex<HashMap<String, String>>,
    /// Últimas lecturas de CPU (%) y memoria (bytes) de cada proceso, la más vieja primero.
    usage: Mutex<HashMap<String, std::collections::VecDeque<(f32, u64)>>>,
}

impl Services {
    /// Si algún servicio se está levantando, deteniendo o reiniciando.
    pub fn busy_any(&self) -> bool {
        !self.busy.lock().unwrap().is_empty()
    }
}

/// Lecturas que se guardan por proceso (a una cada ~3 s, unos 90 s).
const USAGE_KEEP: usize = 30;

/// CPU (%) y memoria residente (bytes) de cada grupo de procesos, por su líder
/// (el daemon lanza cada servicio en su propio grupo).
async fn usage_by_group() -> HashMap<u32, (f32, u64)> {
    let out = run("ps", &["-A", "-o", "pgid=,%cpu=,rss="], None, &[], Duration::from_secs(5)).await;
    let mut map: HashMap<u32, (f32, u64)> = HashMap::new();
    for line in out.stdout.lines() {
        let mut parts = line.split_whitespace();
        let (Some(g), Some(c), Some(r)) = (parts.next(), parts.next(), parts.next()) else { continue };
        let (Ok(g), Ok(c), Ok(r)) = (g.parse::<u32>(), c.replace(',', ".").parse::<f32>(), r.parse::<u64>()) else { continue };
        let e = map.entry(g).or_default();
        e.0 += c;
        e.1 += r * 1024;
    }
    map
}

/// Servicios del proyecto: el frontend es `vite` en heskala-erp y `frontend`
/// en los demás (si se detectó uno).
fn names(project: &Project) -> Vec<&'static str> {
    match (project.is_default(), project.frontend.is_empty()) {
        (true, _) => NAMES.to_vec(),
        (false, true) => vec!["backend", "deployer"],
        (false, false) => vec!["backend", "deployer", "frontend"],
    }
}

fn check(project: &Project, name: &str) -> Result<&'static str> {
    let list = names(project);
    // `vite` y `frontend` valen para el frontend de cualquier proyecto.
    let wanted = if matches!(name, "vite" | "frontend") { list.last().copied().filter(|n| matches!(*n, "vite" | "frontend")) } else { list.iter().copied().find(|n| *n == name) };
    wanted.ok_or_else(|| anyhow!("Unknown service in {}: {name} ({})", project.name, list.join(", ")))
}

fn is_frontend(name: &str) -> bool {
    matches!(name, "vite" | "frontend")
}

/// Mode del proyecto: solo heskala-erp puede seguir en Docker.
fn project_mode(state: &Shared, project: &Project) -> Mode {
    // En el sitio en la nube no hay Docker: todo corre como procesos del daemon.
    if project.is_default() && !crate::site::is_cloud() {
        native::mode(state)
    } else {
        Mode::Native
    }
}

/// Proceso del daemon detrás de un servicio (si en este modo lo es).
fn proc_name(state: &Shared, project: &Project, name: &str) -> Option<String> {
    match (name, project_mode(state, project)) {
        (n, _) if is_frontend(n) => Some(project.proc("frontend")),
        ("backend", Mode::Native) => Some(project.proc("backend")),
        ("deployer", Mode::Native) => Some(project.proc("deployer")),
        _ => None,
    }
}

/// Clave de busy/error y del log de acciones: única entre proyectos.
fn key(project: &Project, name: &str) -> String {
    if project.is_default() {
        name.to_string()
    } else {
        format!("{}.{name}", project.name)
    }
}

fn frontend_port(project: &Project) -> u16 {
    if project.is_default() {
        VITE_PORT
    } else {
        project.frontend_port
    }
}

fn compose_service(name: &str) -> &'static str {
    if name == "backend" {
        "backend"
    } else {
        "deployer-dev"
    }
}

/// Quién escucha en el puerto (si alguien): pid y comando.
async fn port_owner(port: u16) -> Option<(u32, String)> {
    let out = run("lsof", &["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fpc"], None, &[], Duration::from_secs(5)).await;
    let mut pid = None;
    let mut name = String::new();
    for line in out.stdout.lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.parse().ok();
        } else if let Some(c) = line.strip_prefix('c') {
            name = c.to_string();
        }
    }
    pid.map(|p| (p, name))
}

fn info(project: &Project, name: &str, mode: Mode) -> (String, String) {
    let port = native::backend_port(project);
    let (title, description) = match (name, mode) {
        ("backend", Mode::Native) => (
            "Convex backend (native)".to_string(),
            format!("convex-local-backend as a daemon process (localhost:{port}), without Docker. Data in {}.", project.native_dir().display()),
        ),
        ("backend", _) => ("Convex backend (Docker)".into(), "Self-hosted convex-backend in a container (localhost:3210). Data lives in its volume.".into()),
        ("deployer", Mode::Native) => ("Deployer (convex dev)".into(), "convex dev in the repo, as a daemon process: deploys the functions every time you save.".into()),
        ("deployer", _) => ("Deployer (convex dev, Docker)".into(), "Container that deploys the checkout's functions every time you save a file.".into()),
        _ if project.is_default() => ("Frontend (Vite)".into(), format!("The web app at http://localhost:{VITE_PORT}, pointing at the Convex in .env.local.")),
        _ => {
            let kind = match project.frontend_kind.as_deref() {
                Some("next") => "Next.js",
                Some("vite") => "Vite",
                _ => "frontend",
            };
            (format!("Frontend ({kind})"), format!("The web app at http://localhost:{}, pointing at this project's local backend (localhost:{port}).", project.frontend_port))
        }
    };
    (title, description)
}

/// Estado de todos los servicios (y del modo).
pub async fn list(state: &Shared, project: &Project) -> Vec<Value> {
    let mode = project_mode(state, project);
    let groups = usage_by_group().await;
    let mut out = vec![];
    for name in names(project) {
        let (title, description) = info(project, name, mode);
        let k = key(project, name);
        let mut v = json!({
            "name": name,
            "project": project.name,
            "title": title,
            "description": description,
            "mode": if mode == Mode::Native { "native" } else { "docker" },
            "busy": state.services.busy.lock().unwrap().get(&k).cloned(),
            "error": state.services.last_error.lock().unwrap().get(&k).cloned(),
        });
        match proc_name(state, project, name) {
            Some(proc) => {
                let managed = state.procs.get(&proc);
                let port = match name {
                    "backend" => Some(native::backend_port(project)),
                    "deployer" => None,
                    _ => Some(frontend_port(project)),
                };
                let owner = match (managed, port) {
                    (None, Some(p)) => port_owner(p).await,
                    _ => None,
                };
                let (status, detail) = match (managed, &owner) {
                    (Some(m), _) => ("running", format!("pid {} · since {}", m.pid, crate::jobs::stamp(m.started_at))),
                    (None, Some((pid, cmd))) => ("external", format!("port {} is used by another process ({cmd}, pid {pid})", port.unwrap_or(0))),
                    (None, None) => ("stopped", "detenido".to_string()),
                };
                v["status"] = json!(status);
                v["detail"] = json!(detail);
                v["port"] = json!(port);
                if let Some((cpu, mem)) = managed.and_then(|m| groups.get(&m.pid).copied()) {
                    let mut usage = state.services.usage.lock().unwrap();
                    let hist = usage.entry(proc.clone()).or_default();
                    hist.push_back((cpu, mem));
                    while hist.len() > USAGE_KEEP {
                        hist.pop_front();
                    }
                    v["cpu"] = json!(cpu);
                    v["memory"] = json!(mem);
                    v["cpuHistory"] = json!(hist.iter().map(|h| h.0).collect::<Vec<_>>());
                    v["memoryHistory"] = json!(hist.iter().map(|h| h.1).collect::<Vec<_>>());
                }
                match name {
                    "backend" => {
                        let port = native::backend_port(project);
                        v["url"] = json!(format!("http://localhost:{port}"));
                        v["healthy"] = json!(status == "running" && http_ok(format!("http://127.0.0.1:{port}/version")).await);
                    }
                    "deployer" if project.is_default() => {
                        v["phase"] = state.probe_data("deployer").map(|p| p["phase"].clone()).unwrap_or(Value::Null);
                    }
                    "deployer" => {
                        v["phase"] = json!(deployer_phase(&proc));
                    }
                    _ => {
                        let port = frontend_port(project);
                        v["url"] = json!(format!("http://localhost:{port}"));
                        v["healthy"] = json!(status != "stopped" && http_ok(format!("http://127.0.0.1:{port}/")).await);
                        v["convexUrl"] = json!(if project.is_default() {
                            env_file_value(&project.repo.join(".env.local"), "VITE_CONVEX_URL")
                        } else {
                            Some(format!("http://127.0.0.1:{}", native::backend_port(project)))
                        });
                    }
                }
            }
            None => {
                let c = docker_container(state, COMPOSE_PROJECT, compose_service(name));
                let running = c.as_ref().map(|c| c["state"] == "running").unwrap_or(false);
                v["status"] = json!(if running { "running" } else if c.is_some() { "stopped" } else { "absent" });
                v["detail"] = json!(c.as_ref().and_then(|c| c["status"].as_str()).unwrap_or("no container"));
                if name == "backend" {
                    v["url"] = json!("http://localhost:3210");
                    v["healthy"] = json!(running && http_ok("http://127.0.0.1:3210/version".into()).await);
                } else {
                    v["phase"] = state.probe_data("deployer").map(|p| p["phase"].clone()).unwrap_or(Value::Null);
                }
            }
        }
        out.push(v);
    }
    out
}

/// Fase del deployer de otro proyecto, según la última línea útil de su log.
pub fn deployer_phase(proc: &str) -> &'static str {
    let text = std::fs::read_to_string(log_file(proc)).unwrap_or_default();
    if text.trim().is_empty() {
        return "unknown";
    }
    let lines: Vec<&str> = text.lines().collect();
    let recent = &lines[lines.len().saturating_sub(200)..];
    match devtool_core::deployer_phase(recent.iter().copied()) {
        devtool_core::DeployerPhase::Error => "error",
        devtool_core::DeployerPhase::Syncing => "deploying",
        _ => "idle",
    }
}

/// start | stop | restart de un servicio, o de `all`. Corre en segundo plano.
pub fn action(state: &Shared, project: &Project, name: &str, action: &str) -> Result<Value> {
    if !matches!(action, "start" | "stop" | "restart") {
        bail!("unknown action: {action} (start, stop, restart)");
    }
    let names: Vec<&'static str> = if name == "all" {
        let mut v = names(project);
        if action == "stop" {
            v.reverse();
        }
        v
    } else {
        vec![check(project, name)?]
    };
    for n in &names {
        if let Some(b) = state.services.busy.lock().unwrap().get(&key(project, n)) {
            bail!("{n} is already busy ({b})");
        }
    }
    let st = state.clone();
    let action = action.to_string();
    for n in &names {
        st.services.busy.lock().unwrap().insert(key(project, n), action.clone());
    }
    let project = project.clone();
    tokio::spawn(async move {
        for n in names {
            let k = key(&project, n);
            let res = run_action(&st, &project, n, &action).await;
            st.services.busy.lock().unwrap().remove(&k);
            match res {
                Ok(()) => {
                    st.services.last_error.lock().unwrap().remove(&k);
                }
                Err(e) => {
                    append(&k, &format!("✖ {action}: {e:#}"));
                    st.services.last_error.lock().unwrap().insert(k, format!("{e:#}"));
                }
            }
        }
    });
    Ok(json!({ "accepted": true }))
}

async fn compose(state: &Shared, name: &str, args: &[&str]) -> Result<()> {
    let mut full = vec!["compose", "-f", COMPOSE_FILE, "-p", COMPOSE_PROJECT];
    full.extend_from_slice(args);
    append(name, &format!("$ docker {}", full.join(" ")));
    let out = run("docker", &full, Some(state.config.erp()?), &[], Duration::from_secs(600)).await;
    for line in out.stdout.lines().chain(out.stderr.lines()) {
        append(name, line);
    }
    if !out.ok {
        bail!(out.error());
    }
    Ok(())
}

/// Frontend del proyecto. En los que no son heskala-erp apunta al backend
/// local por variables de entorno (ganan al `.env.local`, que no se toca).
fn frontend_spec(project: &Project) -> Result<Spec> {
    let port = frontend_port(project);
    let mut parts = project.frontend.iter().map(|a| a.replace("{port}", &port.to_string()));
    let first = parts.next().ok_or_else(|| anyhow!("{} has no frontend configured", project.name))?;
    let program = project.repo.join(&first);
    if !program.exists() {
        bail!("couldn't find {}: install the repo's dependencies (bun install / npm install)", program.display());
    }
    let mut env = vec![];
    let cloud = crate::site::is_cloud();
    if !project.is_default() || cloud {
        // En el sitio en la nube el navegador del teléfono llega al backend por su URL de boat.
        let backend = native::backend_port(project);
        let convex = crate::site::public_url(backend).unwrap_or_else(|| format!("http://127.0.0.1:{backend}"));
        let site = crate::site::public_url(backend + 1).unwrap_or_else(|| format!("http://127.0.0.1:{}", backend + 1));
        for prefix in ["VITE_", "NEXT_PUBLIC_", "EXPO_PUBLIC_", ""] {
            env.push((format!("{prefix}CONVEX_URL"), convex.clone()));
            env.push((format!("{prefix}CONVEX_SITE_URL"), site.clone()));
        }
        env.push(("PORT".into(), port.to_string()));
    }
    // Vite rechaza los Host que no son localhost: el de boat se agrega.
    if let Some(host) = crate::site::public_url(port).and_then(|u| u.split("://").nth(1).map(|h| h.trim_end_matches('/').to_string())) {
        env.push(("__VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS".into(), host));
    }
    Ok(Spec {
        name: project.proc("frontend"),
        program: program.display().to_string(),
        args: parts.collect(),
        env,
        cwd: project.repo.clone(),
        ready_url: Some(format!("http://127.0.0.1:{port}/")),
        ready_secs: if project.frontend_kind.as_deref() == Some("next") { 60 } else { 20 },
    })
}

/// En el sitio en la nube el clon llega sin dependencias: se instalan la primera vez (bun si el repo
/// usa bun, si no npm).
async fn ensure_deps(project: &Project, log: &str) -> Result<()> {
    if !crate::site::is_cloud() || project.repo.join("node_modules").is_dir() {
        return Ok(());
    }
    let bun = project.repo.join("bun.lock").exists() || project.repo.join("bun.lockb").exists();
    let (program, args): (&str, &[&str]) = match (bun, project.repo.join("package-lock.json").exists()) {
        (true, _) => ("bun", &["install"]),
        (false, true) => ("npm", &["ci"]),
        _ => ("npm", &["install"]),
    };
    append(log, &format!("$ {program} {}", args.join(" ")));
    let out = run(program, args, Some(&project.repo), &[], Duration::from_secs(900)).await;
    for line in out.stdout.lines().chain(out.stderr.lines()).rev().take(20).collect::<Vec<_>>().into_iter().rev() {
        append(log, line);
    }
    if !out.ok {
        bail!("installing dependencies failed: {}", out.error());
    }
    Ok(())
}

async fn start_proc(state: &Shared, project: &Project, name: &str) -> Result<()> {
    if name != "backend" {
        ensure_deps(project, &key(project, name)).await?;
    }
    match name {
        "backend" => {
            if state.procs.get(&project.proc("backend")).is_none() {
                let port = native::backend_port(project);
                if let Some((pid, cmd)) = port_owner(port).await {
                    bail!("port {port} is already used by another process ({cmd}, pid {pid}); is the Docker backend still running?");
                }
            }
            native::start_backend(state, project).await
        }
        "deployer" => native::start_deployer(state, project).await,
        _ => {
            if state.procs.get(&project.proc("frontend")).is_none() {
                let port = frontend_port(project);
                if let Some((pid, cmd)) = port_owner(port).await {
                    bail!("port {port} is already used by another process ({cmd}, pid {pid}); stop it or use that one");
                }
            }
            procs::start(state, frontend_spec(project)?).await.map(|_| ())
        }
    }
}

async fn run_action(state: &Shared, project: &Project, name: &str, action: &str) -> Result<()> {
    append(&key(project, name), &format!("— {action} ({}) —", crate::jobs::stamp(now_ms())));
    match proc_name(state, project, name) {
        Some(proc) => match action {
            "start" => start_proc(state, project, name).await,
            "stop" => procs::stop(state, &proc).await,
            _ => {
                procs::stop(state, &proc).await?;
                start_proc(state, project, name).await
            }
        },
        None => {
            let service = compose_service(name);
            match action {
                "start" => compose(state, name, &["up", "-d", service]).await,
                "stop" => compose(state, name, &["stop", service]).await,
                _ => compose(state, name, &["restart", service]).await,
            }
        }
    }
}

/// Últimas `tail` líneas de un servicio (redactadas, sin ANSI).
pub async fn logs(state: &Shared, project: &Project, name: &str, tail: usize) -> Result<String> {
    let name = check(project, name)?;
    let tail = tail.clamp(10, 2000);
    let mut lines: Vec<String> = vec![];
    let proc = proc_name(state, project, name);
    let k = key(project, name);
    match &proc {
        Some(p) => {
            if let Ok(text) = std::fs::read_to_string(log_file(p)) {
                let all: Vec<&str> = text.lines().collect();
                lines = all[all.len().saturating_sub(tail)..].iter().map(|l| l.to_string()).collect();
            }
        }
        None => {
            if let Some(c) = docker_container(state, COMPOSE_PROJECT, compose_service(name)) {
                let container = c["name"].as_str().unwrap_or_default().to_string();
                let out = run("docker", &["logs", "--tail", &tail.to_string(), "--timestamps", &container], None, &[], Duration::from_secs(10)).await;
                let mut all: Vec<String> = out.stdout.lines().chain(out.stderr.lines()).map(str::to_string).collect();
                // Cada línea empieza con la hora RFC 3339: ordenar mezcla bien stdout y stderr.
                all.sort();
                lines = all
                    .into_iter()
                    .map(|l| match l.split_once(' ') {
                        Some((ts, rest)) if ts.len() > 19 => format!("{} {rest}", &ts[11..19]),
                        _ => l,
                    })
                    .collect();
            }
        }
    }
    // Las acciones del devtool sobre el servicio (levantar, detener, errores).
    if proc.as_deref() != Some(k.as_str()) {
        if let Ok(text) = std::fs::read_to_string(log_file(&k)) {
            let all: Vec<&str> = text.lines().collect();
            let actions: Vec<String> = all[all.len().saturating_sub(30)..].iter().map(|l| l.to_string()).collect();
            if !actions.is_empty() {
                lines.push("— Condx actions —".into());
                lines.extend(actions);
            }
        }
    }
    let skip = lines.len().saturating_sub(tail + 31);
    static ANSI: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let ansi = ANSI.get_or_init(|| regex::Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]").unwrap());
    Ok(redact(&ansi.replace_all(&lines[skip..].join("\n"), "")))
}
