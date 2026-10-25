// Tokens con alcances (§9). Dos clientes locales:
//
//   ~/.condx/daemon/token      CLI y MCP: leer y pedir trabajos
//   ~/.condx/daemon/app-token  la app de escritorio: además, aprobar
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
}

#[derive(Debug, Clone, Serialize)]
pub struct Client {
    pub name: &'static str,
    pub scopes: &'static [Scope],
}

impl Client {
    pub fn can(&self, scope: Scope) -> bool {
        self.scopes.contains(&scope)
    }
}

pub const CLI: Client = Client { name: "cli", scopes: &[Scope::Read, Scope::JobsRequest] };
pub const APP: Client = Client { name: "app", scopes: &[Scope::Read, Scope::JobsRequest, Scope::JobsApprove] };

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

/// Deja el archivo solo para su dueño.
fn restrict(file: &Path) -> Result<()> {
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
}

/// Comparación sin cortar en el primer byte distinto.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl Tokens {
    pub fn load() -> Result<Self> {
        let cli = token_file(&home().join("token"))?;
        let app = token_file(&home().join("app-token"))?;
        Ok(Self { entries: vec![(cli, CLI), (app, APP)] })
    }

    #[cfg(test)]
    pub fn fixed(cli: &str, app: &str) -> Self {
        Self { entries: vec![(cli.into(), CLI), (app.into(), APP)] }
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
