// Proyectos: cada repo de Convex que el devtool maneja. Un proyecto tiene su
// carpeta, el proyecto de Convex al que corresponde (si se sabe), un bloque de
// puertos para su instancia local (backend, sitio y frontend) y el comando de
// su frontend. `heskala-erp` es el primero y conserva sus rutas y nombres de
// procesos de siempre (modo Docker/nativo, recetas propias).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::home;
use crate::state::Shared;
use crate::store::now_ms;

pub const DEFAULT: &str = "heskala-erp";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub name: String,
    pub title: String,
    pub repo: PathBuf,
    /// Proyecto de Convex (slug), si se detectó del `.env.local`.
    #[serde(default)]
    pub convex_project: Option<String>,
    #[serde(default)]
    pub convex_team: Option<String>,
    pub backend_port: u16,
    pub frontend_port: u16,
    /// Comando del frontend con `{port}`; vacío = sin frontend.
    #[serde(default)]
    pub frontend: Vec<String>,
    #[serde(default)]
    pub frontend_kind: Option<String>,
    pub created_at: i64,
}

impl Project {
    pub fn is_default(&self) -> bool {
        self.name == DEFAULT
    }

    /// Proceso del daemon de un servicio (`backend`, `deployer`, `frontend`).
    pub fn proc(&self, service: &str) -> String {
        match (self.is_default(), service) {
            (true, "frontend") => "vite".into(),
            (true, "backend") => crate::native::BACKEND.into(),
            (true, "deployer") => crate::native::DEPLOYER.into(),
            (_, s) => format!("{}.{s}", self.name),
        }
    }

    pub fn native_dir(&self) -> PathBuf {
        home().join("native").join(if self.is_default() { "local" } else { &self.name })
    }

    pub fn env_file(&self) -> PathBuf {
        home().join("native").join(if self.is_default() { "local.env".to_string() } else { format!("{}.env", self.name) })
    }

    pub fn instance(&self) -> String {
        if self.is_default() {
            "heskala-local".into()
        } else {
            format!("{}-local", self.name)
        }
    }

    pub fn mirror_dir(&self) -> PathBuf {
        home().join("native").join(if self.is_default() { "deployer-root".to_string() } else { format!("{}-root", self.name) })
    }
}

