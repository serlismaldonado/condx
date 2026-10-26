// Modo nativo del entorno local: el backend corre con el binario
// `convex-local-backend` (el mismo que embebe la edición de escritorio) y el
// deployer es `convex dev` en el repo, los dos como procesos del daemon. No
// hace falta Docker. Los desechables también pueden ser nativos.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use devtool_core::{Credential, EnvKind, Environment};
use serde_json::json;
use tokio::io::AsyncWriteExt;

use crate::cmd::env_file_value;
use crate::config::home;
use crate::jobs::{log_line, Job};
use crate::probes::convex::{convex_bin, target};
use crate::procs::{self, Spec};
use crate::projects::Project;
use crate::state::Shared;
use crate::store::now_ms;

pub const BACKEND: &str = "native-backend";
pub const DEPLOYER: &str = "native-deployer";
const MODE_KEY: &str = "meta:local-mode";
const DEFAULT_LOCAL_PORT: u16 = 3210;

/// Puerto del backend local; `CONDX_LOCAL_PORT` solo para pruebas aisladas.
pub fn local_port() -> u16 {
    devtool_core::paths::var(&["CONDX_LOCAL_PORT", "HESKALA_DEVD_LOCAL_PORT"]).and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_LOCAL_PORT)
}

/// Proyecto de compose del devbox; `CONDX_COMPOSE_PROJECT` solo para pruebas.
fn compose_project() -> String {
    devtool_core::paths::var(&["CONDX_COMPOSE_PROJECT", "HESKALA_DEVD_COMPOSE_PROJECT"]).unwrap_or_else(|| "heskala".into())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Docker,
    Native,
}

pub fn mode(state: &Shared) -> Mode {
    match state.probe(MODE_KEY).and_then(|p| p["data"]["mode"].as_str().map(str::to_string)).as_deref() {
        Some("native") => Mode::Native,
        _ => Mode::Docker,
    }
}

fn set_mode(state: &Shared, m: Mode) {
    state.set_probe(MODE_KEY, true, json!({ "mode": if m == Mode::Native { "native" } else { "docker" } }), None);
}

pub fn native_dir(name: &str) -> PathBuf {
    home().join("native").join(name)
}

pub fn local_env_file() -> PathBuf {
    home().join("native").join("local.env")
}

/// El binario, copiado fuera del repo (~/Documents está protegido por macOS
/// y un binario lanzado desde ahí por launchd se traba esperando permiso).
pub fn binary(state: &Shared) -> Result<PathBuf> {
    // El sitio en la nube trae el de Linux en la imagen.
    if let Some(p) = bundled() {
        return Ok(p);
    }
    let installed = home().join("bin").join("convex-local-backend");
    let triple = if cfg!(target_arch = "aarch64") { "aarch64-apple-darwin" } else { "x86_64-apple-darwin" };
    // El del repo de heskala-erp si está; si no, el que ya bajó el CLI de Convex (`convex dev --local`).
    let source = state
        .config
        .repo
        .as_ref()
        .map(|r| r.join(format!("src-tauri/binaries/convex-local-backend-{triple}")))
        .filter(|p| p.is_file())
        .or_else(cli_cached_binary)
        .unwrap_or_default();
    let source_len = std::fs::metadata(&source).ok().filter(|m| m.is_file()).map(|m| m.len());
    let installed_len = std::fs::metadata(&installed).map(|m| m.len()).ok();
    match (source_len, installed_len) {
        (Some(a), Some(b)) if a == b => {}
        (Some(_), _) => {
            std::fs::create_dir_all(installed.parent().unwrap())?;
            let tmp = installed.with_extension("new");
            std::fs::copy(&source, &tmp)?;
            std::fs::rename(&tmp, &installed)?;
        }
        (None, Some(_)) => {}
        (None, None) => bail!("the local backend binary is missing: download it in Condx (Welcome or Settings) or run `npx convex dev --local` once"),
    }
    Ok(installed)
}

