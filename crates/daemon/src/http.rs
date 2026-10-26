// API local del daemon (solo 127.0.0.1, §9):
//
//   GET  /health                      sin token
//   GET  /api/overview                vista general
//   GET  /api/probes[/{key}]          últimos resultados de las sondas
//   GET  /api/tabs                    pestañas conectadas
//   POST /api/tools/{name}            cualquier herramienta del MCP, body = argumentos
//   GET  /api/recipes                 recetas
//   GET  /api/jobs[?status=a,b]       trabajos (resumen)
//   GET  /api/jobs/{id}               trabajo con sus pasos
//   GET  /api/jobs/{id}/log?offset=N  log desde un byte
//   POST /api/jobs                    { recipe, env, args } (alcance jobs:request)
//   POST /api/jobs/{id}/cancel|retry
//   GET  /api/approvals               esperando aprobación
//   GET  /api/promotion?env=&commit=  precondiciones de la promoción (sin pedirla)
//   GET  /api/backups, /api/secrets   respaldos registrados; qué secretos existen (sin valores)
//   GET  /api/deployments?env=        despliegues hechos por el daemon en un entorno
//   GET  /api/services                entorno local: backend, deployer, Vite
//   POST /api/services/{name|all}/{start|stop|restart}
//   GET  /api/services/{name}/logs?tail=N
//   POST /api/approvals/{id}          { approve, confirm } (alcance jobs:approve: solo la app)
//   POST /mcp                         MCP (JSON-RPC, Streamable HTTP sin SSE)
//   GET  /telemetry                   WebSocket de las pestañas de la app (por Origin, sin token)
//
// Todo lo que no es /health ni /telemetry pide `Authorization: Bearer <token>`:
// ~/.condx/daemon/token (CLI y MCP) o ~/.condx/daemon/app-token (app).

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Extension, Path, Query, Request, State as AxState};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::auth::{Client, Scope};
use crate::state::Shared;
use crate::{jobs, mcp, overview, recipes, workflows};

fn host_ok(headers: &HeaderMap, port: u16) -> bool {
    // Defensa contra DNS rebinding: solo nombres de loopback.
    let Some(host) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) else { return false };
    [format!("127.0.0.1:{port}"), format!("localhost:{port}"), format!("[::1]:{port}")].iter().any(|h| h == host)
}

pub fn origin_allowed(origin: &str, extra: &[String]) -> bool {
    let local = ["http://localhost", "http://127.0.0.1", "https://localhost", "https://127.0.0.1", "tauri://localhost"];
    local.iter().any(|base| origin == *base || origin.strip_prefix(base).map(|rest| rest.starts_with(':')).unwrap_or(false))
        || extra.iter().any(|o| o == origin)
}

async fn guard(AxState(state): AxState<Shared>, mut req: Request, next: Next) -> Response {
    if !host_ok(req.headers(), state.config.port) {
        return (StatusCode::FORBIDDEN, "Host not allowed").into_response();
    }
    let path = req.uri().path();
    if path == "/health" || path == "/telemetry" {
        return next.run(req).await;
    }
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    let Some(client) = token.and_then(|t| state.tokens.resolve(t).or_else(|| crate::connections::resolve(t))) else {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "Missing token (~/.condx/daemon/token)" }))).into_response();
    };
    // El token de los agentes solo habla por el MCP (que además limita sus herramientas).
    if matches!(client.name, "agent" | "external") && req.uri().path() != "/mcp" {
        return (StatusCode::FORBIDDEN, Json(json!({ "error": "agents only use the MCP" }))).into_response();
    }
    req.extensions_mut().insert(client);
    next.run(req).await
}

fn fail(status: StatusCode, e: impl std::fmt::Display) -> Response {
    // `{:#}` muestra la causa de un error con contexto («argumento x: JSON inválido…»).
    (status, Json(json!({ "error": format!("{e:#}") }))).into_response()
}

/// Respuesta 403 si al token le falta el alcance.
fn deny(client: &Client, scope: Scope) -> Option<Response> {
    (!client.can(scope)).then(|| fail(StatusCode::FORBIDDEN, format!("Token “{}” doesn't have scope {scope:?}", client.name)))
}

// ---------- Flujos de despliegue (workflows.rs) ----------

fn project_or_default(q: &ProjectQuery) -> String {
    q.project.clone().unwrap_or_else(|| "heskala-erp".into())
}