fn valid_name(name: &str) -> bool {
    (2..=40).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `# team: heskala, project: heskala-crm` (lo que deja `convex dev` en .env.local).
pub fn convex_project_of(repo: &Path) -> (Option<String>, Option<String>) {
    let Ok(text) = std::fs::read_to_string(repo.join(".env.local")) else { return (None, None) };
    for line in text.lines() {
        if let Some(comment) = line.split('#').nth(1) {
            let mut team = None;
            let mut project = None;
            for part in comment.split(',') {
                let (k, v) = part.split_once(':').map(|(k, v)| (k.trim(), v.trim())).unwrap_or(("", ""));
                match k {
                    "team" => team = Some(v.to_string()),
                    "project" => project = Some(v.to_string()),
                    _ => {}
                }
            }
            if project.is_some() {
                return (team, project);
            }
        }
    }
    (None, None)
}

/// Comando del frontend según el package.json (Vite o Next); vacío si no se reconoce.
pub fn detect_frontend(repo: &Path) -> (Vec<String>, Option<String>) {
    let pkg: Value = std::fs::read_to_string(repo.join("package.json")).ok().and_then(|r| serde_json::from_str(&r).ok()).unwrap_or(Value::Null);
    let has = |dep: &str| pkg["dependencies"][dep].is_string() || pkg["devDependencies"][dep].is_string();
    let cmd = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    if has("next") {
        (cmd(&["node_modules/.bin/next", "dev", "-p", "{port}"]), Some("next".into()))
    } else if has("vite") {
        (cmd(&["node_modules/.bin/vite", "--port", "{port}", "--strictPort"]), Some("vite".into()))
    } else {
        (vec![], None)
    }
}

/// El proyecto por defecto, con los puertos de siempre.
pub fn ensure_default(state: &Shared) -> Result<()> {
    if state.store.project(DEFAULT)?.is_some() {
        return Ok(());
    }
    // Sin checkout del ERP no hay proyecto por defecto: todos se agregan igual.
    let Some(repo) = state.config.repo.clone() else { return Ok(()) };
    let (team, convex_project) = convex_project_of(&repo);
    state.store.save_project(&Project {
        name: DEFAULT.into(),
        title: "Heskala ERP".into(),
        convex_project: convex_project.or_else(|| Some("heskala-erp".into())),
        convex_team: team.or_else(|| Some("heskala".into())),
        backend_port: 3210,
        frontend_port: crate::services::VITE_PORT,
        frontend: vec!["node_modules/.bin/vite".into(), "--port".into(), "{port}".into(), "--strictPort".into()],
        frontend_kind: Some("vite".into()),
        repo,
        created_at: now_ms(),
    })
}

/// Registra un repo como proyecto: nombre, puertos libres y frontend detectado.
pub fn register(state: &Shared, repo: &Path, name: Option<&str>) -> Result<Project> {
    let repo = repo.canonicalize().with_context(|| format!("{} doesn't exist", repo.display()))?;
    if !repo.join("convex").is_dir() || !repo.join("package.json").is_file() {
        bail!("{} doesn't look like a Convex project (missing convex/ or package.json)", repo.display());
    }
    let existing = state.store.projects()?;
    if let Some(p) = existing.iter().find(|p| p.repo == repo) {
        bail!("that folder is already project “{}”", p.name);
    }
    let (team, convex_project) = convex_project_of(&repo);
    let default_name = convex_project
        .clone()
        .unwrap_or_else(|| repo.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default())
        .replace(['_', ' ', '.'], "-");
    let name = name.map(str::to_string).unwrap_or(default_name);
    if !valid_name(&name) {
        bail!("invalid name “{name}”: lowercase letters, digits and hyphens, starting with a letter");
    }
    if existing.iter().any(|p| p.name == name) {
        bail!("project “{name}” already exists");
    }
    // Bloques de 10 desde 3220 para el backend; frontend desde 3001.
    let used_backend: Vec<u16> = existing.iter().map(|p| p.backend_port).collect();
    let used_front: Vec<u16> = existing.iter().map(|p| p.frontend_port).collect();
    let backend_port = (0..100).map(|i| 3220 + i * 10).find(|p| !used_backend.contains(p)).unwrap_or(3990);
    let frontend_port = (3001..3100).find(|p| !used_front.contains(p)).unwrap_or(3099);
    let (frontend, frontend_kind) = detect_frontend(&repo);
    let title = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| name.clone());
    let project = Project { name, title, repo, convex_project, convex_team: team, backend_port, frontend_port, frontend, frontend_kind, created_at: now_ms() };
    state.store.save_project(&project)?;
    ensure_envs(state, &project)?;
    Ok(project)
}

/// Repo donde corren la CLI y los trabajos de un entorno (el de su proyecto).
pub fn repo_for(state: &Shared, env: Option<&devtool_core::Environment>) -> PathBuf {
    env.and_then(|e| e.project.as_deref())
        .and_then(|name| state.store.project(name).ok().flatten())
        .map(|p| p.repo)
        .unwrap_or_else(|| state.config.workdir())
}

/// Proyecto de un entorno, por su nombre (`None` = heskala-erp).
pub fn of_env(env: &devtool_core::Environment) -> &str {
    env.project.as_deref().unwrap_or(DEFAULT)
}

/// ¿El trabajo es de `project`? Por su entorno o, en las recetas project-*, por su argumento.
pub fn job_in(state: &Shared, job: &Value, project: &str) -> bool {
    if let Some(p) = job["args"]["project"].as_str() {
        return p == project;
    }
    let env = job["env"].as_str().unwrap_or_default();
    match state.store.environment(env).ok().flatten() {
        Some(e) => of_env(&e) == project,
        // Entorno borrado (desechable destruido): del ERP.
        None => project == DEFAULT,
    }
}

