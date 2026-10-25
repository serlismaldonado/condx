// Datos de un entorno (pestaña Datos de la app), de solo lectura:
// - locales (devbox, desechables): las consultas de sistema del backend (las del dashboard de
//   Convex) con su admin key, que nunca sale del daemon; con tamaño de cada tabla y cursor.
// - cloud con una deploy key en el Llavero (pestaña Claves del entorno, o las generales de
//   `condxd secret set`): las mismas consultas de sistema, con tamaños y cursor.
// - cloud sin key: `convex data` con la sesión de la CLI (sin pedir admin keys). La CLI no da
//   tamaños ni cursor: se piden los N más nuevos y la página siguiente es pedir más.
// Siempre según las reglas de Configuración (policy.rs; por defecto, producción no expone
// registros).

use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use devtool_core::{EnvKind, Environment};
use serde_json::{json, Value};

use crate::state::Shared;

/// Como mucho, documentos por página.
pub const MAX_PAGE: usize = 500;
/// Por la CLI se piden los N más nuevos: hasta acá se puede «cargar más».
pub const MAX_CLI: usize = 2000;

fn is_local(env: &Environment) -> bool {
    matches!(env.kind, EnvKind::Devbox | EnvKind::Throwaway)
}

fn allowed_by(policy: &crate::policy::Policy, env: &Environment) -> Result<()> {
    if !policy.can_read(env.kind) {
        bail!("the policy doesn't allow reading records of {} (Condx settings → Data and changes)", env.name);
    }
    Ok(())
}

fn allowed(state: &Shared, env: &Environment) -> Result<()> {
    allowed_by(&crate::policy::load(state), env)
}

fn query(url: &str, admin: &str, path: &str, args: Value) -> Result<Value> {
    let body = json!({ "path": path, "args": args, "format": "json" });
    let text = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .build()
        .post(&format!("{url}/api/query"))
        .set("Authorization", &format!("Convex {admin}"))
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(|e| anyhow!("the backend didn't respond: {}", devtool_core::redact(&e.to_string())))?
        .into_string()?;
    let res: Value = serde_json::from_str(&text)?;
    if res["status"] != "success" {
        bail!("{path} failed: {}", res["errorMessage"].as_str().or(res["message"].as_str()).unwrap_or("no details"));
    }
    Ok(res["value"].clone())
}

/// URL y key para las consultas de sistema: la admin key en local; en cloud, la deploy key del
/// Llavero si el usuario la guardó (si no, `None` y se lee con la CLI). La key tiene que ser del
/// tipo y del deployment del entorno.
pub(crate) fn system_access(env: &Environment) -> Result<Option<(String, String)>> {
    if is_local(env) {
        return crate::schema::local_access(env).map(Some);
    }
    let Some((account, key)) = crate::secrets::deploy_key(env)? else { return Ok(None) };
    let name = crate::secrets::check_deploy_key(env, &key).map_err(|e| anyhow!("Keychain key “{account}”: {e}"))?;
    let url = env.convex_url.clone().unwrap_or_else(|| format!("https://{name}.convex.cloud"));
    Ok(Some((url.trim_end_matches('/').to_string(), key)))
}

/// Cuántos documentos tiene cada tabla, de a varias a la vez (en cloud cada consulta es un viaje).
fn sizes(url: &str, admin: &str, names: &[String]) -> Vec<Option<u64>> {
    let mut out = vec![None; names.len()];
    for (chunk_ix, chunk) in names.chunks(16).enumerate() {
        std::thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|name| s.spawn(move || query(url, admin, "_system/frontend/tableSize:default", json!({ "tableName": name, "componentId": null })).ok().and_then(|v| v.as_f64()).map(|n| n as u64)))
                .collect();
            for (i, h) in handles.into_iter().enumerate() {
                out[chunk_ix * 16 + i] = h.join().ok().flatten();
            }
        });
    }
    out
}

/// Las tablas del entorno (sin las de sistema), con cuántos documentos tiene cada una (en
/// cloud sin deploy key, sin tamaño).
pub async fn tables(state: &Shared, env: &Environment) -> Result<Value> {
    allowed(state, env)?;
    let access = system_access(env)?;
    let via = if is_local(env) { "local" } else if access.is_some() { "key" } else { "cli" };
    let Some((url, admin)) = access else {
        let out = crate::probes::convex::convex(state, env, &["data"], None).await?;
        if !out.ok {
            bail!(out.error());
        }
        let mut names: Vec<&str> = out.stdout.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('_') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).collect();
        names.sort();
        names.dedup();
        let list: Vec<Value> = names.into_iter().map(|n| json!({ "name": n, "count": null })).collect();
        return Ok(json!({ "environment": null, "tables": list, "counts": false, "via": via }));
    };
    tokio::task::spawn_blocking(move || -> Result<Value> {
        let mapping = query(&url, &admin, "_system/frontend/getTableMapping", json!({ "componentId": null }))?;
        let mut names: Vec<String> = mapping.as_object().into_iter().flatten().filter_map(|(_, v)| v.as_str().map(str::to_string)).filter(|n| !n.starts_with('_')).collect();
        names.sort();
        names.dedup();
        let counts = sizes(&url, &admin, &names);
        let list: Vec<Value> = names.into_iter().zip(counts).map(|(name, count)| json!({ "name": name, "count": count })).collect();
        Ok(json!({ "environment": null, "tables": list, "counts": true, "via": via }))
    })
    .await?
}

