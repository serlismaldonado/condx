// Esquema desplegado de un entorno: tablas, campos con su tipo, referencias
// `v.id("tabla")` e índices, leídos de `_system/frontend/getSchemas` con la
// admin key del backend (la que el daemon ya tiene para los entornos locales).
// Los entornos cloud no la tienen (la CLI no deja llamar funciones de sistema):
// muestran el esquema del backend local del mismo proyecto, marcado como tal.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use devtool_core::{Credential, EnvKind, Environment};
use serde_json::{json, Value};

use crate::cmd::env_file_value;
use crate::state::Shared;

/// Carpeta de cada tabla según `convex/schema.ts`: las tablas se importan de
/// sus módulos (`import { invoices } from "./modules/invoices"`) y se listan
/// en `defineSchema({ … })` (`invoices,` o `nombre: variable,`). Las que llegan
/// por un `...spread` (authTables) quedan con el paquete de donde vienen.
pub fn table_modules(schema_ts: &str) -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;
    let import = regex::Regex::new(r#"import\s*\{([^}]*)\}\s*from\s*["']([^"']+)["']"#).unwrap();
    let mut module_of: HashMap<String, String> = HashMap::new();
    for cap in import.captures_iter(schema_ts) {
        let path = cap[2].trim_start_matches("./").trim_end_matches("/index").to_string();
        for item in cap[1].split(',') {
            // Sin comentarios de línea dentro de la lista.
            let item = item.lines().map(|l| l.split("//").next().unwrap_or_default()).collect::<Vec<_>>().join(" ");
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let local = item.rsplit(" as ").next().unwrap_or(item).trim().to_string();
            module_of.insert(local, path.clone());
        }
    }
    let mut out = HashMap::new();
    let Some(start) = schema_ts.find("defineSchema(") else { return out };
    let body = &schema_ts[start..];
    let Some(open) = body.find('{') else { return out };
    // Hasta la llave que cierra el objeto de defineSchema.
    let mut depth = 0usize;
    let mut end = body.len();
    for (i, ch) in body[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = open + i;
                    break;
                }
            }
            _ => {}
        }
    }
    let entries = &body[open + 1..end];
    let line_comment = regex::Regex::new(r"//[^\n]*").unwrap();
    let entries = line_comment.replace_all(entries, "");
    for raw in entries.split(',') {
        let e = raw.trim();
        if e.is_empty() || e.starts_with("...") {
            continue;
        }
        let (name, var) = match e.split_once(':') {
            Some((n, v)) => (n.trim(), v.trim()),
            None => (e, e),
        };
        let ident = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_');
        if ident(name) && ident(var) {
            if let Some(path) = module_of.get(var) {
                out.insert(name.to_string(), path.clone());
            }
        }
    }
    out
}

/// El tipo de un validador de Convex como se escribe en `schema.ts`, abreviado.
pub(crate) fn type_text(t: &Value) -> String {
    match t["type"].as_str().unwrap_or_default() {
        "id" => format!("id<{}>", t["tableName"].as_str().unwrap_or("?")),
        "array" => format!("{}[]", type_text(&t["value"])),
        "object" => {
            let n = t["value"].as_object().map(|o| o.len()).unwrap_or(0);
            format!("object ({n})")
        }
        "record" => format!("record<{}, {}>", type_text(&t["keys"]), type_text(&t["values"]["fieldType"])),
        "union" => t["value"].as_array().map(|v| v.iter().map(type_text).collect::<Vec<_>>().join(" | ")).unwrap_or_default(),
        "literal" => match &t["value"] {
            Value::String(s) => format!("\"{s}\""),
            other => other.to_string(),
        },
        "float64" | "number" => "number".into(),
        "int64" | "bigint" => "int64".into(),
        other => other.to_string(),
    }
}

/// Tablas a las que apunta un tipo (en uniones, arreglos y objetos también).
fn refs(t: &Value, out: &mut Vec<String>) {
    match t["type"].as_str().unwrap_or_default() {
        "id" => {
            if let Some(n) = t["tableName"].as_str() {
                out.push(n.to_string());
            }
        }
        "array" => refs(&t["value"], out),
        "union" => t["value"].as_array().into_iter().flatten().for_each(|v| refs(v, out)),
        "object" => t["value"].as_object().into_iter().flatten().for_each(|(_, f)| refs(&f["fieldType"], out)),
        "record" => refs(&t["values"]["fieldType"], out),
        _ => {}
    }
}