/// Trabajos recientes de un proyecto (o todos, sin proyecto).
pub fn jobs_of(state: &Shared, statuses: &[&str], limit: usize, project: Option<&str>) -> Result<Vec<Value>> {
    let Some(project) = project.filter(|p| !p.is_empty()) else {
        return state.store.jobs(statuses, limit);
    };
    let mut list = state.store.jobs(statuses, (limit * 5).min(500))?;
    list.retain(|j| job_in(state, j, project));
    list.truncate(limit);
    Ok(list)
}

/// Rama de HEAD leyendo `.git/HEAD` (sin lanzar git: al arrancar, un git
/// trabado esperando un permiso de macOS dejaba al daemon sin escuchar).
fn current_branch(repo: &Path) -> Option<String> {
    let dot = repo.join(".git");
    let dir = if dot.is_file() {
        // Worktree o submódulo: `.git` apunta al directorio real.
        let text = std::fs::read_to_string(&dot).ok()?;
        let path = text.trim().strip_prefix("gitdir:")?.trim().to_string();
        let p = PathBuf::from(&path);
        if p.is_absolute() { p } else { repo.join(p) }
    } else {
        dot
    };
    let head = std::fs::read_to_string(dir.join("HEAD")).ok()?;
    head.trim().strip_prefix("ref: refs/heads/").map(str::to_string).filter(|b| !b.is_empty())
}

/// Entornos de un proyecto que no es heskala-erp: `<nombre>-local` (su backend
/// nativo) y `<nombre>-dev` (su cloud dev, si el .env.local lo dice). No pisa
/// entornos de otro proyecto con el mismo nombre.
pub fn ensure_envs(state: &Shared, p: &Project) -> Result<Vec<String>> {
    use devtool_core::{Credential, EnvKind, Environment};
    if p.is_default() {
        return Ok(vec![]);
    }
    let branch = current_branch(&p.repo).or_else(|| Some("main".into()));
    let mut wanted = vec![Environment {
        name: format!("{}-local", p.name),
        kind: EnvKind::Devbox,
        label: Some(format!("{} · local", p.title)),
        convex_url: Some(format!("http://127.0.0.1:{}", p.backend_port)),
        convex_site_url: Some(format!("http://127.0.0.1:{}", p.backend_port + 1)),
        convex_deployment: None,
        frontend_url: (!p.frontend.is_empty()).then(|| format!("http://localhost:{}", p.frontend_port)),
        branch: branch.clone(),
        credential: Some(Credential::EnvFile { path: p.env_file().display().to_string(), key: "CONVEX_SELF_HOSTED_ADMIN_KEY".into() }),
        created_at: now_ms(),
        project: Some(p.name.clone()),
    }];
    if let Some(dev) = crate::seed::cloud_dev_of(&p.repo) {
        wanted.push(Environment {
            name: format!("{}-dev", p.name),
            kind: EnvKind::CloudDev,
            label: Some(format!("{} · cloud dev ({dev})", p.title)),
            convex_url: Some(format!("https://{dev}.convex.cloud")),
            convex_site_url: Some(format!("https://{dev}.convex.site")),
            convex_deployment: Some(dev),
            frontend_url: None,
            branch,
            credential: Some(Credential::ConvexLogin),
            created_at: now_ms(),
            project: Some(p.name.clone()),
        });
    }
    let mut names = vec![];
    for mut env in wanted {
        match state.store.environment(&env.name)? {
            Some(old) if old.project.as_deref() != Some(p.name.as_str()) => {
                tracing::warn!("environment “{}” already exists and isn't {}'s: leaving it alone", env.name, p.name);
                continue;
            }
            Some(old) => env.created_at = old.created_at,
            None => {}
        }
        state.store.upsert_environment(&env)?;
        names.push(env.name);
    }
    Ok(names)
}

