// Datos de ejemplo de la demo de la web: un proyecto inventado (`my-app`) con un backend local,
// un desechable, cloud dev y producción. Responde a las mismas rutas que el daemon; lo que se pide
// o se aprueba en la demo cambia este estado en memoria (se pierde al recargar). Nada de esto sale
// del navegador ni toca un backend real.

use std::cell::RefCell;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

const PROJECT: &str = "my-app";
const DEV: &str = "my-app-dev";
const PROD: &str = "my-app-prod";
const RECIPES: &str = include_str!("recipes.json");

fn now() -> i64 {
    crate::ui::now_ms()
}

const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;

/// Lo que cambia mientras se usa la demo.
struct State {
    jobs: Vec<Value>,
    policy: Value,
    workflows: Vec<Value>,
    runs: Vec<Value>,
    next: u64,
    /// Con `?welcome` en la URL la demo arranca sin proyectos: la guía del primer arranque.
    registered: bool,
    backend: bool,
}

fn welcome_mode() -> bool {
    web_sys::window().and_then(|w| w.location().search().ok()).is_some_and(|s| s.contains("welcome"))
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::new());
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

// ---------- Rutas ----------

pub fn get(path: &str) -> Result<Value> {
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    let q = |key: &str| query.split('&').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v.to_string()));
    let parts: Vec<&str> = route.trim_start_matches("/api/").split('/').collect();
    Ok(match parts.as_slice() {
        ["overview"] => overview(),
        ["projects"] => if with(|s| s.registered) { json!([project()]) } else { json!([]) },
        ["projects", "discover"] => json!({ "root": "~/code", "candidates": [{ "path": "~/code/my-app", "convexProject": "my-app", "frontend": "vite", "project": null }] }),
        ["doctor"] => doctor(),
        ["probes"] => json!([ci_probe()]),
        ["jobs"] => with(|s| json!(s.jobs)),
        ["jobs", id] => with(|s| s.jobs.iter().find(|j| j["id"] == *id).cloned()).ok_or_else(|| anyhow!("job {id} doesn't exist"))?,
        ["jobs", id, "log"] => json!({ "offset": 0, "text": job_log(id) }),
        ["approvals"] => with(|s| json!(s.jobs.iter().filter(|j| j["status"] == "waiting-approval").cloned().collect::<Vec<_>>())),
        ["recipes"] => serde_json::from_str(RECIPES)?,
        ["backups"] => backups(),
        ["services"] => services(),
        ["services", name, "logs"] => json!({ "lines": service_log(name) }),
        ["policy"] => with(|s| s.policy.clone()),
        ["workflows"] => workflows(),
        ["workflows", "templates"] => with(|s| json!(s.workflows.clone())),
        ["workflows", id, "export"] => with(|s| s.workflows.iter().find(|w| w["id"] == *id).cloned()).unwrap_or(Value::Null),
        ["workflow-runs"] => {
            let wf = q("workflow").unwrap_or_default();
            with(|s| json!(s.runs.iter().filter(|r| r["workflowId"] == wf.as_str()).cloned().collect::<Vec<_>>()))
        }
        ["mcp"] => json!({ "lastAt": now() - 3 * MIN, "client": "claude-code", "command": "~/.condx/daemon/bin/condxd", "url": "http://127.0.0.1:4790/mcp", "tokenFile": "~/.condx/daemon/token" }),
        ["git"] => git(),
        ["deployments"] => deployments(&q("env").unwrap_or_default()),
        ["approval"] => approval(&q("recipe").unwrap_or_default(), &q("env").unwrap_or_default(), q("function").as_deref()),
        ["promotion"] => json!({ "ok": true, "checks": [] }),
        ["environments", env, "tables"] => tables(env),
        ["environments", env, "first-table"] => json!({ "table": "tasks" }),
        ["environments", env, "documents"] => documents(env, &q("table").unwrap_or_default(), q("cursor").and_then(|c| c.parse().ok()).unwrap_or(0), q("limit").and_then(|l| l.parse().ok()).unwrap_or(50)),
        ["environments", env, "schema"] => schema(env),
        ["environments", env, "functions"] => json!({ "environment": env, "functions": functions() }),
        ["environments", env, "keys"] => keys(env),
        _ => return Err(anyhow!("Not available in the demo")),
    })
}