/// La respuesta de `getSchemas` a algo cómodo para la app y el MCP.
fn simplify(active: &Value) -> Value {
    let mut tables: Vec<Value> = active["tables"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            let doc = &t["documentType"];
            let mut fields = vec![json!({ "name": "_id", "type": format!("id<{}>", t["tableName"].as_str().unwrap_or_default()), "optional": false, "primary": true })];
            let mut list: Vec<(&String, &Value)> = doc["value"].as_object().map(|o| o.iter().collect()).unwrap_or_default();
            list.sort_by(|a, b| a.0.cmp(b.0));
            for (name, f) in list {
                let mut r = vec![];
                refs(&f["fieldType"], &mut r);
                r.dedup();
                fields.push(json!({ "name": name, "type": type_text(&f["fieldType"]), "optional": f["optional"] == true, "refs": r }));
            }
            fields.push(json!({ "name": "_creationTime", "type": "number", "optional": false }));
            json!({
                "name": t["tableName"],
                // Sin validador (`v.any()` o tabla sin definir): sin campos conocidos.
                "typed": doc["type"] == "object",
                "fields": fields,
                "indexes": t["indexes"].as_array().into_iter().flatten().map(|i| json!({ "name": i["indexDescriptor"], "fields": i["fields"] })).collect::<Vec<_>>(),
                "searchIndexes": t["searchIndexes"].as_array().into_iter().flatten().map(|i| json!({ "name": i["indexDescriptor"], "searchField": i["searchField"], "filterFields": i["filterFields"] })).collect::<Vec<_>>(),
                "vectorIndexes": t["vectorIndexes"].as_array().into_iter().flatten().map(|i| json!({ "name": i["indexDescriptor"], "vectorField": i["vectorField"], "dimensions": i["dimensions"], "filterFields": i["filterFields"] })).collect::<Vec<_>>(),
            })
        })
        .collect();
    tables.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    json!({ "tables": tables })
}

/// URL y admin key de un backend local (nunca salen del daemon).
pub(crate) fn local_access(env: &Environment) -> Result<(String, String)> {
    let Some(Credential::EnvFile { path, key }) = &env.credential else { bail!("{} isn't a local backend", env.name) };
    let file = std::path::Path::new(path);
    let admin = env_file_value(file, key).ok_or_else(|| anyhow!("couldn't find the admin key of {}", env.name))?;
    let url = match env_file_value(file, "RUNTIME").as_deref() {
        Some("native") => env_file_value(file, "CONVEX_SELF_HOSTED_URL"),
        _ => None,
    }
    .or_else(|| env.convex_url.clone())
    .ok_or_else(|| anyhow!("{} has no URL", env.name))?;
    Ok((url.trim_end_matches('/').replace("localhost", "127.0.0.1"), admin))
}

async fn fetch(env: &Environment) -> Result<Value> {
    let (url, admin) = local_access(env)?;
    fetch_at(url, admin).await
}

async fn fetch_at(url: String, admin: String) -> Result<Value> {
    let res = tokio::task::spawn_blocking(move || -> Result<Value> {
        let body = json!({ "path": "_system/frontend/getSchemas", "args": {}, "format": "json" });
        let text = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(20))
            .build()
            .post(&format!("{url}/api/query"))
            .set("Authorization", &format!("Convex {admin}"))
            .set("Content-Type", "application/json")
            .send_string(&body.to_string())
            .map_err(|e| anyhow!("the backend didn't respond: {}", devtool_core::redact(&e.to_string())))?
            .into_string()?;
        Ok(serde_json::from_str(&text)?)
    })
    .await??;
    if res["status"] != "success" {
        bail!("getSchemas failed: {}", res["errorMessage"].as_str().unwrap_or("no details"));
    }
    let active = res["value"]["active"].as_str().context("the backend has no active schema")?;
    Ok(simplify(&serde_json::from_str(active)?))
}