/// Un entorno cloud (cloud dev o producción) a partir de su deploy key: `dev:<deployment>|…` o
/// `prod:<deployment>|…`. El nombre por defecto es `<proyecto>-dev` / `<proyecto>-prod` (`dev` /
/// `prod` en el proyecto por defecto). Si ya hay un entorno con ese nombre tiene que ser del mismo
/// proyecto y deployment (entonces solo se guarda la key).
pub fn add_cloud_env(state: &Shared, p: &Project, key: &str, name: Option<String>) -> Result<devtool_core::Environment> {
    use devtool_core::{EnvKind, Environment};
    let target = key.split('|').next().unwrap_or_default();
    let (kind, deployment) = match (target.strip_prefix("prod:"), target.strip_prefix("dev:")) {
        (Some(d), _) => (EnvKind::CloudProd, d),
        (_, Some(d)) => (EnvKind::CloudDev, d),
        _ => anyhow::bail!("that doesn't look like a Convex deploy key (“prod:<deployment>|…” or “dev:<deployment>|…”)"),
    };
    if deployment.is_empty() || !deployment.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        anyhow::bail!("that doesn't look like a Convex deploy key");
    }
    let prod = kind == EnvKind::CloudProd;
    let name = name.unwrap_or_else(|| match (p.is_default(), prod) {
        (true, true) => "prod".into(),
        (true, false) => "dev".into(),
        (false, true) => format!("{}-prod", p.name),
        (false, false) => format!("{}-dev", p.name),
    });
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        anyhow::bail!("the environment's name can only have letters, numbers, - and _");
    }
    let env = match state.store.environment(&name)? {
        Some(old) => {
            let same_project = old.project.as_deref().unwrap_or(DEFAULT) == p.name;
            if !same_project || old.kind != kind || old.convex_deployment.as_deref().is_some_and(|d| d != deployment) {
                anyhow::bail!("“{name}” already exists for another project or deployment: give the new one another name");
            }
            old
        }
        None => {
            let env = Environment {
                name: name.clone(),
                kind,
                label: Some(format!("{} · {} ({deployment})", p.title, if prod { "production" } else { "cloud dev" })),
                convex_url: Some(format!("https://{deployment}.convex.cloud")),
                convex_site_url: Some(format!("https://{deployment}.convex.site")),
                convex_deployment: Some(deployment.to_string()),
                frontend_url: None,
                branch: Some(if prod { "main".into() } else { current_branch(&p.repo).unwrap_or_else(|| "main".into()) }),
                credential: None,
                created_at: now_ms(),
                project: (!p.is_default()).then(|| p.name.clone()),
            };
            state.store.upsert_environment(&env)?;
            env
        }
    };
    crate::secrets::check_deploy_key(&env, key)?;
    crate::secrets::set(&crate::secrets::deploy_account(&env), key.trim())?;
    Ok(env)
}

/// Al arrancar: entornos de todos los proyectos (puertos o cloud dev pudieron cambiar).
pub fn ensure_all_envs(state: &Shared) {
    for p in state.store.projects().unwrap_or_default() {
        if let Err(e) = ensure_envs(state, &p) {
            tracing::warn!("environments of {}: {e:#}", p.name);
        }
    }
}

pub fn get(state: &Shared, name: Option<&str>) -> Result<Project> {
    let name = name.filter(|n| !n.is_empty()).unwrap_or(DEFAULT);
    state.store.project(name)?.with_context(|| format!("Project “{name}” doesn't exist (see list_projects)"))
}

/// `~/x` → ruta absoluta.
pub fn expand(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
        None => path.to_string(),
    }
}

/// Proyectos con sus URLs locales, para la app, la API y el MCP.
pub fn list_json(state: &Shared) -> Result<Value> {
    let list: Vec<Value> = state
        .store
        .projects()?
        .into_iter()
        .map(|p| to_json(&p))
        .collect();
    Ok(Value::Array(list))
}

