// Funciones de un entorno (`convex function-spec`), simplificadas para el
// selector de la receta `run-function`: ruta como la espera `convex run`, tipo,
// visibilidad y argumentos. La salida completa pesa varios MB (trae los
// validadores de retorno), así que se guarda un rato por entorno.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use devtool_core::Environment;
use serde_json::{json, Value};

use crate::state::Shared;

const TTL: Duration = Duration::from_secs(60);

static CACHE: Mutex<Option<HashMap<String, (Instant, Value)>>> = Mutex::new(None);

/// `auth.js:signIn` → `auth:signIn` (la ruta que acepta `convex run`).
fn run_path(identifier: &str) -> String {
    match identifier.split_once(':') {
        Some((module, name)) => format!("{}:{name}", module.trim_end_matches(".js")),
        None => identifier.trim_end_matches(".js").to_string(),
    }
}

/// Las funciones de la salida de `function-spec`, sin las HTTP (no se corren con `convex run`).
pub fn parse(spec: &Value) -> Vec<Value> {
    let mut out: Vec<Value> = spec["functions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let kind = match f["functionType"].as_str()? {
                "Query" => "query",
                "Mutation" => "mutation",
                "Action" => "action",
                _ => return None,
            };
            let identifier = run_path(f["identifier"].as_str()?);
            // Sin validador (`args: any`): no se sabe qué recibe.
            let args = match f["args"]["type"].as_str() {
                Some("object") => {
                    let mut list: Vec<(&String, &Value)> = f["args"]["value"].as_object().map(|o| o.iter().collect()).unwrap_or_default();
                    list.sort_by(|a, b| a.0.cmp(b.0));
                    json!(list
                        .into_iter()
                        .map(|(name, a)| json!({ "name": name, "type": crate::schema::type_text(&a["fieldType"]), "optional": a["optional"] == true }))
                        .collect::<Vec<_>>())
                }
                _ => Value::Null,
            };
            Some(json!({
                "identifier": identifier,
                "kind": kind,
                "visibility": f["visibility"]["kind"].as_str().unwrap_or("public"),
                "args": args,
            }))
        })
        .collect();
    out.sort_by(|a, b| a["identifier"].as_str().cmp(&b["identifier"].as_str()));
    out
}

async fn fetch(state: &Shared, env: &Environment) -> Result<Value> {
    let repo = crate::projects::repo_for(state, Some(env));
    let (bin, pre) = crate::probes::convex::convex_bin_at(&repo);
    let (extra, vars) = crate::probes::convex::target(env, false)?;
    let mut args: Vec<&str> = pre.iter().map(String::as_str).collect();
    args.push("function-spec");
    args.extend(extra.iter().map(String::as_str));
    let vars: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let out = crate::cmd::run(&bin, &args, Some(&repo), &vars, Duration::from_secs(90)).await;
    if !out.ok {
        bail!("convex function-spec failed: {}", out.error());
    }
    let spec: Value = serde_json::from_str(&out.stdout).map_err(|e| anyhow::anyhow!("convex function-spec didn't return JSON: {e}"))?;
    Ok(json!(parse(&spec)))
}

/// Las funciones de `env`; `fresh` salta el caché.
pub async fn functions(state: &Shared, env: &Environment, fresh: bool) -> Result<Value> {
    let cached = (!fresh)
        .then(|| CACHE.lock().ok()?.as_ref()?.get(&env.name).filter(|(at, _)| at.elapsed() < TTL).map(|(_, v)| v.clone()))
        .flatten();
    let list = match cached {
        Some(v) => v,
        None => {
            let v = fetch(state, env).await?;
            if let Ok(mut c) = CACHE.lock() {
                c.get_or_insert_with(HashMap::new).insert(env.name.clone(), (Instant::now(), v.clone()));
            }
            v
        }
    };
    Ok(json!({ "environment": env.name, "functions": list }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn simplifica_function_spec() {
        let spec = json!({
            "url": "http://127.0.0.1:3210",
            "functions": [
                { "functionType": "Mutation", "identifier": "migrations.js:backfillDefaults",
                  "visibility": { "kind": "internal" }, "returns": null,
                  "args": { "type": "object", "value": {
                      "companyId": { "fieldType": { "type": "id", "tableName": "companies" }, "optional": false },
                      "dryRun": { "fieldType": { "type": "boolean" }, "optional": true } } } },
                { "functionType": "Query", "identifier": "auth.js:isAuthenticated", "visibility": { "kind": "public" },
                  "args": { "type": "object", "value": {} }, "returns": null },
                { "functionType": "Action", "identifier": "lib/legacy.js:run", "visibility": { "kind": "public" }, "args": { "type": "any" } },
                { "functionType": "HttpAction", "method": "GET", "path": "/.well-known/jwks.json" }
            ]
        });
        let list = super::parse(&spec);
        assert_eq!(list.len(), 3, "sin las HTTP");
        assert_eq!(list[0]["identifier"], "auth:isAuthenticated");
        assert_eq!(list[0]["kind"], "query");
        assert_eq!(list[0]["args"], json!([]));
        assert_eq!(list[1]["identifier"], "lib/legacy:run");
        assert!(list[1]["args"].is_null());
        let m = &list[2];
        assert_eq!(m["identifier"], "migrations:backfillDefaults");
        assert_eq!(m["kind"], "mutation");
        assert_eq!(m["visibility"], "internal");
        assert_eq!(m["args"][0], json!({ "name": "companyId", "type": "id<companies>", "optional": false }));
        assert_eq!(m["args"][1]["optional"], true);
    }
}