pub fn post(path: &str, body: Value) -> Result<Value> {
    let route = path.split('?').next().unwrap_or(path);
    let parts: Vec<&str> = route.trim_start_matches("/api/").split('/').collect();
    Ok(match parts.as_slice() {
        ["jobs"] => request_job(&body),
        ["projects"] => with(|s| {
            s.registered = true;
            project()
        }),
        ["doctor", "backend"] => with(|s| {
            s.backend = true;
            json!({ "version": "precompiled-2026-10-24", "path": "~/.condx/daemon/bin/convex-local-backend" })
        }),
        ["jobs", id, "cancel"] => set_status(id, "cancelled", None),
        ["jobs", id, "retry"] => set_status(id, "succeeded", None),
        ["approvals", id] => {
            if body["approve"] == true {
                set_status(id, "succeeded", Some("you"))
            } else {
                set_status(id, "rejected", Some("you"))
            }
        }
        ["policy"] => with(|s| {
            for part in ["read", "write"] {
                if let Some(o) = body[part].as_object() {
                    for (k, v) in o {
                        if !(part == "write" && k == "cloud-prod" && v == "none") {
                            s.policy[part][k] = v.clone();
                        }
                    }
                }
            }
            s.policy.clone()
        }),
        ["workflows"] => save_workflow(body),
        ["workflows", "suggested"] => json!({ "created": [] }),
        ["workflows", id, "run"] => run_workflow(id, body["commit"].as_str().unwrap_or("main")),
        ["environments", _, "keys"] => json!({ "ok": true, "deployment": "happy-otter-123" }),
        _ => json!({ "ok": true }),
    })
}

pub fn delete(_: &str) -> Result<Value> {
    Ok(json!({ "ok": true }))
}

pub fn tool(name: &str, _: Value) -> Result<Value> {
    Ok(match name {
        "get_env_var_names" => json!({ "names": ["AUTH_SECRET", "OPENAI_API_KEY", "RESEND_API_KEY", "SITE_URL", "STRIPE_SECRET_KEY"] }),
        "get_function_logs" => function_logs(),
        _ => json!({}),
    })
}

// ---------- Proyecto y entornos ----------

fn doctor() -> Value {
    let backend = with(|s| s.backend);
    let check = |id: &str, ok: bool, detail: &str, fix: Option<&str>, optional: bool| json!({ "id": id, "ok": ok, "detail": detail, "fix": fix, "optional": optional });
    json!({
        "ready": backend,
        "checks": [
            check("node", true, "v22.11.0", None, false),
            check("convex-login", true, "~/.convex/config.json", None, false),
            check("backend", backend, if backend { "~/.condx/daemon/bin/convex-local-backend" } else { "missing" }, None, false),
            check("git", true, "git version 2.50.1", None, false),
            check("gh", false, "not logged in", Some("gh auth login"), true),
            check("docker", false, "not running (native mode doesn't need it)", None, true),
        ]
    })
}

fn is_local(env: &str) -> bool {
    env == "local" || env == "rehearsal"
}

fn project() -> Value {
    json!({
        "name": PROJECT, "title": "My App", "default": false,
        "repo": "~/code/my-app", "dataDir": "~/.condx/daemon/projects/my-app",
        "convexProject": "my-app", "convexTeam": "acme", "cloudDev": DEV,
        "backendPort": 3210, "backendUrl": "http://127.0.0.1:3210", "siteUrl": "http://127.0.0.1:3211",
        "frontend": ["node_modules/.bin/vite"], "frontendKind": "vite", "frontendPort": 5173, "frontendUrl": "http://localhost:5173",
        "createdAt": now() - 30 * 24 * HOUR,
    })
}

const HEAD: &str = "a4f2c91e7b3d58a06c2e9f1b4d7a3c5e8f0b2d61";
const PREV: &str = "7c1e93b5d2a4f6081e3c5b7d9f1a2c4e6b8d0f13";

fn env(name: &str, kind: &str, label: &str, url: &str, deployment: Option<&str>, commit: &str, latency: i64) -> Value {
    json!({
        "name": name, "kind": kind, "label": label,
        "convex": { "url": url, "deployment": deployment, "reachable": true, "latencyMs": latency, "version": "1.29.3", "error": null, "checkedAt": now() - 4_000 },
        "deployed": { "commit": commit, "branch": "main", "source": "condx", "behindBranch": 0, "uncommittedChanges": false },
    })
}

fn environments() -> Vec<Value> {
    vec![
        env("local", "devbox", "Local", "http://127.0.0.1:3210", None, HEAD, 3),
        env("rehearsal", "throwaway", "Rehearsal (throwaway)", "http://127.0.0.1:3330", None, HEAD, 4),
        env(DEV, "cloud-dev", "Cloud dev", "https://happy-otter-123.convex.cloud", Some("happy-otter-123"), HEAD, 48),
        env(PROD, "cloud-prod", "Production", "https://brave-falcon-456.convex.cloud", Some("brave-falcon-456"), PREV, 52),
    ]
}

fn overview() -> Value {
    let jobs = with(|s| s.jobs.clone());
    let approvals: Vec<Value> = jobs.iter().filter(|j| j["status"] == "waiting-approval").cloned().collect();
    json!({
        "generatedAt": now(),
        "daemon": { "version": "0.1.0", "startedAt": now() - 6 * HOUR, "repo": null },
        "environments": environments(),
        "jobs": jobs.iter().take(8).cloned().collect::<Vec<_>>(),
        "approvals": approvals,
        "attention": [],
        "ci": [{ "repo": "acme/my-app", "branch": "main", "commit": HEAD, "status": "completed", "conclusion": "success", "title": "CI", "at": now() - 22 * MIN, "url": null }],
        "git": { "branch": "main", "head": &HEAD[..8], "main": &HEAD[..8], "dev": &HEAD[..8], "ahead": 0, "behind": 0, "changed": 0, "untracked": 0, "subject": "feat: task comments with mentions", "submodules": [] },
        "deployer": null,
        "frontendProduction": null,
        "tabs": { "count": 0, "list": [] },
    })
}

