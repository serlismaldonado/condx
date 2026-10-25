// Procesos que el daemon lanza y vigila (Vite, backend y deployer nativos,
// desechables nativos). La salida va directo a un archivo de log y el pid a
// un pidfile: un proceso sobrevive a un reinicio del daemon, que lo vuelve a
// adoptar al arrancar.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use devtool_core::redact;

use crate::cmd::command;
use crate::config::home;
use crate::state::Shared;
use crate::store::now_ms;

pub struct Spec {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    /// URL que tiene que responder para dar el arranque por bueno.
    pub ready_url: Option<String>,
    pub ready_secs: u64,
}

#[derive(Clone, Copy)]
pub struct Managed {
    pub pid: u32,
    pub started_at: i64,
}

#[derive(Default)]
pub struct Procs {
    map: Mutex<HashMap<String, Managed>>,
}

fn dir() -> PathBuf {
    home().join("procs")
}

pub fn log_file(name: &str) -> PathBuf {
    home().join("services").join(format!("{name}.log"))
}

fn pid_file(name: &str) -> PathBuf {
    dir().join(format!("{name}.pid"))
}

/// Escribe una línea (redactada) en el log del proceso o servicio.
pub fn append(name: &str, line: &str) {
    let path = log_file(name);
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        use std::io::Write;
        let _ = writeln!(f, "{}", redact(line));
    }
}

pub fn pid_alive(pid: u32) -> bool {
    // SAFETY: señal 0 solo consulta si el proceso existe.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Responde la URL. Con 127.0.0.1 también prueba [::1]: Vite a veces solo escucha en IPv6.
pub async fn http_ok(url: String) -> bool {
    tokio::task::spawn_blocking(move || {
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(2)).build();
        agent.get(&url).call().is_ok() || (url.contains("://127.0.0.1:") && agent.get(&url.replacen("://127.0.0.1:", "://[::1]:", 1)).call().is_ok())
    })
    .await
    .unwrap_or(false)
}

impl Procs {
    pub fn get(&self, name: &str) -> Option<Managed> {
        let mut map = self.map.lock().unwrap();
        match map.get(name).copied() {
            Some(m) if pid_alive(m.pid) => Some(m),
            Some(_) => {
                map.remove(name);
                let _ = std::fs::remove_file(pid_file(name));
                None
            }
            None => None,
        }
    }

    fn set(&self, name: &str, m: Option<Managed>) {
        let mut map = self.map.lock().unwrap();
        match m {
            Some(m) => {
                map.insert(name.to_string(), m);
            }
            None => {
                map.remove(name);
            }
        }
    }
}

/// Al arrancar el daemon: vuelve a vigilar los procesos que siguen vivos.
pub fn adopt_all(state: &Shared) {
    // Pidfile viejo de Vite (antes de procs/).
    let legacy = home().join("services").join("vite.pid");
    if legacy.exists() && !pid_file("vite").exists() {
        let _ = std::fs::create_dir_all(dir());
        let _ = std::fs::rename(&legacy, pid_file("vite"));
    }
    let Ok(entries) = std::fs::read_dir(dir()) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(str::to_string) else { continue };
        if path.extension().map(|e| e != "pid").unwrap_or(true) {
            continue;
        }
        let raw = std::fs::read_to_string(&path).unwrap_or_default();
        let mut parts = raw.split_whitespace();
        let (Some(pid), at) = (parts.next().and_then(|p| p.parse::<u32>().ok()), parts.next().and_then(|a| a.parse::<i64>().ok())) else {
            continue;
        };
        if pid_alive(pid) {
            append(&name, &format!("{name} (pid {pid}) is still alive: the daemon watches it again"));
            state.procs.set(&name, Some(Managed { pid, started_at: at.unwrap_or_else(now_ms) }));
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Lanza el proceso (si no corre ya) y espera a que responda `ready_url`.
pub async fn start(state: &Shared, spec: Spec) -> Result<u32> {
    if let Some(m) = state.procs.get(&spec.name) {
        return Ok(m.pid);
    }
    let log = log_file(&spec.name);
    std::fs::create_dir_all(log.parent().unwrap())?;
    std::fs::create_dir_all(dir())?;
    let shown: Vec<&str> = spec.args.iter().map(String::as_str).collect();
    append(&spec.name, &format!("$ {} {}", spec.program, shown.join(" ")));
    let out = std::fs::OpenOptions::new().create(true).append(true).open(&log)?;
    let err = out.try_clone()?;
    let vars: Vec<(&str, &str)> = spec.env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut cmd = command(&spec.program, &shown, Some(&spec.cwd), &vars);
    cmd.stdin(Stdio::null()).stdout(Stdio::from(out)).stderr(Stdio::from(err)).process_group(0).env("FORCE_COLOR", "0").env("NO_COLOR", "1");
    let mut child = cmd.spawn().map_err(|e| anyhow!("couldn't launch {}: {e}", spec.name))?;
    let pid = child.id().ok_or_else(|| anyhow!("{} didn't start", spec.name))?;
    let started_at = now_ms();
    let _ = std::fs::write(pid_file(&spec.name), format!("{pid} {started_at}"));
    state.procs.set(&spec.name, Some(Managed { pid, started_at }));
    let (st, name) = (state.clone(), spec.name.clone());
    tokio::spawn(async move {
        let status = child.wait().await;
        append(&name, &format!("{name} exited ({})", status.map(|s| s.to_string()).unwrap_or_else(|e| e.to_string())));
        if st.procs.map.lock().unwrap().get(&name).map(|m| m.pid) == Some(pid) {
            st.procs.set(&name, None);
            let _ = std::fs::remove_file(pid_file(&name));
        }
    });
    let Some(url) = spec.ready_url else { return Ok(pid) };
    for _ in 0..spec.ready_secs * 2 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if http_ok(url.clone()).await {
            append(&spec.name, &format!("✔ responding at {url}"));
            return Ok(pid);
        }
        if state.procs.get(&spec.name).is_none() {
            bail!("{} closed while starting (see its log)", spec.name);
        }
    }
    bail!("{} didn't respond in {} s (see its log)", spec.name, spec.ready_secs)
}

/// SIGTERM al grupo del proceso; SIGKILL si no termina en 5 s.
pub async fn stop(state: &Shared, name: &str) -> Result<()> {
    let Some(m) = state.procs.get(name) else { return Ok(()) };
    append(name, &format!("Stopping {name} (pid {})", m.pid));
    // SAFETY: señal al grupo de procesos que lanzó el daemon.
    unsafe {
        libc::kill(-(m.pid as i32), libc::SIGTERM);
    }
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if !pid_alive(m.pid) {
            break;
        }
    }
    if pid_alive(m.pid) {
        // SAFETY: igual que arriba.
        unsafe {
            libc::kill(-(m.pid as i32), libc::SIGKILL);
        }
    }
    state.procs.set(name, None);
    let _ = std::fs::remove_file(pid_file(name));
    Ok(())
}
