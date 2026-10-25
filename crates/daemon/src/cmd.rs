// Corre un CLI externo con tiempo límite y devuelve su salida redactada: lo
// que no pasa por aquí no puede filtrar secretos (§9).

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use devtool_core::redact;
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct Out {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    /// Mensaje corto para mostrar cuando el comando falló.
    pub fn error(&self) -> String {
        let text = if self.stderr.trim().is_empty() { &self.stdout } else { &self.stderr };
        let text = text.trim();
        let tail: Vec<&str> = text.lines().rev().take(5).collect();
        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
    }
}

pub async fn run(program: &str, args: &[&str], cwd: Option<&Path>, env: &[(&str, &str)], timeout: Duration) -> Out {
    let cmd = command(program, args, cwd, env);
    let child = match spawn(cmd, program) {
        Ok(c) => c,
        Err(out) => return out,
    };
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => Out {
            ok: out.status.success(),
            stdout: redact(&String::from_utf8_lossy(&out.stdout)),
            stderr: redact(&String::from_utf8_lossy(&out.stderr)),
        },
        Ok(Err(e)) => Out { ok: false, stdout: String::new(), stderr: e.to_string() },
        Err(_) => Out { ok: false, stdout: String::new(), stderr: format!("{program} didn't respond in {}s", timeout.as_secs()) },
    }
}

/// Para comandos que no terminan solos (`convex logs`): junta la salida
/// durante `window` y corta el proceso.
pub async fn capture_for(program: &str, args: &[&str], cwd: Option<&Path>, env: &[(&str, &str)], window: Duration) -> Out {
    use tokio::io::AsyncReadExt;
    let cmd = command(program, args, cwd, env);
    let mut child = match spawn(cmd, program) {
        Ok(c) => c,
        Err(out) => return out,
    };
    let mut stdout = child.stdout.take().expect("stdout");
    let mut stderr = child.stderr.take().expect("stderr");
    let out_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf).await;
        buf
    });
    let err_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        buf
    });
    let exited = tokio::select! {
        status = child.wait() => status.ok().map(|s| s.success()),
        _ = tokio::time::sleep(window) => {
            let _ = child.kill().await;
            None
        }
    };
    let stdout = out_task.await.unwrap_or_default();
    let stderr = err_task.await.unwrap_or_default();
    Out {
        // Cortado por tiempo es lo esperado; falla solo si terminó con error.
        ok: exited.unwrap_or(true),
        stdout: redact(&String::from_utf8_lossy(&stdout)),
        stderr: redact(&String::from_utf8_lossy(&stderr)),
    }
}

fn spawn(mut cmd: Command, program: &str) -> Result<tokio::process::Child, Out> {
    cmd.spawn()
        .map_err(|e| Out { ok: false, stdout: String::new(), stderr: format!("couldn't run {program}: {e}") })
}

pub fn command(program: &str, args: &[&str], cwd: Option<&Path>, env: &[(&str, &str)]) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    // launchd arranca con un PATH mínimo: sin esto no aparecen docker, gh, node.
    if std::env::var("PATH").map(|p| !p.contains("/opt/homebrew/bin")).unwrap_or(true) {
        let home = dirs::home_dir().map(|h| h.display().to_string()).unwrap_or_default();
        cmd.env(
            "PATH",
            format!(
                "/opt/homebrew/bin:/usr/local/bin:{home}/.bun/bin:{home}/Library/pnpm:{home}/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin:{}",
                std::env::var("PATH").unwrap_or_default()
            ),
        );
    }
    cmd
}

/// Lee una variable de un archivo .env sin pasar por el shell.
pub fn env_file_value(path: &Path, key: &str) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    raw.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').trim_matches('\'').to_string())
    })
}