/// El CI de la demo como lo deja la sonda (`gh run list` del origin del proyecto).
fn ci_probe() -> Value {
    let iso = |ago: i64| {
        let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64((now() - ago) as f64));
        d.to_iso_string().as_string().unwrap_or_default()
    };
    let run = |wf: &str, title: &str, branch: &str, conclusion: &str, ago: i64| {
        json!({ "status": "completed", "conclusion": conclusion, "workflowName": wf, "displayTitle": title, "headBranch": branch, "headSha": HEAD, "updatedAt": iso(ago), "url": "https://github.com/serlismaldonado/condx/actions" })
    };
    let runs = vec![
        run("CI", "feat: task comments with mentions", "main", "success", 22 * MIN),
        run("CI", "fix: invite expiry uses the server clock", "main", "success", 3 * HOUR),
        run("Preview", "feat: file previews", "files-preview", "failure", 5 * HOUR),
        run("CI", "chore: bump convex", "main", "success", 26 * HOUR),
    ];
    json!({ "key": format!("ci:{PROJECT}"), "ok": true, "data": { "repos": [{ "repo": "acme/my-app", "latest": runs[0].clone(), "runs": runs }] } })
}

fn services() -> Value {
    let svc = |name: &str, title: &str, description: &str, url: &str| {
        json!({ "name": name, "title": title, "description": description, "project": PROJECT, "status": "running", "healthy": true, "mode": "native", "url": url, "detail": "running for 6 h", "busy": null, "error": null })
    };
    json!([
        svc("backend", "Convex backend", "Convex's local backend, as a process managed by condxd", "http://127.0.0.1:3210"),
        svc("deployer", "Deployer", "Pushes your functions on every change (convex dev)", ""),
        svc("frontend", "Frontend (Vite)", "Your app's dev server", "http://localhost:5173"),
    ])
}

fn service_log(name: &str) -> Vec<Value> {
    let lines: &[&str] = match name {
        "deployer" => &["✔ 14:02:11 Convex functions ready! (1.2s)", "✔ 14:05:47 Convex functions ready! (0.9s)", "✔ 14:09:03 Convex functions ready! (1.1s)"],
        "frontend" => &["VITE v6.0.7  ready in 412 ms", "➜  Local:   http://localhost:5173/", "14:09:04 [vite] hmr update /src/routes/tasks.tsx"],
        _ => &["INFO  convex-local-backend: listening on 127.0.0.1:3210", "INFO  database: 6 tables, 22,590 documents", "INFO  isolate: pushed 12 functions"],
    };
    lines.iter().map(|l| json!(l)).collect()
}

fn backups() -> Value {
    json!([
        { "id": 12, "env": DEV, "at": now() - 2 * HOUR, "size": 18_432_112, "encrypted": true, "commit": HEAD, "job": "j-demo-3", "path": "~/.condx/backups/my-app-dev.zip.age" },
        { "id": 11, "env": PROD, "at": now() - 26 * HOUR, "size": 41_220_871, "encrypted": true, "commit": PREV, "job": "j-demo-9", "path": "~/.condx/backups/my-app-prod.zip.age" },
        { "id": 10, "env": "local", "at": now() - 3 * 24 * HOUR, "size": 2_904_118, "encrypted": false, "commit": PREV, "job": "j-demo-10", "path": "~/.condx/backups/local.zip" },
    ])
}

fn git() -> Value {
    let commit = |id: &str, subject: &str, when: &str, refs: Value| json!({ "id": id, "subject": subject, "author": "Ada Lovelace", "when": when, "parents": [], "refs": refs });
    json!({
        "project": PROJECT,
        "branches": [
            { "name": "main", "current": true, "upstream": "origin/main", "ahead": 0, "behind": 0, "subject": "feat: task comments with mentions", "when": "22 minutes ago" },
            { "name": "feat/billing", "current": false, "upstream": "origin/feat/billing", "ahead": 3, "behind": 1, "subject": "wip: stripe checkout", "when": "2 days ago" },
        ],
        "changes": { "staged": [], "unstaged": [], "untracked": [], "conflicted": [] },
        "commits": [
            commit(HEAD, "feat: task comments with mentions", "22 minutes ago", json!(["HEAD", "main", "origin/main"])),
            commit("e2b91f0c4a7d3e58b1c6f9a2d4e7b0c3a5f8d1e4", "fix: keep task order when moving between projects", "3 hours ago", json!([])),
            commit(PREV, "feat: invites expire after 7 days", "yesterday", json!(["v1.4.0"])),
            commit("3d5f7a9c1e2b4d6f8a0c2e4b6d8f0a1c3e5b7d9f", "chore: index tasks by assignee and status", "2 days ago", json!([])),
            commit("9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d3e2f1a0b", "feat: file attachments on tasks", "4 days ago", json!([])),
        ],
    })
}

