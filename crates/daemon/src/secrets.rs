// Secretos del daemon (§8.1, §9): Llavero de macOS y respaldos cifrados.
//
// - Llavero, servicio `condx` (o `CONDX_KEYCHAIN` en pruebas):
//     prod-deploy-key  deploy key de Convex de producción (la registra el
//                      usuario con `condxd secret set prod-deploy-key`)
//     dev-deploy-key   deploy key de cloud dev, opcional: la pestaña Datos la usa
//                      para leer tamaños y páginas (`condxd secret set dev-deploy-key`)
//     backup-key       identidad age para cifrar respaldos de cloud (la crea
//                      el daemon la primera vez)
// - El ítem lo crea el mismo binario que lo lee (el instalado en
//   ~/.condx/daemon/bin), así macOS no pide permiso en cada lectura.
// - Antes el servicio se llamaba `condxd`: si falta en `condx`, se lee del viejo
//   y se copia (macOS pide permiso una vez, porque el binario cambió).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use age::secrecy::ExposeSecret;
use anyhow::{anyhow, bail, Context, Result};

pub const BACKUP_KEY: &str = "backup-key";
pub const PROD_DEPLOY_KEY: &str = "prod-deploy-key";
/// Deploy key de cloud dev: con ella la pestaña Datos lee tamaños y páginas (solo lectura).
pub const DEV_DEPLOY_KEY: &str = "dev-deploy-key";

const LEGACY_SERVICE: &str = "heskala-devd";

/// Servicio de prueba si hay una variable; si no, `None` (el de siempre, `condx`).
fn override_service() -> Option<String> {
    devtool_core::paths::var(&["CONDX_KEYCHAIN", "HESKALA_DEVD_KEYCHAIN"])
}

fn service() -> String {
    override_service().unwrap_or_else(|| "condx".into())
}

fn entry(account: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(&service(), account).map_err(|e| anyhow!("Keychain: {e}"))
}

fn read(entry: &keyring::Entry, account: &str) -> Result<Option<String>> {
    match entry.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(anyhow!("Keychain ({account}): {e}")),
    }
}

pub fn get(account: &str) -> Result<Option<String>> {
    if let Some(v) = read(&entry(account)?, account)? {
        return Ok(Some(v));
    }
    if override_service().is_some() {
        return Ok(None);
    }
    let legacy = keyring::Entry::new(LEGACY_SERVICE, account).map_err(|e| anyhow!("Keychain: {e}"))?;
    let Some(v) = read(&legacy, account)? else { return Ok(None) };
    set(account, &v)?;
    Ok(Some(v))
}

pub fn set(account: &str, value: &str) -> Result<()> {
    entry(account)?.set_password(value).map_err(|e| anyhow!("Keychain ({account}): {e}"))
}

pub fn delete(account: &str) -> Result<()> {
    match entry(account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(anyhow!("Keychain ({account}): {e}")),
    }
}

/// Solo los nombres que la app puede mostrar: si existe, nunca el valor.
pub fn exists(account: &str) -> bool {
    get(account).map(|v| v.is_some()).unwrap_or(false)
}

fn backup_identity() -> Result<age::x25519::Identity> {
    if let Some(raw) = get(BACKUP_KEY)? {
        return age::x25519::Identity::from_str(raw.trim()).map_err(|e| anyhow!("invalid backup key in the Keychain: {e}"));
    }
    let identity = age::x25519::Identity::generate();
    set(BACKUP_KEY, identity.to_string().expose_secret())?;
    Ok(identity)
}

/// Cifra `plain` en `plain.age` (0600) y borra el original.
pub fn encrypt_file(plain: &Path) -> Result<PathBuf> {
    let identity = backup_identity()?;
    let recipient = identity.to_public();
    let out_path = PathBuf::from(format!("{}.age", plain.display()));
    let encryptor = age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
        .map_err(|e| anyhow!("age: {e}"))?;
    let out = std::fs::File::create(&out_path).with_context(|| format!("couldn't create {}", out_path.display()))?;
    restrict(&out_path)?;
    let mut writer = encryptor.wrap_output(std::io::BufWriter::new(out))?;
    let mut input = std::io::BufReader::new(std::fs::File::open(plain)?);
    std::io::copy(&mut input, &mut writer)?;
    writer.finish()?.flush()?;
    std::fs::remove_file(plain)?;
    Ok(out_path)
}

