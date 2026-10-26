// Actualizaciones de Condx (docs/design/cloud-site.md §9.3, fase S8): el daemon revisa el último
// release, verifica su firma y, con la aprobación de la persona (la receta `update-condx`, que se
// aprueba en el teléfono o en la Mac como cualquier otra), instala la app, el daemon y el host de
// agentes y se reinicia.
//
// - El release trae `manifest.json` (versión, archivo, SHA-256) y `manifest.json.sig`: la firma
//   Ed25519 de esos bytes, hecha en el CI con la llave de actualización. El daemon solo cree en un
//   manifiesto firmado con la llave cuya parte pública está aquí, y en un archivo con ese SHA-256.
// - Lo instala el daemon (launchd lo mantiene vivo aunque la app esté rota): la app nueva reemplaza a
//   la anterior (que queda en ~/.condx/backups), el daemon como `install-daemon.sh` (archivo nuevo +
//   rename) y después se reinicia solo.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use crate::state::Shared;

/// La parte pública de la llave de actualización (Ed25519, 32 bytes en base64). La privada vive en
/// los secretos del CI (`CONDX_UPDATE_KEY`).
const PUBLIC_KEY: &str = "jlX5WWQfx1fVoPVGtztgLTxL3Qs25Z3iK1IFp42/vzw=";
const REPO: &str = "serlismaldonado/condx";
const STATE_KEY: &str = "meta:update";
pub const RECIPE: &str = "update-condx";
pub const RELEASE_RECIPE: &str = "release-condx";

pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// `1.2.3` mayor que `1.2.0` (sin sufijos: los releases son `vX.Y.Z`).
pub fn newer(candidate: &str, than: &str) -> bool {
    let parse = |v: &str| v.trim_start_matches('v').split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parse(candidate) > parse(than)
}

/// El manifiesto si la firma es de la llave de actualización.
pub fn verify(manifest: &[u8], sig: &[u8]) -> Result<Value> {
    verify_with(PUBLIC_KEY, manifest, sig)
}

fn verify_with(public_key: &str, manifest: &[u8], sig: &[u8]) -> Result<Value> {
    use base64::Engine;
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let key: [u8; 32] = base64::engine::general_purpose::STANDARD.decode(public_key)?.try_into().map_err(|_| anyhow!("bad update key"))?;
    let vk = VerifyingKey::from_bytes(&key)?;
    let sig = Signature::from_slice(sig).map_err(|_| anyhow!("the release's signature is malformed"))?;
    vk.verify(manifest, &sig).map_err(|_| anyhow!("the release isn't signed with Condx's update key"))?;
    let m: Value = serde_json::from_slice(manifest)?;
    for k in ["version", "url", "sha256"] {
        if !m[k].is_string() {
            bail!("the manifest has no {k}");
        }
    }
    Ok(m)
}

fn get(url: &str, limit: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let r = ureq::get(url).timeout(Duration::from_secs(600)).call().map_err(|e| anyhow!("couldn't download {url}: {e}"))?;
    let mut buf = Vec::new();
    r.into_reader().take(limit).read_to_end(&mut buf)?;
    Ok(buf)
}

/// La versión del último release según la API de GitHub (al día; el enlace «latest/download» sale
/// de una caché que tarda minutos en cambiar después de publicar).
fn latest_version() -> Result<String> {
    if let Ok(base) = std::env::var("CONDX_RELEASES_URL") {
        let m: Value = serde_json::from_slice(&get(&format!("{base}/manifest.json"), 64 * 1024)?)?;
        return Ok(m["version"].as_str().unwrap_or_default().to_string());
    }
    let mut req = ureq::get(&format!("https://api.github.com/repos/{REPO}/releases/latest")).timeout(Duration::from_secs(20)).set("Accept", "application/vnd.github+json").set("User-Agent", "condx");
    if let Some(t) = crate::secrets::get(crate::gitflow::TOKEN_ACCOUNT).ok().flatten() {
        req = req.set("Authorization", &format!("Bearer {}", t.trim()));
    }
    let r: Value = req.call().map_err(|e| anyhow!("couldn't ask GitHub for the latest release: {e}"))?.into_json()?;
    Ok(r["tag_name"].as_str().unwrap_or_default().trim_start_matches('v').to_string())
}