/// El entorno local del mismo proyecto que `env` (el devbox, o `<proyecto>-local`).
pub fn local_of(state: &Shared, env: &Environment) -> Option<Environment> {
    state.store.environments().ok()?.into_iter().find(|e| e.kind == EnvKind::Devbox && e.project == env.project)
}

/// Esquema de `env`: el desplegado si es local; si es cloud, el del backend local de su proyecto.
pub async fn schema(state: &Shared, env: &Environment) -> Result<Value> {
    let (source, from) = if matches!(env.credential, Some(Credential::EnvFile { .. })) {
        (env.clone(), None)
    } else {
        let local = local_of(state, env).ok_or_else(|| anyhow!("{} is cloud and its project has no local backend to read the schema from", env.name))?;
        let name = local.name.clone();
        (local, Some(name))
    };
    let mut v = fetch(&source).await.map_err(|e| match &from {
        Some(local) => anyhow!("{e:#} (it's the local backend “{local}”: start it in Local environment)"),
        None => e,
    })?;
    // La carpeta de cada tabla, del schema.ts del repo del proyecto.
    let repo = crate::projects::repo_for(state, Some(env));
    if let Ok(text) = std::fs::read_to_string(repo.join("convex/schema.ts")) {
        let modules = table_modules(&text);
        if let Some(tables) = v["tables"].as_array_mut() {
            for t in tables {
                let name = t["name"].as_str().unwrap_or_default().to_string();
                // Las de un spread (authTables): del paquete de Convex Auth si empiezan con «auth».
                let module = modules.get(&name).cloned().or_else(|| name.starts_with("auth").then(|| "@convex-dev/auth".to_string()));
                t["module"] = json!(module);
            }
        }
    }
    v["environment"] = json!(env.name);
    v["source"] = json!(if from.is_some() { "checkout" } else { "deployed" });
    v["fromEnvironment"] = json!(from.unwrap_or_else(|| env.name.clone()));
    Ok(v)
}

/// El esquema que de verdad está desplegado en `env`: local con su admin key, cloud con la deploy
/// key del Llavero (pestaña Claves). Sin key no se puede leer un cloud.
pub async fn deployed(env: &Environment) -> Result<Value> {
    let (url, admin) = crate::data::system_access(env)?
        .ok_or_else(|| anyhow!("{} needs its deploy key to read the deployed schema (save it in its Keys tab)", env.name))?;
    fetch_at(url, admin).await
}

fn by_name(list: &Value) -> std::collections::BTreeMap<String, Value> {
    list.as_array().into_iter().flatten().filter_map(|x| Some((x["name"].as_str()?.to_string(), x.clone()))).collect()
}