fn deployments(env: &str) -> Value {
    let (first, second) = if env == PROD { (PREV, "3d5f7a9c1e2b4d6f8a0c2e4b6d8f0a1c3e5b7d9f") } else { (HEAD, PREV) };
    let latest = if env == PROD { 26 * HOUR } else { 20 * MIN };
    json!([
        { "commit": first, "at": now() - latest, "by": "workflow “Deploy to cloud dev”", "subject": "", "author": "Ada Lovelace" },
        { "commit": second, "at": now() - 3 * 24 * HOUR, "by": "you", "subject": "", "author": "Ada Lovelace" },
    ])
}

fn keys(env: &str) -> Value {
    if is_local(env) {
        return json!({ "local": true, "deployKey": null });
    }
    let deployment = if env == PROD { "brave-falcon-456" } else { "happy-otter-123" };
    let prefix = if env == PROD { "prod" } else { "dev" };
    json!({ "local": false, "deployment": deployment, "deployKey": { "stored": true, "account": format!("deploy-key:{env}"), "shared": false, "deployment": format!("{prefix}:{deployment}"), "valid": true, "problem": null } })
}

fn approval(recipe: &str, env: &str, function: Option<&str>) -> Value {
    let query = function.is_some_and(|f| functions().iter().any(|x| x["identifier"] == f && x["kind"] == "query"));
    let (approval, why) = match (recipe, is_local(env)) {
        (_, true) => ("none", "recipe"),
        ("run-function", false) if query => ("none", "query"),
        _ if env == PROD => ("typed", "recipe"),
        _ => ("simple", "recipe"),
    };
    json!({ "approval": approval, "why": why, "credential": if is_local(env) { "admin-key" } else { "deploy-key" } })
}

// ---------- Trabajos ----------

impl State {
    fn new() -> Self {
        let t = now();
        let job = |id: &str, recipe: &str, title: &str, env: &str, status: &str, by: &str, ago: i64, args: Value, approval: &str| {
            let ended = status == "succeeded" || status == "failed";
            json!({
                "id": id, "recipe": recipe, "title": title, "env": env, "envKind": kind_of(env), "status": status,
                "requestedBy": by, "createdAt": t - ago, "startedAt": t - ago + 1_000,
                "endedAt": if ended { json!(t - ago + 42_000) } else { Value::Null },
                "args": args, "approval": approval,
                "approvedBy": if status == "succeeded" && approval != "none" { json!("you") } else { Value::Null },
                "approvedAt": null, "commit": null,
                "error": if status == "failed" { json!("Uncaught Error: seed:demoData only runs on an empty database") } else { Value::Null },
                "outputs": {}, "preconditions": [], "attempts": 1, "retryFrom": 0, "steps": steps_of(recipe, status),
            })
        };
        let jobs = vec![
            job("j-demo-1", "deploy-cloud", "Deploy to cloud dev", DEV, "waiting-approval", "claude-code", MIN, json!({ "commit": &HEAD[..8] }), "simple"),
            job("j-demo-2", "run-function", "Run a function", DEV, "succeeded", "you", 9 * MIN, json!({ "function": "tasks:listByProject", "payload": "{\"projectId\":\"…\"}" }), "none"),
            job("j-demo-3", "export", "Backup (export)", DEV, "succeeded", "workflow “Cloud backups”", 2 * HOUR, json!({}), "none"),
            job("j-demo-4", "deploy", "Deploy a commit", "local", "succeeded", "you", 3 * HOUR, json!({ "commit": "main" }), "none"),
            job("j-demo-5", "run-function", "Run a function", "local", "failed", "claude-code", 5 * HOUR, json!({ "function": "seed:demoData", "payload": "{}" }), "none"),
            job("j-demo-6", "deploy-cloud", "Deploy to cloud dev", DEV, "succeeded", "workflow “Deploy to cloud dev”", 20 * HOUR, json!({ "commit": &PREV[..8] }), "simple"),
        ];
        Self { jobs, policy: default_policy(), workflows: default_workflows(), runs: default_runs(), next: 7, registered: !welcome_mode(), backend: false }
    }
}

fn kind_of(env: &str) -> &'static str {
    match env {
        "local" => "devbox",
        "rehearsal" => "throwaway",
        PROD => "cloud-prod",
        _ => "cloud-dev",
    }
}

