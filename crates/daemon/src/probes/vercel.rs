// Despliegues del frontend en Vercel (`vercel ls --format json` en el repo
// vinculado): qué commit está en producción y cuál es el último de preview.

use std::time::Duration;

use serde_json::{json, Value};

use crate::cmd::run;
use crate::state::Shared;

const T: Duration = Duration::from_secs(40);

fn summarize(d: &Value) -> Value {
    json!({
        "url": d["url"].as_str().map(|u| format!("https://{u}")),
        "state": d["state"],
        "target": d["target"],
        "createdAt": d["createdAt"],
        "commit": d["meta"]["githubCommitSha"],
        "branch": d["meta"]["githubCommitRef"],
    })
}

pub async fn deployments(state: Shared) {
    let Some(repo) = state.config.repo.clone() else { return };
    if !repo.join(".vercel/project.json").exists() {
        state.set_probe("vercel", false, Value::Null, Some("the repo isn't linked to Vercel (missing .vercel/project.json)".into()));
        return;
    }
    let out = run("vercel", &["ls", "--format", "json"], Some(&repo), &[], T).await;
    if !out.ok {
        state.set_probe("vercel", false, Value::Null, Some(out.error()));
        return;
    }
    let Ok(body) = serde_json::from_str::<Value>(&out.stdout) else {
        state.set_probe("vercel", false, Value::Null, Some("unreadable vercel response".into()));
        return;
    };
    let list = body["deployments"].as_array().cloned().unwrap_or_default();
    let production = list.iter().find(|d| d["target"] == "production" && d["state"] == "READY").map(summarize);
    let latest_preview = list.iter().find(|d| d["target"] != "production").map(summarize);
    let building: Vec<Value> =
        list.iter().filter(|d| matches!(d["state"].as_str(), Some("BUILDING" | "QUEUED" | "INITIALIZING"))).map(summarize).collect();
    let failed = list.iter().find(|d| d["state"] == "ERROR").map(summarize);
    state.set_probe(
        "vercel",
        true,
        json!({
            "project": body["deployments"][0]["name"],
            "production": production,
            "latestPreview": latest_preview,
            "building": building,
            "lastFailed": failed,
        }),
        None,
    );
}
