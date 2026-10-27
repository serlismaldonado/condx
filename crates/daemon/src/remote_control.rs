//! Claude Code con Remote Control en el sitio en la nube: con una sesión de claude.ai iniciada en el
//! contenedor (`claude auth login`, una vez; queda en el volumen), el sitio deja corriendo
//! `claude remote-control` y la máquina aparece como un dispositivo en Claude Code (el teléfono o
//! claude.ai/code), con la Mac apagada.
//!
//! Trabaja en copias aparte (`/data/workspace/<proyecto>`: worktrees del clon en la rama de desarrollo,
//! con sus submódulos y dependencias), no en los clones de Condx, donde trabajan los agentes y corre
//! la vista previa. Git sube con el token de GitHub del dueño de cada repo. Esa sesión tiene una
//! terminal completa en el contenedor (también ve sus secretos): es la persona trabajando, como en su Mac.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;

use crate::config::home;
use crate::state::Shared;

const NAME: &str = "condx-cloud";

static CHILD: Mutex<Option<Child>> = Mutex::new(None);

fn workspace() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/data".into())).join("workspace")
}

/// Si hay una sesión de claude.ai en el contenedor (`claude auth status`).
fn logged_in() -> bool {
    Command::new("claude")
        .args(["auth", "status"])
        .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .is_some_and(|v| v["loggedIn"] == true)
}

/// La copia de trabajo de un proyecto: un worktree del clon en su rama de desarrollo (`dev`, si no la
/// de siempre), con git que sube con el token del dueño, y lista (submódulos y dependencias).
fn ensure_workspace(repo: &Path, name: &str) -> anyhow::Result<()> {
    let dir = workspace().join(name);
    if !dir.join(".git").exists() {
        std::fs::create_dir_all(workspace())?;
        let git = |args: &[&str]| Command::new("git").arg("-C").arg(repo).args(args).output();
        let base = ["origin/dev", "origin/develop", "origin/HEAD"].into_iter().find(|b| git(&["rev-parse", "--verify", "-q", b]).is_ok_and(|o| o.status.success())).unwrap_or("HEAD");
        let out = git(&["worktree", "add", "-q", "--detach", &dir.display().to_string(), base])?;
        if !out.status.success() {
            anyhow::bail!("git worktree add: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        tracing::info!("remote control: workspace for {name} at {base}");
    }
    // Subir con el token de GitHub del dueño del repo (el mismo que usan los agentes del sitio).
    let helper = r#"!f() { test "$1" = get || exit 0; node -e 'let d="";process.stdin.on("data",c=>d+=c).on("end",()=>{const p=(d.match(/^path=(.*)$/m)||[])[1]||"";const s=JSON.parse(require("fs").readFileSync(process.env.CONDX_HOME+"/secrets.json","utf8"));const t=s["github-token:"+p.split("/")[0]]||s["github-token"]||"";process.stdout.write("username=x-access-token\npassword="+String(t).trim()+"\n")})'; }; f"#;
    for (k, v) in [("credential.helper", helper), ("credential.useHttpPath", "true")] {
        let _ = Command::new("git").arg("-C").arg(&dir).args(["config", k, v]).output();
    }
    crate::site::make_ready(&dir)
}

fn alive() -> bool {
    let mut c = CHILD.lock().unwrap();
    match c.as_mut().map(|ch| ch.try_wait()) {
        Some(Ok(None)) => true,
        Some(_) => {
            *c = None;
            false
        }
        None => false,
    }
}

fn start() -> anyhow::Result<()> {
    let log = std::fs::OpenOptions::new().create(true).append(true).open(home().join("remote-control.log"))?;
    // `script` le da una terminal (remote-control la espera); la sesión de claude.ai es la del login.
    let child = Command::new("script")
        .args(["-qfc", &format!("claude remote-control --name {NAME} --permission-mode acceptEdits"), "/dev/null"])
        .current_dir(workspace())
        .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    tracing::info!("remote control: started ({NAME}, pid {})", child.id());
    *CHILD.lock().unwrap() = Some(child);
    Ok(())
}

fn stop() {
    if let Some(mut c) = CHILD.lock().unwrap().take() {
        let _ = c.kill();
        let _ = c.wait();
        tracing::info!("remote control: stopped (signed out)");
    }
}

/// El vigilante: cada 30 s, con sesión iniciada, copias al día y remote-control vivo; sin sesión, nada.
pub fn spawn(state: Shared) {
    let _ = std::thread::Builder::new().name("remote-control".into()).spawn(move || loop {
        if logged_in() {
            for p in state.store.projects().unwrap_or_default() {
                if let Err(e) = ensure_workspace(&p.repo, &p.name) {
                    tracing::warn!("remote control: workspace for {}: {e:#}", p.name);
                }
            }
            if !alive() {
                if let Err(e) = start() {
                    tracing::warn!("remote control: couldn't start: {e:#}");
                }
            }
        } else if alive() {
            stop();
        }
        std::thread::sleep(Duration::from_secs(30));
    });
}