/// El binario que trae la imagen del sitio en la nube (`CONDX_CONVEX_BACKEND`).
fn bundled() -> Option<PathBuf> {
    std::env::var_os("CONDX_CONVEX_BACKEND").map(PathBuf::from).filter(|p| p.is_file())
}

/// El binario más nuevo que dejó el CLI de Convex en ~/.cache/convex/binaries/<versión>/.
pub fn cli_cached_binary() -> Option<PathBuf> {
    let dir = crate::config::user_home().join(".cache/convex/binaries");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path().join("convex-local-backend")).filter(|p| p.is_file()).collect();
    // Los nombres son `precompiled-AAAA-MM-DD-hash`: el orden de texto es el de fecha.
    versions.sort();
    versions.pop()
}

/// Dónde está el binario, sin copiar nada (para el chequeo del primer arranque): el instalado,
/// el del repo o el que dejó el CLI de Convex.
pub fn find_binary(state: &Shared) -> Option<PathBuf> {
    if let Some(p) = bundled() {
        return Some(p);
    }
    let installed = home().join("bin").join("convex-local-backend");
    let triple = if cfg!(target_arch = "aarch64") { "aarch64-apple-darwin" } else { "x86_64-apple-darwin" };
    let repo = state.config.repo.as_ref().map(|r| r.join(format!("src-tauri/binaries/convex-local-backend-{triple}")));
    [Some(installed), repo].into_iter().flatten().find(|p| p.is_file()).or_else(cli_cached_binary)
}

pub fn available(state: &Shared) -> bool {
    binary(state).is_ok()
}