/// Un proyecto con sus URLs locales y su carpeta de datos.
pub fn to_json(p: &Project) -> Value {
    let backend = crate::native::backend_port(p);
    let mut v = serde_json::to_value(p).unwrap_or(Value::Null);
    v["default"] = Value::Bool(p.is_default());
    v["backendUrl"] = Value::String(format!("http://127.0.0.1:{backend}"));
    v["siteUrl"] = Value::String(format!("http://127.0.0.1:{}", backend + 1));
    v["frontendUrl"] = if p.frontend.is_empty() { Value::Null } else { Value::String(format!("http://localhost:{}", p.frontend_port)) };
    v["dataDir"] = Value::String(p.native_dir().display().to_string());
    v["cloudDev"] = crate::seed::cloud_dev_of(&p.repo).map(Value::String).unwrap_or(Value::Null);
    v
}

/// Quita un proyecto (no el por defecto): detiene sus procesos y conserva sus datos.
pub async fn remove(state: &Shared, name: &str) -> Result<Value> {
    if name == DEFAULT {
        bail!("heskala-erp can't be removed");
    }
    let project = get(state, Some(name))?;
    for svc in ["frontend", "deployer", "backend"] {
        crate::procs::stop(state, &project.proc(svc)).await?;
    }
    state.store.remove_project(name)?;
    for env in state.store.environments()? {
        if env.project.as_deref() == Some(name) {
            state.store.remove_environment(&env.name)?;
        }
    }
    Ok(serde_json::json!({ "removed": name, "dataKept": project.native_dir().display().to_string() }))
}

/// Repos candidatos en `root` (por defecto, la carpeta padre de heskala-erp),
/// marcando los que ya son proyectos.
pub fn discover_json(state: &Shared, root: Option<&str>) -> Value {
    let root = root.map(|r| PathBuf::from(expand(r))).unwrap_or_else(|| {
        // La carpeta padre de heskala-erp, o ~/Documents.
        state.config.repo.as_ref().and_then(|r| r.parent()).map(Path::to_path_buf).unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("Documents"))
    });
    let registered = state.store.projects().unwrap_or_default();
    let list: Vec<Value> = discover(&root)
        .into_iter()
        .map(|mut v| {
            let path = PathBuf::from(v["path"].as_str().unwrap_or_default());
            v["project"] = registered.iter().find(|p| p.repo == path).map(|p| Value::String(p.name.clone())).unwrap_or(Value::Null);
            v
        })
        .collect();
    serde_json::json!({ "root": root.display().to_string(), "candidates": list })
}

/// Repos con `convex/` y `package.json` en `root` (un nivel), con su proyecto de Convex.
pub fn discover(root: &Path) -> Vec<Value> {
    let mut out = vec![];
    let Ok(entries) = std::fs::read_dir(root) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.join("convex").is_dir() && path.join("package.json").is_file() {
            let (team, project) = convex_project_of(&path);
            let (_, kind) = detect_frontend(&path);
            out.push(serde_json::json!({ "path": path.display().to_string(), "convexProject": project, "convexTeam": team, "frontend": kind }));
        }
    }
    out.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lee_el_proyecto_de_convex() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env.local"), "CONVEX_DEPLOYMENT=dev:beaming-clownfish-557 # team: heskala, project: heskala-logistic-saas\n").unwrap();
        let (team, project) = convex_project_of(dir.path());
        assert_eq!(team.as_deref(), Some("heskala"));
        assert_eq!(project.as_deref(), Some("heskala-logistic-saas"));
        std::fs::write(dir.path().join("package.json"), r#"{"dependencies":{"next":"15"}}"#).unwrap();
        assert_eq!(detect_frontend(dir.path()).1.as_deref(), Some("next"));
        assert!(valid_name("heskala-crm"));
        assert!(!valid_name("Heskala"));
    }
}