/// El manifiesto firmado de una versión (su propio enlace, sin caché de «latest»).
fn manifest_of(version: &str) -> Result<Value> {
    let base = std::env::var("CONDX_RELEASES_URL").unwrap_or_else(|_| format!("https://github.com/{REPO}/releases/download/v{version}"));
    let manifest = get(&format!("{base}/manifest.json"), 64 * 1024)?;
    let sig = get(&format!("{base}/manifest.json.sig"), 1024)?;
    verify(&manifest, &sig)
}

/// El último release firmado (o `version`, recién publicado), si es más nuevo que este daemon.
pub fn latest_or(version: Option<&str>) -> Result<Option<Value>> {
    let v = match version {
        Some(v) => v.to_string(),
        None => latest_version()?,
    };
    if v.is_empty() || !newer(&v, current()) {
        return Ok(None);
    }
    let m = manifest_of(&v)?;
    Ok(newer(m["version"].as_str().unwrap_or("0"), current()).then_some(m))
}


fn saved(state: &Shared) -> Value {
    state.store.probes().ok().and_then(|l| l.into_iter().find(|(k, _)| k == STATE_KEY)).map(|(_, v)| v).unwrap_or(json!({}))
}

pub fn status(state: &Shared) -> Value {
    let mut s = saved(state);
    s["current"] = json!(current());
    s
}

/// Revisa y, si hay una versión nueva que no se ofreció, pide la aprobación para instalarla.
pub async fn check(state: &Shared) -> Result<Value> {
    check_for(state, None).await
}

/// Como `check`, para una versión que se acaba de publicar (sin esperar a que GitHub la dé como la última).
pub async fn check_for(state: &Shared, version: Option<String>) -> Result<Value> {
    let found = tokio::task::spawn_blocking(move || latest_or(version.as_deref())).await??;
    let mut s = saved(state);
    s["checkedAt"] = json!(crate::store::now_ms());
    s["available"] = found.clone().unwrap_or(Value::Null);
    if let Some(m) = &found {
        let version = m["version"].as_str().unwrap_or("?").to_string();
        if s["offered"].as_str() != Some(version.as_str()) {
            match local_env(state) {
                Some(env) => match crate::jobs::request(state, RECIPE, &env, &json!({ "version": version }), "condx-updater").await {
                    Ok(job) => {
                        tracing::info!("update: Condx {version} is out; waiting for approval ({})", job.id);
                        s["offered"] = json!(version);
                        s["job"] = json!(job.id);
                    }
                    Err(e) => tracing::warn!("update: couldn't ask to install {version}: {e:#}"),
                },
                None => tracing::warn!("update: Condx {version} is out, but there's no local environment to approve it in"),
            }
        }
    }
    state.store.save_probe(STATE_KEY, &s)?;
    Ok(status(state))
}

/// El entorno local del proyecto de siempre (donde se aprueba la actualización).
fn local_env(state: &Shared) -> Option<String> {
    let envs = state.store.environments().ok()?;
    envs.iter().find(|e| e.kind.as_str() == "devbox" && e.project.as_deref().is_none_or(|p| p == crate::projects::DEFAULT)).or_else(|| envs.iter().find(|e| e.kind.as_str() == "devbox")).map(|e| e.name.clone())
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
}

/// Dónde está la app instalada (o dónde va).
fn app_path() -> PathBuf {
    let user = home().join("Applications/Condx.app");
    let system = PathBuf::from("/Applications/Condx.app");
    if !user.exists() && system.exists() {
        system
    } else {
        user
    }
}

