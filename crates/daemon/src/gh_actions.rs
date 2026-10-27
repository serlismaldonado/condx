//! Releases en GitHub Actions sin la Mac: el daemon (la Mac sin las herramientas, o el sitio en la nube)
//! lanza el workflow del repo por la API, espera a que termine y baja lo que publicó. El token es el
//! del dueño del repo (`gitflow::token_for`), el mismo que sube las ramas de los agentes.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

/// Un workflow lanzado: dónde verlo.
pub struct Run {
    pub id: u64,
    pub url: String,
}

struct Repo {
    slug: String,
    token: String,
}

fn repo(dir: &Path) -> Result<Repo> {
    let url = crate::gitflow::origin(dir).with_context(|| format!("{} has no origin remote", dir.display()))?;
    let (owner, name) = crate::gitflow::github_slug(&url).with_context(|| format!("{url} isn't a GitHub repo"))?;
    let token = crate::gitflow::token_for(dir).ok_or_else(|| anyhow!("there's no GitHub token for {owner} (condxd secret set github-token:{owner})"))?;
    Ok(Repo { slug: format!("{owner}/{name}"), token })
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(Duration::from_secs(60)).build()
}

fn call(r: &Repo, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
    let url = format!("{}/repos/{}{path}", crate::gitflow::api(), r.slug);
    let req = agent().request(method, &url).set("Authorization", &format!("Bearer {}", r.token)).set("Accept", "application/vnd.github+json").set("User-Agent", "condx");
    let res = match body {
        Some(b) => req.send_json(b),
        None => req.call(),
    };
    match res {
        Ok(resp) => {
            let text = resp.into_string().unwrap_or_default();
            Ok(if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::Null) })
        }
        Err(ureq::Error::Status(code, resp)) => {
            let msg = resp.into_json::<Value>().ok().and_then(|v| v["message"].as_str().map(str::to_string)).unwrap_or_default();
            let hint = match code {
                401 => " (the token isn't valid)",
                403 | 404 => " (the token needs Actions: write and Contents: write on the repo)",
                _ => "",
            };
            bail!("GitHub answered {code} on {path}: {msg}{hint}")
        }
        Err(e) => bail!("couldn't reach GitHub: {}", devtool_core::redact(&e.to_string())),
    }
}

/// Lanza `workflow` (el archivo en .github/workflows) en la rama principal con esos inputs y devuelve
/// la corrida que arrancó (GitHub no la devuelve al lanzar: se busca la nueva).
pub fn dispatch(dir: &Path, workflow: &str, inputs: Value) -> Result<Run> {
    let r = repo(dir)?;
    let branch = call(&r, "GET", "", None)?["default_branch"].as_str().unwrap_or("main").to_string();
    let before: Vec<u64> = runs(&r, workflow)?.iter().filter_map(|x| x["id"].as_u64()).collect();
    call(&r, "POST", &format!("/actions/workflows/{workflow}/dispatches"), Some(json!({ "ref": branch, "inputs": inputs })))?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(90) {
        std::thread::sleep(Duration::from_secs(4));
        if let Some(run) = runs(&r, workflow)?.into_iter().find(|x| x["id"].as_u64().is_some_and(|id| !before.contains(&id))) {
            return Ok(Run { id: run["id"].as_u64().unwrap_or_default(), url: run["html_url"].as_str().unwrap_or_default().to_string() });
        }
    }
    bail!("GitHub took the request but no run of {workflow} showed up in 90 s ({})", r.slug)
}

fn runs(r: &Repo, workflow: &str) -> Result<Vec<Value>> {
    Ok(call(r, "GET", &format!("/actions/workflows/{workflow}/runs?event=workflow_dispatch&per_page=10"), None)?["workflow_runs"].as_array().cloned().unwrap_or_default())
}

/// Espera a que la corrida termine (o pase `limit`): Ok si salió bien, el error con su enlace si no.
pub fn wait(dir: &Path, run: &Run, limit: Duration) -> Result<()> {
    let r = repo(dir)?;
    let start = Instant::now();
    loop {
        let v = call(&r, "GET", &format!("/actions/runs/{}", run.id), None)?;
        if v["status"] == "completed" {
            return match v["conclusion"].as_str() {
                Some("success") => Ok(()),
                other => bail!("the GitHub build ended {} — {}", other.unwrap_or("without a result"), run.url),
            };
        }
        if start.elapsed() > limit {
            bail!("the GitHub build is still running after {} min — {}", limit.as_secs() / 60, run.url);
        }
        std::thread::sleep(Duration::from_secs(20));
    }
}

/// El último release del repo y el archivo que termina en `suffix`, bajado a `to`. Devuelve el tag.
pub fn download_latest_asset(dir: &Path, suffix: &str, to: &Path) -> Result<String> {
    let r = repo(dir)?;
    let rel = call(&r, "GET", "/releases/latest", None)?;
    let tag = rel["tag_name"].as_str().context("the release has no tag")?.to_string();
    let asset = rel["assets"].as_array().and_then(|a| a.iter().find(|x| x["name"].as_str().is_some_and(|n| n.ends_with(suffix)))).with_context(|| format!("the release {tag} has no *{suffix}"))?;
    let id = asset["id"].as_u64().context("the asset has no id")?;
    let url = format!("{}/repos/{}/releases/assets/{id}", crate::gitflow::api(), r.slug);
    let resp = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(600))
        .build()
        .get(&url)
        .set("Authorization", &format!("Bearer {}", r.token))
        .set("Accept", "application/octet-stream")
        .set("User-Agent", "condx")
        .call()
        .map_err(|e| anyhow!("couldn't download {}: {}", asset["name"], devtool_core::redact(&e.to_string())))?;
    let mut bytes = vec![];
    std::io::Read::read_to_end(&mut resp.into_reader().take(200 * 1024 * 1024), &mut bytes)?;
    std::fs::write(to, &bytes)?;
    Ok(tag)
}

use std::io::Read as _;