fn steps_of(recipe: &str, status: &str) -> Value {
    let names: &[&str] = match recipe {
        "deploy-cloud" => &["Deploy and record"],
        "deploy" => &["Typecheck", "Deploy"],
        "export" => &["Export"],
        _ => &["Run function"],
    };
    let st = match status {
        "succeeded" => "ok",
        "failed" => "failed",
        _ => "pending",
    };
    let t = now();
    json!(names.iter().map(|n| json!({ "name": n, "status": st, "startedAt": t - 60_000, "endedAt": t - 20_000, "message": "" })).collect::<Vec<_>>())
}

fn job_log(id: &str) -> String {
    let job = with(|s| s.jobs.iter().find(|j| j["id"] == id).cloned()).unwrap_or_default();
    let env = job["env"].as_str().unwrap_or_default();
    let by = job["requestedBy"].as_str().unwrap_or("you");
    match job["recipe"].as_str().unwrap_or_default() {
        "deploy-cloud" => format!("Requested by {by}: deploy-cloud on {env}\n▶ Deploy and record\n$ npx convex dev --once (→ {env})\n✔ Typechecked\n✔ Pushed 12 functions\n✔ Convex functions ready! (2.8s)\ndeployed to {env}"),
        "export" => format!("▶ Export\n$ npx convex export --path ~/.condx/backups/{env}.zip (→ {env})\n✔ Created snapshot export\n✔ Encrypted with age\nbackup #12"),
        "deploy" => "▶ Typecheck\n$ npx tsc --noEmit\n✔ no errors\n▶ Deploy\n$ npx convex dev --once (→ local)\n✔ Convex functions ready! (1.1s)".into(),
        _ if job["status"] == "failed" => format!("▶ Run function\n$ npx convex run seed:demoData {{}} (→ {env})\nUncaught Error: seed:demoData only runs on an empty database\n    at handler (convex/seed.ts:14:11)"),
        _ => format!("▶ Run function\n$ npx convex run tasks:listByProject '{{\"projectId\":\"…\"}}' (→ {env})\n[\n  {{ \"_id\": \"kh7a…\", \"title\": \"Ship billing page\", \"status\": \"in_progress\" }},\n  {{ \"_id\": \"kh7b…\", \"title\": \"Fix invite emails\", \"status\": \"todo\" }}\n]"),
    }
}

fn request_job(body: &Value) -> Value {
    let recipe = body["recipe"].as_str().unwrap_or_default().to_string();
    let env = body["env"].as_str().unwrap_or("local").to_string();
    let function = body["args"]["function"].as_str().map(str::to_string);
    let level = approval(&recipe, &env, function.as_deref())["approval"].as_str().unwrap_or("simple").to_string();
    let title = serde_json::from_str::<Value>(RECIPES)
        .ok()
        .and_then(|r| r.as_array()?.iter().find(|x| x["name"] == recipe.as_str()).and_then(|x| x["title"].as_str().map(str::to_string)))
        .unwrap_or_else(|| recipe.clone());
    let status = if level == "none" { "succeeded" } else { "waiting-approval" };
    with(|s| {
        let id = format!("j-demo-{}", s.next);
        s.next += 1;
        let t = now();
        let job = json!({
            "id": id, "recipe": recipe, "title": title, "env": env, "envKind": kind_of(&env), "status": status,
            "requestedBy": "you", "createdAt": t, "startedAt": t, "endedAt": if status == "succeeded" { json!(t + 1_500) } else { Value::Null },
            "args": body["args"], "approval": level, "approvedBy": null, "approvedAt": null, "commit": null, "error": null,
            "outputs": {}, "preconditions": [], "attempts": 1, "retryFrom": 0, "steps": steps_of(&recipe, status),
        });
        s.jobs.insert(0, job.clone());
        job
    })
}

fn set_status(id: &str, status: &str, by: Option<&str>) -> Value {
    with(|s| {
        let Some(job) = s.jobs.iter_mut().find(|j| j["id"] == id) else { return json!({ "ok": false }) };
        let recipe = job["recipe"].as_str().unwrap_or_default().to_string();
        job["status"] = json!(status);
        job["endedAt"] = json!(now());
        job["steps"] = steps_of(&recipe, status);
        if let Some(by) = by {
            job["approvedBy"] = json!(by);
        }
        job.clone()
    })
}

// ---------- Reglas y flujos ----------

fn default_policy() -> Value {
    json!({
        "kinds": ["devbox", "throwaway", "cloud-dev", "cloud-prod"],
        "read": { "devbox": true, "throwaway": true, "cloud-dev": true, "cloud-prod": false },
        "write": { "devbox": "recipe", "throwaway": "recipe", "cloud-dev": "recipe", "cloud-prod": "recipe" },
    })
}

fn node(id: &str, kind: &str, title: &str, x: f32, params: Value) -> Value {
    json!({ "id": id, "kind": kind, "title": title, "enabled": true, "onFailure": "stop", "params": params, "x": x, "y": 0.0 })
}

fn chain(nodes: &[Value]) -> Value {
    json!(nodes.windows(2).map(|w| json!([w[0]["id"], w[1]["id"]])).collect::<Vec<_>>())
}

