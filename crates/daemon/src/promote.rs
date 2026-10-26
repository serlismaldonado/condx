// Pasar a producción fusionando en la rama principal (main) por GitHub: en los proyectos que
// despliegan producción al fusionar (el build de Vercel corre `convex deploy` y después el
// frontend), Condx no necesita la key de producción. Desde el teléfono, con la Mac o con el sitio en
// la nube: vista previa (commits que entran, CI de la rama), y con la aprobación firmada, el PR
// `dev → main` (o el que ya estaba abierto) se fusiona si el CI no falla ni sigue corriendo.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::store::now_ms;

fn api() -> String {
    std::env::var("CONDX_GITHUB_API").unwrap_or_else(|_| "https://api.github.com".into())
}

/// Los tokens de GitHub de este daemon: el general primero, después los de cada dueño.
fn tokens() -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for account in std::iter::once("github-token".to_string()).chain(crate::secrets::accounts_with_prefix("github-token:")) {
        if let Ok(Some(t)) = crate::secrets::get(&account) {
            let t = t.trim().to_string();
            if !t.is_empty() && !out.contains(&t) {
                out.push(t);
            }
        }
    }
    out
}

fn slug(dir: &Path) -> Option<(String, String)> {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(["remote", "get-url", "origin"]).output().ok()?;
    crate::gitflow::github_slug(String::from_utf8_lossy(&out.stdout).trim())
}

/// Una llamada a la API de GitHub probando cada token hasta que uno vea el repo.
fn call(method: &str, path: &str, body: Option<Value>) -> Result<Value> {
    // Un repo renombrado redirige: el token se conserva dentro de api.github.com.
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).redirect_auth_headers(ureq::RedirectAuthHeaders::SameHost).build();
    let mut last = anyhow!("there's no GitHub token (condxd secret set github-token)");
    for token in tokens() {
        let req = agent.request(method, &format!("{}{path}", api())).set("Authorization", &format!("Bearer {token}")).set("Accept", "application/vnd.github+json").set("User-Agent", "condx");
        let reply = match &body {
            Some(b) => req.send_json(b.clone()),
            None => req.call(),
        };
        match reply {
            Ok(r) => return Ok(r.into_json().unwrap_or(Value::Null)),
            // Sin acceso con este token (otro dueño): el siguiente.
            Err(ureq::Error::Status(401 | 403 | 404, _)) => last = anyhow!("no GitHub token can see this repo (it needs Contents and Pull requests: write)"),
            Err(ureq::Error::Status(code, r)) => {
                let text = r.into_string().unwrap_or_default();
                let msg = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["message"].as_str().map(str::to_string)).unwrap_or(text);
                bail!("GitHub answered {code}: {}", devtool_core::redact(&msg).chars().take(200).collect::<String>());
            }
            Err(e) => bail!("couldn't reach GitHub: {e}"),
        }
    }
    Err(last)
}

/// El estado del CI de un commit: `success`, `failure`, `pending` o `none` (sin chequeos).
fn ci_of(owner: &str, repo: &str, sha: &str) -> String {
    let Ok(v) = call("GET", &format!("/repos/{owner}/{repo}/commits/{sha}/check-runs?per_page=50"), None) else { return "none".into() };
    let runs = v["check_runs"].as_array().cloned().unwrap_or_default();
    if runs.is_empty() {
        return "none".into();
    }
    if runs.iter().any(|r| matches!(r["conclusion"].as_str(), Some("failure" | "timed_out" | "cancelled" | "action_required"))) {
        "failure".into()
    } else if runs.iter().any(|r| r["status"] != "completed") {
        "pending".into()
    } else {
        "success".into()
    }
}

/// La rama que se pasa a producción: `dev` (o `develop`) si existe en GitHub.
fn head_branch(owner: &str, repo: &str) -> Option<String> {
    ["dev", "develop"].into_iter().find(|b| call("GET", &format!("/repos/{owner}/{repo}/branches/{b}"), None).is_ok()).map(str::to_string)
}

