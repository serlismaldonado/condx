// Configuración del daemon: `~/.condx/daemon/config.json` (se crea con los
// valores por defecto la primera vez) y variables de entorno para pruebas.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: u16 = 4790;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Checkout de heskala-erp, si hay (de ahí salen su promoción, git, CI, Vercel y
    /// el compose del devbox). Condx anda sin él: los demás proyectos se agregan aparte.
    #[serde(default)]
    pub repo: Option<PathBuf>,
    pub port: u16,
    /// Orígenes https de la app que pueden mandar telemetría además de
    /// localhost (p. ej. la app en producción abierta en esta Mac).
    #[serde(default)]
    pub telemetry_origins: Vec<String>,
}

/// `~/.condx/daemon` (o `CONDX_HOME`; la vieja `~/.heskala/devd` mientras no se migre).
pub fn home() -> PathBuf {
    match devtool_core::paths::var(&["CONDX_HOME", "HESKALA_DEVD_HOME"]) {
        Some(dir) => PathBuf::from(dir),
        None => devtool_core::paths::resolve(&user_home(), devtool_core::paths::DAEMON),
    }
}

fn user_home() -> PathBuf {
    dirs::home_dir().expect("no HOME")
}

/// Mueve `~/.heskala/{devd,backups}` a `~/.condx` (una vez; deja enlaces). Solo al
/// arrancar el servicio o instalarlo, con el daemon viejo ya detenido. Con `CONDX_HOME`
/// (pruebas) no toca nada.
pub fn migrate_legacy() {
    if devtool_core::paths::var(&["CONDX_HOME", "HESKALA_DEVD_HOME"]).is_some() {
        return;
    }
    for dirs in [devtool_core::paths::DAEMON, devtool_core::paths::BACKUPS] {
        match devtool_core::paths::migrate(&user_home(), dirs) {
            Ok(true) => tracing::info!("moved ~/{} to ~/{}", dirs.1, dirs.0),
            Ok(false) => {}
            Err(e) => tracing::warn!("couldn't move ~/{} to ~/{}: {e}", dirs.1, dirs.0),
        }
    }
}

/// Carpeta de respaldos (`~/.condx/backups`).
pub fn backups_dir() -> PathBuf {
    devtool_core::paths::resolve(&user_home(), devtool_core::paths::BACKUPS)
}

fn looks_like_repo(dir: &Path) -> bool {
    dir.join("convex").is_dir() && dir.join("docker/onprem-dev").is_dir() && dir.join("package.json").is_file()
}

/// Busca el checkout del ERP subiendo desde `start`.
pub fn find_repo(start: &Path) -> Option<PathBuf> {
    start.ancestors().find(|d| looks_like_repo(d)).map(Path::to_path_buf)
}

impl Config {
    /// El checkout de heskala-erp, para lo que solo existe ahí (su promoción, su compose).
    pub fn erp(&self) -> Result<&Path> {
        self.repo.as_deref().context("this needs the heskala-erp checkout and Condx doesn't have one configured (repo in config.json)")
    }

    /// Carpeta donde correr comandos que no son de un proyecto: el ERP o la de datos.
    pub fn workdir(&self) -> PathBuf {
        self.repo.clone().unwrap_or_else(home)
    }

    pub fn load_or_init() -> Result<Self> {
        let dir = home();
        std::fs::create_dir_all(&dir).with_context(|| format!("couldn't create {}", dir.display()))?;
        let file = dir.join("config.json");
        let mut config = if file.exists() {
            let raw = std::fs::read_to_string(&file)?;
            serde_json::from_str::<Config>(&raw).with_context(|| format!("{} is unreadable", file.display()))?
        } else {
            // Solo si se pide (variable) o si el daemon corre desde el checkout del ERP.
            let repo = devtool_core::paths::var(&["CONDX_ERP_REPO", "HESKALA_REPO"])
                .map(PathBuf::from)
                .or_else(|| std::env::current_dir().ok().and_then(|d| find_repo(&d)))
                .or_else(|| std::env::current_exe().ok().and_then(|e| find_repo(&e)));
            let config = Config { repo, port: DEFAULT_PORT, telemetry_origins: vec![] };
            std::fs::write(&file, serde_json::to_string_pretty(&config)?)?;
            config
        };
        if let Some(port) = devtool_core::paths::var(&["CONDX_PORT", "HESKALA_DEVD_PORT"]).and_then(|p| p.parse().ok()) {
            config.port = port;
        }
        Ok(config)
    }
}

/// Token del CLI y del MCP (ver auth.rs para los alcances).
pub fn token() -> Result<String> {
    crate::auth::token_file(&home().join("token"))
}
