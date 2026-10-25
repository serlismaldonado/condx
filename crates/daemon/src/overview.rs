// Vista general (§4.2): lo que se mira primero, en una sola respuesta. Se arma
// con los últimos resultados de las sondas; no corre comandos salvo
// `git rev-list` para contar commits pendientes de promover.

use devtool_core::EnvKind;
use serde_json::{json, Value};

use crate::probes::commits_between;
use crate::state::Shared;
use crate::store::now_ms;

fn short(sha: &Value) -> Value {
    sha.as_str().map(|s| json!(&s[..s.len().min(8)])).unwrap_or(Value::Null)
}

/// Vista general de un proyecto; `None` o heskala-erp = la del ERP de siempre.
pub async fn build_for(state: &Shared, project: Option<&str>) -> Value {
    match project.filter(|p| !p.is_empty() && *p != crate::projects::DEFAULT) {
        None => build(state).await,
        Some(name) => match crate::projects::get(state, Some(name)) {
            Ok(p) => build_project(state, &p).await,
            Err(e) => json!({ "error": e.to_string() }),
        },
    }
}

/// Git de un repo sin sonda propia: rama, HEAD, cambios y adelanto/atraso.
async fn git_light(repo: &std::path::Path) -> Value {
    let t = std::time::Duration::from_secs(10);
    let head = crate::cmd::run("git", &["log", "-1", "--format=%H%x09%s"], Some(repo), &[], t).await.stdout;
    let (sha, subject) = head.trim().split_once('\t').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or_default();
    let status = crate::cmd::run("git", &["status", "--porcelain=v1", "-b"], Some(repo), &[], t).await.stdout;
    let mut lines = status.lines();
    let first = lines.next().unwrap_or_default().trim_start_matches("## ").to_string();
    let branch = first.split("...").next().unwrap_or_default().to_string();
    let num = |key: &str| first.split(key).nth(1).and_then(|r| r.trim_start().split(|c: char| !c.is_ascii_digit()).next()).and_then(|n| n.parse::<i64>().ok()).unwrap_or(0);
    let (mut changed, mut untracked) = (0, 0);
    for l in lines {
        if l.starts_with("??") {
            untracked += 1;
        } else {
            changed += 1;
        }
    }
    json!({ "branch": branch, "head": sha, "subject": subject, "ahead": num("ahead"), "behind": num("behind"), "changed": changed, "untracked": untracked })
}

/// Vista general de otro proyecto: sus entornos, su git y su deployer local.
async fn build_project(state: &Shared, project: &crate::projects::Project) -> Value {
    let git = git_light(&project.repo).await;
    let deployer_proc = project.proc("deployer");
    let deployer_running = state.procs.get(&deployer_proc).is_some();
    let mut attention: Vec<String> = vec![];
    let mut environments = vec![];
    for env in state.store.environments().unwrap_or_default().into_iter().filter(|e| e.project.as_deref() == Some(project.name.as_str())) {
        let probe = state.probe(&format!("convex:{}", env.name)).unwrap_or(Value::Null);
        let (commit, source, dirty) = match env.kind {
            EnvKind::Devbox if deployer_running => (git["head"].clone(), "checkout", git["changed"].as_i64().unwrap_or(0) > 0),
            EnvKind::Devbox => (Value::Null, "unknown", false),
            _ => match state.store.last_deployment(&env.name).ok().flatten() {
                Some(d) => (json!(d.commit), "history", false),
                None => (Value::Null, "unknown", false),
            },
        };
        let target = env.branch.clone().unwrap_or_else(|| "main".into());
        let behind = match commit.as_str() {
            Some(c) => commits_between(&project.repo, c, &target).await,
            None => None,
        };
        let reachable = probe["ok"] == true;
        // El backend local apagado no es una alerta: se levanta cuando se usa.
        if !reachable && probe != Value::Null && env.kind != EnvKind::Devbox {
            attention.push(format!("Convex for “{}” isn't responding: {}", env.name, probe["error"].as_str().unwrap_or("no details")));
        }
        environments.push(json!({
            "name": env.name,
            "kind": env.kind,
            "label": env.label,
            "project": env.project,
            "convex": {
                "url": env.convex_url,
                "deployment": env.convex_deployment,
                "reachable": reachable,
                "version": probe["data"]["version"],
                "latencyMs": probe["data"]["latencyMs"],
                "checkedAt": probe["at"],
                "error": probe["error"],
            },
            "deployed": { "commit": short(&commit), "source": source, "uncommittedChanges": dirty, "branch": target, "behindBranch": behind },
        }));
    }
    let phase = crate::services::deployer_phase(&deployer_proc);
    if phase == "error" {
        attention.push(format!("The local deployer of {} is in error (see get_service_logs)", project.name));
    }
    let jobs = crate::projects::jobs_of(state, &[], 10, Some(&project.name)).unwrap_or_default();
    for job in jobs.iter().filter(|j| j["status"] == "failed").take(3) {
        if now_ms() - job["endedAt"].as_i64().unwrap_or(0) < 3_600_000 {
            attention.push(format!("Job failed {} on {}: {}", job["recipe"].as_str().unwrap_or("?"), job["env"].as_str().unwrap_or("?"), job["error"].as_str().unwrap_or("")));
        }
    }
    let approvals: Vec<Value> =
        crate::projects::jobs_of(state, &["waiting-approval"], 50, Some(&project.name)).unwrap_or_default().iter().map(crate::jobs::summary).collect();
    if !approvals.is_empty() {
        attention.push(format!("{} job(s) waiting for your approval in the app", approvals.len()));
    }
    json!({
        "generatedAt": now_ms(),
        "project": crate::projects::to_json(project),
        "daemon": { "version": env!("CARGO_PKG_VERSION"), "startedAt": state.started_at, "repo": project.repo.display().to_string() },
        "attention": attention,
        "environments": environments,
        "frontendProduction": Value::Null,
        "git": {
            "branch": git["branch"], "head": short(&git["head"]), "subject": git["subject"], "ahead": git["ahead"],
            "behind": git["behind"], "changed": git["changed"], "untracked": git["untracked"], "submodules": [],
        },
        "ci": ci_summary(&state.probe(&crate::probes::ci::probe_key(&project.name)).unwrap_or(Value::Null), &mut attention),
        "deployer": { "phase": phase, "running": deployer_running, "image": "convex dev (native)" },
        "tabs": { "count": 0, "list": [] },
        "jobs": jobs.iter().map(crate::jobs::summary).collect::<Vec<_>>(),
        "approvals": approvals,
    })
}

