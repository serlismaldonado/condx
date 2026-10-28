// La política de Git de los agentes (docs/design/cloud-site.md §9.4, fase S6):
//
// - Respaldo: lo que deja un agente en su rama (`condx/fix-…`) se sube a GitHub en cuanto el daemon
//   hace el commit; ningún arreglo vive solo en esta Mac.
// - Despliegues por PR: antes de desplegar un arreglo se sube la rama y se abre (o se reusa) un PR
//   contra la rama principal del repo; el flujo arranca después, con sus aprobaciones. «Directo»
//   (pedido explícito) se salta el PR; producción igual pide su aprobación.
//
// El token de GitHub es un secreto del daemon (`github-token`): solo va en el entorno del `git push`
// de ese momento (un credential helper que lo lee de ahí) y en la cabecera del pedido a la API; la
// config de git del usuario no se toca. Con un remoto por SSH, `git push` usa el agente SSH.

use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use crate::state::Shared;

/// El token de GitHub en los secretos del daemon (`condxd secret set github-token`).
pub const TOKEN_ACCOUNT: &str = "github-token";

/// Lo que hace la política (Configuración → Agentes; las dos encendidas de fábrica).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Policy {
    /// Subir la rama de cada arreglo en cuanto queda el commit.
    pub backup_push: bool,
    /// Abrir un PR antes de desplegar un arreglo (si no se pide directo).
    pub deploy_via_pr: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self { backup_push: true, deploy_via_pr: true }
    }
}

pub(crate) fn api() -> String {
    std::env::var("CONDX_GITHUB_API").unwrap_or_else(|_| "https://api.github.com".into())
}

/// El token para el repo de `dir`: primero el de su dueño (`github-token:<dueño>`, como en el sitio en
/// la nube, que tiene uno por organización), después el general y los demás.
pub(crate) fn token_for(dir: &Path) -> Option<String> {
    let owner = origin(dir).and_then(|u| github_slug(&u)).map(|(o, _)| o);
    let mut accounts: Vec<String> = owner.iter().map(|o| format!("{TOKEN_ACCOUNT}:{o}")).collect();
    accounts.push(TOKEN_ACCOUNT.to_string());
    accounts.extend(crate::secrets::accounts_with_prefix(&format!("{TOKEN_ACCOUNT}:")));
    accounts.iter().find_map(|a| crate::secrets::get(a).ok().flatten().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()))
}

