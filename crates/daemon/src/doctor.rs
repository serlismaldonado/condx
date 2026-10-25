// Chequeo del primer arranque: lo que hace falta en esta Mac para trabajar con Convex desde
// Condx (Node, la sesión del CLI, el backend local, git y gh), cada cosa con cómo arreglarla.
// También baja el binario del backend local desde los releases de convex/convex-backend.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use crate::cmd;
use crate::config::{home, user_home};
use crate::state::Shared;

const QUICK: Duration = Duration::from_secs(8);
const RELEASES: &str = "https://api.github.com/repos/convex/convex-backend/releases?per_page=10";
/// De dónde se acepta bajar el zip (el repo se llamaba get-convex/convex-backend).
const TRUSTED: [&str; 2] = ["https://github.com/convex/convex-backend/releases/download/", "https://github.com/get-convex/convex-backend/releases/download/"];

fn check(id: &str, ok: bool, detail: impl Into<String>, fix: Option<&str>, optional: bool) -> Value {
    json!({ "id": id, "ok": ok, "detail": detail.into(), "fix": fix, "optional": optional })
}

/// Primera línea de la salida, sin espacios: la versión de un `--version`.
fn first_line(out: &cmd::Out) -> String {
    out.stdout.lines().next().unwrap_or_default().trim().to_string()
}

/// La sesión de `npx convex login` existe (solo se mira si el archivo está, nunca el token).
pub fn convex_logged_in(home: &Path) -> bool {
    home.join(".convex/config.json").is_file()
}

pub async fn report(state: &Shared) -> Value {
    let (node, git, gh, docker) = tokio::join!(
        cmd::run("node", &["--version"], None, &[], QUICK),
        cmd::run("git", &["--version"], None, &[], QUICK),
        cmd::run("gh", &["auth", "status"], None, &[], QUICK),
        cmd::run("docker", &["info", "--format", "{{.ServerVersion}}"], None, &[], QUICK),
    );
    let node_ok = node.ok && node_major(&first_line(&node)).is_some_and(|m| m >= 18);
    // Solo mirar: `native::binary` copia el binario, y un chequeo no debe cambiar nada.
    let backend = crate::native::find_binary(state);
    let logged = convex_logged_in(&user_home());
    let gh_installed = !gh.stderr.contains("couldn't run");
    let checks = vec![
        check(
            "node",
            node_ok,
            if node.ok { first_line(&node) } else { "not found".into() },
            (!node_ok).then_some("brew install node"),
            false,
        ),
        check(
            "convex-login",
            logged,
            if logged { "~/.convex/config.json" } else { "not logged in" },
            (!logged).then_some("npx convex login"),
            false,
        ),
        check(
            "backend",
            backend.is_some(),
            match &backend {
                Some(p) => p.display().to_string(),
                None => "missing".into(),
            },
            None,
            false,
        ),
        check("git", git.ok, if git.ok { first_line(&git) } else { "not found".into() }, (!git.ok).then_some("xcode-select --install"), false),
        check(
            "gh",
            gh.ok,
            if gh.ok { "logged in".to_string() } else if gh_installed { "not logged in".into() } else { "not found".into() },
            (!gh.ok).then_some(if gh_installed { "gh auth login" } else { "brew install gh" }),
            true,
        ),
        check(
            "docker",
            docker.ok,
            if docker.ok { format!("Docker {}", first_line(&docker)) } else { "not running (native mode doesn't need it)".into() },
            None,
            true,
        ),
    ];
    let ready = checks.iter().all(|c| c["ok"] == true || c["optional"] == true);
    json!({ "ready": ready, "checks": checks })
}

fn node_major(version: &str) -> Option<u32> {
    version.trim_start_matches('v').split('.').next()?.parse().ok()
}

fn triple() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64-apple-darwin"
    } else {
        "x86_64-apple-darwin"
    }
}

/// El zip del binario para esta Mac en el release más nuevo que lo traiga.
pub fn pick_asset(releases: &Value, triple: &str) -> Option<(String, String)> {
    let name = format!("convex-local-backend-{triple}.zip");
    releases.as_array()?.iter().filter(|r| r["draft"] != true).find_map(|r| {
        let asset = r["assets"].as_array()?.iter().find(|a| a["name"] == name.as_str())?;
        let url = asset["browser_download_url"].as_str()?;
        // Solo de los releases de Convex en GitHub: nada de URLs que traiga la respuesta a otro lado.
        TRUSTED.iter().any(|t| url.starts_with(t))
            .then(|| (r["tag_name"].as_str().unwrap_or_default().to_string(), url.to_string()))
    })
}

