// Tokens con alcances (§9). Tres clientes locales:
//
//   ~/.condx/daemon/token      CLI y MCP: leer y pedir trabajos
//   ~/.condx/daemon/app-token  la app de escritorio: además, aprobar
//   (en memoria)               el host de agentes (agent_host.rs): solo leer, y solo las
//                              herramientas operativas del MCP (`AGENT_TOOLS`); nuevo en cada arranque
//
// Ambos son archivos 0600 del usuario. Modelo de amenaza: el alcance impide
// que un agente que habla por el MCP (que además no tiene herramienta para
// aprobar) apruebe su propio trabajo. Un proceso con shell completo como el
// usuario no es una frontera que el daemon pueda defender: podría correr la
// CLI de Convex por su cuenta. Las aprobaciones dejan rastro en el historial.

use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::config::home;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    Read,
    JobsRequest,
    JobsApprove,
    /// Leer los hilos (agentes externos por el MCP).
    ThreadsRead,
    /// Escribir en los hilos donde lo mencionaron.
    ThreadsWrite,
    /// Orquestar: escribir en cualquier hilo, llamar a los agentes, arrancar chats y tareas, y
    /// arrancar, cancelar y reintentar flujos.
    Orchestrate,
    /// Contestar las preguntas de los flujos.
    AnswerQuestions,
    /// Cambiar flujos y agentes del catálogo.
    EditFlows,
}

#[derive(Debug, Clone, Serialize)]
pub struct Client {
    pub name: &'static str,
    pub scopes: &'static [Scope],
    /// Un agente externo conectado con su propio token (connections.rs): quién es.
    #[serde(skip)]
    pub conn: Option<std::sync::Arc<crate::connections::Connection>>,
}

impl Client {
    pub fn can(&self, scope: Scope) -> bool {
        self.scopes.contains(&scope)
    }
}

pub const CLI: Client = Client { name: "cli", scopes: &[Scope::Read, Scope::JobsRequest, Scope::ThreadsRead], conn: None };
pub const APP: Client = Client { name: "app", scopes: &[Scope::Read, Scope::JobsRequest, Scope::JobsApprove], conn: None };
pub const AGENT: Client = Client { name: "agent", scopes: &[Scope::Read], conn: None };

/// Lo que el token de los agentes puede pedir al MCP: estado, trabajos, logs y despliegues; no
/// registros de las bases ni telemetría de las pestañas. (El host tiene la misma lista.)
pub const AGENT_TOOLS: &[&str] = &[
    "get_overview", "list_environments", "get_environment", "get_env_var_names", "get_git", "get_deployer", "get_vercel", "get_frontend",
    "list_incidents", "list_recipes", "list_jobs", "get_job", "get_job_log", "list_workflows", "get_workflow_run", "get_errors",
    // Pedir un release de Condx: espera la aprobación de la persona (Mayer).
    "request_release",
];

/// Lee o crea un token aleatorio en `file`, que queda en 0600: se crea así desde el principio
/// (sin un instante legible por otros) y, si ya existía con permisos más abiertos, se corrigen.
pub fn token_file(file: &Path) -> Result<String> {
    if let Ok(t) = std::fs::read_to_string(file) {
        let t = t.trim().to_string();
        if !t.is_empty() {
            restrict(file)?;
            return Ok(t);
        }
    }
    let mut bytes = [0u8; 32];
    use std::io::{Read, Write};
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let t: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(file)?.write_all(t.as_bytes())?;
    restrict(file)?;
    Ok(t)
}

pub fn random_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Deja el archivo solo para su dueño.
pub(crate) fn restrict(file: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(file)?.permissions().mode() & 0o077 != 0 {
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

pub struct Tokens {
    entries: Vec<(String, Client)>,
    /// El del host de agentes (solo en memoria).
    pub agent: String,
}

/// Comparación sin cortar en el primer byte distinto.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl Tokens {
    pub fn load() -> Result<Self> {
        let cli = token_file(&home().join("token"))?;
        let app = token_file(&home().join("app-token"))?;
        let agent = random_token()?;
        Ok(Self { entries: vec![(cli, CLI), (app, APP), (agent.clone(), AGENT)], agent })
    }

    #[cfg(test)]
    pub fn fixed(cli: &str, app: &str) -> Self {
        let agent = "agent-token-for-tests".to_string();
        Self { entries: vec![(cli.into(), CLI), (app.into(), APP), (agent.clone(), AGENT)], agent }
    }

    pub fn resolve(&self, token: &str) -> Option<Client> {
        self.entries.iter().find(|(t, _)| same(t, token)).map(|(_, c)| c.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alcances_por_token() {
        let tokens = Tokens::fixed("aaaa", "bbbb");
        let cli = tokens.resolve("aaaa").unwrap();
        assert!(cli.can(Scope::JobsRequest));
        assert!(!cli.can(Scope::JobsApprove));
        assert!(tokens.resolve("bbbb").unwrap().can(Scope::JobsApprove));
        assert!(tokens.resolve("aaab").is_none());
        assert!(tokens.resolve("").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn tokens_solo_para_el_dueno() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("token");
        let t = token_file(&file).unwrap();
        assert_eq!(t.len(), 64);
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        // Uno viejo con permisos abiertos se corrige al leerlo, sin cambiar su valor.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(token_file(&file).unwrap(), t);
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