fn git(dir: &Path, args: &[&str], token: Option<&str>) -> Result<String> {
    let mut cmd = Command::new("git");
    if let Some(t) = token {
        // El helper lee el token del entorno de este proceso; nada queda en la config ni en la URL.
        cmd.args(["-c", "credential.helper=", "-c", "credential.helper=!f() { echo username=x-access-token; echo \"password=$CONDX_GIT_TOKEN\"; }; f"]);
        cmd.env("CONDX_GIT_TOKEN", t);
    }
    let out = cmd.args(args).current_dir(dir).env("GIT_TERMINAL_PROMPT", "0").output().context("couldn't run git")?;
    if !out.status.success() {
        bail!("git {}: {}", args.first().unwrap_or(&""), devtool_core::redact(String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `owner/repo` de un remoto de GitHub (https o SSH).
pub fn github_slug(url: &str) -> Option<(String, String)> {
    let rest = url.trim().strip_prefix("https://github.com/").or_else(|| url.trim().strip_prefix("git@github.com:")).or_else(|| url.trim().strip_prefix("ssh://git@github.com/"))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let (owner, repo) = rest.split_once('/')?;
    (!owner.is_empty() && !repo.is_empty() && !repo.contains('/')).then(|| (owner.to_string(), repo.to_string()))
}

pub(crate) fn origin(dir: &Path) -> Option<String> {
    git(dir, &["remote", "get-url", "origin"], None).ok().filter(|u| !u.is_empty())
}

/// Trae lo nuevo de GitHub (`origin`), solo lectura: primero como esté configurado el remoto (SSH en la
/// Mac), si no por https con el token del dueño del repo (el sitio en la nube). Sin remoto, nada.
pub fn fetch_origin(dir: &Path) -> Result<()> {
    let Some(url) = origin(dir) else { return Ok(()) };
    if git(dir, &["fetch", "-q", "--prune", "origin"], None).is_ok() {
        sync_local_branches(dir);
        return Ok(());
    }
    let (owner, repo) = github_slug(&url).ok_or_else(|| anyhow!("the remote isn't on GitHub ({})", devtool_core::redact(&url)))?;
    let token = token_for(dir).ok_or_else(|| anyhow!("there's no GitHub token for {owner} (condxd secret set github-token:{owner})"))?;
    git(dir, &["fetch", "-q", "--prune", &format!("https://github.com/{owner}/{repo}.git"), "+refs/heads/*:refs/remotes/origin/*"], Some(&token))?;
    sync_local_branches(dir);
    Ok(())
}

/// En el sitio en la nube, sus ramas locales `dev`, `develop` y `main` quedan como las de GitHub
/// (nadie trabaja en ellas ahí; viejas, un agente que compara contra `dev` vería cambios que no son
/// suyos). En la Mac no se tocan: son las de la persona.
fn sync_local_branches(dir: &Path) {
    if !crate::site::is_cloud() {
        return;
    }
    // `update-ref` sobre una rama en checkout mueve su HEAD sin el árbol: esas no se tocan.
    let in_checkout = checked_out_branches(dir);
    for b in ["dev", "develop", "main"] {
        if in_checkout.iter().any(|c| c == b) {
            continue;
        }
        let remote = format!("refs/remotes/origin/{b}");
        if git(dir, &["rev-parse", "--verify", "-q", &remote], None).is_ok() {
            let _ = git(dir, &["update-ref", &format!("refs/heads/{b}"), &remote], None);
        }
    }
}

/// Las ramas que algún worktree del repo tiene en checkout (el principal incluido).
fn checked_out_branches(dir: &Path) -> Vec<String> {
    git(dir, &["worktree", "list", "--porcelain"], None).map(|out| branches_in_worktree_list(&out)).unwrap_or_default()
}

fn branches_in_worktree_list(porcelain: &str) -> Vec<String> {
    porcelain.lines().filter_map(|l| l.strip_prefix("branch refs/heads/")).map(str::to_string).collect()
}

/// La rama de desarrollo del remoto (`dev` o `develop`) y su commit, si la hay.
pub fn dev_head(dir: &Path) -> Option<(String, String)> {
    ["dev", "develop"].into_iter().find_map(|b| git(dir, &["rev-parse", "--verify", "-q", &format!("refs/remotes/origin/{b}^{{commit}}")], None).ok().map(|sha| (b.to_string(), sha)))
}

/// La rama principal del remoto (`origin/HEAD`), o `main`.
fn default_branch(dir: &Path) -> String {
    git(dir, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"], None).ok().and_then(|r| r.strip_prefix("origin/").map(str::to_string)).unwrap_or_else(|| "main".into())
}

/// Sube `branch` a `origin`. Por https necesita el token; por SSH, el agente del usuario.
pub fn push(dir: &Path, branch: &str) -> Result<()> {
    let url = origin(dir).ok_or_else(|| anyhow!("the repo has no remote “origin” (create one on GitHub to back it up)"))?;
    let https = url.starts_with("https://");
    let token = if https { Some(token_for(dir).ok_or_else(|| anyhow!("there's no GitHub token (condxd secret set github-token)"))?) } else { None };
    git(dir, &["push", "-q", "-u", "origin", &format!("{branch}:refs/heads/{branch}")], token.as_deref())?;
    Ok(())
}

/// Abre un PR de `branch` contra la rama principal, o devuelve el que ya estaba abierto.
pub fn open_pr(dir: &Path, branch: &str, title: &str, body: &str) -> Result<String> {
    let token = token_for(dir).ok_or_else(|| anyhow!("there's no GitHub token (condxd secret set github-token)"))?;
    open_pr_with(dir, branch, title, body, &token, &api())
}

fn open_pr_with(dir: &Path, branch: &str, title: &str, body: &str, token: &str, api_base: &str) -> Result<String> {
    let url = origin(dir).ok_or_else(|| anyhow!("the repo has no remote “origin”"))?;
    let (owner, repo) = github_slug(&url).ok_or_else(|| anyhow!("the remote isn't on GitHub ({})", devtool_core::redact(&url)))?;
    let base = default_branch(dir);
    let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(20)).build();
    let auth = format!("Bearer {token}");
    let reply = agent
        .post(&format!("{api_base}/repos/{owner}/{repo}/pulls"))
        .set("Authorization", &auth)
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", "condx")
        .send_json(json!({ "title": title, "head": branch, "base": base, "body": body }));
    match reply {
        Ok(r) => Ok(r.into_json::<Value>()?["html_url"].as_str().unwrap_or_default().to_string()),
        // Ya hay uno abierto para esa rama: se reusa.
        Err(ureq::Error::Status(422, _)) => {
            let list: Value = agent
                .get(&format!("{api_base}/repos/{owner}/{repo}/pulls"))
                .query("head", &format!("{owner}:{branch}"))
                .query("state", "open")
                .set("Authorization", &auth)
                .set("Accept", "application/vnd.github+json")
                .set("User-Agent", "condx")
                .call()?
                .into_json()?;
            list[0]["html_url"].as_str().map(str::to_string).ok_or_else(|| anyhow!("GitHub didn't open the PR and there's none open for {branch}"))
        }
        Err(ureq::Error::Status(code, r)) => bail!("GitHub answered {code}: {}", devtool_core::redact(&r.into_string().unwrap_or_default()).chars().take(200).collect::<String>()),
        Err(e) => bail!("couldn't reach GitHub: {e}"),
    }
}

pub fn policy(state: &Shared) -> Policy {
    crate::agents::settings(state).git
}

/// Sube `local` sobre la rama `remote` de `origin` (avance directo: un agente que continúa la rama de
/// una tarea parte de su punta recién traída).
pub fn push_onto(dir: &Path, local: &str, remote: &str) -> Result<()> {
    let url = origin(dir).ok_or_else(|| anyhow!("the repo has no remote “origin”"))?;
    let token = if url.starts_with("https://") { Some(token_for(dir).ok_or_else(|| anyhow!("there's no GitHub token (condxd secret set github-token)"))?) } else { None };
    git(dir, &["push", "-q", "origin", &format!("{local}:refs/heads/{remote}")], token.as_deref())?;
    Ok(())
}

/// Respaldo de un arreglo recién guardado: sube su rama si la política lo pide (o, si continúa la rama
/// de una tarea, `onto`, siempre: sin subir, el commit no llega a esa rama). Devuelve lo que pasó para
/// guardarlo en la corrida (`pushed` o `pushError`), sin cortar el arreglo si falla.
pub fn backup(state: &Shared, dir: &Path, branch: &str, onto: Option<&str>) -> Value {
    if let Some(remote) = onto {
        return match push_onto(dir, branch, remote) {
            Ok(()) => json!({ "pushed": true, "branch": remote }),
            Err(e) => json!({ "pushed": false, "pushError": format!("{e:#}") }),
        };
    }
    if !policy(state).backup_push {
        return json!({});
    }
    match push(dir, branch) {
        Ok(()) => json!({ "pushed": true }),
        Err(e) => {
            tracing::warn!("gitflow: couldn't back up {branch}: {e:#}");
            json!({ "pushed": false, "pushError": format!("{e:#}") })
        }
    }
}

/// Antes de desplegar el arreglo de `run`: sube la rama y abre (o reusa) su PR. `None` si va
/// directo o la política no pide PR; un error frena el despliegue (no se despliega sin respaldo).
pub fn before_deploy(state: &Shared, run: &Value, direct: bool) -> Result<Option<String>> {
    if direct || !policy(state).deploy_via_pr {
        return Ok(None);
    }
    let dir = run["worktree"].as_str().map(std::path::PathBuf::from).filter(|d| d.exists()).ok_or_else(|| anyhow!("the fix's worktree is gone"))?;
    let branch = run["branch"].as_str().ok_or_else(|| anyhow!("the fix has no branch"))?;
    push(&dir, branch)?;
    let title = format!("{} (Condx agent)", run["title"].as_str().filter(|t| !t.is_empty()).unwrap_or("Fix"));
    let body = format!(
        "Prepared by a Condx agent (run `{}`).\n\n{}\n\n```\n{}\n```\n\nDeployed through a Condx workflow after this PR, with the app's approval.",
        run["id"].as_str().unwrap_or("?"),
        run["answer"].as_str().unwrap_or("").chars().take(3000).collect::<String>(),
        run["fix"]["stat"].as_str().unwrap_or("")
    );
    open_pr(&dir, branch, &title, &body).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn las_ramas_en_checkout_salen_de_la_lista_de_worktrees() {
        let out = "worktree /a\nHEAD abc\nbranch refs/heads/main\n\nworktree /b\nHEAD def\ndetached\n\nworktree /c\nHEAD 123\nbranch refs/heads/condx/fix-1\n";
        assert_eq!(branches_in_worktree_list(out), ["main", "condx/fix-1"]);
    }

    #[test]
    fn el_slug_de_github_sale_de_https_y_ssh() {
        let s = |u: &str| github_slug(u).map(|(o, r)| format!("{o}/{r}"));
        assert_eq!(s("https://github.com/serlismaldonado/condx.git").as_deref(), Some("serlismaldonado/condx"));
        assert_eq!(s("git@github.com:acme/app.git").as_deref(), Some("acme/app"));
        assert_eq!(s("ssh://git@github.com/acme/app/").as_deref(), Some("acme/app"));
        assert_eq!(s("https://gitlab.com/acme/app"), None);
        assert_eq!(s("https://github.com/acme"), None);
    }

    /// Un GitHub de mentira: contesta los pedidos en orden y devuelve lo que recibió.
    fn fake_github(replies: Vec<(u16, &'static str)>) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (code, body) in replies {
                let (mut sock, _) = listener.accept().unwrap();
                // El pedido entero: cabeceras y, si trae, el cuerpo (Content-Length).
                let mut req = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = sock.read(&mut buf).unwrap();
                    req.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&req).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0);
                        if req.len() >= end + 4 + len {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                tx.send(String::from_utf8_lossy(&req).to_string()).unwrap();
                let _ = write!(sock, "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        (addr, rx)
    }

    fn repo_with_github_remote() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| assert!(Command::new("git").args(args).current_dir(tmp.path()).output().unwrap().status.success());
        run(&["init", "-q", "-b", "main"]);
        run(&["remote", "add", "origin", "https://github.com/acme/app.git"]);
        tmp
    }

    #[test]
    fn abre_el_pr_o_reusa_el_que_esta_abierto() {
        let repo = repo_with_github_remote();
        let (api, rx) = fake_github(vec![(201, r#"{"html_url":"https://github.com/acme/app/pull/7"}"#)]);
        let url = open_pr_with(repo.path(), "condx/fix-1", "Fix", "body", "tok123", &api).unwrap();
        assert_eq!(url, "https://github.com/acme/app/pull/7");
        let req = rx.recv().unwrap();
        assert!(req.starts_with("POST /repos/acme/app/pulls"), "{req}");
        assert!(req.contains("Bearer tok123") && req.contains("\"head\":\"condx/fix-1\"") && req.contains("\"base\":\"main\""), "{req}");

        // Ya existe (422): busca el abierto de esa rama.
        let (api, rx) = fake_github(vec![(422, r#"{"message":"A pull request already exists"}"#), (200, r#"[{"html_url":"https://github.com/acme/app/pull/3"}]"#)]);
        assert_eq!(open_pr_with(repo.path(), "condx/fix-1", "Fix", "b", "t", &api).unwrap(), "https://github.com/acme/app/pull/3");
        let _ = rx.recv().unwrap();
        let get = rx.recv().unwrap();
        assert!(get.starts_with("GET /repos/acme/app/pulls?head=acme%3Acondx%2Ffix-1&state=open"), "{get}");
    }

    #[test]
    fn sube_la_rama_a_un_remoto_sin_tocar_la_config() {
        let tmp = tempfile::tempdir().unwrap();
        let (remote, work) = (tmp.path().join("remote.git"), tmp.path().join("work"));
        let run = |dir: &Path, args: &[&str]| assert!(Command::new("git").args(args).current_dir(dir).output().unwrap().status.success(), "git {args:?}");
        run(tmp.path(), &["init", "-q", "--bare", remote.to_str().unwrap()]);
        run(tmp.path(), &["init", "-q", "-b", "main", work.to_str().unwrap()]);
        std::fs::write(work.join("a.txt"), "a").unwrap();
        run(&work, &["add", "."]);
        run(&work, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "a"]);
        run(&work, &["checkout", "-q", "-b", "condx/fix-1"]);
        run(&work, &["remote", "add", "origin", remote.to_str().unwrap()]);
        // Un remoto local (no https): sin token.
        push(&work, "condx/fix-1").unwrap();
        let heads = git(&remote, &["branch", "--list"], None).unwrap();
        assert!(heads.contains("condx/fix-1"), "{heads}");
        assert!(!std::fs::read_to_string(work.join(".git/config")).unwrap().contains("credential"));
    }
}