fn flow(id: &str, name: &str, description: &str, enabled: bool, nodes: Vec<Value>) -> Value {
    json!({ "id": id, "project": PROJECT, "name": name, "description": description, "enabled": enabled, "edges": chain(&nodes), "nodes": nodes, "updatedAt": now() - 24 * HOUR })
}

fn default_workflows() -> Vec<Value> {
    vec![
        flow(
            "w-deploy-dev",
            "Deploy to cloud dev",
            "CI green, cloud dev responds, your approval, deploy and a notice.",
            true,
            vec![
                node("ci", "check", "CI is green", 0., json!({ "check": "ci-green" })),
                node("check", "check", "Cloud dev responds", 300., json!({ "check": "env-reachable", "env": DEV })),
                node("approve", "approval", "Approve the deploy", 600., json!({ "env": DEV, "level": "simple", "message": "Deploy to cloud dev?" })),
                node("deploy", "recipe", "Deploy to cloud dev", 900., json!({ "recipe": "deploy-cloud", "env": DEV, "args": { "commit": "{run.commit}" } })),
                node("notify", "notify", "Notice", 1200., json!({ "message": "{run.commit} deployed to cloud dev" })),
            ],
        ),
        flow(
            "w-backups",
            "Cloud backups",
            "An encrypted backup of each cloud environment.",
            true,
            vec![
                node("backup-0", "recipe", "Backup of my-app-dev", 0., json!({ "recipe": "export", "env": DEV, "args": {} })),
                node("backup-1", "recipe", "Backup of my-app-prod", 300., json!({ "recipe": "export", "env": PROD, "args": {} })),
                node("notify", "notify", "Notice", 600., json!({ "message": "Cloud backups done" })),
            ],
        ),
        flow(
            "w-local",
            "Deploy to local",
            "The local backend responds, the commit is deployed and you get a notice.",
            false,
            vec![
                node("check", "check", "Local responds", 0., json!({ "check": "env-reachable", "env": "local" })),
                node("deploy", "recipe", "Deploy to local", 300., json!({ "recipe": "deploy", "env": "local", "args": { "commit": "{run.commit}" } })),
                node("notify", "notify", "Notice", 600., json!({ "message": "{run.commit} deployed to local" })),
            ],
        ),
    ]
}

fn default_runs() -> Vec<Value> {
    let t = now();
    let flows = default_workflows();
    let run = |w: &Value, ago: i64| {
        let order: Vec<Value> = w["nodes"].as_array().into_iter().flatten().map(|n| n["id"].clone()).collect();
        let nodes: Vec<Value> = order.iter().map(|id| json!({ "id": id, "status": "succeeded", "job": null, "startedAt": t - ago, "endedAt": t - ago + 30_000, "message": "", "outputs": {} })).collect();
        json!({ "id": format!("r-{}", w["id"].as_str().unwrap_or_default()), "workflowId": w["id"], "workflowName": w["name"], "project": PROJECT, "commit": "main", "status": "succeeded", "requestedBy": "app", "createdAt": t - ago, "endedAt": t - ago + 40_000, "error": null, "order": order, "nodes": nodes, "snapshot": w })
    };
    vec![run(&flows[0], 20 * HOUR), run(&flows[1], 2 * HOUR)]
}

fn workflows() -> Value {
    with(|s| {
        let list: Vec<Value> = s
            .workflows
            .iter()
            .map(|w| {
                let last = s.runs.iter().find(|r| r["workflowId"] == w["id"]).cloned().unwrap_or(Value::Null);
                json!({ "workflow": w, "problems": [], "lastRun": last })
            })
            .collect();
        let envs: Vec<Value> = environments().iter().map(|e| json!([e["name"], e["kind"]])).collect();
        json!({ "workflows": list, "environments": envs })
    })
}

fn save_workflow(mut body: Value) -> Value {
    with(|s| {
        if body["id"].as_str().unwrap_or_default().is_empty() {
            body["id"] = json!(format!("w-demo-{}", s.next));
            s.next += 1;
        }
        body["updatedAt"] = json!(now());
        match s.workflows.iter_mut().find(|w| w["id"] == body["id"]) {
            Some(w) => *w = body.clone(),
            None => s.workflows.push(body.clone()),
        }
        body
    })
}

fn run_workflow(id: &str, commit: &str) -> Value {
    with(|s| {
        let Some(w) = s.workflows.iter().find(|w| w["id"] == id).cloned() else { return json!({ "error": "no workflow" }) };
        let t = now();
        let order: Vec<Value> = w["nodes"].as_array().into_iter().flatten().map(|n| n["id"].clone()).collect();
        let nodes: Vec<Value> = w["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|n| {
                let status = if n["kind"] == "approval" { "waiting-approval" } else { "pending" };
                json!({ "id": n["id"], "status": status, "job": null, "startedAt": null, "endedAt": null, "message": "", "outputs": {} })
            })
            .collect();
        let run = json!({ "id": format!("r-demo-{}", s.next), "workflowId": id, "workflowName": w["name"], "project": PROJECT, "commit": commit, "status": "running", "requestedBy": "app", "createdAt": t, "endedAt": null, "error": null, "order": order, "nodes": nodes, "snapshot": w });
        s.next += 1;
        s.runs.insert(0, run.clone());
        run
    })
}