/// Baja el binario del backend local a ~/.condx/daemon/bin (donde lo busca el modo nativo).
pub async fn download_backend() -> Result<Value> {
    let list = cmd::run("curl", &["-fsSL", "-H", "Accept: application/vnd.github+json", RELEASES], None, &[], Duration::from_secs(30)).await;
    if !list.ok {
        bail!("couldn't list the Convex backend releases: {}", list.error());
    }
    let releases: Value = serde_json::from_str(&list.stdout).context("unexpected answer from GitHub")?;
    let (tag, url) = pick_asset(&releases, triple()).ok_or_else(|| anyhow!("no release has a backend for {}", triple()))?;
    let work = home().join("tmp").join(format!("backend-{}", crate::store::now_ms()));
    std::fs::create_dir_all(&work)?;
    let zip = work.join("backend.zip");
    let zip_s = zip.display().to_string();
    let got = cmd::run("curl", &["-fsSL", "-o", &zip_s, &url], None, &[], Duration::from_secs(300)).await;
    if !got.ok {
        let _ = std::fs::remove_dir_all(&work);
        bail!("download failed: {}", got.error());
    }
    let work_s = work.display().to_string();
    let unzip = cmd::run("unzip", &["-o", "-q", &zip_s, "-d", &work_s], None, &[], Duration::from_secs(60)).await;
    let binary = work.join("convex-local-backend");
    if !unzip.ok || !binary.is_file() {
        let _ = std::fs::remove_dir_all(&work);
        bail!("the zip didn't have convex-local-backend: {}", unzip.error());
    }
    let installed = install(&binary)?;
    let _ = std::fs::remove_dir_all(&work);
    Ok(json!({ "version": tag, "path": installed.display().to_string() }))
}

fn install(binary: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let installed = home().join("bin").join("convex-local-backend");
    std::fs::create_dir_all(installed.parent().unwrap())?;
    let tmp = installed.with_extension("new");
    std::fs::copy(binary, &tmp)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&tmp, &installed)?;
    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_de_node() {
        assert_eq!(node_major("v22.11.0"), Some(22));
        assert_eq!(node_major("18.0.0"), Some(18));
        assert_eq!(node_major("nope"), None);
    }

    #[test]
    fn elige_el_zip_de_esta_mac_y_solo_de_convex() {
        let releases = json!([
            { "tag_name": "draft", "draft": true, "assets": [{ "name": "convex-local-backend-aarch64-apple-darwin.zip", "browser_download_url": "https://github.com/get-convex/convex-backend/releases/download/draft/x.zip" }] },
            { "tag_name": "evil", "assets": [{ "name": "convex-local-backend-aarch64-apple-darwin.zip", "browser_download_url": "https://evil.example/x.zip" }] },
            { "tag_name": "lookalike", "assets": [{ "name": "convex-local-backend-aarch64-apple-darwin.zip", "browser_download_url": "https://github.com/convex/convex-backend.evil/releases/download/x.zip" }] },
            { "tag_name": "linux-only", "assets": [{ "name": "convex-local-backend-x86_64-unknown-linux-gnu.zip", "browser_download_url": "https://github.com/get-convex/convex-backend/releases/download/linux-only/x.zip" }] },
            { "tag_name": "precompiled-2026-10-10", "assets": [{ "name": "convex-local-backend-aarch64-apple-darwin.zip", "browser_download_url": "https://github.com/convex/convex-backend/releases/download/precompiled-2026-10-10/convex-local-backend-aarch64-apple-darwin.zip" }] }
        ]);
        let (tag, url) = pick_asset(&releases, "aarch64-apple-darwin").unwrap();
        assert_eq!(tag, "precompiled-2026-10-10");
        assert!(url.ends_with("aarch64-apple-darwin.zip"));
        assert!(pick_asset(&releases, "riscv").is_none());
    }

    #[test]
    fn sesion_de_convex_solo_por_el_archivo() {
        let dir = std::env::temp_dir().join(format!("condx-doctor-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".convex")).unwrap();
        assert!(!convex_logged_in(&dir));
        std::fs::write(dir.join(".convex/config.json"), "{}").unwrap();
        assert!(convex_logged_in(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
