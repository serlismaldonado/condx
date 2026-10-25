// El frontend de un proyecto en Vercel, para la vista Frontend de la app y el MCP:
// - producción (el último despliegue listo), previews (el último de cada rama), lo que se está
//   construyendo y el último que falló, con el final de su log de compilación;
// - si el frontend y Convex coinciden: a qué deployment de Convex apunta lo publicado (se lee del
//   HTML y del JS del sitio, donde Vite y Next dejan la URL) frente al entorno que corresponde, y
//   qué commit tiene el frontend de producción frente al que se desplegó en Convex.
//
// Usa la CLI de Vercel con la sesión de la Mac (`vercel ls`, `vercel inspect --logs`); nada de su
// token sale de aquí. Los despliegues de Vercel no cambian una vez hechos: el log de uno que falló
// y lo que se leyó de un sitio se guardan por URL.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use devtool_core::{EnvKind, Environment};
use regex::Regex;
use serde_json::{json, Value};

use crate::cmd::run;
use crate::projects::Project;
use crate::state::Shared;

const T: Duration = Duration::from_secs(40);
/// Líneas del log de un despliegue fallido.
const LOG_LINES: usize = 40;
/// Previews (una por rama) que se muestran.
const PREVIEWS: usize = 8;
/// Archivos JS de un sitio que se leen buscando la URL de Convex, y su tamaño máximo.
const MAX_SCRIPTS: usize = 30;
const MAX_BYTES: u64 = 6 * 1024 * 1024;
/// El informe de un proyecto se reutiliza este tiempo (la vista lo pide al abrirse y al refrescar).
const FRESH_MS: i64 = 30_000;

/// La CLI de Vercel (`CONDX_VERCEL_BIN` en las pruebas).
fn vercel() -> String {
    std::env::var("CONDX_VERCEL_BIN").unwrap_or_else(|_| "vercel".into())
}

fn cache<T: Clone + Send + 'static>(cell: &'static OnceLock<Mutex<HashMap<String, T>>>) -> &'static Mutex<HashMap<String, T>> {
    cell.get_or_init(Default::default)
}

static LOGS: OnceLock<Mutex<HashMap<String, Value>>> = OnceLock::new();
static SCANS: OnceLock<Mutex<HashMap<String, Value>>> = OnceLock::new();
static REPORTS: OnceLock<Mutex<HashMap<String, (i64, Value)>>> = OnceLock::new();

/// Un despliegue de `vercel ls --format json`, con lo que la app usa.
pub fn summarize(d: &Value) -> Value {
    json!({
        "url": d["url"].as_str().map(|u| format!("https://{u}")),
        "state": d["state"],
        "target": d["target"],
        "createdAt": d["createdAt"],
        "commit": d["meta"]["githubCommitSha"],
        "message": d["meta"]["githubCommitMessage"].as_str().map(|m| m.lines().next().unwrap_or("").to_string()),
        "branch": d["meta"]["githubCommitRef"],
        "creator": d["creator"]["username"],
    })
}

const BUILDING: [&str; 3] = ["BUILDING", "QUEUED", "INITIALIZING"];

/// Producción, previews (la última de cada rama), en curso y el último fallido.
pub fn classify(list: &[Value]) -> Value {
    // Del más nuevo al más viejo (como suele darlos `vercel ls`, sin depender de eso).
    let mut sorted = list.to_vec();
    sorted.sort_by_key(|d| std::cmp::Reverse(d["createdAt"].as_i64().unwrap_or(0)));
    let list = sorted.as_slice();
    let production = list.iter().find(|d| d["target"] == "production" && d["state"] == "READY").map(summarize);
    let mut seen = std::collections::HashSet::new();
    let previews: Vec<Value> = list
        .iter()
        .filter(|d| d["target"] != "production" && !BUILDING.contains(&d["state"].as_str().unwrap_or("")))
        .filter(|d| seen.insert(d["meta"]["githubCommitRef"].as_str().unwrap_or("").to_string()))
        .take(PREVIEWS)
        .map(summarize)
        .collect();
    let building: Vec<Value> = list.iter().filter(|d| BUILDING.contains(&d["state"].as_str().unwrap_or(""))).map(summarize).collect();
    let failed = list.iter().position(|d| d["state"] == "ERROR").map(|i| {
        let d = &list[i];
        // Resuelto si después hubo uno listo del mismo destino (producción) o de la misma rama.
        let resolved = list[..i].iter().any(|n| {
            n["state"] == "READY"
                && if d["target"] == "production" { n["target"] == "production" } else { n["meta"]["githubCommitRef"] == d["meta"]["githubCommitRef"] }
        });
        let mut s = summarize(d);
        s["resolved"] = json!(resolved);
        s
    });
    json!({ "production": production, "previews": previews, "building": building, "lastFailed": failed })
}