// ---------- Datos ----------

/// Las tablas del proyecto de ejemplo y cuántos documentos tiene cada una (en cloud).
const TABLES: &[(&str, u64)] = &[("comments", 14_230), ("files", 902), ("invites", 41), ("projects", 312), ("tasks", 5_821), ("users", 1_284)];

fn count(env: &str, base: u64) -> u64 {
    match env {
        "local" | "rehearsal" => (base / 10).max(4),
        PROD => base * 3,
        _ => base,
    }
}

fn tables(env: &str) -> Value {
    let list: Vec<Value> = TABLES.iter().map(|(n, c)| json!({ "name": n, "count": count(env, *c) })).collect();
    json!({ "environment": env, "tables": list, "counts": true, "via": if is_local(env) { "local" } else { "key" } })
}

/// Generador pseudoaleatorio con semilla: los mismos datos en cada visita.
struct Rng(u64);

impl Rng {
    fn new(seed: &str) -> Self {
        Self(seed.bytes().fold(1469598103934665603u64, |h, b| (h ^ b as u64).wrapping_mul(1099511628211)) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn pick<'a>(&mut self, list: &[&'a str]) -> &'a str {
        list[(self.next() % list.len() as u64) as usize]
    }
}

/// Un id con la forma de los de Convex (32 caracteres de su alfabeto).
fn id_for(table: &str, i: u64) -> String {
    const ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
    let mut r = Rng::new(&format!("{table}:{i}"));
    let prefix = match table {
        "users" => "jd",
        "projects" => "kn",
        "tasks" => "kh",
        "comments" => "km",
        "files" => "kg",
        _ => "kx",
    };
    let rest: String = (0..30).map(|_| ALPHABET[(r.next() % 32) as usize] as char).collect();
    format!("{prefix}{rest}")
}

fn documents(env: &str, table: &str, start: usize, limit: usize) -> Value {
    let total = TABLES.iter().find(|(n, _)| *n == table).map(|(_, c)| count(env, *c)).unwrap_or(0) as usize;
    let end = (start + limit.clamp(1, 500)).min(total);
    let first = ["Ada", "Grace", "Alan", "Linus", "Margaret", "Ken", "Barbara", "Dennis", "Frances", "Edsger"];
    let last = ["Lovelace", "Hopper", "Turing", "Torvalds", "Hamilton", "Thompson", "Liskov", "Ritchie", "Allen", "Dijkstra"];
    let titles = ["Ship billing page", "Fix invite emails", "Add dark mode", "Index tasks by status", "Migrate to new auth", "Write onboarding guide", "Refactor uploads", "Speed up search", "Weekly report", "Audit permissions"];
    let docs: Vec<Value> = (start..end)
        .map(|i| {
            let i = i as u64;
            let mut r = Rng::new(&format!("{env}:{table}:{i}"));
            let created = now() as f64 - (i as f64 * 3_600_000.0 * 0.37) - (r.next() % 60_000) as f64;
            let id = id_for(table, i);
            match table {
                "users" => {
                    let (f, l) = (r.pick(&first), r.pick(&last));
                    json!({ "_id": id, "_creationTime": created, "name": format!("{f} {l}"), "email": format!("{}.{}@example.com", f.to_lowercase(), l.to_lowercase()), "role": r.pick(&["admin", "member", "member", "viewer"]), "lastSeenAt": created + 86_400_000.0 })
                }
                "projects" => json!({ "_id": id, "_creationTime": created, "name": format!("{} {}", r.pick(&["Atlas", "Nova", "Orbit", "Pulse", "Quartz", "Vega"]), i % 97), "ownerId": id_for("users", r.next() % 1284), "archived": r.next() % 9 == 0 }),
                "tasks" => json!({ "_id": id, "_creationTime": created, "title": r.pick(&titles), "status": r.pick(&["todo", "in_progress", "in_review", "done"]), "projectId": id_for("projects", r.next() % 312), "assigneeId": id_for("users", r.next() % 1284), "points": r.next() % 8 + 1, "dueAt": created + ((r.next() % 14) as f64) * 86_400_000.0 }),
                "comments" => json!({ "_id": id, "_creationTime": created, "taskId": id_for("tasks", r.next() % 5821), "authorId": id_for("users", r.next() % 1284), "body": r.pick(&["Looks good to me", "Can we split this in two?", "Done in the last deploy", "@grace can you take a look?", "Blocked by the auth migration"]) }),
                "files" => json!({ "_id": id, "_creationTime": created, "name": format!("{}.{}", r.pick(&["mockup", "spec", "invoice", "screenshot", "notes"]), r.pick(&["png", "pdf", "md"])), "taskId": id_for("tasks", r.next() % 5821), "size": r.next() % 4_000_000 }),
                _ => json!({ "_id": id, "_creationTime": created, "email": format!("guest{i}@example.com"), "projectId": id_for("projects", r.next() % 312), "expiresAt": created + 7.0 * 86_400_000.0 }),
            }
        })
        .collect();
    let done = end >= total;
    json!({ "table": table, "documents": docs, "cursor": if done { Value::Null } else { json!(end.to_string()) }, "isDone": done, "total": total })
}