/// Qué cambia al desplegar el esquema `from` donde hoy está `to`: tablas y campos que entran,
/// salen o cambian, índices, y lo que puede hacer fallar el despliegue o romper datos (`risks`).
pub fn diff(from: &Value, to: &Value) -> Value {
    let (a, b) = (by_name(&from["tables"]), by_name(&to["tables"]));
    let mut risks: Vec<String> = vec![];
    let added: Vec<&String> = a.keys().filter(|k| !b.contains_key(*k)).collect();
    let removed: Vec<&String> = b.keys().filter(|k| !a.contains_key(*k)).collect();
    for t in &removed {
        risks.push(format!("table {t} leaves the schema (its documents stay, without validation)"));
    }
    let mut changed = vec![];
    for (name, ta) in &a {
        let Some(tb) = b.get(name) else { continue };
        let (fa, fb) = (by_name(&ta["fields"]), by_name(&tb["fields"]));
        let mut fields_added = vec![];
        let mut fields_removed = vec![];
        let mut fields_changed = vec![];
        for (f, x) in &fa {
            match fb.get(f) {
                None => {
                    if x["optional"] != true && tb["typed"] == true {
                        risks.push(format!("{name}.{f} is new and required: existing documents don't have it"));
                    }
                    fields_added.push(json!({ "name": f, "type": x["type"], "optional": x["optional"] }));
                }
                Some(y) if y["type"] != x["type"] || y["optional"] != x["optional"] => {
                    if y["type"] != x["type"] {
                        risks.push(format!("{name}.{f} changes type ({} → {})", y["type"].as_str().unwrap_or("?"), x["type"].as_str().unwrap_or("?")));
                    } else if x["optional"] != true {
                        risks.push(format!("{name}.{f} becomes required: documents without it fail validation"));
                    }
                    fields_changed.push(json!({ "name": f, "from": { "type": y["type"], "optional": y["optional"] }, "to": { "type": x["type"], "optional": x["optional"] } }));
                }
                _ => {}
            }
        }
        for (f, y) in &fb {
            if !fa.contains_key(f) {
                if ta["typed"] == true {
                    risks.push(format!("{name}.{f} leaves the schema: documents that still have it fail validation"));
                }
                fields_removed.push(json!({ "name": f, "type": y["type"] }));
            }
        }
        let index_names = |t: &Value| -> std::collections::BTreeSet<String> {
            ["indexes", "searchIndexes", "vectorIndexes"].iter().flat_map(|k| t[*k].as_array().cloned().unwrap_or_default()).filter_map(|i| i["name"].as_str().map(str::to_string)).collect()
        };
        let (ia, ib) = (index_names(ta), index_names(tb));
        let indexes_added: Vec<&String> = ia.difference(&ib).collect();
        let indexes_removed: Vec<&String> = ib.difference(&ia).collect();
        if !(fields_added.is_empty() && fields_removed.is_empty() && fields_changed.is_empty() && indexes_added.is_empty() && indexes_removed.is_empty()) {
            changed.push(json!({
                "table": name,
                "fieldsAdded": fields_added,
                "fieldsRemoved": fields_removed,
                "fieldsChanged": fields_changed,
                "indexesAdded": indexes_added,
                "indexesRemoved": indexes_removed,
            }));
        }
    }
    let same = added.is_empty() && removed.is_empty() && changed.is_empty();
    json!({ "same": same, "tablesAdded": added, "tablesRemoved": removed, "changed": changed, "risks": risks })
}

/// El diff entre lo desplegado en `from` (el backend local tiene lo del checkout) y en `to`, con
/// los nombres de variables de entorno que hay en uno y no en el otro (nunca valores). Si las
/// variables no se pueden leer, el diff de esquema sigue y `envVars.error` dice por qué.
pub async fn diff_envs(state: &Shared, from: &Environment, to: &Environment) -> Result<Value> {
    use crate::probes::convex::env_var_names;
    let (a, b, va, vb) = tokio::join!(deployed(from), deployed(to), env_var_names(state, from), env_var_names(state, to));
    let mut d = diff(&a?, &b?);
    d["from"] = json!(from.name);
    d["to"] = json!(to.name);
    d["envVars"] = match (va, vb) {
        (Ok(va), Ok(vb)) => env_var_diff(&va, &vb),
        (Err(e), _) | (_, Err(e)) => json!({ "error": devtool_core::redact(&format!("{e:#}")) }),
    };
    Ok(d)
}

