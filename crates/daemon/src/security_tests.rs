// Pruebas de seguridad del daemon contra el router HTTP real (sin abrir puertos): quién puede
// hablarle, qué puede hacer cada token y qué nunca sale en claro. El modelo de amenaza está en
// auth.rs: un agente por el MCP (token `cli`) puede mirar y pedir trabajos, pero no aprobar, ni
// cambiar las reglas de datos, ni guardar o borrar deploy keys, ni bajar ejecutables.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::auth::Tokens;
use crate::config::Config;
use crate::state::State;
use crate::store::Store;

const CLI: &str = "cli-token-for-tests";
const APP: &str = "app-token-for-tests";
const PORT: u16 = 4790;

fn router() -> axum::Router {
    let config = Config { repo: None, port: PORT, telemetry_origins: vec![] };
    let state = Arc::new(State::new(config, Store::memory(), Tokens::fixed(CLI, APP)));
    crate::http::router(state)
}

async fn call(method: Method, path: &str, host: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path).header(header::HOST, host);
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = match body {
        Some(b) => req.header(header::CONTENT_TYPE, "application/json").body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = router().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn local() -> String {
    format!("127.0.0.1:{PORT}")
}

#[tokio::test]
async fn solo_nombres_de_loopback() {
    // DNS rebinding: una página que apunta su dominio a 127.0.0.1 manda su propio Host.
    for host in ["evil.example", "127.0.0.1.evil.example", "localhost", &format!("127.0.0.1:{}", PORT + 1), "0.0.0.0:4790"] {
        let (status, _) = call(Method::GET, "/health", host, None, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{host}");
    }
    for host in [local(), format!("localhost:{PORT}"), format!("[::1]:{PORT}")] {
        let (status, _) = call(Method::GET, "/health", &host, None, None).await;
        assert_eq!(status, StatusCode::OK, "{host}");
    }
}

#[tokio::test]
async fn sin_token_no_hay_api() {
    for token in [None, Some(""), Some("wrong"), Some("cli-token-for-test")] {
        let (status, _) = call(Method::GET, "/api/overview", &local(), token, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{token:?}");
        let (status, _) = call(Method::POST, "/mcp", &local(), token, Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{token:?}");
    }
}

#[tokio::test]
async fn el_token_de_agentes_no_aprueba_ni_toca_reglas_ni_claves() {
    let host = local();
    let cases = [
        (Method::POST, "/api/approvals/j123", json!({ "approve": true })),
        (Method::POST, "/api/policy", json!({ "read": { "cloud-prod": true }, "write": {} })),
        (Method::POST, "/api/environments/prod/keys", json!({ "deployKey": "prod:x|y" })),
        (Method::DELETE, "/api/environments/prod/keys", json!({})),
        (Method::POST, "/api/doctor/backend", json!({})),
        (Method::POST, "/api/cloud/link", json!({ "url": "https://x.convex.cloud", "key": "k".repeat(40) })),
        (Method::POST, "/api/cloud/unlink", json!({})),
        (Method::POST, "/api/cloud/pair", json!({})),
        (Method::POST, "/api/cloud/pair/confirm", json!({ "deviceId": "phone-x" })),
        (Method::POST, "/api/cloud/pair/cancel", json!({})),
        (Method::DELETE, "/api/cloud/devices/phone-x", json!({})),
        // Un agente no apaga avisos ni toca proveedores, claves o su propia autonomía.
        (Method::POST, "/api/incidents/inc-x/ignore", json!({})),
        (Method::POST, "/api/incidents/inc-x/resolve", json!({})),
        (Method::POST, "/api/agents/settings", json!({ "autonomy": { "job": "fix" } })),
        (Method::POST, "/api/agents/providers", json!({ "kind": "anthropic", "apiKey": "sk-x" })),
        (Method::DELETE, "/api/agents/providers/anthropic", json!({})),
        (Method::POST, "/api/agents/import-pi", json!({})),
        (Method::POST, "/api/notify", json!({ "enabled": true, "topic": "condx-x" })),
        (Method::POST, "/api/notify/test", json!({})),
        (Method::POST, "/api/agents/templates", json!({ "name": "x" })),
        (Method::POST, "/api/agents/templates/reset", json!({})),
        (Method::DELETE, "/api/agents/templates/generic", json!({})),
        // Arrancar un agente, escribirle o detenerlo cuesta tokens: solo la app.
        (Method::POST, "/api/incidents/inc-x/diagnose", json!({})),
        (Method::POST, "/api/agent-runs/run-x/message", json!({ "text": "hi" })),
        (Method::POST, "/api/agent-runs/run-x/abort", json!({})),
        (Method::POST, "/api/agent-runs/run-x/discard", json!({})),
        (Method::POST, "/api/incidents/inc-x/fix", json!({})),
        (Method::POST, "/api/agent-chats", json!({ "text": "hi" })),
        (Method::POST, "/api/threads/r1/message", json!({ "text": "hi" })),
        (Method::POST, "/api/sdk/keys", json!({ "project": "shop" })),
        (Method::POST, "/api/tasks", json!({ "text": "add a field" })),
        (Method::DELETE, "/api/sdk/keys/ck_pub_x", json!({})),
        (Method::POST, "/api/agents/roles", json!({ "name": "x" })),
        (Method::POST, "/api/agents/roles/reset", json!({})),
        (Method::DELETE, "/api/agents/roles/triage", json!({})),
    ];
    for (method, path, body) in cases {
        let (status, res) = call(method.clone(), path, &host, Some(CLI), Some(body)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {res}");
        assert!(res["error"].as_str().unwrap_or_default().contains("JobsApprove"), "{res}");
    }
    // La app sí puede cambiar las reglas, y nunca dejar producción sin aprobación.
    let (status, res) = call(Method::POST, "/api/policy", &host, Some(APP), Some(json!({ "read": {}, "write": { "cloud-dev": "typed" } }))).await;
    assert_eq!(status, StatusCode::OK, "{res}");
    let (status, _) = call(Method::POST, "/api/policy", &host, Some(APP), Some(json!({ "read": {}, "write": { "cloud-prod": "none" } }))).await;
    assert_ne!(status, StatusCode::OK);
}

#[tokio::test]
async fn el_mcp_no_tiene_herramientas_de_aprobar() {
    let (status, res) = call(Method::POST, "/mcp", &local(), Some(CLI), Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<String> = res["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str().map(str::to_string)).collect();
    assert!(names.len() > 20, "{names:?}");
    for name in &names {
        let n = name.to_lowercase();
        assert!(!["approve", "reject", "policy", "secret", "deploy_key", "set_key"].iter().any(|w| n.contains(w)), "MCP tool {name} looks privileged");
    }
}

#[tokio::test]
async fn las_claves_nunca_salen() {
    // Lo que se lee de las claves de un entorno nunca trae el valor (aquí, sin entorno: 404).
    let (status, res) = call(Method::GET, "/api/environments/nope/keys", &local(), Some(APP), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!res.to_string().contains('|'));
}

#[tokio::test]
async fn rutas_nuevas_piden_token_y_no_filtran() {
    // El chequeo del primer arranque y el diff de esquemas son de lectura: sin token, nada.
    for path in ["/api/doctor", "/api/schema-diff?from=a&to=b"] {
        let (status, _) = call(Method::GET, path, &local(), None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
    }
    // Con el token de agentes se lee el chequeo, que no trae secretos (solo versiones y rutas).
    let (status, res) = call(Method::GET, "/api/doctor", &local(), Some(CLI), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(res["checks"].as_array().is_some_and(|c| c.len() >= 5), "{res}");
    assert!(!res.to_string().contains('|'), "{res}");
    // Un entorno que no existe en el diff: 404, sin tocar ningún backend.
    let (status, _) = call(Method::GET, "/api/schema-diff?from=nope&to=nada", &local(), Some(CLI), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[test]
fn telemetria_solo_de_origenes_permitidos() {
    use crate::http::origin_allowed;
    for ok in ["http://localhost:5173", "http://127.0.0.1:3000", "https://localhost"] {
        assert!(origin_allowed(ok, &[]), "{ok}");
    }
    for bad in ["http://localhost.evil.example", "http://127.0.0.1.evil.example:3000", "https://evil.example", "null", ""] {
        assert!(!origin_allowed(bad, &[]), "{bad}");
    }
    assert!(origin_allowed("https://app.example.com", &["https://app.example.com".into()]));
    assert!(!origin_allowed("https://app.example.com.evil", &["https://app.example.com".into()]));
}

#[test]
fn secretos_redactados_en_logs() {
    let line = "CONVEX_DEPLOY_KEY=prod:brave-falcon-456|eyJ2MiI6IjEyMzQ1Njc4OTAxMjM0NTY3ODkwIn0= and admin heskala-local|01a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4 token ghp_abcdefghijklmnopqrstuvwxyz123456";
    let out = devtool_core::redact(line);
    for leaked in ["eyJ2MiI6", "01a2b3c4d5e6f708", "ghp_abcdefghijklmnop"] {
        assert!(!out.contains(leaked), "{out}");
    }
}

#[tokio::test]
async fn el_token_de_los_agentes_solo_lee_por_el_mcp_y_solo_lo_operativo() {
    let host = format!("127.0.0.1:{PORT}");
    let agent = "agent-token-for-tests";
    // Ninguna ruta de la API, ni siquiera las de lectura.
    for path in ["/api/overview", "/api/environments/dev/documents?table=users", "/api/agents", "/api/incidents", "/api/agent-runs", "/api/threads"] {
        let (status, res) = call(Method::GET, path, &host, Some(agent), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {res}");
    }
    let mcp = |name: &str| json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": {} } });
    // Lo operativo sí.
    let (status, res) = call(Method::POST, "/mcp", &host, Some(agent), Some(mcp("list_incidents"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(res["result"]["isError"], true, "{res}");
    // Pedir trabajos, cancelarlos o leer telemetría, no.
    for name in ["request_job", "cancel_job", "get_snapshot", "list_approvals", "report_event"] {
        let (_, res) = call(Method::POST, "/mcp", &host, Some(agent), Some(mcp(name))).await;
        assert_eq!(res["result"]["isError"], true, "{name}: {res}");
        assert!(res["result"]["content"][0]["text"].as_str().unwrap_or_default().contains("isn't available to agents"), "{name}: {res}");
    }
}