/// Descifra `cipher` en `out` (0600). Quien llama borra `out` al terminar.
pub fn decrypt_file(cipher: &Path, out: &Path) -> Result<()> {
    let identity = backup_identity()?;
    let input = std::io::BufReader::new(std::fs::File::open(cipher)?);
    let decryptor = age::Decryptor::new(input).map_err(|e| anyhow!("age: {e}"))?;
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|e| anyhow!("couldn't decrypt {}: {e}", cipher.display()))?;
    let mut file = std::fs::File::create(out)?;
    restrict(out)?;
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
    }
    Ok(())
}

fn restrict(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// La cuenta del Llavero con la deploy key de un entorno cloud: la de su credencial si la tiene,
/// o una propia (`deploy-key:<entorno>`), que se guarda desde la pestaña Claves de la app.
pub fn deploy_account(env: &devtool_core::Environment) -> String {
    match &env.credential {
        Some(devtool_core::Credential::Keychain { account }) => account.clone(),
        _ => format!("deploy-key:{}", env.name),
    }
}

/// La cuenta general de su tipo (de antes de que hubiera una por entorno): `prod-deploy-key` o
/// `dev-deploy-key`, guardadas con `condxd secret set`.
fn shared_account(env: &devtool_core::Environment) -> &'static str {
    match env.kind {
        devtool_core::EnvKind::CloudProd => PROD_DEPLOY_KEY,
        _ => DEV_DEPLOY_KEY,
    }
}

/// La deploy key de un entorno cloud y de qué cuenta salió: la suya o, si no hay, la general de
/// su tipo. `None` si no hay ninguna.
pub fn deploy_key(env: &devtool_core::Environment) -> Result<Option<(String, String)>> {
    let own = deploy_account(env);
    if let Some(key) = get(&own)? {
        return Ok(Some((own, key)));
    }
    let shared = shared_account(env);
    Ok(get(shared)?.map(|key| (shared.to_string(), key)))
}

/// Comprueba que una deploy key sea del tipo (`dev:`/`prod:`) y del deployment del entorno;
/// devuelve el nombre del deployment. Nunca incluye la key en el error.
pub fn check_deploy_key(env: &devtool_core::Environment, key: &str) -> Result<String> {
    let prefix = if env.kind == devtool_core::EnvKind::CloudProd { "prod:" } else { "dev:" };
    let Some((target, rest)) = key.trim().split_once('|') else {
        bail!("that doesn't look like a Convex deploy key (“{prefix}<deployment>|…”)");
    };
    let Some(name) = target.strip_prefix(prefix) else {
        bail!("{} needs a {prefix} deploy key", env.name);
    };
    if name.is_empty() || rest.is_empty() {
        bail!("that doesn't look like a Convex deploy key (“{prefix}<deployment>|…”)");
    }
    if env.convex_deployment.as_deref().is_some_and(|d| d != name) {
        bail!("the key is for “{name}”, not for {}", env.name);
    }
    Ok(name.to_string())
}

/// Valida el nombre de una cuenta que el usuario puede escribir con el CLI.
pub fn writable_account(account: &str) -> Result<()> {
    if account != PROD_DEPLOY_KEY && account != DEV_DEPLOY_KEY {
        bail!("only “{PROD_DEPLOY_KEY}” or “{DEV_DEPLOY_KEY}” can be stored");
    }
    Ok(())
}

#[cfg(test)]
mod deploy_tests {
    use devtool_core::{EnvKind, Environment};

    fn env(kind: EnvKind, deployment: Option<&str>) -> Environment {
        Environment { name: "dev".into(), kind, label: None, convex_url: None, convex_site_url: None, convex_deployment: deployment.map(str::to_string), frontend_url: None, branch: None, credential: None, created_at: 0, project: None }
    }

    #[test]
    fn deploy_key_del_entorno() {
        let dev = env(EnvKind::CloudDev, Some("happy-cat-1"));
        assert_eq!(super::check_deploy_key(&dev, "dev:happy-cat-1|abc").unwrap(), "happy-cat-1");
        assert!(super::check_deploy_key(&dev, "prod:happy-cat-1|abc").is_err());
        assert!(super::check_deploy_key(&dev, "dev:other-dog-2|abc").is_err());
        assert!(super::check_deploy_key(&dev, "nonsense").is_err());
        let err = super::check_deploy_key(&dev, "dev:other-dog-2|SECRET").unwrap_err().to_string();
        assert!(!err.contains("SECRET"));
        assert_eq!(super::deploy_account(&dev), "deploy-key:dev");
    }
}