/// Nombres en `from` que faltan en `to` (lo que el código puede leer y allá no está) y al revés.
pub fn env_var_diff(from: &[String], to: &[String]) -> Value {
    let missing: Vec<&String> = from.iter().filter(|n| !to.contains(n)).collect();
    let extra: Vec<&String> = to.iter().filter(|n| !from.contains(n)).collect();
    json!({ "missing": missing, "extra": extra })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn lee_la_carpeta_de_cada_tabla() {
        let ts = r#"
import { defineSchema } from "convex/server";
import { authTables } from "@convex-dev/auth/server";
import { companies } from "./companies";
import {
  aiAgentConfigTable as ai_agent_config, // config del agente
  aiUsageTable as ai_usage,
} from "./modules/agents";
import { invoices } from "./modules/invoices/index";
export default defineSchema({
  // comentario
  ...authTables,
  companies,
  ai_agent_config,
  ai_usage,
  facturas: invoices,
});
"#;
        let m = super::table_modules(ts);
        assert_eq!(m["companies"], "companies");
        assert_eq!(m["ai_agent_config"], "modules/agents");
        assert_eq!(m["ai_usage"], "modules/agents");
        assert_eq!(m["facturas"], "modules/invoices");
        assert!(!m.contains_key("authTables"));
    }

    #[test]
    fn simplifica_tipos_y_referencias() {
        let active = json!({ "tables": [{
            "tableName": "posts",
            "indexes": [{ "indexDescriptor": "by_author", "fields": ["author", "_creationTime"] }],
            "searchIndexes": [], "vectorIndexes": [],
            "documentType": { "type": "object", "value": {
                "author": { "fieldType": { "type": "id", "tableName": "users" }, "optional": false },
                "tags": { "fieldType": { "type": "array", "value": { "type": "string" } }, "optional": true },
                "state": { "fieldType": { "type": "union", "value": [{ "type": "literal", "value": "a" }, { "type": "literal", "value": "b" }] }, "optional": false }
            }}
        }]});
        let s = super::simplify(&active);
        let fields = s["tables"][0]["fields"].as_array().unwrap();
        assert_eq!(fields[0]["name"], "_id");
        let author = fields.iter().find(|f| f["name"] == "author").unwrap();
        assert_eq!(author["type"], "id<users>");
        assert_eq!(author["refs"][0], "users");
        assert_eq!(fields.iter().find(|f| f["name"] == "tags").unwrap()["type"], "string[]");
        assert_eq!(fields.iter().find(|f| f["name"] == "state").unwrap()["type"], "\"a\" | \"b\"");
        assert_eq!(s["tables"][0]["indexes"][0]["name"], "by_author");
    }

    #[test]
    fn diff_de_esquemas() {
        let local = json!({ "tables": [
            { "name": "tasks", "typed": true, "fields": [
                { "name": "title", "type": "string", "optional": false },
                { "name": "done", "type": "boolean", "optional": false },
                { "name": "priority", "type": "number", "optional": true },
                { "name": "owner", "type": "id<users>", "optional": false }
            ], "indexes": [{ "name": "by_owner" }, { "name": "by_done" }] },
            { "name": "tags", "typed": true, "fields": [], "indexes": [] }
        ]});
        let cloud = json!({ "tables": [
            { "name": "tasks", "typed": true, "fields": [
                { "name": "title", "type": "string", "optional": false },
                { "name": "done", "type": "string", "optional": false },
                { "name": "legacy", "type": "string", "optional": true }
            ], "indexes": [{ "name": "by_owner" }, { "name": "by_title" }] },
            { "name": "old", "typed": true, "fields": [], "indexes": [] }
        ]});
        let d = super::diff(&local, &cloud);
        assert_eq!(d["same"], false);
        assert_eq!(d["tablesAdded"], json!(["tags"]));
        assert_eq!(d["tablesRemoved"], json!(["old"]));
        let t = &d["changed"][0];
        assert_eq!(t["table"], "tasks");
        assert_eq!(t["fieldsAdded"].as_array().unwrap().len(), 2);
        assert_eq!(t["fieldsRemoved"][0]["name"], "legacy");
        assert_eq!(t["fieldsChanged"][0]["name"], "done");
        assert_eq!(t["indexesAdded"], json!(["by_done"]));
        assert_eq!(t["indexesRemoved"], json!(["by_title"]));
        let risks: Vec<String> = serde_json::from_value(d["risks"].clone()).unwrap();
        assert!(risks.iter().any(|r| r.contains("tasks.owner is new and required")), "{risks:?}");
        assert!(risks.iter().any(|r| r.contains("tasks.done changes type")), "{risks:?}");
        assert!(risks.iter().any(|r| r.contains("tasks.legacy leaves")), "{risks:?}");
        assert!(risks.iter().any(|r| r.contains("table old leaves")), "{risks:?}");
        // Un campo opcional nuevo no es un riesgo.
        assert!(!risks.iter().any(|r| r.contains("priority")), "{risks:?}");
        assert_eq!(super::diff(&local, &local)["same"], true);
    }

    #[test]
    fn diff_de_variables_solo_nombres() {
        let local = vec!["AUTH_SECRET".to_string(), "OPENAI_API_KEY".to_string(), "SITE_URL".to_string()];
        let cloud = vec!["AUTH_SECRET".to_string(), "SITE_URL".to_string(), "SENTRY_DSN".to_string()];
        let d = super::env_var_diff(&local, &cloud);
        assert_eq!(d["missing"], json!(["OPENAI_API_KEY"]));
        assert_eq!(d["extra"], json!(["SENTRY_DSN"]));
    }
}