/// Qué entraría a producción: `{base, head, ahead, commits, ci, pr}`; `Null` si el repo no está en
/// GitHub o no tiene rama de desarrollo.
pub fn preview(dir: &Path) -> Value {
    let Some((owner, repo)) = slug(dir) else { return Value::Null };
    let info = match call("GET", &format!("/repos/{owner}/{repo}"), None) {
        Ok(v) => v,
        Err(e) => {
            tracing::info!("promote: {owner}/{repo}: {e:#}");
            return Value::Null;
        }
    };
    // Un repo renombrado: la API redirige; se sigue con el nombre actual.
    let (owner, repo) = info["full_name"].as_str().and_then(|f| f.split_once('/')).map(|(o, r)| (o.to_string(), r.to_string())).unwrap_or((owner, repo));
    let base = info["default_branch"].as_str().unwrap_or("main").to_string();
    let Some(head) = head_branch(&owner, &repo) else {
        tracing::info!("promote: {owner}/{repo} has no dev branch");
        return Value::Null;
    };
    let cmp = match call("GET", &format!("/repos/{owner}/{repo}/compare/{base}...{head}"), None) {
        Ok(v) => v,
        Err(e) => {
            tracing::info!("promote: {owner}/{repo} compare: {e:#}");
            return Value::Null;
        }
    };
    let commits: Vec<Value> = cmp["commits"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .take(5)
        .map(|c| json!({ "sha": c["sha"].as_str().map(|s| &s[..s.len().min(7)]), "subject": c["commit"]["message"].as_str().unwrap_or("").lines().next().unwrap_or("") }))
        .collect();
    let sha = cmp["commits"].as_array().and_then(|c| c.last()).and_then(|c| c["sha"].as_str()).unwrap_or(&head).to_string();
    let pr = call("GET", &format!("/repos/{owner}/{repo}/pulls?state=open&head={owner}:{head}&base={base}"), None).ok().and_then(|v| v.as_array().and_then(|a| a.first().cloned()));
    json!({
        "repo": format!("{owner}/{repo}"),
        "base": base,
        "head": head,
        "ahead": cmp["ahead_by"],
        "commits": commits,
        "ci": if cmp["ahead_by"].as_i64().unwrap_or(0) > 0 { ci_of(&owner, &repo, &sha) } else { "none".into() },
        "pr": pr.map(|p| json!({ "number": p["number"], "url": p["html_url"] })),
        "at": now_ms(),
    })
}

/// La vista previa de cada repo, guardada 5 min (la vista general se arma seguido).
static CACHE: Mutex<Option<HashMap<String, Value>>> = Mutex::new(None);

pub fn preview_cached(dir: &Path) -> Value {
    let key = dir.display().to_string();
    if let Some(v) = CACHE.lock().unwrap().as_ref().and_then(|m| m.get(&key).cloned()) {
        if now_ms() - v["at"].as_i64().unwrap_or(0) < 5 * 60_000 {
            return if v["none"] == true { Value::Null } else { v };
        }
    }
    let v = preview(dir);
    let stamped = if v.is_null() { json!({ "at": now_ms(), "none": true }) } else { v.clone() };
    CACHE.lock().unwrap().get_or_insert_with(HashMap::new).insert(key, stamped);
    v
}

/// Fusiona la rama de desarrollo en la principal (abre el PR si no hay uno). No fusiona si el CI
/// falló o sigue corriendo. Devuelve qué hizo.
pub fn merge(dir: &Path, by: &str) -> Result<String> {
    let p = preview(dir);
    let (owner, repo) = p["repo"].as_str().and_then(|r| r.split_once('/')).map(|(o, r)| (o.to_string(), r.to_string())).or_else(|| slug(dir)).ok_or_else(|| anyhow!("the repo isn't on GitHub"))?;
    if p.is_null() {
        bail!("{owner}/{repo} has no dev branch on GitHub to pass to production");
    }
    let (base, head) = (p["base"].as_str().unwrap_or("main").to_string(), p["head"].as_str().unwrap_or("dev").to_string());
    if p["ahead"].as_i64().unwrap_or(0) == 0 {
        bail!("{head} has nothing new for {base}");
    }
    match p["ci"].as_str() {
        Some("failure") => bail!("the CI of {head} failed: fix it before passing to production"),
        Some("pending") => bail!("the CI of {head} is still running: try again when it finishes"),
        _ => {}
    }
    let number = match p["pr"]["number"].as_i64() {
        Some(n) => n,
        None => {
            let body = format!("Passing {head} to production from Condx (asked by {by}).");
            let pr = call("POST", &format!("/repos/{owner}/{repo}/pulls"), Some(json!({ "title": format!("Release: {head} → {base}"), "head": head, "base": base, "body": body })))?;
            pr["number"].as_i64().ok_or_else(|| anyhow!("GitHub didn't return the PR"))?
        }
    };
    let done = call("PUT", &format!("/repos/{owner}/{repo}/pulls/{number}/merge"), Some(json!({ "merge_method": "merge", "commit_title": format!("Release: {head} → {base} (#{number})") })))?;
    if done["merged"] != true {
        bail!("GitHub didn't merge PR #{number}: {}", done["message"].as_str().unwrap_or("no details"));
    }
    if let Some(m) = CACHE.lock().unwrap().as_mut() {
        m.remove(&dir.display().to_string());
    }
    Ok(format!("merged PR #{number} ({head} → {base}): production deploys from {base} (Vercel)"))
}