/// Los flujos del proyecto, cada uno con sus problemas (vacío = se puede correr), sus entornos y la
/// última corrida.
async fn get_workflows(AxState(state): AxState<Shared>, Query(q): Query<ProjectQuery>) -> Response {
    let project = project_or_default(&q);
    let list = match workflows::list(&state, &project) {
        Ok(l) => l,
        Err(e) => return fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let envs = workflows::project_envs(&state, &project);
    let names: std::collections::HashSet<String> = state.store.environments().unwrap_or_default().into_iter().map(|e| e.name).collect();
    let out: Vec<Value> = list
        .iter()
        .map(|w| {
            let last = state.store.runs(Some(&w.id), &[], 1).ok().and_then(|r| r.into_iter().next());
            json!({ "workflow": w, "problems": workflows::problems_for(&state, w, &names), "lastRun": last })
        })
        .collect();
    Json(json!({ "workflows": out, "environments": envs })).into_response()
}

async fn get_workflow_templates(AxState(state): AxState<Shared>, Query(q): Query<ProjectQuery>) -> Response {
    let project = project_or_default(&q);
    let default = crate::projects::get(&state, Some(&project)).map(|p| p.is_default()).unwrap_or(false);
    Json(json!(workflows::templates(&project, default, &workflows::project_envs(&state, &project)))).into_response()
}

#[derive(serde::Deserialize)]
struct SuggestedBody {
    project: Option<String>,
}

/// Crea, apagados, los flujos sugeridos que le faltan al proyecto.
async fn post_workflows_suggested(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Json(body): Json<SuggestedBody>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsRequest) {
        return r;
    }
    let project = match crate::projects::get(&state, body.project.as_deref()) {
        Ok(p) => p.name,
        Err(e) => return fail(StatusCode::NOT_FOUND, e),
    };
    match workflows::seed_suggested(&state, &project) {
        Ok(created) => Json(json!({ "created": created })).into_response(),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn post_workflow(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Json(body): Json<Value>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsRequest) {
        return r;
    }
    let w: workflows::Workflow = match serde_json::from_value(body) {
        Ok(w) => w,
        Err(e) => return fail(StatusCode::BAD_REQUEST, format!("invalid workflow: {e}")),
    };
    match workflows::save(&state, w) {
        Ok(w) => Json(json!(w)).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

async fn delete_workflow(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Path(id): Path<String>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsRequest) {
        return r;
    }
    match state.store.remove_workflow(&id) {
        Ok(true) => Json(json!({ "removed": id })).into_response(),
        Ok(false) => fail(StatusCode::NOT_FOUND, "that workflow doesn't exist"),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn get_workflow_export(AxState(state): AxState<Shared>, Path(id): Path<String>) -> Response {
    match workflows::get(&state, &id) {
        Ok(w) => Json(workflows::export(&w)).into_response(),
        Err(e) => fail(StatusCode::NOT_FOUND, e),
    }
}

async fn post_workflow_import(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Json(body): Json<Value>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsRequest) {
        return r;
    }
    let project = body["project"].as_str().unwrap_or("heskala-erp").to_string();
    // El JSON puede llegar como objeto o como texto pegado.
    let raw = match &body["json"] {
        Value::String(t) => match serde_json::from_str::<Value>(t) {
            Ok(v) => v,
            Err(e) => return fail(StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")),
        },
        v => v.clone(),
    };
    match workflows::import(&project, &raw).and_then(|w| workflows::save(&state, w)) {
        Ok(w) => Json(json!(w)).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

async fn post_workflow_run(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsRequest) {
        return r;
    }
    // Un evento opcional (para probar un flujo con agentes a mano): `{ "event": { … } }`.
    let event = body.get("event").filter(|e| e.is_object()).cloned();
    match workflows::start_with(&state, &id, body["commit"].as_str().unwrap_or("dev"), client.name, event) {
        Ok(run) => Json(json!(run)).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

#[derive(serde::Deserialize)]
struct RunsQuery {
    workflow: Option<String>,
    limit: Option<usize>,
}

async fn get_workflow_runs(AxState(state): AxState<Shared>, Query(q): Query<RunsQuery>) -> Response {
    match state.store.runs(q.workflow.as_deref(), &[], q.limit.unwrap_or(20).min(200)) {
        Ok(list) => Json(json!(list)).into_response(),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn get_workflow_run(AxState(state): AxState<Shared>, Path(id): Path<String>) -> Response {
    match workflows::load_run(&state, &id) {
        Ok(run) => Json(json!(run)).into_response(),
        Err(e) => fail(StatusCode::NOT_FOUND, e),
    }
}

async fn post_workflow_run_action(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Path((id, action)): Path<(String, String)>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsRequest) {
        return r;
    }
    let res = match action.as_str() {
        "cancel" => workflows::cancel(&state, &id, client.name),
        "retry" => workflows::retry(&state, &id),
        _ => return fail(StatusCode::NOT_FOUND, "unknown action (cancel, retry)"),
    };
    match res {
        Ok(run) => Json(json!(run)).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

async fn get_recipes() -> Json<Value> {
    Json(json!(recipes::all()))
}

#[derive(serde::Deserialize)]
struct JobsQuery {
    status: Option<String>,
    limit: Option<usize>,
    project: Option<String>,
}

async fn get_jobs(AxState(state): AxState<Shared>, Query(q): Query<JobsQuery>) -> Response {
    let statuses: Vec<String> = q.status.map(|s| s.split(',').map(str::to_string).collect()).unwrap_or_default();
    let refs: Vec<&str> = statuses.iter().map(String::as_str).collect();
    match crate::projects::jobs_of(&state, &refs, q.limit.unwrap_or(50).min(500), q.project.as_deref()) {
        Ok(list) => Json(json!(list.iter().map(jobs::summary).collect::<Vec<_>>())).into_response(),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn get_job(AxState(state): AxState<Shared>, Path(id): Path<String>) -> Response {
    match jobs::load(&state, &id) {
        Ok(job) => Json(json!(job)).into_response(),
        Err(e) => fail(StatusCode::NOT_FOUND, e),
    }
}

#[derive(serde::Deserialize)]
struct LogQuery {
    offset: Option<u64>,
}

async fn get_job_log(Path(id): Path<String>, Query(q): Query<LogQuery>) -> Response {
    match jobs::read_log(&id, q.offset.unwrap_or(0)) {
        Ok((text, offset)) => Json(json!({ "text": text, "offset": offset })).into_response(),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn post_job(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Json(body): Json<Value>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsRequest) {
        return r;
    }
    let recipe = body["recipe"].as_str().unwrap_or_default();
    let env = body["env"].as_str().unwrap_or_default();
    match jobs::request(&state, recipe, env, &body["args"], client.name).await {
        Ok(job) => Json(json!(job)).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

async fn post_job_action(
    AxState(state): AxState<Shared>,
    Extension(client): Extension<Client>,
    Path((id, action)): Path<(String, String)>,
) -> Response {
    if let Some(r) = deny(&client, Scope::JobsRequest) {
        return r;
    }
    let res = match action.as_str() {
        "cancel" => jobs::cancel(&state, &id, client.name),
        "retry" => jobs::retry(&state, &id, client.name),
        _ => return fail(StatusCode::NOT_FOUND, "unknown action (cancel, retry)"),
    };
    match res {
        Ok(job) => Json(json!(job)).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

#[derive(serde::Deserialize)]
struct PromotionQuery {
    env: Option<String>,
    commit: Option<String>,
}

/// Estado de las precondiciones para promover `commit` a `env` (sin pedir nada).
async fn get_promotion(AxState(state): AxState<Shared>, Query(q): Query<PromotionQuery>) -> Response {
    let env = match q.env.as_deref().map(|n| state.store.environment(n)) {
        Some(Ok(Some(e))) => e,
        _ => match state.store.environments().ok().and_then(|l| l.into_iter().find(|e| e.kind == devtool_core::EnvKind::CloudProd)) {
            Some(e) => e,
            None => return fail(StatusCode::NOT_FOUND, "no production environment registered"),
        },
    };
    let reference = q.commit.unwrap_or_else(|| "dev".into());
    let repo = match state.config.erp() {
        Ok(r) => r.to_path_buf(),
        Err(e) => return fail(StatusCode::NOT_FOUND, e),
    };
    let sha = match jobs::resolve_commit(&repo, &reference).await {
        Ok(s) => s,
        Err(e) => return fail(StatusCode::BAD_REQUEST, e),
    };
    let list = crate::pipeline::preconditions(&state, &env, &sha).await;
    Json(json!({ "env": env.name, "commit": sha, "ready": list.iter().all(|p| p["ok"] == true), "preconditions": list })).into_response()
}

#[derive(serde::Deserialize)]
struct DeploymentsQuery {
    env: String,
}

async fn get_deployments(AxState(state): AxState<Shared>, Query(q): Query<DeploymentsQuery>) -> Response {
    let list = match state.store.deployments(&q.env, 30) {
        Ok(list) => list,
        Err(e) => return fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    // Con el asunto y el autor de cada commit, del repo del proyecto del entorno.
    let repo = crate::projects::repo_for(&state, state.store.environment(&q.env).ok().flatten().as_ref());
    let mut out = vec![];
    for d in list {
        let mut v = json!(d);
        let (subject, author) = crate::gitinfo::describe(&repo, v["commit"].as_str().unwrap_or_default()).await;
        v["subject"] = json!(subject);
        v["author"] = json!(author);
        out.push(v);
    }
    Json(json!(out)).into_response()
}

/// Git del repo de un proyecto: cambios, ramas y últimos commits.
async fn get_git(AxState(state): AxState<Shared>, Query(q): Query<ProjectQuery>) -> Response {
    let project = match crate::projects::get(&state, q.project.as_deref()) {
        Ok(p) => p,
        Err(e) => return fail(StatusCode::NOT_FOUND, e),
    };
    let repo = project.repo.clone();
    let (changes, branches, commits) = tokio::join!(crate::gitinfo::changes(&repo), crate::gitinfo::branches(&repo), crate::gitinfo::commits(&repo, 40));
    Json(json!({ "project": project.name, "changes": changes, "branches": branches, "commits": commits })).into_response()
}

#[derive(serde::Deserialize)]
struct FrontendQuery {
    project: Option<String>,
    /// Sin reutilizar el informe anterior (el botón «Refrescar»).
    #[serde(default)]
    fresh: bool,
}

/// La vista Frontend: producción, previews, en curso, el último fallido con su log y si el
/// frontend y Convex coinciden (ver frontend.rs).
async fn get_frontend(AxState(state): AxState<Shared>, Query(q): Query<FrontendQuery>) -> Response {
    match crate::projects::get(&state, q.project.as_deref()) {
        Ok(p) => Json(crate::frontend::report(&state, &p, q.fresh).await).into_response(),
        Err(e) => fail(StatusCode::NOT_FOUND, e),
    }
}

#[derive(serde::Deserialize)]
struct IncidentsQuery {
    status: Option<String>,
    project: Option<String>,
    limit: Option<usize>,
}

async fn get_incidents(AxState(state): AxState<Shared>, Query(q): Query<IncidentsQuery>) -> Response {
    Json(json!(crate::incidents::list(&state, q.status.as_deref(), q.project.as_deref(), q.limit.unwrap_or(100).min(500)))).into_response()
}

/// Ignorar, resolver o reabrir es de la persona: solo la app (un agente no apaga avisos).
async fn post_incident_action(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Path((id, action)): Path<(String, String)>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    // Diagnosticar o arreglar arranca un agente (cuesta tokens): también solo la app.
    if action == "diagnose" || action == "fix" {
        return match crate::agent_host::start(&state, &id, "app", &action).await {
            Ok(v) => Json(v).into_response(),
            Err(e) => fail(StatusCode::BAD_REQUEST, e),
        };
    }
    match crate::incidents::act(&state, &id, &action) {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

/// Configuración → Agentes: proveedores (sin sus claves), autonomía y presupuestos.
async fn get_agents(AxState(state): AxState<Shared>) -> Response {
    Json(crate::agents::json(&state)).into_response()
}

async fn post_agent_settings(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Json(body): Json<Value>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    match crate::agents::save_settings(&state, &body) {
        Ok(s) => Json(json!(s)).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

/// La API key llega una vez y se guarda cifrada (agents.rs); no hay ruta que la devuelva.
async fn post_agent_provider(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Json(body): Json<crate::agents::ProviderBody>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    let st = state.clone();
    match tokio::task::spawn_blocking(move || crate::agents::upsert_provider(&st, body, &crate::agents::seal_for_mac)).await {
        Ok(Ok(p)) => {
            crate::agent_host::push_providers(&state).await;
            Json(p).into_response()
        }
        Ok(Err(e)) => fail(StatusCode::BAD_REQUEST, e),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, anyhow::anyhow!(e.to_string())),
    }
}

/// Trae los proveedores y claves de pi (agents::import_from_pi): lo pulsa la persona en la app.
async fn post_agents_import_pi(AxState(state): AxState<Shared>, Extension(client): Extension<Client>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    let st = state.clone();
    match tokio::task::spawn_blocking(move || crate::agents::import_from_pi(&st, &crate::agents::pi_dir(), &crate::agents::seal_for_mac)).await {
        Ok(Ok(v)) => {
            crate::agent_host::push_providers(&state).await;
            Json(v).into_response()
        }
        Ok(Err(e)) => fail(StatusCode::BAD_REQUEST, e),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, anyhow::anyhow!(e.to_string())),
    }
}

/// Plantillas de agente (templates.rs): solo la app las cambia.
async fn post_agent_template(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Json(body): Json<Value>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    match crate::templates::upsert(&state, &body) {
        Ok(t) => Json(json!(t)).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

async fn post_agent_templates_reset(AxState(state): AxState<Shared>, Extension(client): Extension<Client>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    match crate::templates::reset(&state) {
        Ok(l) => Json(json!(l)).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

async fn get_agent_outcomes(AxState(state): AxState<Shared>) -> Response {
    Json(crate::outcomes::json(&state)).into_response()
}

async fn get_agent_roles(AxState(state): AxState<Shared>) -> Response {
    Json(crate::roles::json(&state)).into_response()
}

/// El catálogo de agentes (roles.rs): solo la app lo cambia.
async fn post_agent_role(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Json(body): Json<Value>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    match crate::roles::upsert(&state, &body) {
        Ok(r) => Json(json!(r)).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

async fn post_agent_roles_reset(AxState(state): AxState<Shared>, Extension(client): Extension<Client>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    match crate::roles::reset(&state) {
        Ok(l) => Json(json!(l)).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

async fn delete_agent_role(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Path(id): Path<String>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    match crate::roles::delete(&state, &id) {
        Ok(()) => Json(json!({ "deleted": id })).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

async fn delete_agent_template(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Path(id): Path<String>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    match crate::templates::delete(&state, &id) {
        Ok(()) => Json(json!({ "deleted": id })).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

async fn delete_agent_provider(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Path(id): Path<String>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    match crate::agents::delete_provider(&state, &id) {
        Ok(()) => {
            crate::agent_host::push_providers(&state).await;
            Json(json!({ "deleted": id })).into_response()
        }
        Err(e) => fail(StatusCode::NOT_FOUND, e),
    }
}

/// Notificaciones con ntfy (notify.rs): la configuración, sin el token.
/// Las actualizaciones de Condx: la versión de este daemon, la última revisada y la ofrecida.
async fn get_update(AxState(state): AxState<Shared>) -> Response {
    Json(crate::update::status(&state)).into_response()
}

/// Revisa ya (y si hay una versión nueva, pide la aprobación para instalarla).
async fn post_update_check(AxState(state): AxState<Shared>, Extension(client): Extension<Client>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    match crate::update::check(&state).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(StatusCode::BAD_GATEWAY, e),
    }
}

/// El correo de respaldo (AgentMail): ajustes y si hay key (nunca su valor).
async fn get_mail(AxState(state): AxState<Shared>) -> Response {
    let sent: Vec<Value> = crate::mail::sent(&state).into_iter().take(20).map(|m| json!({ "at": m["at"], "subject": m["subject"], "via": m["via"] })).collect();
    Json(json!({ "settings": crate::mail::settings(&state), "hasKey": crate::secrets::exists(crate::mail::KEY_ACCOUNT), "hasResendKey": crate::secrets::exists(crate::mail::RESEND_ACCOUNT), "sent": sent })).into_response()
}

async fn post_mail(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Json(body): Json<Value>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    let s: crate::mail::Settings = match serde_json::from_value(body) {
        Ok(s) => s,
        Err(e) => return fail(StatusCode::BAD_REQUEST, anyhow::anyhow!(e.to_string())),
    };
    match crate::mail::save(&state, &s) {
        Ok(()) => Json(json!(crate::mail::settings(&state))).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

async fn post_mail_test(AxState(state): AxState<Shared>, Extension(client): Extension<Client>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    let st = state.clone();
    match tokio::task::spawn_blocking(move || crate::mail::send_test(&st)).await {
        Ok(Ok(via)) => Json(json!({ "sent": true, "via": via })).into_response(),
        Ok(Err(e)) => fail(StatusCode::BAD_REQUEST, e),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, anyhow::anyhow!(e.to_string())),
    }
}

async fn get_notify(AxState(state): AxState<Shared>) -> Response {
    let st = state.clone();
    match tokio::task::spawn_blocking(move || crate::notify::json(&st)).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, anyhow::anyhow!(e.to_string())),
    }
}

async fn post_notify(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Json(body): Json<Value>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    let st = state.clone();
    match tokio::task::spawn_blocking(move || crate::notify::save(&st, &body)).await {
        Ok(Ok(s)) => Json(json!(s)).into_response(),
        Ok(Err(e)) => fail(StatusCode::BAD_REQUEST, e),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, anyhow::anyhow!(e.to_string())),
    }
}

/// Una notificación de prueba, ahora (y el error si no llega).
async fn post_notify_test(AxState(state): AxState<Shared>, Extension(client): Extension<Client>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    let s = crate::notify::settings(&state);
    let res = tokio::task::spawn_blocking(move || {
        let token = crate::secrets::get(crate::notify::TOKEN_ACCOUNT).ok().flatten();
        crate::notify::send_to(&s, token.as_deref(), &crate::notify::Message { title: "Condx: test notification".into(), body: "Notifications from this Mac reach this phone.".into(), tags: "white_check_mark", priority: 3 })
    })
    .await;
    match res {
        Ok(Ok(())) => Json(json!({ "sent": true })).into_response(),
        Ok(Err(e)) => fail(StatusCode::BAD_REQUEST, e),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, anyhow::anyhow!(e.to_string())),
    }
}

/// Una corrida de agente con su transcripción (si el host la tiene).
/// Una tarea para los agentes (la empiezan los flujos con disparador «task»): solo la app.
async fn post_task(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Json(body): Json<Value>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    let text = body["text"].as_str().unwrap_or_default().to_string();
    let project = body["project"].as_str().map(str::to_string);
    let res = tokio::task::spawn_blocking(move || crate::events::from_task(&text, project.as_deref()).and_then(|ev| crate::events::record(&state, ev))).await;
    match res {
        Ok(Ok(ev)) if ev["status"] == "unmatched" => fail(StatusCode::BAD_REQUEST, "no flow starts on tasks: create one in Workflows (the suggested “Do a task”) and turn it on"),
        Ok(Ok(ev)) => Json(ev).into_response(),
        Ok(Err(e)) => fail(StatusCode::BAD_REQUEST, e),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

// ---------- Agentes externos por el MCP (connections.rs) ----------

async fn get_connections(Extension(client): Extension<Client>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    Json(json!({ "connections": crate::connections::list().iter().map(|c| c.json()).collect::<Vec<_>>() })).into_response()
}

/// Una conexión nueva: el token se ve solo en esta respuesta, con lo que hay que pegar en el agente.
async fn post_connection(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Json(body): Json<Value>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    let (name, kind, level) = (body["name"].as_str().unwrap_or_default(), body["kind"].as_str().unwrap_or("other"), body["level"].as_str().unwrap_or("reply"));
    match crate::connections::create(&state, name, kind, level, body["answerQuestions"].as_bool(), body["editFlows"].as_bool().unwrap_or(false)) {
        Ok((c, token)) => {
            let url = format!("http://127.0.0.1:{}/mcp", state.config.port);
            let mut v = c.json();
            v["token"] = json!(token);
            v["url"] = json!(url);
            v["claudeCode"] = json!(format!("claude mcp add --transport http condx {url} --header \"Authorization: Bearer {token}\""));
            v["mcpJson"] = json!({ "mcpServers": { "condx": { "type": "http", "url": url, "headers": { "Authorization": format!("Bearer {token}") } } } });
            Json(v).into_response()
        }
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

async fn delete_connection(Extension(client): Extension<Client>, Path(id): Path<String>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    match crate::connections::revoke(&id) {
        Ok(()) => Json(json!({ "revoked": id })).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

async fn get_sdk() -> Response {
    match tokio::task::spawn_blocking(crate::cloud::sdk_keys).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => fail(StatusCode::BAD_REQUEST, e),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Una llave nueva del SDK para un proyecto: solo la app (la secreta se muestra una vez).
async fn post_sdk_key(Extension(client): Extension<Client>, Json(body): Json<Value>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    let project = body["project"].as_str().unwrap_or_default().to_string();
    match tokio::task::spawn_blocking(move || crate::cloud::sdk_create_key(&project)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => fail(StatusCode::BAD_REQUEST, e),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn delete_sdk_key(Extension(client): Extension<Client>, Path(key): Path<String>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    match tokio::task::spawn_blocking(move || crate::cloud::sdk_revoke_key(&key)).await {
        Ok(Ok(ok)) => Json(json!({ "revoked": ok })).into_response(),
        Ok(Err(e)) => fail(StatusCode::BAD_REQUEST, e),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn get_threads(AxState(state): AxState<Shared>) -> Response {
    match crate::threads::list(&state) {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn get_thread(AxState(state): AxState<Shared>, Path(id): Path<String>) -> Response {
    match crate::threads::get(&state, &id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(StatusCode::NOT_FOUND, e),
    }
}

/// Escribir en un hilo (a un agente, a la pregunta que espera, o al Explicador): solo la app.
async fn post_thread_message(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    let text = body["text"].as_str().unwrap_or_default().chars().take(4_000).collect::<String>();
    match crate::threads::message(&state, &id, &text).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

async fn get_agent_runs(AxState(state): AxState<Shared>) -> Response {
    match crate::agent_host::list(&state) {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Un chat nuevo con el agente: solo la app (como escribirle a una corrida).
async fn post_agent_chat(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Json(body): Json<Value>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    let text = body["text"].as_str().unwrap_or_default().chars().take(4_000).collect::<String>();
    match crate::agent_host::start_chat(&state, &text, body["project"].as_str(), "app").await {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

async fn get_agent_run(AxState(state): AxState<Shared>, Path(id): Path<String>) -> Response {
    match crate::agent_host::get(&state, &id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(StatusCode::NOT_FOUND, e),
    }
}

/// Escribirle al agente o detenerlo: solo la app.
async fn post_agent_run_action(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Path((id, action)): Path<(String, String)>, body: Option<Json<Value>>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    let res = match action.as_str() {
        "message" => {
            let text = body.as_ref().and_then(|b| b["text"].as_str()).unwrap_or_default().chars().take(4_000).collect::<String>();
            crate::agent_host::message(&state, &id, &text, &[]).await
        }
        "abort" => crate::agent_host::abort(&state, &id).await,
        "discard" => crate::agent_host::discard(&state, &id),
        other => Err(anyhow::anyhow!("unknown action “{other}”: message, abort or discard")),
    };
    match res {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

#[derive(serde::Deserialize)]
struct ProjectQuery {
    project: Option<String>,
}

async fn get_services(AxState(state): AxState<Shared>, Query(q): Query<ProjectQuery>) -> Response {
    match crate::projects::get(&state, q.project.as_deref()) {
        Ok(p) => Json(json!(crate::services::list(&state, &p).await)).into_response(),
        Err(e) => fail(StatusCode::NOT_FOUND, e),
    }
}

async fn post_service(
    AxState(state): AxState<Shared>,
    Extension(client): Extension<Client>,
    Path((name, action)): Path<(String, String)>,
    Query(q): Query<ProjectQuery>,
) -> Response {
    if let Some(r) = deny(&client, Scope::JobsRequest) {
        return r;
    }
    let res = crate::projects::get(&state, q.project.as_deref()).and_then(|p| crate::services::action(&state, &p, &name, &action));
    match res {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

#[derive(serde::Deserialize)]
struct TailQuery {
    tail: Option<usize>,
    project: Option<String>,
}

async fn get_service_logs(AxState(state): AxState<Shared>, Path(name): Path<String>, Query(q): Query<TailQuery>) -> Response {
    let project = match crate::projects::get(&state, q.project.as_deref()) {
        Ok(p) => p,
        Err(e) => return fail(StatusCode::NOT_FOUND, e),
    };
    match crate::services::logs(&state, &project, &name, q.tail.unwrap_or(400)).await {
        Ok(text) => Json(json!({ "text": text })).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

async fn get_policy(AxState(state): AxState<Shared>) -> Response {
    Json(crate::policy::json(&crate::policy::load(&state))).into_response()
}

/// Solo la app (alcance de aprobar) cambia las reglas de datos: un agente no puede aflojarlas.
async fn post_policy(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Json(body): Json<Value>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    match crate::policy::save(&state, &body) {
        Ok(p) => Json(crate::policy::json(&p)).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

async fn get_tables(AxState(state): AxState<Shared>, Path(name): Path<String>) -> Response {
    let env = match state.store.environment(&name) {
        Ok(Some(e)) => e,
        _ => return fail(StatusCode::NOT_FOUND, format!("Environment “{name}” doesn't exist")),
    };
    match crate::data::tables(&state, &env).await {
        Ok(mut v) => {
            v["environment"] = json!(name);
            Json(v).into_response()
        }
        Err(e) => fail(StatusCode::BAD_GATEWAY, format!("{e:#}")),
    }
}

#[derive(serde::Deserialize)]
struct ApprovalQuery {
    recipe: String,
    env: String,
    function: Option<String>,
}

/// Lo que va a pedir un trabajo antes de pedirlo: su aprobación (y por qué) y con qué
/// credencial corre la CLI. El formulario de Trabajos lo muestra mientras se llena.
async fn get_approval(AxState(state): AxState<Shared>, Query(q): Query<ApprovalQuery>) -> Response {
    let Some(recipe) = crate::recipes::get(&q.recipe) else {
        return fail(StatusCode::NOT_FOUND, format!("Unknown recipe: {}", q.recipe));
    };
    let env = match state.store.environment(&q.env) {
        Ok(Some(e)) => e,
        _ => return fail(StatusCode::NOT_FOUND, format!("Environment “{}” doesn't exist", q.env)),
    };
    let mut args = json!({});
    if let (Some(f), Some(a)) = (&q.function, recipe.args.iter().find(|a| a.kind == crate::recipes::ArgType::Function)) {
        args[a.name.as_str()] = json!(f);
    }
    let (approval, why) = crate::jobs::approval_for(&state, recipe, &env, &args).await;
    let credential = crate::probes::convex::credential_kind(&env, recipe.deploy_key);
    Json(json!({ "approval": approval, "why": why, "credential": credential })).into_response()
}

/// Las claves de un entorno: si tiene deploy key y de qué deployment (nunca su valor).
async fn get_keys(AxState(state): AxState<Shared>, Path(name): Path<String>) -> Response {
    let env = match state.store.environment(&name) {
        Ok(Some(e)) => e,
        _ => return fail(StatusCode::NOT_FOUND, format!("Environment “{name}” doesn't exist")),
    };
    let local = matches!(env.kind, devtool_core::EnvKind::Devbox | devtool_core::EnvKind::Throwaway);
    if local {
        return Json(json!({ "local": true, "deployKey": null })).into_response();
    }
    let own = crate::secrets::deploy_account(&env);
    let key = match crate::secrets::deploy_key(&env) {
        Ok(k) => k,
        Err(e) => return fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let info = match key {
        None => json!({ "stored": false, "account": own }),
        Some((account, key)) => json!({
            "stored": true,
            "account": account,
            "shared": account != own,
            "deployment": key.split('|').next(),
            "valid": crate::secrets::check_deploy_key(&env, &key).err().map(|e| e.to_string()).is_none(),
            "problem": crate::secrets::check_deploy_key(&env, &key).err().map(|e| e.to_string()),
        }),
    };
    Json(json!({ "local": false, "deployKey": info, "deployment": env.convex_deployment })).into_response()
}

/// Guarda la deploy key de un entorno cloud en el Llavero. Solo la app (alcance de aprobar):
/// un agente por el MCP no puede ni guardarla ni borrarla.
async fn post_keys(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Path(name): Path<String>, Json(body): Json<Value>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    let env = match state.store.environment(&name) {
        Ok(Some(e)) if !matches!(e.kind, devtool_core::EnvKind::Devbox | devtool_core::EnvKind::Throwaway) => e,
        Ok(Some(_)) => return fail(StatusCode::BAD_REQUEST, "local environments use the admin key of their .env file"),
        _ => return fail(StatusCode::NOT_FOUND, format!("Environment “{name}” doesn't exist")),
    };
    let key = body["deployKey"].as_str().unwrap_or_default().trim().to_string();
    let deployment = match crate::secrets::check_deploy_key(&env, &key) {
        Ok(d) => d,
        Err(e) => return fail(StatusCode::BAD_REQUEST, e),
    };
    let account = crate::secrets::deploy_account(&env);
    if let Err(e) = crate::secrets::set(&account, &key) {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    tracing::info!("deploy key saved for {} (deployment {deployment}, account {account})", env.name);
    Json(json!({ "ok": true, "deployment": deployment })).into_response()
}

/// Borra la deploy key propia del entorno (no las generales de `condxd secret`).
async fn delete_keys(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Path(name): Path<String>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    let env = match state.store.environment(&name) {
        Ok(Some(e)) => e,
        _ => return fail(StatusCode::NOT_FOUND, format!("Environment “{name}” doesn't exist")),
    };
    let account = crate::secrets::deploy_account(&env);
    match crate::secrets::delete(&account) {
        Ok(()) => {
            tracing::info!("deploy key deleted for {} (account {account})", env.name);
            Json(json!({ "ok": true })).into_response()
        }
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn get_first_table(AxState(state): AxState<Shared>, Path(name): Path<String>) -> Response {
    let env = match state.store.environment(&name) {
        Ok(Some(e)) => e,
        _ => return fail(StatusCode::NOT_FOUND, format!("Environment “{name}” doesn't exist")),
    };
    match crate::data::first_with_documents(&state, &env).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(StatusCode::BAD_GATEWAY, format!("{e:#}")),
    }
}

#[derive(serde::Deserialize)]
struct DocumentsQuery {
    table: String,
    cursor: Option<String>,
    limit: Option<usize>,
}

async fn get_documents(AxState(state): AxState<Shared>, Path(name): Path<String>, Query(q): Query<DocumentsQuery>) -> Response {
    let env = match state.store.environment(&name) {
        Ok(Some(e)) => e,
        _ => return fail(StatusCode::NOT_FOUND, format!("Environment “{name}” doesn't exist")),
    };
    match crate::data::documents(&state, &env, &q.table, q.cursor.filter(|c| !c.is_empty()), q.limit.unwrap_or(200)).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(StatusCode::BAD_GATEWAY, format!("{e:#}")),
    }
}

#[derive(serde::Deserialize)]
struct DiffQuery {
    from: String,
    to: String,
}

/// Qué cambiaría el esquema de `from` (normalmente el local) al desplegarse en `to`.
async fn get_schema_diff(AxState(state): AxState<Shared>, Query(q): Query<DiffQuery>) -> Response {
    let env = |name: &str| state.store.environment(name).ok().flatten().ok_or_else(|| format!("environment {name} doesn't exist"));
    let (from, to) = match (env(&q.from), env(&q.to)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return fail(StatusCode::NOT_FOUND, e),
    };
    match crate::schema::diff_envs(&state, &from, &to).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(StatusCode::BAD_GATEWAY, format!("{e:#}")),
    }
}

async fn get_schema(AxState(state): AxState<Shared>, Path(name): Path<String>) -> Response {
    let env = match state.store.environment(&name) {
        Ok(Some(e)) => e,
        _ => return fail(StatusCode::NOT_FOUND, format!("Environment “{name}” doesn't exist")),
    };
    match crate::schema::schema(&state, &env).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(StatusCode::BAD_GATEWAY, format!("{e:#}")),
    }
}

#[derive(serde::Deserialize)]
struct FunctionsQuery {
    #[serde(default)]
    fresh: bool,
}

/// Funciones del entorno para el selector de `run-function` (`?fresh=true` salta el caché).
async fn get_functions(AxState(state): AxState<Shared>, Path(name): Path<String>, Query(q): Query<FunctionsQuery>) -> Response {
    let env = match state.store.environment(&name) {
        Ok(Some(e)) => e,
        _ => return fail(StatusCode::NOT_FOUND, format!("Environment “{name}” doesn't exist")),
    };
    match crate::functions::functions(&state, &env, q.fresh).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(StatusCode::BAD_GATEWAY, format!("{e:#}")),
    }
}

async fn get_projects(AxState(state): AxState<Shared>) -> Response {
    match crate::projects::list_json(&state) {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(serde::Deserialize)]
struct RegisterBody {
    path: String,
    name: Option<String>,
}

async fn post_project(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Json(body): Json<RegisterBody>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsRequest) {
        return r;
    }
    match crate::projects::register(&state, std::path::Path::new(&crate::projects::expand(&body.path)), body.name.as_deref()) {
        Ok(p) => Json(crate::projects::to_json(&p)).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

async fn delete_project(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Path(name): Path<String>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsRequest) {
        return r;
    }
    match crate::projects::remove(&state, &name).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

#[derive(serde::Deserialize)]
struct DiscoverQuery {
    root: Option<String>,
}

async fn get_discover(AxState(state): AxState<Shared>, Query(q): Query<DiscoverQuery>) -> Json<Value> {
    Json(crate::projects::discover_json(&state, q.root.as_deref()))
}

/// Condx Cloud: el vínculo (sin el token) y el estado del último sincronizado.
async fn get_cloud(AxState(state): AxState<Shared>) -> Json<Value> {
    Json(json!({ "link": crate::cloud::load_link(), "status": state.probe("cloud") }))
}

#[derive(serde::Deserialize)]
struct CloudLinkBody {
    url: String,
    key: String,
    name: Option<String>,
}

/// Vincular o desvincular: solo la app (la key de admin pasa por aquí una vez y no se guarda).
async fn post_cloud_link(Extension(client): Extension<Client>, Json(body): Json<CloudLinkBody>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    let name = body.name.unwrap_or_else(|| "Condx".into());
    match tokio::task::spawn_blocking(move || crate::cloud::link(&body.url, &body.key, &name)).await {
        Ok(Ok(link)) => Json(json!({ "link": link })).into_response(),
        Ok(Err(e)) => fail(StatusCode::BAD_REQUEST, format!("{e:#}")),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn post_cloud_unlink(Extension(client): Extension<Client>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    match crate::cloud::unlink() {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    }
}

/// Vincular teléfonos: solo la app (un agente nunca abre ni confirma una vinculación).
async fn cloud_app_only<T: serde::Serialize + Send + 'static>(client: &Client, f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Response {
    if let Some(r) = deny(client, Scope::JobsApprove) {
        return r;
    }
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(v)) => Json(json!(v)).into_response(),
        Ok(Err(e)) => fail(StatusCode::BAD_REQUEST, format!("{e:#}")),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn post_cloud_pair(Extension(client): Extension<Client>) -> Response {
    cloud_app_only(&client, crate::cloud::pair_start).await
}

async fn get_cloud_pair(Extension(client): Extension<Client>) -> Response {
    cloud_app_only(&client, crate::cloud::pair_status).await
}

#[derive(serde::Deserialize)]
struct PairConfirm {
    #[serde(rename = "deviceId")]
    device_id: String,
}

async fn post_cloud_pair_confirm(Extension(client): Extension<Client>, Json(body): Json<PairConfirm>) -> Response {
    cloud_app_only(&client, move || crate::cloud::pair_confirm(&body.device_id)).await
}

async fn post_cloud_pair_cancel(Extension(client): Extension<Client>) -> Response {
    cloud_app_only(&client, || crate::cloud::pair_cancel().map(|_| json!({ "ok": true }))).await
}

/// Los teléfonos de confianza de esta Mac (nombre y huella; la llave es pública).
async fn get_cloud_devices() -> Json<Value> {
    Json(json!(crate::cloud::trusted_devices().into_iter().map(|d| json!({ "deviceId": d.device_id, "name": d.name, "fingerprint": crate::cloud::fingerprint(&d.public_key), "pairedAt": d.paired_at })).collect::<Vec<_>>()))
}

async fn delete_cloud_device(Extension(client): Extension<Client>, Path(id): Path<String>) -> Response {
    cloud_app_only(&client, move || crate::cloud::revoke_device(&id).map(|_| json!({ "ok": true }))).await
}

async fn get_doctor(AxState(state): AxState<Shared>) -> Json<Value> {
    Json(crate::doctor::report(&state).await)
}

/// Baja un ejecutable a esta Mac: solo la app (como aprobar), nunca un agente por el MCP.
async fn post_doctor_backend(Extension(client): Extension<Client>) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    match crate::doctor::download_backend().await {
        Ok(v) => Json(v).into_response(),
        Err(e) => fail(StatusCode::BAD_GATEWAY, e),
    }
}

async fn get_secrets() -> Json<Value> {
    Json(json!({
        "prodDeployKey": crate::secrets::exists(crate::secrets::PROD_DEPLOY_KEY),
        "backupKey": crate::secrets::exists(crate::secrets::BACKUP_KEY),
    }))
}

async fn get_backups(AxState(state): AxState<Shared>) -> Response {
    match state.store.backups(None, 50) {
        Ok(list) => Json(json!(list)).into_response(),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn get_approvals(AxState(state): AxState<Shared>) -> Response {
    match state.store.jobs(&["waiting-approval"], 100) {
        Ok(list) => Json(json!(list)).into_response(),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn post_approval(
    AxState(state): AxState<Shared>,
    Extension(client): Extension<Client>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Some(r) = deny(&client, Scope::JobsApprove) {
        return r;
    }
    let approve = body["approve"].as_bool().unwrap_or(false);
    let touch_id = body["touchId"].as_bool().unwrap_or(false);
    match jobs::decide(&state, &id, approve, body["confirm"].as_str(), touch_id, client.name) {
        Ok(job) => Json(json!(job)).into_response(),
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}

async fn health(AxState(state): AxState<Shared>) -> Json<Value> {
    Json(json!({ "ok": true, "name": "condx", "version": env!("CARGO_PKG_VERSION"), "startedAt": state.started_at }))
}

async fn get_overview(AxState(state): AxState<Shared>, Query(q): Query<ProjectQuery>) -> Json<Value> {
    Json(overview::build_for(&state, q.project.as_deref()).await)
}

async fn get_probes(AxState(state): AxState<Shared>) -> Json<Value> {
    Json(json!(state.probes()))
}

async fn get_probe(AxState(state): AxState<Shared>, Path(key): Path<String>) -> Response {
    match state.probe(&key) {
        Some(p) => Json(p).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({ "error": format!("Unknown probe “{key}”") }))).into_response(),
    }
}

async fn get_tabs(AxState(state): AxState<Shared>) -> Json<Value> {
    Json(json!(state.hub.tabs()))
}

async fn post_tool(
    AxState(state): AxState<Shared>,
    Extension(client): Extension<Client>,
    Path(name): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let args = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    match mcp::call(&state, &client, client.name, &name, &args).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

/// Para la tarjeta «Conectar un agente»: el comando del puente stdio, la URL HTTP y el último uso.
async fn get_mcp_status(AxState(state): AxState<Shared>) -> Response {
    let mut v = mcp::last_use();
    v["command"] = json!(crate::config::home().join("bin").join("condxd").display().to_string());
    v["url"] = json!(format!("http://127.0.0.1:{}/mcp", state.config.port));
    v["tokenFile"] = json!(crate::config::home().join("token").display().to_string());
    Json(v).into_response()
}

async fn post_mcp(AxState(state): AxState<Shared>, Extension(client): Extension<Client>, Json(msg): Json<Value>) -> Response {
    let reply = if let Some(batch) = msg.as_array() {
        let mut out = vec![];
        for m in batch {
            if let Some(r) = mcp::handle(&state, &client, m).await {
                out.push(r);
            }
        }
        (!out.is_empty()).then(|| json!(out))
    } else {
        mcp::handle(&state, &client, &msg).await
    };
    match reply {
        Some(r) => Json(r).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

async fn telemetry(AxState(state): AxState<Shared>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let origin = headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()).map(str::to_string);
    match &origin {
        Some(o) if origin_allowed(o, &state.config.telemetry_origins) => {}
        _ => return (StatusCode::FORBIDDEN, "Origin not allowed for telemetry").into_response(),
    }
    ws.max_message_size(2_000_000)
        .on_upgrade(move |socket| async move { state.hub.serve(socket, origin).await })
}

pub fn router(state: Shared) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/overview", get(get_overview))
        .route("/api/probes", get(get_probes))
        .route("/api/probes/{key}", get(get_probe))
        .route("/api/tabs", get(get_tabs))
        .route("/api/tools/{name}", post(post_tool))
        .route("/api/recipes", get(get_recipes))
        .route("/api/jobs", get(get_jobs).post(post_job))
        .route("/api/jobs/{id}", get(get_job))
        .route("/api/jobs/{id}/log", get(get_job_log))
        .route("/api/jobs/{id}/{action}", post(post_job_action))
        .route("/api/approvals", get(get_approvals))
        .route("/api/promotion", get(get_promotion))
        .route("/api/workflows", get(get_workflows).post(post_workflow))
        .route("/api/workflows/templates", get(get_workflow_templates))
        .route("/api/workflows/suggested", post(post_workflows_suggested))
        .route("/api/workflows/import", post(post_workflow_import))
        .route("/api/workflows/{id}", axum::routing::delete(delete_workflow))
        .route("/api/workflows/{id}/export", get(get_workflow_export))
        .route("/api/workflows/{id}/run", post(post_workflow_run))
        .route("/api/workflow-runs", get(get_workflow_runs))
        .route("/api/workflow-runs/{id}", get(get_workflow_run))
        .route("/api/workflow-runs/{id}/{action}", post(post_workflow_run_action))
        .route("/api/secrets", get(get_secrets))
        .route("/api/deployments", get(get_deployments))
        .route("/api/environments/{name}/schema", get(get_schema))
        .route("/api/schema-diff", get(get_schema_diff))
        .route("/api/environments/{name}/tables", get(get_tables))
        .route("/api/policy", get(get_policy).post(post_policy))
        .route("/api/environments/{name}/documents", get(get_documents))
        .route("/api/environments/{name}/first-table", get(get_first_table))
        .route("/api/approval", get(get_approval))
        .route("/api/environments/{name}/keys", get(get_keys).post(post_keys).delete(delete_keys))
        .route("/api/environments/{name}/functions", get(get_functions))
        .route("/api/git", get(get_git))
        .route("/api/frontend", get(get_frontend))
        .route("/api/incidents", get(get_incidents))
        .route("/api/incidents/{id}/{action}", post(post_incident_action))
        .route("/api/agents", get(get_agents))
        .route("/api/agents/settings", post(post_agent_settings))
        .route("/api/agents/providers", post(post_agent_provider))
        .route("/api/agents/providers/{id}", delete(delete_agent_provider))
        .route("/api/agents/import-pi", post(post_agents_import_pi))
        .route("/api/agents/templates", post(post_agent_template))
        .route("/api/agents/templates/reset", post(post_agent_templates_reset))
        .route("/api/agents/templates/{id}", delete(delete_agent_template))
        .route("/api/agents/roles", get(get_agent_roles).post(post_agent_role))
        .route("/api/agents/outcomes", get(get_agent_outcomes))
        .route("/api/agents/roles/reset", post(post_agent_roles_reset))
        .route("/api/agents/roles/{id}", delete(delete_agent_role))
        .route("/api/agent-runs", get(get_agent_runs))
        .route("/api/threads", get(get_threads))
        .route("/api/tasks", post(post_task))
        .route("/api/sdk", get(get_sdk))
        .route("/api/sdk/keys", post(post_sdk_key))
        .route("/api/sdk/keys/{key}", delete(delete_sdk_key))
        .route("/api/threads/{id}", get(get_thread))
        .route("/api/threads/{id}/message", post(post_thread_message))
        .route("/api/agent-chats", post(post_agent_chat))
        .route("/api/agent-runs/{id}", get(get_agent_run))
        .route("/api/notify", get(get_notify).post(post_notify))
        .route("/api/mail", get(get_mail).post(post_mail))
        .route("/api/mail/test", post(post_mail_test))
        .route("/api/update", get(get_update))
        .route("/api/update/check", post(post_update_check))
        .route("/api/notify/test", post(post_notify_test))
        .route("/api/agent-runs/{id}/{action}", post(post_agent_run_action))
        .route("/api/projects", get(get_projects).post(post_project))
        .route("/api/projects/discover", get(get_discover))
        .route("/api/doctor", get(get_doctor))
        .route("/api/connections", get(get_connections).post(post_connection))
        .route("/api/connections/{id}", axum::routing::delete(delete_connection))
        .route("/api/cloud", get(get_cloud))
        .route("/api/cloud/link", post(post_cloud_link))
        .route("/api/cloud/unlink", post(post_cloud_unlink))
        .route("/api/cloud/pair", get(get_cloud_pair).post(post_cloud_pair))
        .route("/api/cloud/pair/confirm", post(post_cloud_pair_confirm))
        .route("/api/cloud/pair/cancel", post(post_cloud_pair_cancel))
        .route("/api/cloud/devices", get(get_cloud_devices))
        .route("/api/cloud/devices/{id}", axum::routing::delete(delete_cloud_device))
        .route("/api/doctor/backend", post(post_doctor_backend))
        .route("/api/projects/{name}", axum::routing::delete(delete_project))
        .route("/api/services", get(get_services))
        .route("/api/services/{name}/logs", get(get_service_logs))
        .route("/api/services/{name}/{action}", post(post_service))
        .route("/api/backups", get(get_backups))
        .route("/api/approvals/{id}", post(post_approval))
        .route("/api/mcp", get(get_mcp_status))
        .route("/mcp", post(post_mcp).get(|| async { StatusCode::METHOD_NOT_ALLOWED }))
        .route("/telemetry", get(telemetry))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origenes_de_telemetria() {
        assert!(origin_allowed("http://localhost:3000", &[]));
        assert!(origin_allowed("http://127.0.0.1:5173", &[]));
        assert!(!origin_allowed("http://localhost.evil.com", &[]));
        assert!(!origin_allowed("https://evil.com", &[]));
        assert!(origin_allowed("https://app.example.com", &["https://app.example.com".into()]));
    }
}