fn schema(env: &str) -> Value {
    let field = |name: &str, ty: &str, refs: &[&str]| json!({ "name": name, "type": ty, "optional": false, "refs": refs });
    let table = |name: &str, fields: Vec<Value>, indexes: Value| {
        let mut all = vec![json!({ "name": "_id", "type": format!("id<{name}>"), "optional": false, "primary": true })];
        all.extend(fields);
        all.push(json!({ "name": "_creationTime", "type": "number", "optional": false }));
        json!({ "name": name, "module": name, "typed": true, "fields": all, "indexes": indexes, "searchIndexes": [], "vectorIndexes": [] })
    };
    json!({
        "environment": env, "fromEnvironment": env, "source": "deployed",
        "tables": [
            table("comments", vec![field("taskId", "id<tasks>", &["tasks"]), field("authorId", "id<users>", &["users"]), field("body", "string", &[])], json!([{ "name": "by_task", "fields": ["taskId"] }])),
            table("files", vec![field("taskId", "id<tasks>", &["tasks"]), field("name", "string", &[]), field("size", "number", &[])], json!([{ "name": "by_task", "fields": ["taskId"] }])),
            table("invites", vec![field("projectId", "id<projects>", &["projects"]), field("email", "string", &[]), field("expiresAt", "number", &[])], json!([])),
            table("projects", vec![field("ownerId", "id<users>", &["users"]), field("name", "string", &[]), field("archived", "boolean", &[])], json!([{ "name": "by_owner", "fields": ["ownerId"] }])),
            table("tasks", vec![field("projectId", "id<projects>", &["projects"]), field("assigneeId", "id<users>", &["users"]), field("title", "string", &[]), field("status", "\"todo\" | \"in_progress\" | \"in_review\" | \"done\"", &[]), field("points", "number", &[]), field("dueAt", "number", &[])], json!([{ "name": "by_project", "fields": ["projectId"] }, { "name": "by_assignee_status", "fields": ["assigneeId", "status"] }])),
            table("users", vec![field("name", "string", &[]), field("email", "string", &[]), field("role", "\"admin\" | \"member\" | \"viewer\"", &[])], json!([{ "name": "by_email", "fields": ["email"] }])),
        ],
    })
}

fn functions() -> Vec<Value> {
    let f = |identifier: &str, kind: &str, args: Value| json!({ "identifier": identifier, "kind": kind, "visibility": "public", "args": args });
    let a = |name: &str, ty: &str, optional: bool| json!({ "name": name, "type": ty, "optional": optional });
    vec![
        f("tasks:listByProject", "query", json!([a("projectId", "id<projects>", false), a("status", "string", true)])),
        f("tasks:get", "query", json!([a("taskId", "id<tasks>", false)])),
        f("tasks:create", "mutation", json!([a("projectId", "id<projects>", false), a("title", "string", false), a("assigneeId", "id<users>", true)])),
        f("tasks:move", "mutation", json!([a("taskId", "id<tasks>", false), a("status", "string", false)])),
        f("projects:list", "query", json!([])),
        f("projects:archive", "mutation", json!([a("projectId", "id<projects>", false)])),
        f("comments:add", "mutation", json!([a("taskId", "id<tasks>", false), a("body", "string", false)])),
        f("users:me", "query", json!([])),
        f("invites:send", "action", json!([a("projectId", "id<projects>", false), a("email", "string", false)])),
        f("files:generateUploadUrl", "mutation", json!([])),
        f("search:tasks", "query", json!([a("query", "string", false)])),
        f("seed:demoData", "mutation", json!([])),
    ]
}

fn function_logs() -> Value {
    let t = now() as f64 / 1000.0;
    let entry = |ago: f64, kind: &str, identifier: &str, lines: &[&str], error: Option<&str>| json!({ "timestamp": t - ago, "udfType": kind, "identifier": identifier, "logLines": lines, "error": error });
    json!({ "logs": [
        entry(4.0, "Query", "tasks:listByProject", &[], None),
        entry(12.0, "Mutation", "tasks:move", &["moved a task to in_review"], None),
        entry(31.0, "Action", "invites:send", &["sending invite to guest41@example.com"], None),
        entry(58.0, "Mutation", "comments:add", &[], Some("Uncaught Error: task not found")),
        entry(73.0, "Query", "users:me", &[], None),
    ] })
}