pub async fn build(state: &Shared) -> Value {
    let repo = state.config.workdir();
    let git = state.probe_data("git").unwrap_or(Value::Null);
    let deployer = state.probe_data("deployer").unwrap_or(Value::Null);
    let vercel = state.probe("vercel").unwrap_or(Value::Null);
    let ci = state.probe("ci").unwrap_or(Value::Null);
    let mut attention: Vec<String> = vec![];

    let mut environments = vec![];
    for env in state.store.environments().unwrap_or_default().into_iter().filter(|e| e.project.is_none()) {
        let probe = state.probe(&format!("convex:{}", env.name)).unwrap_or(Value::Null);
        let (commit, source, dirty) = match env.kind {
            EnvKind::Devbox => {
                // El deployer del devbox observa el checkout: corre lo que hay en disco.
                let watching = deployer["running"] == true;
                if watching {
                    (git["head"].clone(), "checkout", git["changed"].as_i64().unwrap_or(0) > 0)
                } else {
                    (Value::Null, "unknown", false)
                }
            }
            _ => match state.store.last_deployment(&env.name).ok().flatten() {
                Some(d) => (json!(d.commit), "history", false),
                None => (Value::Null, "unknown", false),
            },
        };
        let target = env.branch.clone().unwrap_or_else(|| "dev".into());
        let behind = match commit.as_str() {
            Some(c) => commits_between(&repo, c, &target).await,
            None => None,
        };
        let reachable = probe["ok"] == true;
        if !reachable && probe != Value::Null {
            attention.push(format!(
                "Convex for “{}” isn't responding: {}",
                env.name,
                probe["error"].as_str().unwrap_or("no details")
            ));
        }
        environments.push(json!({
            "name": env.name,
            "kind": env.kind,
            "label": env.label,
            "convex": {
                "url": env.convex_url,
                "deployment": env.convex_deployment,
                "reachable": reachable,
                "version": probe["data"]["version"],
                "latencyMs": probe["data"]["latencyMs"],
                "checkedAt": probe["at"],
                "error": probe["error"],
            },
            "deployed": {
                "commit": short(&commit),
                "source": source,
                "uncommittedChanges": dirty,
                "branch": target,
                "behindBranch": behind,
            },
        }));
    }

    // Frontend de producción (Vercel) frente a main.
    let production = &vercel["data"]["production"];
    let frontend = if production.is_object() {
        let behind = match production["commit"].as_str() {
            Some(c) => commits_between(&repo, c, "main").await,
            None => None,
        };
        json!({
            "url": production["url"],
            "commit": short(&production["commit"]),
            "branch": production["branch"],
            "deployedAt": production["createdAt"],
            "behindMain": behind,
        })
    } else {
        json!({ "error": vercel["error"] })
    };
    if let Some(failed) = vercel["data"]["lastFailed"].as_object() {
        let newer_ok = vercel["data"]["production"]["createdAt"].as_i64().unwrap_or(0)
            > failed.get("createdAt").and_then(Value::as_i64).unwrap_or(0);
        if !newer_ok {
            attention.push(format!(
                "Vercel: the latest deploy of {} failed",
                failed.get("branch").and_then(Value::as_str).unwrap_or("?")
            ));
        }
    }

    let ci_summary = ci_summary(&ci, &mut attention);

    if deployer["phase"] == "error" {
        attention.push("The devbox deployer is in error (see get_deployer)".into());
    }
    let submodules: Vec<Value> = git["submodules"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            if s["remote"].is_string() && s["matchesRemote"] == false {
                attention.push(format!(
                    "Submodule {} points at {} and its remote ({}) is at {}",
                    s["path"].as_str().unwrap_or("?"),
                    short(&s["recorded"]).as_str().unwrap_or("?"),
                    s["branch"].as_str().unwrap_or("main"),
                    short(&s["remote"]).as_str().unwrap_or("?"),
                ));
            }
            json!({ "path": s["path"], "recorded": short(&s["recorded"]), "remote": short(&s["remote"]), "matchesRemote": s["matchesRemote"], "dirty": s["dirty"] })
        })
        .collect();

    // Las aprobaciones son de todos los proyectos: nada espera sin que se vea.
    let approvals: Vec<Value> =
        state.store.jobs(&["waiting-approval"], 50).unwrap_or_default().iter().map(crate::jobs::summary).collect();
    if !approvals.is_empty() {
        attention.push(format!("{} job(s) waiting for your approval in the app", approvals.len()));
    }
    for job in state.store.jobs(&["failed"], 3).unwrap_or_default() {
        if crate::store::now_ms() - job["endedAt"].as_i64().unwrap_or(0) < 3_600_000 {
            attention.push(format!("Job failed {} on {}: {}", job["recipe"].as_str().unwrap_or("?"), job["env"].as_str().unwrap_or("?"), job["error"].as_str().unwrap_or("")));
        }
    }
    let tabs = state.hub.tabs();
    json!({
        "generatedAt": now_ms(),
        "daemon": { "version": env!("CARGO_PKG_VERSION"), "startedAt": state.started_at, "repo": repo.display().to_string() },
        "attention": attention,
        "environments": environments,
        "frontendProduction": frontend,
        "git": {
            "branch": git["branch"],
            "head": short(&git["head"]),
            "subject": git["subject"],
            "ahead": git["ahead"],
            "behind": git["behind"],
            "changed": git["changed"],
            "untracked": git["untracked"],
            "dev": short(&git["refs"]["dev"]),
            "main": short(&git["refs"]["main"]),
            "submodules": submodules,
        },
        "ci": ci_summary,
        "deployer": {
            "phase": deployer["phase"],
            "running": deployer["running"],
            "image": deployer["image"],
            "imageCreatedAt": deployer["imageCreatedAt"],
        },
        "tabs": { "count": tabs.len(), "list": tabs },
        "jobs": crate::projects::jobs_of(state, &[], 10, Some(crate::projects::DEFAULT)).unwrap_or_default().iter().map(crate::jobs::summary).collect::<Vec<_>>(),
        "approvals": approvals,
    })
}

/// Lo último de cada repo de la sonda de CI, y lo que pide atención (un CI rojo, un error de gh).
fn ci_summary(ci: &Value, attention: &mut Vec<String>) -> Vec<Value> {
    let summary: Vec<Value> = ci["data"]["repos"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let latest = &r["latest"];
            if latest["conclusion"] == "failure" {
                attention.push(format!("CI of {} failed: {}", r["repo"].as_str().unwrap_or("?"), latest["displayTitle"].as_str().unwrap_or("")));
            }
            json!({
                "repo": r["repo"],
                "status": latest["status"],
                "conclusion": latest["conclusion"],
                "title": latest["displayTitle"],
                "branch": latest["headBranch"],
                "commit": short(&latest["headSha"]),
                "url": latest["url"],
                "at": latest["updatedAt"],
            })
        })
        .collect();
    if let Some(err) = ci["error"].as_str() {
        attention.push(format!("CI: {err}"));
    }
    summary
}
