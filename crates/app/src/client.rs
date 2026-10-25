// Cliente del daemon (HTTP local con el token de ~/.condx/daemon). Llamadas
// bloqueantes: la app las corre en el executor de fondo de GPUI.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

#[derive(Clone)]
pub struct Daemon {
    pub base: String,
    /// Checkout del ERP (de config.json del daemon): ahí abre la terminal.
    pub repo: Option<PathBuf>,
    token: String,
    agent: ureq::Agent,
    /// Para las lecturas del sondeo: un daemon trabado no congela la ventana 2,5 minutos.
    reads: ureq::Agent,
}

pub fn home() -> PathBuf {
    match devtool_core::paths::var(&["CONDX_HOME", "HESKALA_DEVD_HOME"]) {
        Some(dir) => PathBuf::from(dir),
        None => devtool_core::paths::resolve(&dirs::home_dir().expect("no HOME"), devtool_core::paths::DAEMON),
    }
}

impl Daemon {
    pub fn from_home() -> Result<Self> {
        let dir = home();
        let config = std::fs::read_to_string(dir.join("config.json")).ok().and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
        let port = config.as_ref().and_then(|c| c["port"].as_u64()).unwrap_or(4790);
        let repo = config.as_ref().and_then(|c| c["repo"].as_str()).map(PathBuf::from);
        let port = devtool_core::paths::var(&["CONDX_PORT", "HESKALA_DEVD_PORT"]).and_then(|p| p.parse().ok()).unwrap_or(port);
        // La app usa su propio token (puede aprobar); si falta, el del CLI.
        let token = std::fs::read_to_string(dir.join("app-token"))
            .or_else(|_| std::fs::read_to_string(dir.join("token")))
            .with_context(|| t!("{a} not found: is condxd installed?", a = dir.join("app-token").display()))?
            .trim()
            .to_string();
        Ok(Self {
            base: format!("http://127.0.0.1:{port}"),
            repo,
            token,
            // Los logs de Convex tardan ~8 s; measure, hasta 2 min.
            agent: ureq::AgentBuilder::new().timeout(Duration::from_secs(150)).build(),
            reads: ureq::AgentBuilder::new().timeout(Duration::from_secs(30)).build(),
        })
    }

    fn read(res: Result<ureq::Response, ureq::Error>) -> Result<Value> {
        match res {
            Ok(r) => Ok(r.into_json()?),
            Err(ureq::Error::Status(_, r)) => {
                let body: Value = r.into_json().unwrap_or(Value::Null);
                Err(anyhow!(body["error"].as_str().unwrap_or(t!("daemon error")).to_string()))
            }
            Err(e) => Err(anyhow!(t!("condxd isn't responding: {e}", e = e))),
        }
    }

    pub fn get(&self, path: &str) -> Result<Value> {
        let t0 = std::time::Instant::now();
        let r = Self::read(self.reads.get(&format!("{}{path}", self.base)).set("Authorization", &format!("Bearer {}", self.token)).call());
        // DEVTOOL_TRACE=1: cada lectura con su tiempo, para depurar la conexión con el daemon.
        if std::env::var_os("DEVTOOL_TRACE").is_some() {
            eprintln!("GET {path} {:?} {}", t0.elapsed(), r.as_ref().map(|_| "ok".to_string()).unwrap_or_else(|e| e.to_string()));
        }
        r
    }

    pub fn post(&self, path: &str, body: Value) -> Result<Value> {
        Self::read(
            self.agent
                .post(&format!("{}{path}", self.base))
                .set("Authorization", &format!("Bearer {}", self.token))
                .send_json(body),
        )
    }

    pub fn delete(&self, path: &str) -> Result<Value> {
        Self::read(self.agent.delete(&format!("{}{path}", self.base)).set("Authorization", &format!("Bearer {}", self.token)).call())
    }

    /// Cualquier herramienta del MCP (`POST /api/tools/{name}`).
    pub fn tool(&self, name: &str, args: Value) -> Result<Value> {
        Self::read(
            self.agent
                .post(&format!("{}/api/tools/{name}", self.base))
                .set("Authorization", &format!("Bearer {}", self.token))
                .send_json(if args.is_null() { json!({}) } else { args }),
        )
    }
}
