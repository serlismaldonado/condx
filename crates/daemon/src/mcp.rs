// MCP del daemon (§10): lectura, telemetría y trabajos. Se sirve por HTTP en
// /mcp y por stdio con `condxd mcp`. Un agente puede pedir trabajos y
// seguirlos, pero no hay herramienta para aprobar: eso es solo de la app.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::auth::{Client, Scope};
use crate::probes::{env_var_names, function_logs};
use crate::{jobs, overview, recipes};
use crate::state::Shared;

const PROTOCOL: &str = "2025-06-18";

fn tab_arg() -> Value {
    json!({ "type": "string", "description": "Tab (see list_tabs). Defaults to the most recent visible one." })
}

fn env_arg() -> Value {
    json!({ "type": "string", "description": "Environment name (see list_environments)." })
}

fn project_arg() -> Value {
    json!({ "type": "string", "description": "Project (see list_projects). Defaults to heskala-erp." })
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({ "name": name, "description": description, "inputSchema": { "type": "object", "properties": properties, "required": required } })
}

pub fn tools() -> Vec<Value> {
    let tab = || json!({ "tabId": tab_arg() });
    vec![
        // Entornos y estado.
        tool("get_overview", "Project overview: commit deployed on each environment (devbox, cloud dev, prod) versus its branch, Convex backends, production frontend on Vercel, CI, deployer, connected tabs and what needs attention. Start here.", json!({ "project": project_arg() }), &[]),
        tool("list_environments", "Registered environments with their kind (devbox, throwaway, cloud-dev, cloud-prod) and project. Never includes credentials. With project, only that project's.", json!({ "project": { "type": "string", "description": "Project (see list_projects); without it, all." } }), &[]),
        tool("get_environment", "One environment with the latest result of its Convex probe.", json!({ "name": env_arg() }), &["name"]),
        tool(
            "get_schema",
            "An environment's Convex schema: tables, typed fields, v.id(table) references and indexes. For local ones it's the deployed schema; for cloud, the local backend of the same project (source = checkout).",
            json!({ "name": env_arg() }),
            &["name"],
        ),
        tool("get_env_var_names", "Names of the Convex deployment's environment variables. Never returns values.", json!({ "name": env_arg() }), &["name"]),
        tool(
            "get_function_logs",
            "Latest Convex function logs for the environment (redacted). Takes about 8 s.",
            json!({ "name": env_arg(), "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 50 }, "errorsOnly": { "type": "boolean", "default": false } }),
            &["name"],
        ),
        tool("get_ci", "Latest GitHub Actions runs of the ERP and its submodules.", json!({}), &[]),
        tool("get_git", "ERP git: branch, ahead/behind, changes, dev/main and submodules versus their remote.", json!({}), &[]),
        tool("get_deployer", "Devbox deployer: phase (idle, syncing, error), image and latest log lines.", json!({}), &[]),
        tool("get_containers", "Docker containers (name, image, state, compose service).", json!({}), &[]),
        tool("get_vercel", "Vercel frontend: production deploy, latest preview, in progress and latest failed.", json!({}), &[]),
        // Trabajos (F2). Aprobar no está: solo desde la app.
        tool("list_recipes", "Available recipes: what they do, which environments they run on, what approval they need, which arguments they take and whether they run on other projects' environments (projects).", json!({}), &[]),
        tool(
            "request_job",
            "Requests a job (recipe + environment + arguments). If the recipe needs approval it stays “waiting-approval” until the user approves it in the app. Returns the job with its id.",
            json!({
                "recipe": { "type": "string" },
                "env": { "type": "string", "description": "Environment (or the new name, for throwaway-create)" },
                "args": { "type": "object" }
            }),
            &["recipe", "env"],
        ),
        tool("list_jobs", "Recent jobs (summary). Filter by status: waiting-approval, queued, running, succeeded, failed, cancelled, rejected, expired; and by project.", json!({ "status": { "type": "string" }, "limit": { "type": "integer", "default": 20 }, "project": { "type": "string", "description": "Project (see list_projects); without it, all." } }), &[]),
        tool("get_job", "One job with its steps, status and error.", json!({ "id": { "type": "string" } }), &["id"]),
        tool("get_job_log", "A job's log (redacted) from a byte offset; returns the new offset to keep following it.", json!({ "id": { "type": "string" }, "offset": { "type": "integer", "default": 0 } }), &["id"]),
        tool("cancel_job", "Cancels a job requested through MCP (queued, waiting for approval or running).", json!({ "id": { "type": "string" } }), &["id"]),
        tool("list_approvals", "Jobs waiting for the user's approval in the app.", json!({}), &[]),
        tool(
            "check_promotion",
            "Preconditions to promote a commit to production (CI, cloud dev, backup < 2 h old, rehearsal). Requests nothing.",
            json!({ "commit": { "type": "string", "default": "dev" }, "env": { "type": "string", "description": "Production environment (defaults to the registered one)" } }),
            &[],
        ),
        tool("list_projects", "Convex projects managed by Condx: folder, Convex project, local instance ports and frontend.", json!({}), &[]),
        tool(
            "register_project",
            "Adds a Convex repo (with convex/ and package.json) as a project: assigns ports and detects its frontend (Vite or Next). Starts nothing.",
            json!({ "path": { "type": "string", "description": "Repo folder (~ allowed)" }, "name": { "type": "string", "description": "Short name; defaults to the Convex project in .env.local" } }),
            &["path"],
        ),
        tool("list_services", "A project's local development environment: Convex backend, deployer (convex dev) and frontend, with their status.", json!({ "project": project_arg() }), &[]),
        tool(
            "service_action",
            "Starts, stops or restarts a local service (backend, deployer, frontend; in heskala-erp the frontend is vite) or all of them (all).",
            json!({ "name": { "type": "string", "enum": ["backend", "deployer", "frontend", "vite", "all"] }, "action": { "type": "string", "enum": ["start", "stop", "restart"] }, "project": project_arg() }),
            &["name", "action"],
        ),
        tool(
            "get_service_logs",
            "Latest log lines of a local service (redacted).",
            json!({ "name": { "type": "string", "enum": ["backend", "deployer", "frontend", "vite"] }, "tail": { "type": "integer", "default": 200 }, "project": project_arg() }),
            &["name"],
        ),
        tool("list_backups", "Recorded backups (cloud ones encrypted). Metadata only.", json!({}), &[]),
        // Flujos de despliegue: leer, ver corridas y empezar una. Las aprobaciones de sus nodos
        // solo se deciden en la app.
        tool("list_workflows", "The project's deploy workflows, with their problems and latest run.", json!({ "project": project_arg() }), &[]),
        tool(
            "run_workflow",
            "Starts a workflow run (id from list_workflows) for a commit or branch. Its approvals wait for the user in the app.",
            json!({ "id": { "type": "string" }, "commit": { "type": "string", "default": "dev" } }),
            &["id"],
        ),
        tool("get_workflow_run", "A run's status: each node with its status, message and job.", json!({ "id": { "type": "string" } }), &["id"]),
        // Telemetría de la app (las de devtools/mcp-server.mjs).
        tool("list_tabs", "App tabs connected to the daemon.", json!({}), &[]),
        tool("get_snapshot", "Full tab state: session, route, Convex calls per function, live queries and errors (devtools/schema.json).", tab(), &[]),
        tool("get_report", "Markdown report of the tab (session, most-called functions, errors).", tab(), &[]),
        tool(
            "convex_top",
            "Convex functions with the most executions in the tab.",
            json!({ "n": { "type": "integer", "minimum": 1, "maximum": 100, "default": 10 }, "tabId": tab_arg() }),
            &[],
        ),
        tool(
            "measure",
            "Measures Convex usage: resets counters, waits `seconds` and returns totals and top 10.",
            json!({ "seconds": { "type": "integer", "minimum": 5, "maximum": 120, "default": 30 }, "tabId": tab_arg() }),
            &[],
        ),
        tool("get_errors", "Grouped tab errors (console, window, promises, Convex functions). Redacted.", tab(), &[]),
        tool("get_live", "Widgets' live queries: whether visible, last and next request.", tab(), &[]),
        tool("get_offline", "Backend connection, service worker, IndexedDB and localStorage (counts only).", tab(), &[]),
        tool("reset_counters", "Resets the tab's Convex counters and error list.", tab(), &[]),
        tool(
            "simulate_hidden",
            "Simulates the tab being hidden (or makes it visible again).",
            json!({ "hidden": { "type": "boolean", "default": true }, "tabId": tab_arg() }),
            &[],
        ),
        tool(
            "simulate_offline",
            "Simulates the backend being down in the tab (or restores it).",
            json!({ "offline": { "type": "boolean", "default": true }, "tabId": tab_arg() }),
            &[],
        ),
        tool(
            "navigate",
            "Navigates the tab to an internal route, e.g. /dashboard.",
            json!({ "path": { "type": "string", "pattern": "^/(?!/)" }, "tabId": tab_arg() }),
            &["path"],
        ),
    ]
}

fn env_by_name(state: &Shared, args: &Value) -> Result<devtool_core::Environment> {
    let name = args["name"].as_str().ok_or_else(|| anyhow!("Missing name"))?;
    state.store.environment(name)?.ok_or_else(|| anyhow!("Environment “{name}” doesn't exist (see list_environments)"))
}

fn probe(state: &Shared, key: &str) -> Value {
    state.probe(key).unwrap_or_else(|| json!({ "key": key, "ok": false, "error": "The probe hasn't run yet" }))
}

/// `by`: quién pide (para el historial). `client`: el token y sus alcances.
pub async fn call(state: &Shared, client: &Client, by: &str, name: &str, args: &Value) -> Result<Value> {
    let tab = args["tabId"].as_str();
    let need = |scope: Scope| -> Result<()> {
        if client.can(scope) { Ok(()) } else { Err(anyhow!("The token doesn't have scope {scope:?}")) }
    };
    let hub = &state.hub;
    Ok(match name {
        "get_overview" => overview::build_for(state, args["project"].as_str()).await,
        "list_environments" => {
            let mut list = state.store.environments()?;
            if let Some(p) = args["project"].as_str().filter(|p| !p.is_empty()) {
                list.retain(|e| crate::projects::of_env(e) == p);
            }
            json!(list)
        }
        "get_environment" => {
            let env = env_by_name(state, args)?;
            json!({ "environment": env, "convex": state.probe(&format!("convex:{}", env.name)) })
        }
        "get_schema" => {
            let env = env_by_name(state, args)?;
            crate::schema::schema(state, &env).await?
        }
        "get_env_var_names" => {
            let env = env_by_name(state, args)?;
            json!({ "environment": env.name, "names": env_var_names(state, &env).await? })
        }
        "get_function_logs" => {
            let env = env_by_name(state, args)?;
            let limit = args["limit"].as_u64().unwrap_or(50) as usize;
            let errors_only = args["errorsOnly"].as_bool().unwrap_or(false);
            json!({ "environment": env.name, "logs": function_logs(state, &env, limit, errors_only).await? })
        }
        "get_ci" => probe(state, "ci"),
        "get_git" => probe(state, "git"),
        "get_deployer" => probe(state, "deployer"),
        "get_containers" => probe(state, "docker"),
        "get_vercel" => probe(state, "vercel"),
        "list_recipes" => json!(recipes::all()
            .iter()
            .map(|r| json!({
                "name": r.name,
                "title": r.title,
                "description": r.description,
                "kinds": r.kinds,
                "projects": r.projects,
                "approval": r.approval,
                "args": r.args,
                "steps": r.steps.iter().map(|s| &s.name).collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>()),
        "request_job" => {
            need(Scope::JobsRequest)?;
            let recipe = args["recipe"].as_str().ok_or_else(|| anyhow!("Missing recipe"))?;
            let env = args["env"].as_str().ok_or_else(|| anyhow!("Missing env"))?;
            json!(jobs::request(state, recipe, env, &args["args"], by).await?)
        }
        "list_jobs" => {
            let statuses: Vec<String> = args["status"].as_str().map(|s| s.split(',').map(str::to_string).collect()).unwrap_or_default();
            let refs: Vec<&str> = statuses.iter().map(String::as_str).collect();
            let limit = args["limit"].as_u64().unwrap_or(20).min(200) as usize;
            json!(crate::projects::jobs_of(state, &refs, limit, args["project"].as_str())?.iter().map(jobs::summary).collect::<Vec<_>>())
        }
        "get_job" => json!(jobs::load(state, args["id"].as_str().unwrap_or_default())?),
        "get_job_log" => {
            let (text, offset) = jobs::read_log(args["id"].as_str().unwrap_or_default(), args["offset"].as_u64().unwrap_or(0))?;
            json!({ "text": text, "offset": offset })
        }
        "cancel_job" => {
            need(Scope::JobsRequest)?;
            let id = args["id"].as_str().unwrap_or_default();
            let job = jobs::load(state, id)?;
            if by == "mcp" && job.requested_by != "mcp" {
                return Err(anyhow!("Through MCP you can only cancel jobs requested through MCP"));
            }
            json!(jobs::cancel(state, id, by)?)
        }
        "check_promotion" => {
            let env = match args["env"].as_str() {
                Some(n) => state.store.environment(n)?.ok_or_else(|| anyhow!("Environment “{n}” doesn't exist"))?,
                None => state
                    .store
                    .environments()?
                    .into_iter()
                    .find(|e| e.kind == devtool_core::EnvKind::CloudProd)
                    .ok_or_else(|| anyhow!("no production environment registered"))?,
            };
            let sha = jobs::resolve_commit(state.config.erp()?, args["commit"].as_str().unwrap_or("dev")).await?;
            let list = crate::pipeline::preconditions(state, &env, &sha).await;
            json!({ "env": env.name, "commit": sha, "ready": list.iter().all(|p| p["ok"] == true), "preconditions": list })
        }
        "list_backups" => json!(state.store.backups(None, 50)?),
        "list_workflows" => {
            let project = args["project"].as_str().unwrap_or("heskala-erp");
            let names: std::collections::HashSet<String> = state.store.environments()?.into_iter().map(|e| e.name).collect();
            json!(crate::workflows::list(state, project)?
                .iter()
                .map(|w| json!({
                    "id": w.id,
                    "name": w.name,
                    "nodes": w.nodes.iter().map(|n| json!({ "id": n.id, "title": n.title, "kind": n.kind, "enabled": n.enabled })).collect::<Vec<_>>(),
                    "problems": crate::workflows::problems(w, &names, |r| crate::recipes::get(r).is_some()),
                    "lastRun": state.store.runs(Some(&w.id), &[], 1).ok().and_then(|r| r.into_iter().next()).map(|r| json!({ "id": r["id"], "status": r["status"] })),
                }))
                .collect::<Vec<_>>())
        }
        "run_workflow" => {
            need(Scope::JobsRequest)?;
            let id = args["id"].as_str().ok_or_else(|| anyhow!("Missing id"))?;
            json!(crate::workflows::start(state, id, args["commit"].as_str().unwrap_or("dev"), by)?)
        }
        "get_workflow_run" => json!(crate::workflows::load_run(state, args["id"].as_str().unwrap_or_default())?),
        "list_projects" => crate::projects::list_json(state)?,
        "register_project" => {
            need(Scope::JobsRequest)?;
            let path = crate::projects::expand(args["path"].as_str().unwrap_or_default());
            crate::projects::to_json(&crate::projects::register(state, std::path::Path::new(&path), args["name"].as_str())?)
        }
        "list_services" => {
            let project = crate::projects::get(state, args["project"].as_str())?;
            json!(crate::services::list(state, &project).await)
        }
        "service_action" => {
            need(Scope::JobsRequest)?;
            let project = crate::projects::get(state, args["project"].as_str())?;
            crate::services::action(state, &project, args["name"].as_str().unwrap_or_default(), args["action"].as_str().unwrap_or_default())?
        }
        "get_service_logs" => {
            let project = crate::projects::get(state, args["project"].as_str())?;
            json!({ "text": crate::services::logs(state, &project, args["name"].as_str().unwrap_or_default(), args["tail"].as_u64().unwrap_or(200) as usize).await? })
        }
        "list_approvals" => json!(state.store.jobs(&["waiting-approval"], 100)?.iter().map(jobs::summary).collect::<Vec<_>>()),
        "list_tabs" => json!(hub.tabs()),
        "get_snapshot" => hub.snapshot(tab)?,
        "get_report" => hub.command("report", Value::Null, tab).await?,
        "convex_top" => hub.command("top", json!({ "n": args["n"].as_u64().unwrap_or(10) }), tab).await?,
        "get_errors" => hub.command("errors", Value::Null, tab).await?,
        "get_live" => hub.command("live", Value::Null, tab).await?,
        "get_offline" => hub.command("offline", Value::Null, tab).await?,
        "reset_counters" => hub.command("reset", Value::Null, tab).await?,
        "simulate_hidden" => hub.command("simulateHidden", json!({ "hidden": args["hidden"].as_bool() != Some(false) }), tab).await?,
        "simulate_offline" => hub.command("simulateOffline", json!({ "offline": args["offline"].as_bool() != Some(false) }), tab).await?,
        "navigate" => hub.command("navigate", json!({ "path": args["path"] }), tab).await?,
        "measure" => {
            let seconds = args["seconds"].as_u64().unwrap_or(30).clamp(5, 120);
            hub.command("reset", Value::Null, tab).await?;
            tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
            let snap = hub.command("snapshot", Value::Null, tab).await?;
            let top: Vec<Value> = snap["convex"]["functions"]
                .as_array()
                .into_iter()
                .flatten()
                .take(10)
                .map(|f| json!({ "name": f["name"], "kind": f["kind"], "active": f["active"], "runs": f["runs"], "reruns": f["reruns"] }))
                .collect();
            json!({ "seconds": seconds, "route": snap["page"]["route"], "visibility": snap["page"]["visibility"], "totals": snap["convex"]["totals"], "top": top })
        }
        _ => return Err(anyhow!("Unknown tool: {name}")),
    })
}

fn text(value: &Value) -> Value {
    let body = match value {
        Value::String(s) => s.clone(),
        v => serde_json::to_string_pretty(v).unwrap_or_default(),
    };
    json!({ "content": [{ "type": "text", "text": body }] })
}

/// El último uso del MCP: cuándo y qué cliente (el `clientInfo` de su `initialize`). La app lo
/// muestra en «Conectar un agente» para saber si ya hay uno conectado.
static LAST: std::sync::Mutex<Option<(i64, String)>> = std::sync::Mutex::new(None);

pub fn last_use() -> Value {
    match LAST.lock().ok().and_then(|l| l.clone()) {
        Some((at, client)) => json!({ "lastAt": at, "client": client }),
        None => json!({ "lastAt": null, "client": null }),
    }
}

/// Procesa un mensaje JSON-RPC; `None` para notificaciones.
pub async fn handle(state: &Shared, client: &Client, msg: &Value) -> Option<Value> {
    if let Ok(mut last) = LAST.lock() {
        let name = msg["params"]["clientInfo"]["name"].as_str().map(str::to_string);
        let previous = last.as_ref().map(|(_, c)| c.clone()).unwrap_or_default();
        *last = Some((crate::store::now_ms(), name.unwrap_or(previous)));
    }
    let id = msg.get("id").cloned().filter(|v| !v.is_null());
    let reply = |result: Value| Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    let fail = |code: i64, message: &str| Some(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }));
    let Some(method) = msg["method"].as_str().filter(|_| msg["jsonrpc"] == "2.0") else {
        return id.as_ref().and_then(|_| fail(-32600, "Invalid request"));
    };
    match method {
        "initialize" => reply(json!({
            "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "condx", "version": env!("CARGO_PKG_VERSION") },
            "instructions": "Condx, a local devtool for Convex (daemon). Start with get_overview: environments (devbox, cloud dev, prod), CI, deployer, tabs, jobs and approvals. App telemetry: get_report, get_errors, measure. Jobs: list_recipes → request_job → get_job / get_job_log. Anything that needs approval waits for the user in the app; there's no approve tool. Production only changes through promote-prod (preconditions: check_promotion) or rollback-prod.",
        })),
        "ping" => reply(json!({})),
        "tools/list" => reply(json!({ "tools": tools() })),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or_default();
            let args = msg["params"].get("arguments").cloned().unwrap_or_else(|| json!({}));
            match call(state, client, "mcp", name, &args).await {
                Ok(v) => reply(text(&v)),
                Err(e) => reply(json!({ "content": [{ "type": "text", "text": format!("{e:#}") }], "isError": true })),
            }
        }
        _ if id.is_none() => None,
        _ => fail(-32601, &format!("Unsupported method: {method}")),
    }
}