fn random_hex() -> Result<String> {
    let mut bytes = [0u8; 32];
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Crea (si falta) la identidad de una instancia: nombre, secreto y admin key.
pub async fn ensure_instance(state: &Shared, env_file: &Path, instance: &str, port: u16) -> Result<()> {
    if env_file.exists() && env_file_value(env_file, "CONVEX_SELF_HOSTED_ADMIN_KEY").is_some() {
        return Ok(());
    }
    let bin = binary(state)?;
    let secret = random_hex()?;
    // Sin pasar por `run`, que redacta: la clave no se loguea.
    let out = tokio::process::Command::new(&bin)
        .args(["keygen", "admin-key", "--instance-name", instance, "--instance-secret", &secret])
        .output()
        .await?;
    if !out.status.success() {
        bail!("keygen failed: {}", devtool_core::redact(&String::from_utf8_lossy(&out.stderr)));
    }
    let key = String::from_utf8_lossy(&out.stdout).lines().last().unwrap_or_default().trim().to_string();
    if !key.contains('|') {
        bail!("keygen didn't return a valid admin key");
    }
    std::fs::create_dir_all(env_file.parent().unwrap())?;
    std::fs::write(
        env_file,
        format!(
            "CONVEX_SELF_HOSTED_URL=http://127.0.0.1:{port}\nCONVEX_SELF_HOSTED_ADMIN_KEY=\"{key}\"\nCONVEX_INSTANCE_NAME={instance}\nCONVEX_INSTANCE_SECRET={secret}\nRUNTIME=native\nPORT={port}\n"
        ),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(env_file, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Backend nativo: datos en `dir`, identidad en `env_file`, en `port` y `port + 1`.
pub fn backend_spec(state: &Shared, proc_name: &str, dir: &Path, env_file: &Path) -> Result<Spec> {
    let bin = binary(state)?;
    let get = |k: &str| env_file_value(env_file, k).ok_or_else(|| anyhow!("missing {k} in {}", env_file.display()));
    let port: u16 = get("PORT")?.parse().unwrap_or(DEFAULT_LOCAL_PORT);
    let site = port + 1;
    std::fs::create_dir_all(dir.join("storage"))?;
    Ok(Spec {
        name: proc_name.to_string(),
        program: bin.display().to_string(),
        args: vec![
            "--port".into(),
            port.to_string(),
            "--site-proxy-port".into(),
            site.to_string(),
            "--interface".into(),
            "127.0.0.1".into(),
            "--instance-name".into(),
            get("CONVEX_INSTANCE_NAME")?,
            "--instance-secret".into(),
            get("CONVEX_INSTANCE_SECRET")?,
            "--convex-origin".into(),
            crate::site::public_url(port).unwrap_or_else(|| format!("http://127.0.0.1:{port}")),
            "--convex-site".into(),
            crate::site::public_url(site).unwrap_or_else(|| format!("http://127.0.0.1:{site}")),
            "--local-storage".into(),
            dir.join("storage").display().to_string(),
            "--disable-beacon".into(),
            "--do-not-require-ssl".into(),
            dir.join("db.sqlite3").display().to_string(),
        ],
        env: vec![],
        cwd: dir.to_path_buf(),
        ready_url: Some(format!("http://127.0.0.1:{port}/version")),
        ready_secs: 60,
    })
}

/// Carpeta espejo del repo para `convex dev`: enlaces a todo lo del repo
/// menos los `.env*`. Así la CLI escribe su `.env.local` (CONVEX_SELF_HOSTED_*,
/// VITE_CONVEX_URL) ahí y no toca el del repo, que usa Vite. Las funciones
/// se resuelven por el enlace a `convex/`, igual que en el contenedor.
pub(crate) fn deployer_root(project: &Project) -> Result<PathBuf> {
    let root = project.mirror_dir();
    std::fs::create_dir_all(&root)?;
    // Enlaces viejos fuera (el repo pudo cambiar); el .env.local propio se conserva.
    for entry in std::fs::read_dir(&root)?.flatten() {
        let path = entry.path();
        if path.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
            let _ = std::fs::remove_file(&path);
        }
    }
    for entry in std::fs::read_dir(&project.repo)?.flatten() {
        let name = entry.file_name();
        let name_s = name.to_string_lossy();
        if name_s.starts_with(".env") || name_s == ".DS_Store" || name_s == "convex" || name_s == "convex.json" {
            continue;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(entry.path(), root.join(&name))?;
    }
    // Las funciones se apuntan por su ruta real (relativa), no por un enlace:
    // con `convex/` enlazado, el bundler y la CLI no coinciden en las rutas
    // («found wrong number of app bundles»).
    let mut config: serde_json::Value = std::fs::read_to_string(project.repo.join("convex.json"))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({}));
    // Respeta un `functions` propio del proyecto (relativo a su repo).
    let functions = config["functions"].as_str().map(str::to_string).unwrap_or_else(|| "convex".into());
    config["functions"] = json!(relative(&root, &project.repo.join(functions)));
    std::fs::write(root.join("convex.json"), serde_json::to_string_pretty(&config)?)?;
    Ok(root)
}

/// Ruta de `to` relativa a `from` (las dos absolutas).
fn relative(from: &Path, to: &Path) -> String {
    let (a, b): (Vec<_>, Vec<_>) = (from.components().collect(), to.components().collect());
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut parts: Vec<String> = std::iter::repeat_n("..".to_string(), a.len() - common).collect();
    parts.extend(b[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    parts.join("/")
}

/// Deployer nativo: `convex dev` (desde la carpeta espejo), como el contenedor deployer-dev.
pub fn deployer_spec(state: &Shared, project: &Project) -> Result<Spec> {
    let file = project.env_file();
    let get = |k: &str| env_file_value(&file, k).ok_or_else(|| anyhow!("missing {k} in {}", file.display()));
    let local_cli = project.repo.join("node_modules/.bin/convex");
    let (bin, prefix) = if project.is_default() && !crate::site::is_cloud() {
        convex_bin(state)
    } else if local_cli.exists() {
        (local_cli.display().to_string(), vec![])
    } else {
        ("npx".to_string(), vec!["convex".to_string()])
    };
    let mut args = prefix;
    args.extend(["dev", "--typecheck=disable", "--tail-logs=always"].map(String::from));
    Ok(Spec {
        name: project.proc("deployer"),
        program: bin,
        args,
        env: vec![
            ("CONVEX_SELF_HOSTED_URL".into(), get("CONVEX_SELF_HOSTED_URL")?),
            ("CONVEX_SELF_HOSTED_ADMIN_KEY".into(), get("CONVEX_SELF_HOSTED_ADMIN_KEY")?),
            ("CONVEX_DEPLOYMENT".into(), String::new()),
        ],
        cwd: deployer_root(project)?,
        ready_url: None,
        ready_secs: 0,
    })
}

/// Puerto del backend local de un proyecto (el por defecto admite el de pruebas).
pub fn backend_port(project: &Project) -> u16 {
    if project.is_default() {
        local_port()
    } else {
        project.backend_port
    }
}

pub async fn start_backend(state: &Shared, project: &Project) -> Result<()> {
    ensure_instance(state, &project.env_file(), &project.instance(), backend_port(project)).await?;
    procs::start(state, backend_spec(state, &project.proc("backend"), &project.native_dir(), &project.env_file())?).await.map(|_| ())
}

pub async fn start_deployer(state: &Shared, project: &Project) -> Result<()> {
    // Dos deployers escribiendo convex/_generated se disparan uno al otro sin fin.
    if project.is_default()
        && crate::probes::docker_container(state, &compose_project(), "deployer-dev").map(|c| c["state"] == "running").unwrap_or(false) {
        bail!("the Docker deployer is running: stop it first (or use “Change local environment mode”)");
    }
    // Sin backend, convex dev reintenta y se cae.
    start_backend(state, project).await?;
    procs::start(state, deployer_spec(state, project)?).await.map(|_| ())
}

fn default_project(state: &Shared) -> Result<Project> {
    crate::projects::get(state, None)
}

/// El entorno devbox registrado (el «local» del switch).
fn devbox(state: &Shared) -> Result<Environment> {
    state
        .store
        .environments()?
        .into_iter()
        .find(|e| e.kind == EnvKind::Devbox && e.project.is_none())
        .ok_or_else(|| anyhow!("no devbox environment registered"))
}

fn docker_credential(state: &Shared) -> Result<Credential> {
    Ok(Credential::EnvFile { path: state.config.erp()?.join("docker/onprem-dev/.env").display().to_string(), key: "CONVEX_SELF_HOSTED_ADMIN_KEY".into() })
}

fn native_credential() -> Credential {
    Credential::EnvFile { path: local_env_file().display().to_string(), key: "CONVEX_SELF_HOSTED_ADMIN_KEY".into() }
}

fn switch_devbox(state: &Shared, m: Mode) -> Result<()> {
    let mut env = devbox(state)?;
    env.credential = Some(if m == Mode::Native { native_credential() } else { docker_credential(state)? });
    env.convex_url = Some(format!("http://localhost:{}", local_port()));
    state.store.upsert_environment(&env)?;
    set_mode(state, m);
    Ok(())
}

async fn compose(state: &Shared, job_id: &str, args: &[&str]) -> Result<()> {
    let project = compose_project();
    let mut full = vec!["compose", "-f", "docker/onprem-dev/compose.yaml", "-p", project.as_str()];
    full.extend_from_slice(args);
    crate::jobs::quick(job_id, "docker", &full, Some(state.config.erp()?), Duration::from_secs(600)).await.map(|_| ())
}

fn env_tmp(job_id: &str) -> PathBuf {
    home().join("tmp").join(format!("{job_id}-env.json"))
}

/// Corre la CLI de Convex contra `env` y devuelve stdout sin redactar (para
/// leer valores de variables): nunca se loguea.
async fn convex_raw(state: &Shared, env: &Environment, sub: &[&str], stdin: Option<&str>) -> Result<String> {
    let (bin, prefix) = convex_bin(state);
    let (extra, vars) = target(env, true)?;
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.args(&prefix).args(sub).args(&extra).current_dir(crate::projects::repo_for(state, Some(env))).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    for (k, v) in &vars {
        cmd.env(k, v);
    }
    let path = std::env::var("PATH").unwrap_or_default();
    cmd.env("PATH", format!("/opt/homebrew/bin:/usr/local/bin:{path}"));
    let mut child = cmd.spawn()?;
    if let (Some(text), Some(mut input)) = (stdin, child.stdin.take()) {
        input.write_all(text.as_bytes()).await?;
        drop(input);
    }
    let out = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output()).await.context("the Convex CLI didn't respond")??;
    if !out.status.success() {
        bail!("{}", devtool_core::redact(String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Lee nombres y valores de las variables de `env` a un archivo 0600.
async fn read_env_vars(state: &Shared, job_id: &str, env: &Environment) -> Result<usize> {
    let names: Vec<String> = convex_raw(state, env, &["env", "list", "--names-only"], None)
        .await?
        .lines()
        .map(|l| l.split('=').next().unwrap_or_default().trim().to_string())
        .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
        .collect();
    let mut values = serde_json::Map::new();
    for name in &names {
        let mut v = convex_raw(state, env, &["env", "get", name], None).await?;
        if v.ends_with('\n') {
            v.pop();
        }
        values.insert(name.clone(), json!(v));
    }
    let file = env_tmp(job_id);
    std::fs::create_dir_all(file.parent().unwrap())?;
    std::fs::write(&file, serde_json::to_string(&values)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
    }
    log_line(job_id, &format!("{} variables read: {}", names.len(), names.join(", ")));
    Ok(names.len())
}

/// Escribe las variables leídas en `env` (valor por stdin, nunca en argv) y borra el archivo.
async fn write_env_vars(state: &Shared, job_id: &str, env: &Environment) -> Result<usize> {
    let file = env_tmp(job_id);
    let raw = std::fs::read_to_string(&file).context("missing the variables read (previous step)")?;
    let values: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&raw)?;
    for (name, value) in &values {
        convex_raw(state, env, &["env", "set", name], Some(value.as_str().unwrap_or_default())).await?;
    }
    let _ = std::fs::remove_file(&file);
    log_line(job_id, &format!("{} variables copied to the native backend", values.len()));
    Ok(values.len())
}

/// Pasos de las recetas `migrate-native` y `local-mode`.
pub async fn builtin(state: &Shared, job: &mut Job, name: &str, cancel: &mut tokio::sync::watch::Receiver<bool>) -> Result<String> {
    let id = job.id.clone();
    match name {
        "native-backup" => {
            let env = devbox(state)?;
            if mode(state) == Mode::Native {
                bail!("the local environment is already in native mode");
            }
            let (backup, path) = crate::pipeline::export(state, &id, &env, cancel).await?;
            job.outputs.insert("backup_path".into(), path.display().to_string());
            Ok(format!("backup #{backup} of the Docker backend"))
        }
        "native-read-env" => {
            let n = read_env_vars(state, &id, &devbox(state)?).await?;
            Ok(format!("{n} environment variables read"))
        }
        "native-prepare" => {
            let bin = binary(state)?;
            let dir = native_dir("local");
            if dir.join("db.sqlite3").exists() {
                // Datos de un modo nativo anterior: se apartan, no se pisan.
                let aside = native_dir(&format!("local-{}", crate::jobs::stamp(now_ms())));
                std::fs::rename(&dir, &aside)?;
                log_line(&id, &format!("Previous native data moved to {}", aside.display()));
            }
            ensure_instance(state, &local_env_file(), "heskala-local", local_port()).await?;
            Ok(format!("binary at {}", bin.display()))
        }
        "native-stop-docker" => {
            compose(state, &id, &["stop", "deployer-dev", "backend"]).await?;
            Ok("Docker backend and deployer stopped".into())
        }
        "native-start-backend" => {
            start_backend(state, &default_project(state)?).await?;
            Ok(format!("native backend at http://localhost:{}", local_port()))
        }
        "native-import" => {
            // En un reintento el backend nativo puede no estar corriendo.
            start_backend(state, &default_project(state)?).await?;
            let path = PathBuf::from(job.outputs.get("backup_path").ok_or_else(|| anyhow!("missing the backup (previous step)"))?);
            let mut env = devbox(state)?;
            env.credential = Some(native_credential());
            crate::pipeline::import(state, &id, &env, &path, cancel).await?;
            Ok("data imported into the native backend".into())
        }
        "native-write-env" => {
            start_backend(state, &default_project(state)?).await?;
            let mut env = devbox(state)?;
            env.credential = Some(native_credential());
            let n = write_env_vars(state, &id, &env).await?;
            Ok(format!("{n} variables copied"))
        }
        "native-switch" => {
            switch_devbox(state, Mode::Native)?;
            Ok("the local environment is now in native mode".into())
        }
        "native-start-deployer" => {
            start_deployer(state, &default_project(state)?).await?;
            Ok("native deployer (convex dev) running".into())
        }
        "local-mode" => {
            let target_mode = match job.args.get("mode").map(String::as_str) {
                Some("native") => Mode::Native,
                Some("docker") => Mode::Docker,
                other => bail!("invalid mode: {other:?} (native or docker)"),
            };
            if target_mode == Mode::Native {
                if !local_env_file().exists() {
                    bail!("native mode has no data yet: use the “Migrate to native mode” recipe");
                }
                compose(state, &id, &["stop", "deployer-dev", "backend"]).await?;
                start_backend(state, &default_project(state)?).await?;
                switch_devbox(state, Mode::Native)?;
                start_deployer(state, &default_project(state)?).await?;
                Ok("native mode: backend and deployer as daemon processes".into())
            } else {
                procs::stop(state, DEPLOYER).await?;
                procs::stop(state, BACKEND).await?;
                compose(state, &id, &["up", "-d", "backend", "deployer-dev"]).await?;
                switch_devbox(state, Mode::Docker)?;
                Ok("Docker mode: backend and deployer in containers (with their pre-migration data)".into())
            }
        }
        other => bail!("unknown internal step: {other}"),
    }
}

/// Desechable nativo: backend con datos en ~/.condx/daemon/native/throwaway-<nombre>.
pub async fn create_throwaway(state: &Shared, job_id: &str, name: &str, port: u16, env_file: &Path) -> Result<String> {
    let instance = crate::jobs::container_name(name);
    ensure_instance(state, env_file, &instance, port).await?;
    let dir = native_dir(&format!("throwaway-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    log_line(job_id, &format!("Native backend {instance} on port {port} (data in {})", dir.display()));
    procs::start(state, backend_spec(state, &format!("throwaway-{name}"), &dir, env_file)?).await?;
    Ok(format!("http://127.0.0.1:{port}"))
}



pub async fn destroy_throwaway(state: &Shared, name: &str) -> Result<()> {
    procs::stop(state, &format!("throwaway-{name}")).await?;
    let _ = std::fs::remove_dir_all(native_dir(&format!("throwaway-{name}")));
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn ruta_relativa() {
        use std::path::Path;
        assert_eq!(super::relative(Path::new("/a/b/c"), Path::new("/a/x/convex")), "../../x/convex");
        assert_eq!(super::relative(Path::new("/Users/u/.heskala/devd/native/deployer-root"), Path::new("/Users/u/Documents/repo/convex")), "../../../../Documents/repo/convex");
    }
}