/// Las últimas líneas de un log, sin colores de terminal y sin secretos.
pub fn log_tail(text: &str) -> String {
    static ANSI: OnceLock<Regex> = OnceLock::new();
    let ansi = ANSI.get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").unwrap());
    let clean = ansi.replace_all(text, "");
    let lines: Vec<&str> = clean.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(LOG_LINES)..].join("\n");
    devtool_core::redact(&tail)
}

/// Las URLs de Convex que aparecen en un texto: las `*.convex.cloud` y las de los entornos
/// conocidos (dominios propios o autohospedados).
pub fn convex_urls(text: &str, known: &[String]) -> Vec<String> {
    static CLOUD: OnceLock<Regex> = OnceLock::new();
    let re = CLOUD.get_or_init(|| Regex::new(r"https://[a-z0-9-]+\.convex\.cloud").unwrap());
    let mut found: Vec<String> = re.find_iter(text).map(|m| m.as_str().to_string()).collect();
    for k in known {
        if !k.is_empty() && text.contains(k.trim_end_matches('/')) {
            found.push(k.trim_end_matches('/').to_string());
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Los scripts que carga una página (`<script src>` y `modulepreload`), como URLs absolutas.
pub fn scripts_of(html: &str, base: &str) -> Vec<String> {
    static SRC: OnceLock<Regex> = OnceLock::new();
    let re = SRC.get_or_init(|| Regex::new(r#"(?:<script[^>]+src|<link[^>]+href)=["']([^"']+\.m?js(?:\?[^"']*)?)["']"#).unwrap());
    let origin = base.trim_end_matches('/');
    let origin = match origin.find("://").and_then(|i| origin[i + 3..].find('/').map(|j| i + 3 + j)) {
        Some(end) => &origin[..end],
        None => origin,
    };
    let mut out = vec![];
    for c in re.captures_iter(html) {
        let src = &c[1];
        let url = if src.starts_with("http://") || src.starts_with("https://") {
            src.to_string()
        } else if let Some(rest) = src.strip_prefix("//") {
            format!("https://{rest}")
        } else if src.starts_with('/') {
            format!("{origin}{src}")
        } else {
            format!("{origin}/{src}")
        };
        // Solo del mismo sitio: los de terceros (analítica, etc.) no traen la URL de Convex.
        if url.starts_with(origin) && !out.contains(&url) {
            out.push(url);
        }
    }
    out.truncate(MAX_SCRIPTS);
    out
}

enum Fetch {
    Ok(String),
    Protected(u16),
    Failed(String),
}

fn fetch(url: &str) -> Fetch {
    use std::io::Read;
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(12)).redirects(5).build();
    match agent.get(url).set("User-Agent", "condx").call() {
        Ok(r) => {
            let mut body = String::new();
            match r.into_reader().take(MAX_BYTES).read_to_string(&mut body) {
                Ok(_) => Fetch::Ok(body),
                Err(e) => Fetch::Failed(e.to_string()),
            }
        }
        Err(ureq::Error::Status(code @ (401 | 403), _)) => Fetch::Protected(code),
        Err(ureq::Error::Status(code, _)) => Fetch::Failed(format!("HTTP {code}")),
        Err(e) => Fetch::Failed(e.to_string()),
    }
}

/// Lee el sitio y sus scripts hasta encontrar URLs de Convex (bloqueante).
fn scan_site(site: &str, known: &[String]) -> Value {
    let html = match fetch(site) {
        Fetch::Ok(b) => b,
        Fetch::Protected(code) => return json!({ "status": "protected", "detail": format!("HTTP {code}: the site asks for a login (Vercel protection)") }),
        Fetch::Failed(e) => return json!({ "status": "error", "detail": e }),
    };
    let mut found = convex_urls(&html, known);
    let scripts = scripts_of(&html, site);
    let mut read = 0;
    for s in &scripts {
        if !found.is_empty() {
            break;
        }
        if let Fetch::Ok(js) = fetch(s) {
            read += 1;
            found = convex_urls(&js, known);
        }
    }
    json!({ "status": "read", "found": found, "scripts": read })
}

/// A qué entorno corresponde una URL de Convex.
fn env_of<'a>(url: &str, envs: &'a [Environment]) -> Option<&'a Environment> {
    envs.iter().find(|e| e.convex_url.as_deref().map(|u| u.trim_end_matches('/')) == Some(url))
}

/// ¿Lo que encontró el sitio es el Convex que corresponde?
pub fn verdict(scan: &Value, expected: Option<&Environment>, envs: &[Environment]) -> Value {
    let mut out = scan.clone();
    out["expectedEnv"] = json!(expected.map(|e| &e.name));
    out["expectedUrl"] = json!(expected.and_then(|e| e.convex_url.clone()));
    if scan["status"] != "read" {
        return out;
    }
    let found: Vec<String> = scan["found"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
    out["matches"] = json!(found.iter().map(|u| json!({ "url": u, "env": env_of(u, envs).map(|e| &e.name) })).collect::<Vec<_>>());
    let want = expected.and_then(|e| e.convex_url.as_deref()).map(|u| u.trim_end_matches('/').to_string());
    out["status"] = json!(match (&want, found.is_empty()) {
        (_, true) => "not-found",
        (None, false) => "unknown-env",
        (Some(w), false) if found.len() == 1 && &found[0] == w => "ok",
        (Some(w), false) if found.contains(w) => "mixed",
        _ => "mismatch",
    });
    out
}

/// Cache de lo leído de un sitio: por la URL del despliegue (no cambia) y el sitio que se leyó.
async fn scan_cached(site: String, deployment: String, known: Vec<String>) -> Value {
    let key = format!("{deployment}|{site}");
    if let Some(v) = cache(&SCANS).lock().unwrap().get(&key).cloned() {
        return v;
    }
    let s = site.clone();
    let v = tokio::task::spawn_blocking(move || scan_site(&s, &known)).await.unwrap_or_else(|e| json!({ "status": "error", "detail": e.to_string() }));
    let mut v = v;
    v["site"] = json!(site);
    // Un error de red se vuelve a intentar; lo leído (o protegido) se guarda.
    if v["status"] != "error" {
        cache(&SCANS).lock().unwrap().insert(key, v.clone());
    }
    v
}

pub async fn failed_log(repo: &Path, url: &str) -> Value {
    if let Some(v) = cache(&LOGS).lock().unwrap().get(url).cloned() {
        return v;
    }
    let out = run(&vercel(), &["inspect", url, "--logs"], Some(repo), &[], T).await;
    let text = format!("{}\n{}", out.stdout, out.stderr);
    let v = if out.ok || !text.trim().is_empty() { json!({ "tail": log_tail(&text) }) } else { json!({ "error": out.error() }) };
    if out.ok {
        cache(&LOGS).lock().unwrap().insert(url.to_string(), v.clone());
    }
    v
}

/// El sitio que se lee para el Convex de un despliegue: el dominio del entorno (las URLs únicas de
/// Vercel suelen estar protegidas), salvo `localhost` (el del devbox); si no, la del despliegue.
fn site_for(deployment_url: &str, env: Option<&Environment>) -> String {
    env.and_then(|e| e.frontend_url.clone())
        .filter(|u| (u.starts_with("https://") || u.starts_with("http://")) && !u.contains("localhost"))
        .unwrap_or_else(|| deployment_url.to_string())
}

/// Commit del frontend de producción frente al que se desplegó en Convex de producción.
async fn commit_skew(repo: &Path, front: Option<&str>, convex: Option<&str>) -> Value {
    let (Some(f), Some(c)) = (front, convex) else {
        return json!({ "frontend": front.map(|s| &s[..s.len().min(8)]), "convex": convex.map(|s| &s[..s.len().min(8)]), "status": "unknown" });
    };
    let short = |s: &str| s[..s.len().min(8)].to_string();
    if f.starts_with(c) || c.starts_with(f) {
        return json!({ "frontend": short(f), "convex": short(c), "status": "same" });
    }
    let (front_ahead, convex_ahead) =
        tokio::join!(crate::probes::git::commits_between(repo, c, f), crate::probes::git::commits_between(repo, f, c));
    let status = match (front_ahead, convex_ahead) {
        (Some(a), Some(0)) if a > 0 => "frontend-ahead",
        (Some(0), Some(b)) if b > 0 => "convex-ahead",
        (Some(_), Some(_)) => "diverged",
        _ => "unknown",
    };
    json!({ "frontend": short(f), "convex": short(c), "frontendAhead": front_ahead, "convexAhead": convex_ahead, "status": status })
}

fn project_envs(state: &Shared, project: &Project) -> Vec<Environment> {
    let name = (project.name != crate::projects::DEFAULT).then_some(project.name.as_str());
    state.store.environments().unwrap_or_default().into_iter().filter(|e| e.project.as_deref() == name).collect()
}

/// El informe de la vista Frontend de un proyecto (con `fresh`, sin reutilizar el anterior).
pub async fn report(state: &Shared, project: &Project, fresh: bool) -> Value {
    if !fresh {
        if let Some((at, v)) = cache(&REPORTS).lock().unwrap().get(&project.name).cloned() {
            if crate::store::now_ms() - at < FRESH_MS {
                return v;
            }
        }
    }
    let v = build(state, project).await;
    cache(&REPORTS).lock().unwrap().insert(project.name.clone(), (crate::store::now_ms(), v.clone()));
    v
}

async fn build(state: &Shared, project: &Project) -> Value {
    let repo = project.repo.clone();
    let checked = crate::store::now_ms();
    if !repo.join(".vercel/project.json").exists() {
        return json!({ "project": project.name, "linked": false, "checkedAt": checked, "error": "The repo isn't linked to Vercel (no .vercel/project.json). Run `vercel link` in it." });
    }
    let out = run(&vercel(), &["ls", "--format", "json"], Some(&repo), &[], T).await;
    if !out.ok {
        return json!({ "project": project.name, "linked": true, "checkedAt": checked, "error": out.error() });
    }
    let Ok(body) = serde_json::from_str::<Value>(&out.stdout) else {
        return json!({ "project": project.name, "linked": true, "checkedAt": checked, "error": "unreadable vercel response" });
    };
    let list = body["deployments"].as_array().cloned().unwrap_or_default();
    let mut v = classify(&list);
    v["project"] = json!(project.name);
    v["vercelProject"] = body["deployments"][0]["name"].clone();
    v["linked"] = json!(true);
    v["checkedAt"] = json!(checked);

    let envs = project_envs(state, project);
    let prod = envs.iter().find(|e| e.kind == EnvKind::CloudProd);
    let dev = envs.iter().find(|e| e.kind == EnvKind::CloudDev);
    let known: Vec<String> = envs.iter().filter_map(|e| e.convex_url.clone()).collect();

    // El log del último que falló (si no se resolvió después).
    if let Some(url) = v["lastFailed"]["url"].as_str().map(str::to_string) {
        if v["lastFailed"]["resolved"] != true {
            v["lastFailed"]["log"] = failed_log(&repo, &url).await;
        }
    }

    // Producción: a qué Convex apunta y qué commit tiene frente a Convex de producción.
    if let Some(url) = v["production"]["url"].as_str().map(str::to_string) {
        let site = site_for(&url, prod);
        let scan = scan_cached(site, url, known.clone()).await;
        v["production"]["convex"] = verdict(&scan, prod, &envs);
        let convex_commit = prod.and_then(|e| state.store.last_deployment(&e.name).ok().flatten()).map(|d| d.commit);
        v["production"]["skew"] = commit_skew(&repo, v["production"]["commit"].as_str(), convex_commit.as_deref()).await;
        v["production"]["behindMain"] = match v["production"]["commit"].as_str() {
            Some(c) => json!(crate::probes::git::commits_between(&repo, c, "main").await),
            None => Value::Null,
        };
    }
    // La preview lista más reciente: debería apuntar al Convex de desarrollo.
    let ready = v["previews"].as_array().and_then(|l| l.iter().position(|p| p["state"] == "READY"));
    if let Some(i) = ready {
        if let Some(url) = v["previews"][i]["url"].as_str().map(str::to_string) {
            let scan = scan_cached(url.clone(), url, known.clone()).await;
            v["previews"][i]["convex"] = verdict(&scan, dev, &envs);
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dep(url: &str, state: &str, target: Option<&str>, branch: &str, at: i64) -> Value {
        json!({ "url": url, "state": state, "target": target, "createdAt": at, "name": "my-app",
                "meta": { "githubCommitSha": format!("{branch}{at}abcdef"), "githubCommitRef": branch, "githubCommitMessage": "Fix\n\nbody" } })
    }

    #[test]
    fn clasifica_produccion_previews_en_curso_y_fallidos() {
        let list = vec![
            dep("e.vercel.app", "BUILDING", None, "feat-x", 6),
            dep("d.vercel.app", "ERROR", None, "feat-y", 5),
            dep("c.vercel.app", "READY", None, "feat-x", 4),
            dep("b.vercel.app", "READY", None, "feat-x", 3),
            dep("a.vercel.app", "READY", Some("production"), "main", 2),
            dep("z.vercel.app", "ERROR", Some("production"), "main", 1),
        ];
        let v = classify(&list);
        assert_eq!(v["production"]["url"], "https://a.vercel.app");
        assert_eq!(v["production"]["message"], "Fix");
        // Una por rama, la más nueva; el que falló cuenta como la de su rama.
        let previews: Vec<&str> = v["previews"].as_array().unwrap().iter().map(|p| p["url"].as_str().unwrap()).collect();
        assert_eq!(previews, vec!["https://d.vercel.app", "https://c.vercel.app"]);
        assert_eq!(v["building"][0]["url"], "https://e.vercel.app");
        assert_eq!(v["lastFailed"]["url"], "https://d.vercel.app");
        assert_eq!(v["lastFailed"]["resolved"], false);
    }

    #[test]
    fn ordena_por_fecha_aunque_vengan_desordenados() {
        let list = vec![dep("old.vercel.app", "READY", None, "feat-x", 1), dep("new.vercel.app", "READY", None, "feat-x", 9)];
        assert_eq!(classify(&list)["previews"][0]["url"], "https://new.vercel.app");
    }

    #[test]
    fn un_fallo_de_produccion_queda_resuelto_si_despues_hubo_uno_listo() {
        let list = vec![dep("a.vercel.app", "READY", Some("production"), "main", 2), dep("z.vercel.app", "ERROR", Some("production"), "main", 1)];
        assert_eq!(classify(&list)["lastFailed"]["resolved"], true);
    }

    #[test]
    fn el_log_queda_corto_sin_colores_ni_secretos() {
        let mut text = String::new();
        for i in 0..100 {
            text.push_str(&format!("\x1b[31mline {i}\x1b[0m\n"));
        }
        text.push_str("Error: CONVEX_DEPLOY_KEY=prod:abc123secretvalue failed\n");
        let tail = log_tail(&text);
        assert_eq!(tail.lines().count(), LOG_LINES);
        assert!(!tail.contains('\x1b'));
        assert!(tail.starts_with("line 61"));
        assert!(!tail.contains("abc123secretvalue"), "{tail}");
    }

    #[test]
    fn encuentra_scripts_y_urls_de_convex() {
        let html = r#"<html><head><script type="module" crossorigin src="/assets/index-abc.js"></script>
            <link rel="modulepreload" href="/assets/vendor-1.js"><script src="https://www.googletagmanager.com/gtag.js"></script>
            <script src="_next/static/chunks/main.js?v=2"></script></head></html>"#;
        assert_eq!(
            scripts_of(html, "https://my-app.com/some/page"),
            vec!["https://my-app.com/assets/index-abc.js", "https://my-app.com/assets/vendor-1.js", "https://my-app.com/_next/static/chunks/main.js?v=2"]
        );
        let js = r#"const u="https://happy-otter-123.convex.cloud";fetch("https://api.example.com");x("https://convex.my-app.com")"#;
        assert_eq!(convex_urls(js, &["https://convex.my-app.com/".into()]), vec!["https://convex.my-app.com", "https://happy-otter-123.convex.cloud"]);
    }

    fn env(name: &str, kind: EnvKind, url: &str) -> Environment {
        Environment { name: name.into(), kind, label: None, convex_url: Some(url.into()), convex_site_url: None, convex_deployment: None, frontend_url: None, branch: None, credential: None, created_at: 0, project: None }
    }

    #[test]
    fn dice_si_apunta_al_convex_que_corresponde() {
        let envs = vec![env("dev", EnvKind::CloudDev, "https://dev-1.convex.cloud"), env("prod", EnvKind::CloudProd, "https://prod-2.convex.cloud")];
        let read = |found: &[&str]| json!({ "status": "read", "found": found });
        assert_eq!(verdict(&read(&["https://prod-2.convex.cloud"]), Some(&envs[1]), &envs)["status"], "ok");
        let wrong = verdict(&read(&["https://dev-1.convex.cloud"]), Some(&envs[1]), &envs);
        assert_eq!(wrong["status"], "mismatch");
        assert_eq!(wrong["matches"][0]["env"], "dev");
        assert_eq!(verdict(&read(&["https://dev-1.convex.cloud", "https://prod-2.convex.cloud"]), Some(&envs[1]), &envs)["status"], "mixed");
        assert_eq!(verdict(&read(&[]), Some(&envs[1]), &envs)["status"], "not-found");
        assert_eq!(verdict(&read(&["https://x.convex.cloud"]), None, &envs)["status"], "unknown-env");
        let protected = json!({ "status": "protected" });
        assert_eq!(verdict(&protected, Some(&envs[1]), &envs)["status"], "protected");
    }

    #[test]
    fn el_sitio_de_produccion_es_el_dominio_publico_del_entorno() {
        let mut prod = env("prod", EnvKind::CloudProd, "https://prod-2.convex.cloud");
        assert_eq!(site_for("https://a.vercel.app", Some(&prod)), "https://a.vercel.app");
        prod.frontend_url = Some("https://my-app.com".into());
        assert_eq!(site_for("https://a.vercel.app", Some(&prod)), "https://my-app.com");
        prod.frontend_url = Some("http://localhost:3000".into());
        assert_eq!(site_for("https://a.vercel.app", Some(&prod)), "https://a.vercel.app");
    }

    #[tokio::test]
    async fn lee_la_url_de_convex_del_sitio_publicado() {
        use axum::{http::StatusCode, routing::get, Router};
        let app = Router::new()
            .route("/", get(|| async { axum::response::Html(r#"<script type="module" src="/assets/index-1.js"></script>"#) }))
            .route("/assets/index-1.js", get(|| async { r#"const c=new ConvexReactClient("https://dev-1.convex.cloud");"# }))
            .route("/locked", get(|| async { (StatusCode::UNAUTHORIZED, "login") }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let envs = vec![env("dev", EnvKind::CloudDev, "https://dev-1.convex.cloud"), env("prod", EnvKind::CloudProd, "https://prod-2.convex.cloud")];
        let scan = scan_cached(format!("{base}/"), "https://a.vercel.app".into(), vec![]).await;
        assert_eq!(scan["found"][0], "https://dev-1.convex.cloud", "{scan}");
        // Producción leyendo el Convex de desarrollo: no coincide, y dice cuál es.
        let v = verdict(&scan, Some(&envs[1]), &envs);
        assert_eq!(v["status"], "mismatch");
        assert_eq!(v["matches"][0]["env"], "dev");
        let locked = scan_cached(format!("{base}/locked"), "https://b.vercel.app".into(), vec![]).await;
        assert_eq!(locked["status"], "protected");
    }
}