/// La primera tabla con documentos, en el orden de la lista. En local sale de los tamaños; en
/// cloud la CLI no los da, así que se prueba de a varias con `convex data <t> --limit 1`, por
/// orden, y se corta en la primera que tiene algo (con un tope de tablas y de tiempo).
pub async fn first_with_documents(state: &Shared, env: &Environment) -> Result<Value> {
    let list = tables(state, env).await?;
    let names: Vec<String> = list["tables"].as_array().into_iter().flatten().filter_map(|t| t["name"].as_str().map(str::to_string)).collect();
    if list["counts"] == true {
        let first = list["tables"].as_array().into_iter().flatten().find(|t| t["count"].as_u64().unwrap_or(0) > 0).and_then(|t| t["name"].as_str());
        return Ok(json!({ "table": first }));
    }
    const BATCH: usize = 6;
    const MAX_TABLES: usize = 60;
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    for chunk in names.chunks(BATCH).take(MAX_TABLES / BATCH) {
        if std::time::Instant::now() > deadline {
            break;
        }
        let probes = chunk.iter().map(|name| {
            let (state, env, name) = (state.clone(), env.clone(), name.clone());
            async move {
                let out = crate::probes::convex::convex(&state, &env, &["data", &name, "--limit", "1", "--format", "jsonl"], None).await.ok()?;
                out.stdout.lines().any(|l| l.trim_start().starts_with('{')).then_some(name)
            }
        });
        // Dentro del lote, la primera en el orden de la lista (no la que respondió primero).
        let found: Vec<Option<String>> = futures_util::future::join_all(probes).await;
        if let Some(name) = found.into_iter().flatten().next() {
            return Ok(json!({ "table": name }));
        }
    }
    Ok(json!({ "table": null }))
}

/// Una página de documentos de `table` (los más nuevos primero), desde `cursor`.
pub async fn documents(state: &Shared, env: &Environment, table: &str, cursor: Option<String>, limit: usize) -> Result<Value> {
    if table.is_empty() || table.starts_with('_') || !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!("invalid table");
    }
    allowed(state, env)?;
    let Some((url, admin)) = system_access(env)? else {
        // Sin cursor en la CLI: el «cursor» es cuántos ya se mostraron.
        let skip: usize = cursor.as_deref().and_then(|c| c.parse().ok()).unwrap_or(0);
        let want = (skip + limit.clamp(1, MAX_PAGE)).min(MAX_CLI);
        let n = want.to_string();
        let out = crate::probes::convex::convex(state, env, &["data", table, "--limit", &n, "--format", "jsonl"], None).await?;
        if !out.ok {
            bail!(out.error());
        }
        let docs: Vec<Value> = out.stdout.lines().filter(|l| l.trim_start().starts_with('{')).filter_map(|l| serde_json::from_str(l).ok()).collect();
        let got = docs.len();
        let page: Vec<Value> = docs.into_iter().skip(skip).collect();
        let done = got < want || want >= MAX_CLI;
        return Ok(json!({
            "table": table,
            "documents": page,
            "cursor": if done { Value::Null } else { json!(got.to_string()) },
            "isDone": done,
            "total": null,
            "capped": want >= MAX_CLI && got >= MAX_CLI,
        }));
    };
    let table = table.to_string();
    let limit = limit.clamp(1, MAX_PAGE);
    tokio::task::spawn_blocking(move || -> Result<Value> {
        let page = query(
            &url,
            &admin,
            "_system/frontend/paginatedTableDocuments:default",
            json!({ "paginationOpts": { "numItems": limit, "cursor": cursor }, "table": table, "filters": null, "componentId": null }),
        )?;
        let total = query(&url, &admin, "_system/frontend/tableSize:default", json!({ "tableName": table, "componentId": null })).ok().and_then(|v| v.as_f64()).map(|n| n as u64);
        Ok(json!({
            "table": table,
            "documents": page["page"],
            "cursor": page["continueCursor"],
            "isDone": page["isDone"],
            "total": total,
        }))
    })
    .await?
}

#[cfg(test)]
mod tests {
    use devtool_core::{EnvKind, Environment};

    fn env(kind: EnvKind) -> Environment {
        Environment { name: "x".into(), kind, label: None, convex_url: None, convex_site_url: None, convex_deployment: None, frontend_url: None, branch: None, credential: None, created_at: 0, project: None }
    }

    #[test]
    fn produccion_no_expone_registros() {
        let policy = crate::policy::Policy::default();
        assert!(super::allowed_by(&policy, &env(EnvKind::CloudProd)).is_err());
        assert!(super::allowed_by(&policy, &env(EnvKind::CloudDev)).is_ok());
        assert!(super::allowed_by(&policy, &env(EnvKind::Devbox)).is_ok());
        assert!(super::is_local(&env(EnvKind::Throwaway)) && !super::is_local(&env(EnvKind::CloudDev)));
    }
}
