//! Carpetas y variables de Condx. Antes se llamaba heskala-devd/devtool y vivía en
//! `~/.heskala/*`: la primera vez que arranca, cada carpeta vieja se mueve a la nueva y
//! en su lugar queda un enlace (el servicio de launchd viejo y los scripts siguen andando).

use std::path::{Path, PathBuf};

/// Datos del daemon (config, tokens, SQLite, log, binario instalado).
pub const DAEMON: (&str, &str) = (".condx/daemon", ".heskala/devd");
/// Preferencias de la app de escritorio.
pub const APP: (&str, &str) = (".condx/app", ".heskala/devtool");
/// Respaldos (`export`).
pub const BACKUPS: (&str, &str) = (".condx/backups", ".heskala/backups");

/// La primera variable definida (las nuevas `CONDX_*` primero, después las viejas).
pub fn var(names: &[&str]) -> Option<String> {
    names.iter().find_map(|n| std::env::var(n).ok().filter(|v| !v.is_empty()))
}

/// La carpeta a usar sin mover nada: la nueva si existe (o si tampoco hay vieja),
/// si no la vieja. Así la app nueva sigue hablando con un daemon viejo.
pub fn resolve(user_home: &Path, (new, old): (&str, &str)) -> PathBuf {
    let new = user_home.join(new);
    let old = user_home.join(old);
    if new.exists() || !old.exists() {
        new
    } else {
        old
    }
}

/// Mueve la carpeta vieja a la nueva y deja un enlace en la vieja. No hace nada si la
/// nueva ya existe o la vieja no es una carpeta de verdad. Devuelve si movió algo.
pub fn migrate(user_home: &Path, (new, old): (&str, &str)) -> std::io::Result<bool> {
    let new = user_home.join(new);
    let old = user_home.join(old);
    let real_dir = std::fs::symlink_metadata(&old).map(|m| m.is_dir()).unwrap_or(false);
    if new.exists() || !real_dir {
        return Ok(false);
    }
    if let Some(parent) = new.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&old, &new)?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(&new, &old)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("condx-paths-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn mueve_la_carpeta_vieja_y_deja_enlace() {
        let home = scratch("mueve");
        std::fs::create_dir_all(home.join(".heskala/devd/bin")).unwrap();
        std::fs::write(home.join(".heskala/devd/token"), "t").unwrap();
        assert_eq!(resolve(&home, DAEMON), home.join(".heskala/devd"));
        assert!(migrate(&home, DAEMON).unwrap());
        assert_eq!(std::fs::read_to_string(home.join(".condx/daemon/token")).unwrap(), "t");
        // El camino viejo sigue sirviendo (binario del launchd viejo).
        assert!(std::fs::symlink_metadata(home.join(".heskala/devd")).unwrap().file_type().is_symlink());
        assert!(home.join(".heskala/devd/bin").is_dir());
        assert_eq!(resolve(&home, DAEMON), home.join(".condx/daemon"));
        // La segunda vez no hace nada.
        assert!(!migrate(&home, DAEMON).unwrap());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn sin_carpeta_vieja_usa_la_nueva() {
        let home = scratch("nueva");
        assert_eq!(resolve(&home, APP), home.join(".condx/app"));
        assert!(!migrate(&home, APP).unwrap());
        assert!(!home.join(".condx/app").exists());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn no_pisa_una_carpeta_nueva() {
        let home = scratch("ambas");
        std::fs::create_dir_all(home.join(".heskala/backups")).unwrap();
        std::fs::create_dir_all(home.join(".condx/backups")).unwrap();
        assert!(!migrate(&home, BACKUPS).unwrap());
        assert!(home.join(".heskala/backups").is_dir());
        std::fs::remove_dir_all(&home).unwrap();
    }
}
