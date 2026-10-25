// CI de GitHub del ERP y de los submódulos (`gh run list`): el 2026-10-24 el
// CI de la librería falló y se vio tarde; aquí se ve apenas termina.

use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use serde_json::{json, Value};

use crate::cmd::run;
use crate::state::Shared;

const T: Duration = Duration::from_secs(20);

/// `owner/name` de una URL de GitHub (ssh o https).
pub fn github_slug(url: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"github\.com[:/]([^/]+)/([^/]+?)(?:\.git)?/?$").unwrap());
    let c = re.captures(url.trim())?;
    Some(format!("{}/{}", &c[1], &c[2]))
}

async fn slugs(state: &Shared) -> Vec<String> {
    let repo = state.config.workdir();
    let mut list = vec![];
    // El ERP: el remoto `origin` (puede haber otros, como un fork sin push).
    let origin = run("git", &["remote", "get-url", "origin"], Some(&repo), &[], T).await;
    if let Some(s) = github_slug(&origin.stdout) {
        list.push(s);
    }
    if let Some(git) = state.probe_data("git") {
        for sub in git["submodules"].as_array().into_iter().flatten() {
            if let Some(s) = sub["url"].as_str().and_then(github_slug) {
                if !list.contains(&s) {
                    list.push(s);
                }
            }
        }
    }
    list
}

/// Un workflow sin nombre aparece con su ruta (`.github/workflows/x.yml`):
/// GitHub no lo pudo leer (p. ej. el ERP tiene los suyos comentados) y falla
/// en cada push sin correr nada. No es un CI rojo.
pub fn is_invalid_workflow(run: &Value) -> bool {
    run["workflowName"].as_str().map(|n| n.starts_with(".github/")).unwrap_or(false)
}

pub async fn runs(state: Shared) -> Duration {
    // Los submódulos salen de la sonda de git: al arrancar, esperarla.
    if state.probe_data("git").is_none() {
        return Duration::from_secs(2);
    }
    let mut repos = vec![];
    let mut in_progress = false;
    let mut errors = vec![];
    for slug in slugs(&state).await {
        let out = run(
            "gh",
            &[
                "run", "list", "-R", &slug, "--limit", "20", "--json",
                "databaseId,status,conclusion,headSha,headBranch,displayTitle,workflowName,event,createdAt,updatedAt,url",
            ],
            None,
            &[],
            T,
        )
        .await;
        if !out.ok {
            errors.push(format!("{slug}: {}", out.error()));
            continue;
        }
        let all: Vec<Value> = serde_json::from_str(&out.stdout).unwrap_or_default();
        let (list, invalid): (Vec<Value>, Vec<Value>) = all.into_iter().partition(|r| !is_invalid_workflow(r));
        in_progress |= list.iter().any(|r| r["status"] != "completed");
        let latest = list.first().cloned();
        let invalid: Vec<&Value> = invalid.iter().map(|r| &r["workflowName"]).collect();
        repos.push(json!({ "repo": slug, "latest": latest, "runs": list, "invalidWorkflows": invalid }));
    }
    let ok = !repos.is_empty();
    state.set_probe("ci", ok, json!({ "repos": repos }), (!errors.is_empty()).then(|| errors.join("\n")));
    if in_progress { Duration::from_secs(10) } else { Duration::from_secs(60) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_de_github() {
        assert_eq!(github_slug("git@github.com:Heskala/heskala-erp.git").as_deref(), Some("Heskala/heskala-erp"));
        assert_eq!(github_slug("https://github.com/Heskala/heskala-localfirst\n").as_deref(), Some("Heskala/heskala-localfirst"));
        assert_eq!(github_slug("../heskala-localfirst"), None);
    }

    #[test]
    fn workflow_ilegible_no_cuenta() {
        assert!(is_invalid_workflow(&json!({ "workflowName": ".github/workflows/onprem-verify.yml" })));
        assert!(!is_invalid_workflow(&json!({ "workflowName": "ci" })));
    }
}