fn run(cmd: &str, args: &[&str], dir: Option<&Path>) -> Result<()> {
    let mut c = Command::new(cmd);
    c.args(args);
    if let Some(d) = dir {
        c.current_dir(d);
    }
    let out = c.output().with_context(|| format!("couldn't run {cmd}"))?;
    if !out.status.success() {
        bail!("{cmd} failed: {}", String::from_utf8_lossy(&out.stderr).trim().chars().take(300).collect::<String>());
    }
    Ok(())
}

/// Descarga, verifica e instala `version` (el paso de la receta). Devuelve qué hizo; el reinicio va
/// después, cuando el trabajo ya figura como terminado.
pub fn apply(version: &str) -> Result<String> {
    use sha2::{Digest, Sha256};
    // El manifiesto de esa versión (no el de «latest»: GitHub lo sirve desde una caché que puede
    // seguir en la anterior unos minutos después de publicar).
    let m = manifest_of(version)?;
    if m["version"].as_str() != Some(version) {
        bail!("the latest release is {}, not {version}", m["version"].as_str().unwrap_or("?"));
    }
    let bytes = get(m["url"].as_str().unwrap_or_default(), 1024 * 1024 * 1024)?;
    let sum = format!("{:x}", Sha256::digest(&bytes));
    if !sum.eq_ignore_ascii_case(m["sha256"].as_str().unwrap_or_default()) {
        bail!("the download doesn't match the signed SHA-256");
    }
    let work = std::env::temp_dir().join(format!("condx-update-{version}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let tar = work.join("release.tar.gz");
    std::fs::write(&tar, &bytes)?;
    run("tar", &["-xzf", "release.tar.gz"], Some(&work))?;
    let (new_app, new_daemon) = (work.join("Condx.app"), work.join("condxd"));
    if !new_app.exists() || !new_daemon.exists() {
        bail!("the release is missing Condx.app or condxd");
    }
    let stamp = crate::store::now_ms();
    let backups = home().join(".condx/backups");
    std::fs::create_dir_all(&backups)?;
    // La app: la anterior queda de respaldo.
    let app = app_path();
    if app.exists() {
        std::fs::rename(&app, backups.join(format!("Condx.app-{stamp}")))?;
    }
    if let Some(parent) = app.parent() {
        std::fs::create_dir_all(parent)?;
    }
    run("ditto", &[new_app.to_str().unwrap_or_default(), app.to_str().unwrap_or_default()], None)?;
    // El host de agentes (si viene): el código y, si cambió el lock, sus dependencias.
    let host = crate::config::home().join("agent-host");
    let new_host = work.join("agent-host");
    if new_host.join("src").exists() {
        run("rsync", &["-a", "--delete", &format!("{}/", new_host.join("src").display()), &format!("{}/", host.join("src").display())], None)?;
        let lock_changed = std::fs::read(new_host.join("package-lock.json")).ok() != std::fs::read(host.join("package-lock.json")).ok();
        if lock_changed {
            for f in ["package.json", "package-lock.json"] {
                std::fs::copy(new_host.join(f), host.join(f))?;
            }
            run("npm", &["ci", "--omit=dev", "--silent"], Some(&host))?;
        }
    }
    // El daemon: archivo nuevo + rename (pisarlo hace que macOS lo mate); firmado como el instalador.
    let bin = crate::config::home().join("bin");
    let next = bin.join("condxd.new");
    std::fs::copy(&new_daemon, &next)?;
    let _ = std::fs::copy(bin.join("condxd"), backups.join(format!("condxd-{stamp}")));
    let _ = Command::new("codesign").args(["--force", "--sign", "Condx Dev", "--identifier", "dev.condx.daemon", next.to_str().unwrap_or_default()]).output();
    std::fs::rename(&next, bin.join("condxd"))?;
    let _ = std::fs::remove_dir_all(&work);
    Ok(format!("Condx {version} installed; restarting"))
}

/// Marca que la app estaba abierta al actualizar: el daemon nuevo la vuelve a abrir al arrancar.
fn reopen_marker() -> PathBuf {
    crate::config::home().join("reopen-app")
}

fn app_running() -> bool {
    Command::new("pgrep").args(["-x", "condx"]).output().map(|o| o.status.success()).unwrap_or(false)
}

/// Reinicia el daemon (launchd lo vuelve a levantar con el binario nuevo). La app la reabre el daemon
/// nuevo (`reopen_after_update`): un proceso lanzado desde este moriría con él en `kickstart -k`.
pub fn restart_soon() {
    if app_running() {
        let _ = std::fs::write(reopen_marker(), crate::store::now_ms().to_string());
    }
    let uid = Command::new("id").arg("-u").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    let script = format!("sleep 3; launchctl kickstart -k gui/{uid}/dev.condx.daemon");
    let _ = Command::new("sh").args(["-c", &format!("nohup sh -c {} >/dev/null 2>&1 &", shell_quote(&script))]).spawn();
}

/// Al arrancar después de una actualización: cierra la app vieja (sigue corriendo desde la carpeta de
/// respaldo) y abre la nueva. Sabe que hubo una por la marca o, si la dejó un daemon anterior a ella,
/// porque cambió la versión y hay un respaldo de la app de hace minutos.
pub fn reopen_after_update(state: &Shared) {
    const LAST: &str = "meta:last-version";
    let before = state.store.probes().ok().and_then(|l| l.into_iter().find(|(k, _)| k == LAST)).and_then(|(_, v)| v["version"].as_str().map(str::to_string));
    let _ = state.store.save_probe(LAST, &json!({ "version": current() }));
    let marked = reopen_marker().exists();
    let _ = std::fs::remove_file(reopen_marker());
    // Sin versión anterior anotada (un daemon de antes de esto) también cuenta: el respaldo lo dice.
    let just_updated = before.as_deref() != Some(current()) && recent_app_backup();
    if !(marked || just_updated) {
        return;
    }
    std::thread::spawn(move || {
        if app_running() {
            let _ = Command::new("osascript").args(["-e", "quit app \"Condx\""]).output();
            for _ in 0..20 {
                if !app_running() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        let app = app_path();
        match Command::new("open").arg(&app).output() {
            Ok(o) if o.status.success() => tracing::info!("update: reopened {}", app.display()),
            _ => tracing::warn!("update: couldn't reopen {}", app.display()),
        }
    });
}

/// Un respaldo de la app de los últimos 15 minutos (lo deja `apply`).
fn recent_app_backup() -> bool {
    let now = crate::store::now_ms();
    std::fs::read_dir(home().join(".condx/backups")).map(|d| d.flatten().any(|e| e.file_name().to_string_lossy().strip_prefix("Condx.app-").and_then(|t| t.parse::<i64>().ok()).is_some_and(|t| now - t < 15 * 60_000))).unwrap_or(false)
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Dónde se compila un release: en esta Mac si se puede (local primero), si no en la nube.
#[derive(Debug, PartialEq)]
pub enum Place {
    Local,
    /// Por qué no aquí.
    Cloud(String),
}

/// Local si este daemon corre en una Mac con el repo, la llave de actualización, OpenSSL 3 y gh.
pub fn place(repo: &Path) -> Place {
    let missing = |what: &str| Place::Cloud(what.to_string());
    if !cfg!(target_os = "macos") {
        return missing("this daemon isn't on a Mac (the app only builds on macOS)");
    }
    if !repo.join("scripts/release.sh").exists() {
        return missing("the Condx repo isn't on this Mac");
    }
    if !home().join(".condx/update-signing-key.pem").exists() {
        return missing("the update key isn't on this Mac");
    }
    if !Path::new("/opt/homebrew/opt/openssl@3/bin/openssl").exists() {
        return missing("OpenSSL 3 isn't installed (brew install openssl@3)");
    }
    if Command::new("gh").arg("--version").output().is_err() {
        return missing("gh isn't installed");
    }
    Place::Local
}

/// El repo de Condx: el primero de Mayer (el que lo mantiene).
pub fn condx_repo(state: &Shared) -> Option<PathBuf> {
    crate::roles::get(state, "mayer").and_then(|r| r.repos.first().map(|x| PathBuf::from(x.path.trim()))).filter(|p| p.join(".git").exists())
}

/// Publica un release: compila en esta Mac o, si no hay, lanza el del CI. Devuelve dónde y qué pasó.
pub fn release(repo: &Path, version: &str) -> Result<String> {
    let token = crate::secrets::get(crate::gitflow::TOKEN_ACCOUNT)?.ok_or_else(|| anyhow!("there's no GitHub token (condxd secret set github-token)"))?;
    let (script, why) = match place(repo) {
        Place::Local => ("scripts/release.sh", "built on this Mac".to_string()),
        Place::Cloud(reason) => ("scripts/release-cloud.sh", format!("no Mac available ({reason}): built in the cloud")),
    };
    let mut cmd = Command::new("bash");
    cmd.arg(script).current_dir(repo).env("GH_TOKEN", token.trim());
    if !version.trim().is_empty() {
        cmd.arg(version.trim());
    }
    let out = cmd.output().with_context(|| format!("couldn't run {script}"))?;
    let tail = |b: &[u8]| String::from_utf8_lossy(b).lines().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
    if !out.status.success() {
        bail!("{why}, but {script} failed:\n{}", devtool_core::redact(&tail(&out.stderr)));
    }
    Ok(format!("{why}.\n{}", devtool_core::redact(&tail(&out.stdout))))
}

/// Lo que piden los agentes (Mayer): un release de Condx, que espera la aprobación de la persona.
pub async fn request_release(state: &Shared, version: Option<&str>, by: &str) -> Result<crate::jobs::Job> {
    let env = local_env(state).ok_or_else(|| anyhow!("there's no local environment to approve the release in"))?;
    crate::jobs::request(state, RELEASE_RECIPE, &env, &json!({ "version": version.unwrap_or("") }), by).await
}

/// Revisa al arrancar y cada 6 horas.
pub fn spawn(state: &Shared) {
    let state = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let mut every = tokio::time::interval(Duration::from_secs(6 * 3600));
        loop {
            every.tick().await;
            if let Err(e) = check(&state).await {
                tracing::warn!("update: couldn't check for a new Condx: {e:#}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sin_el_repo_se_compila_en_la_nube() {
        let tmp = tempfile::tempdir().unwrap();
        match place(tmp.path()) {
            Place::Cloud(why) => assert!(why.contains("repo") || why.contains("Mac"), "{why}"),
            Place::Local => panic!("an empty folder can't build a release"),
        }
    }

    #[test]
    fn compara_versiones() {
        assert!(newer("0.2.0", "0.1.0"));
        assert!(newer("v1.0.0", "0.9.9"));
        assert!(newer("0.10.0", "0.9.0"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn solo_cree_en_lo_firmado_con_la_llave() {
        let m = br#"{"version":"0.2.0","url":"https://x/y.tar.gz","sha256":"ab"}"#;
        // Una firma de otra llave (o ninguna) no pasa.
        assert!(verify(m, &[0u8; 64]).is_err());
        assert!(verify(m, b"short").is_err());
    }

    #[test]
    fn un_manifiesto_firmado_pasa_y_uno_cambiado_no() {
        use base64::Engine;
        use ed25519_dalek::{Signer, SigningKey};
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let pk = base64::engine::general_purpose::STANDARD.encode(sk.verifying_key().to_bytes());
        let m = br#"{"version":"0.2.0","url":"https://x/y.tar.gz","sha256":"ab"}"#;
        let sig = sk.sign(m).to_bytes();
        assert_eq!(verify_with(&pk, m, &sig).unwrap()["version"], "0.2.0");
        let tampered = br#"{"version":"0.2.0","url":"https://evil/y.tar.gz","sha256":"ab"}"#;
        assert!(verify_with(&pk, tampered, &sig).is_err());
        // Firmado, pero sin lo necesario.
        let incomplete = br#"{"version":"0.2.0"}"#;
        assert!(verify_with(&pk, incomplete, &sk.sign(incomplete).to_bytes()).is_err());
    }
}
