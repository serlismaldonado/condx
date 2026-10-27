// Trabajos (§5): cada operación es un trabajo con id, pasos, log, resultado,
// quién lo pidió y quién lo aprobó. Un trabajo de escritura por entorno a la
// vez; cancelar corta el grupo de procesos del paso en curso; reintentar
// sigue desde el paso que falló (los pasos `setup` vuelven a correr y los
// `finally` corren siempre).

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use devtool_core::{redact, Credential, EnvKind, Environment};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{watch, Notify};

use crate::cmd::{command, run};
use crate::config::home;
use crate::probes::convex::target;
use crate::recipes::{self, Approval, Cwd, Recipe, Step, NEW_ENV};
use crate::state::Shared;
use crate::store::now_ms;

/// Una aprobación pendiente vence a los 30 min.
pub const APPROVAL_TTL_MS: i64 = 30 * 60_000;
const BACKEND_IMAGE: &str = "ghcr.io/get-convex/convex-backend:latest";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    WaitingApproval,
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Rejected,
    Expired,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::WaitingApproval => "waiting-approval",
            Status::Queued => "queued",
            Status::Running => "running",
            Status::Succeeded => "succeeded",
            Status::Failed => "failed",
            Status::Cancelled => "cancelled",
            Status::Rejected => "rejected",
            Status::Expired => "expired",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Pending,
    Running,
    Ok,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepState {
    pub name: String,
    pub status: StepStatus,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub recipe: String,
    pub title: String,
    pub env: String,
    pub env_kind: String,
    pub args: HashMap<String, String>,
    pub commit: Option<String>,
    pub requested_by: String,
    pub approval: Approval,
    pub approved_by: Option<String>,
    pub approved_at: Option<i64>,
    pub status: Status,
    pub steps: Vec<StepState>,
    pub retry_from: usize,
    pub attempts: u32,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub error: Option<String>,
    /// Lo que un paso deja para los siguientes (y para un reintento).
    #[serde(default)]
    pub outputs: HashMap<String, String>,
    /// Precondiciones evaluadas al pedirlo (promoción a producción).
    #[serde(default)]
    pub preconditions: Vec<Value>,
}

#[derive(Default)]
pub struct Runner {
    locked_envs: Mutex<HashSet<String>>,
    cancels: Mutex<HashMap<String, watch::Sender<bool>>>,
    wake: Notify,
}

fn new_id() -> String {
    let mut bytes = [0u8; 3];
    use std::io::Read;
    let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes));
    format!("j{}{:02x}{:02x}{:02x}", now_ms(), bytes[0], bytes[1], bytes[2])
}

pub fn backups_dir() -> PathBuf {
    crate::config::backups_dir()
}

fn log_path(id: &str) -> PathBuf {
    home().join("jobs").join(format!("{id}.log"))
}

/// `YYYYMMDD-HHMMSS` en UTC (para nombres de archivo).
pub fn stamp(ms: i64) -> String {
    let secs = ms / 1000;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Fecha civil desde días epoch (algoritmo de Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

pub(crate) fn save(state: &Shared, job: &Job) {
    let value = serde_json::to_value(job).unwrap_or(Value::Null);
    if let Err(e) = state.store.save_job(&job.id, job.created_at, job.status.as_str(), &value) {
        tracing::warn!("couldn't save job {}: {e}", job.id);
    }
}

pub fn load(state: &Shared, id: &str) -> Result<Job> {
    let value = state.store.job(id)?.ok_or_else(|| anyhow!("Job {id} doesn't exist"))?;
    Ok(serde_json::from_value(value)?)
}

pub(crate) fn log_line(id: &str, line: &str) {
    let path = log_path(id);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}", redact(line));
    }
}

/// Log del trabajo desde `offset` (bytes); devuelve el texto y el nuevo offset.
pub fn read_log(id: &str, offset: u64) -> Result<(String, u64)> {
    use std::io::{Read, Seek, SeekFrom};
    let path = log_path(id);
    let Ok(mut f) = std::fs::File::open(&path) else { return Ok((String::new(), 0)) };
    let len = f.metadata()?.len();
    let start = offset.min(len);
    // Más de 200 KB por pedido no sirve en una pantalla.
    let start = start.max(len.saturating_sub(200_000));
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok((String::from_utf8_lossy(&buf).into_owned(), len))
}

fn valid_env_name(name: &str) -> bool {
    let ok_chars = name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    (2..=30).contains(&name.len()) && ok_chars && name.starts_with(|c: char| c.is_ascii_lowercase())
}

pub(crate) async fn resolve_commit(repo: &Path, reference: &str) -> Result<String> {
    let out = run("git", &["rev-parse", "--verify", "--quiet", &format!("{reference}^{{commit}}")], Some(repo), &[], Duration::from_secs(10)).await;
    if !out.ok {
        bail!("couldn't find commit “{reference}” in the repo");
    }
    Ok(out.stdout.trim().to_string())
}

/// Pide un trabajo. Valida receta, entorno, política y argumentos; queda en
/// cola o esperando aprobación.
/// Por qué un trabajo pide la aprobación que pide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Why {
    /// Lo que dice la receta.
    Recipe,
    /// Una query solo lee: no pide aprobación si Configuración deja leer los datos del entorno.
    Query,
    /// Una regla de Configuración (Datos y cambios → Cambios).
    Settings,
}

/// La aprobación de una receta en un entorno existente: la de la receta, salvo que corra una
/// query (solo lee: sin aprobación si las reglas dejan leer ese tipo de entorno) o que una
/// regla de Configuración la cambie para las recetas que modifican el entorno.
pub async fn approval_for(state: &Shared, recipe: &Recipe, env: &Environment, args: &Value) -> (Approval, Why) {
    let kind = env.kind.as_str();
    let base = recipe.approval_for(kind);
    if base == Approval::Forbidden {
        return (base, Why::Recipe);
    }
    let policy = crate::policy::load(state);
    let function = recipe.args.iter().find(|a| a.kind == recipes::ArgType::Function).and_then(|a| args[a.name.as_str()].as_str());
    if let Some(name) = function {
        let list = crate::functions::functions(state, env, false).await.ok();
        let fkind = list.as_ref().and_then(|l| l["functions"].as_array()?.iter().find(|f| f["identifier"] == name)?["kind"].as_str().map(str::to_string));
        if fkind.as_deref() == Some("query") && policy.can_read(env.kind) {
            return (Approval::None, Why::Query);
        }
    }
    if !recipe.writes {
        return (base, Why::Recipe);
    }
    let tightened = policy.write_approval(kind, base);
    (tightened, if tightened == base { Why::Recipe } else { Why::Settings })
}

pub async fn request(state: &Shared, recipe_name: &str, env_name: &str, args: &Value, requested_by: &str) -> Result<Job> {
    let recipe = recipes::get(recipe_name).ok_or_else(|| anyhow!("Unknown recipe: {recipe_name} (see list_recipes)"))?;
    let (env_kind, approval) = if recipe.kinds.iter().any(|k| k == NEW_ENV) {
        if !valid_env_name(env_name) {
            bail!("invalid environment name: lowercase letters, digits and hyphens (2–30), starting with a letter");
        }
        if state.store.environment(env_name)?.is_some() {
            bail!("environment “{env_name}” already exists");
        }
        (NEW_ENV.to_string(), recipe.approval_for(NEW_ENV))
    } else {
        let env = state.store.environment(env_name)?.ok_or_else(|| anyhow!("Environment “{env_name}” doesn't exist"))?;
        // Producción solo se toca desde la Mac (con la persona y Touch ID), nunca desde el sitio en la nube.
        if crate::site::is_cloud() && env.kind == devtool_core::EnvKind::CloudProd {
            bail!("production is only changed from the Mac");
        }
        let kind = env.kind.as_str().to_string();
        (kind, approval_for(state, recipe, &env, args).await.0)
    };
    let policy = crate::policy::load(state);
    if approval == Approval::Forbidden && policy.is_blocked(&env_kind) && recipe.writes && recipe.approval_for(&env_kind) != Approval::Forbidden {
        bail!("changes to {env_kind} environments are blocked in Condx settings (Data and changes)");
    }
    if approval == Approval::Forbidden {
        bail!(
            "“{recipe_name}” doesn't run on {env_kind} environments (see list_recipes: cloud dev deploys with deploy-cloud; production only changes with promote-prod, rollback-prod or an import with Touch ID)"
        );
    }
    // Entornos de otro proyecto: solo las recetas genéricas, y su `project` sale del entorno.
    let env_project = state.store.environment(env_name)?.and_then(|e| e.project);
    let mut args = args.clone();
    if let Some(p) = &env_project {
        if !recipe.projects {
            bail!("“{recipe_name}” is only for heskala-erp; {p}'s environments run the recipes with projects = true (see list_recipes)");
        }
        if recipe.args.iter().any(|a| a.name == "project") {
            match args["project"].as_str() {
                Some(given) if given != p => bail!("environment {env_name} belongs to project {p}, not {given}"),
                _ => {
                    if !args.is_object() {
                        args = serde_json::json!({});
                    }
                    args["project"] = serde_json::json!(p);
                }
            }
        }
    }
    let args = &args;
    let mut resolved = recipe.resolve_args(args, &backups_dir())?;
    if let Some(name) = resolved.get("project") {
        if crate::projects::get(state, Some(name))?.is_default() {
            bail!("“{recipe_name}” is for other projects; heskala-erp uses its own recipes");
        }
    }
    let mut commit = None;
    if let Some(reference) = resolved.get("commit").cloned() {
        // «previous»: el despliegue anterior registrado en ese entorno (reversión).
        let reference = if reference == "previous" {
            let list = state.store.deployments(env_name, 2)?;
            list.get(1).map(|d| d.commit.clone()).ok_or_else(|| anyhow!("no previous deploy recorded on {env_name}"))?
        } else {
            reference
        };
        let repo = crate::projects::repo_for(state, state.store.environment(env_name)?.as_ref());
        let sha = resolve_commit(&repo, &reference).await?;
        resolved.insert("commit".into(), sha.clone());
        commit = Some(sha);
    }
    let mut preconditions = vec![];
    if recipe.preconditions {
        let env = state.store.environment(env_name)?.ok_or_else(|| anyhow!("Environment “{env_name}” doesn't exist"))?;
        let sha = commit.clone().ok_or_else(|| anyhow!("the promotion needs a commit"))?;
        preconditions = crate::pipeline::preconditions(state, &env, &sha).await;
        let failed: Vec<String> = preconditions
            .iter()
            .filter(|p| p["ok"] != true)
            .map(|p| format!("{}: {}", p["label"].as_str().unwrap_or("?"), p["detail"].as_str().unwrap_or("")))
            .collect();
        if !failed.is_empty() {
            bail!("Promotion preconditions not met:\n- {}", failed.join("\n- "));
        }
    }
    // La versión en el título (actualizar o publicar Condx): la tarjeta de aprobación dice cuál.
    let title = match resolved.get("version").map(|v| v.as_str()).filter(|v| !v.is_empty()) {
        Some(v) => format!("{} {v}", recipe.title),
        None => recipe.title.clone(),
    };
    let job = Job {
        id: new_id(),
        recipe: recipe.name.clone(),
        title,
        env: env_name.to_string(),
        env_kind,
        args: resolved,
        commit,
        requested_by: requested_by.to_string(),
        approval,
        approved_by: None,
        approved_at: None,
        status: if approval == Approval::None { Status::Queued } else { Status::WaitingApproval },
        steps: recipe
            .steps
            .iter()
            .map(|s| StepState { name: s.name.clone(), status: StepStatus::Pending, started_at: None, ended_at: None, message: None })
            .collect(),
        retry_from: 0,
        attempts: 0,
        created_at: now_ms(),
        started_at: None,
        ended_at: None,
        error: None,
        outputs: HashMap::new(),
        preconditions,
    };
    log_line(&job.id, &format!("Requested by {requested_by}: {} on {} {:?}", recipe.name, job.env, job.args));
    save(state, &job);
    if job.status == Status::WaitingApproval {
        crate::notify::approval_waiting(state, &job.title, &job.env, requested_by);
    }
    state.runner.wake.notify_one();
    Ok(job)
}

/// Lo que hay que escribir para aprobar: el proyecto, si lo hay, o el entorno.
pub fn confirm_target(job: &Job) -> &str {
    job.args.get("project").map(String::as_str).unwrap_or(job.env.as_str())
}

/// Aprueba o rechaza. `typed` exige el nombre del entorno; `reinforced`
/// (Touch ID) llega con F3.
pub fn decide(state: &Shared, id: &str, approve: bool, confirm: Option<&str>, touch_id: bool, by: &str) -> Result<Job> {
    let mut job = load(state, id)?;
    if job.status != Status::WaitingApproval {
        bail!("Job {id} isn't waiting for approval (status {})", job.status.as_str());
    }
    if now_ms() - job.created_at > APPROVAL_TTL_MS {
        job.status = Status::Expired;
        job.ended_at = Some(now_ms());
        save(state, &job);
        bail!("The approval expired (30 min); request the job again");
    }
    if approve {
        match job.approval {
            // Con un proyecto (recetas project-*), se confirma su nombre en vez del entorno.
            Approval::Typed | Approval::Reinforced if confirm != Some(confirm_target(&job)) => {
                bail!("Type “{}” to approve", confirm_target(&job))
            }
            // Touch ID lo pide la app (LocalAuthentication); el daemon confía en su token.
            Approval::Reinforced if !touch_id => bail!("This approval needs Touch ID in the app"),
            _ => {}
        }
        job.status = Status::Queued;
        job.approved_by = Some(by.to_string());
        job.approved_at = Some(now_ms());
        log_line(id, &format!("Approved by {by}{}", if touch_id { " (Touch ID)" } else { "" }));
    } else {
        job.status = Status::Rejected;
        job.ended_at = Some(now_ms());
        log_line(id, &format!("Rejected by {by}"));
    }
    save(state, &job);
    state.runner.wake.notify_one();
    Ok(job)
}

pub fn cancel(state: &Shared, id: &str, by: &str) -> Result<Job> {
    let mut job = load(state, id)?;
    match job.status {
        Status::Running => {
            if let Some(tx) = state.runner.cancels.lock().unwrap().get(id) {
                let _ = tx.send(true);
            }
            log_line(id, &format!("Cancellation requested by {by}"));
        }
        Status::Queued | Status::WaitingApproval => {
            job.status = Status::Cancelled;
            job.ended_at = Some(now_ms());
            log_line(id, &format!("Cancelled by {by} before starting"));
            save(state, &job);
        }
        other => bail!("The job already finished ({})", other.as_str()),
    }
    Ok(job)
}

/// Vuelve a encolar un trabajo fallido o cancelado desde el paso que falló.
pub fn retry(state: &Shared, id: &str, by: &str) -> Result<Job> {
    let mut job = load(state, id)?;
    if !matches!(job.status, Status::Failed | Status::Cancelled) {
        bail!("Only failed or cancelled jobs can be retried");
    }
    let failed = job.steps.iter().position(|s| matches!(s.status, StepStatus::Failed | StepStatus::Pending | StepStatus::Running)).unwrap_or(0);
    // Si un paso anterior es punto de reanudación, se vuelve a empezar desde él.
    let from = recipes::get(&job.recipe)
        .and_then(|r| r.steps.iter().take(failed + 1).rposition(|s| s.checkpoint))
        .unwrap_or(failed);
    job.retry_from = from;
    for step in job.steps.iter_mut().skip(from) {
        step.status = StepStatus::Pending;
        step.message = None;
    }
    job.error = None;
    job.ended_at = None;
    // Lo que pedía aprobación la vuelve a pedir.
    job.status = if job.approval == Approval::None { Status::Queued } else { Status::WaitingApproval };
    job.created_at = if job.status == Status::WaitingApproval { now_ms() } else { job.created_at };
    log_line(id, &format!("— Retry requested by {by}, from “{}” —", job.steps.get(from).map(|s| s.name.as_str()).unwrap_or("?")));
    save(state, &job);
    if job.status == Status::WaitingApproval {
        crate::notify::approval_waiting(state, &job.title, &job.env, by);
    }
    state.runner.wake.notify_one();
    Ok(job)
}

/// Bucle que vence aprobaciones viejas y arranca lo que está en cola.
pub fn spawn_scheduler(state: &Shared) {
    let state = state.clone();
    tokio::spawn(async move {
        loop {
            let _ = tokio::time::timeout(Duration::from_secs(2), state.runner.wake.notified()).await;
            crate::pipeline::prune_backups(&state);
            for value in state.store.jobs(&["waiting-approval"], 200).unwrap_or_default() {
                if let Ok(mut job) = serde_json::from_value::<Job>(value) {
                    if now_ms() - job.created_at > APPROVAL_TTL_MS {
                        job.status = Status::Expired;
                        job.ended_at = Some(now_ms());
                        log_line(&job.id, "The approval expired");
                        save(&state, &job);
                    }
                }
            }
            let mut queued: Vec<Job> = state
                .store
                .jobs(&["queued"], 200)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|v| serde_json::from_value(v).ok())
                .collect();
            queued.sort_by_key(|j| j.created_at);
            for job in queued {
                let free = state.runner.locked_envs.lock().unwrap().insert(job.env.clone());
                if !free {
                    continue;
                }
                let st = state.clone();
                tokio::spawn(async move {
                    let env = job.env.clone();
                    let id = job.id.clone();
                    execute(&st, job).await;
                    st.runner.locked_envs.lock().unwrap().remove(&env);
                    st.runner.cancels.lock().unwrap().remove(&id);
                    st.runner.wake.notify_one();
                });
            }
        }
    });
}

/// Al arrancar: lo que quedó «running» de una corrida anterior del daemon falló.
pub fn recover(state: &Shared) {
    for value in state.store.jobs(&["running"], 200).unwrap_or_default() {
        if let Ok(mut job) = serde_json::from_value::<Job>(value) {
            job.status = Status::Failed;
            job.error = Some("The daemon restarted during the job".into());
            job.ended_at = Some(now_ms());
            for s in job.steps.iter_mut().filter(|s| s.status == StepStatus::Running) {
                s.status = StepStatus::Failed;
            }
            log_line(&job.id, "The daemon restarted during the job");
            save(state, &job);
        }
    }
}

pub(crate) struct Ctx {
    pub vars: HashMap<String, String>,
    pub convex_bin: (String, Vec<String>),
    pub convex_env: Vec<(String, String)>,
    /// Flags que apuntan la CLI al entorno (`--deployment x`), vía `{target}`.
    pub target_args: Vec<String>,
    pub target_error: Option<String>,
}

impl Ctx {
    /// Expande `{var}` y `{args.x}`; `{convex}` ocupa uno o dos elementos.
    fn argv(&self, template: &[String]) -> Result<Vec<String>> {
        let mut out = vec![];
        for part in template {
            if part == "{convex}" {
                out.push(self.convex_bin.0.clone());
                out.extend(self.convex_bin.1.iter().cloned());
                continue;
            }
            if part == "{target}" {
                out.extend(self.target_args.iter().cloned());
                continue;
            }
            // Lo sustituido no se vuelve a leer: un payload `{}` queda tal cual.
            let mut s = String::new();
            let mut rest = part.as_str();
            while let Some(start) = rest.find('{') {
                let end = rest[start..].find('}').map(|e| start + e).ok_or_else(|| anyhow!("unclosed template: {part}"))?;
                let key = &rest[start + 1..end];
                let value = self.vars.get(key).ok_or_else(|| anyhow!("unknown variable in recipe: {key}"))?;
                s.push_str(&rest[..start]);
                s.push_str(value);
                rest = &rest[end + 1..];
            }
            s.push_str(rest);
            out.push(s);
        }
        Ok(out)
    }
}

async fn execute(state: &Shared, mut job: Job) {
    let recipe = match recipes::get(&job.recipe) {
        Some(r) => r,
        None => return finish(state, &mut job, Status::Failed, Some("the recipe no longer exists".into())),
    };
    let (tx, mut cancel) = watch::channel(false);
    state.runner.cancels.lock().unwrap().insert(job.id.clone(), tx);
    job.status = Status::Running;
    job.attempts += 1;
    job.started_at.get_or_insert(now_ms());
    save(state, &job);
    log_line(&job.id, &format!("Attempt {} · {}", job.attempts, recipe.title));

    let env = state.store.environment(&job.env).ok().flatten();
    // Los entornos de otro proyecto corren en su repo y con su CLI.
    let repo_path = crate::projects::repo_for(state, env.as_ref());
    let mut ctx = Ctx { vars: HashMap::new(), convex_bin: crate::probes::convex::convex_bin_at(&repo_path), convex_env: vec![], target_args: vec![], target_error: None };
    let repo = repo_path.display().to_string();
    let _ = std::fs::create_dir_all(backups_dir());
    ctx.vars.insert("repo".into(), repo);
    ctx.vars.insert("home".into(), home().display().to_string());
    ctx.vars.insert("backups".into(), backups_dir().display().to_string());
    ctx.vars.insert("env".into(), job.env.clone());
    ctx.vars.insert("date".into(), stamp(now_ms()));
    ctx.vars.insert("worktree".into(), home().join("worktrees").join(&job.id).display().to_string());
    ctx.vars.insert("job".into(), job.id.clone());
    for (k, v) in &job.args {
        ctx.vars.insert(format!("args.{k}"), v.clone());
    }
    if let Some(env) = &env {
        let found = if recipe.deploy_key { crate::probes::convex::target_with_key(env, recipe.writes) } else { target(env, recipe.writes) };
        match found {
            Ok((args, vars)) => {
                ctx.target_args = args;
                ctx.convex_env = vars;
            }
            Err(e) => ctx.target_error = Some(format!("{e:#}")),
        }
    }

    for (k, v) in &job.outputs {
        ctx.vars.insert(format!("out.{k}"), v.clone());
    }
    let mut failure: Option<String> = None;
    let mut cancelled = false;
    for (i, step) in recipe.steps.iter().enumerate() {
        if step.finally {
            continue;
        }
        let rerun_setup = step.setup && i < job.retry_from;
        if i < job.retry_from && !rerun_setup {
            continue;
        }
        if failure.is_some() || cancelled {
            mark(&mut job, i, StepStatus::Skipped, None);
            continue;
        }
        if *cancel.borrow() {
            cancelled = true;
            mark(&mut job, i, StepStatus::Skipped, None);
            continue;
        }
        mark(&mut job, i, StepStatus::Running, None);
        save(state, &job);
        log_line(&job.id, &format!("▶ {}", step.name));
        match run_step(state, &mut job, env.as_ref(), recipe, step, &mut ctx, &mut cancel).await {
            Ok(msg) => {
                log_line(&job.id, &format!("✔ {}", step.name));
                // Lo que dijo el paso (un release: dónde se compiló y por qué) también va al log.
                for line in msg.iter().flat_map(|m| m.lines()).filter(|l| !l.trim().is_empty()) {
                    log_line(&job.id, &format!("  {line}"));
                }
                mark(&mut job, i, StepStatus::Ok, msg);
            }
            Err(e) if *cancel.borrow() => {
                log_line(&job.id, &format!("✖ {} (cancelled)", step.name));
                mark(&mut job, i, StepStatus::Failed, Some("cancelado".into()));
                let _ = e;
                cancelled = true;
            }
            Err(e) => {
                log_line(&job.id, &format!("✖ {}: {e:#}", step.name));
                mark(&mut job, i, StepStatus::Failed, Some(format!("{e:#}")));
                failure = Some(format!("{}: {e:#}", step.name));
            }
        }
        save(state, &job);
    }
    // `finally`: siempre, con un canal de cancelación propio.
    let (_keep, mut never) = watch::channel(false);
    for (i, step) in recipe.steps.iter().enumerate().filter(|(_, s)| s.finally) {
        mark(&mut job, i, StepStatus::Running, None);
        log_line(&job.id, &format!("▶ {} (always)", step.name));
        match run_step(state, &mut job, env.as_ref(), recipe, step, &mut ctx, &mut never).await {
            Ok(msg) => mark(&mut job, i, StepStatus::Ok, msg),
            Err(e) => {
                log_line(&job.id, &format!("✖ {}: {e:#}", step.name));
                mark(&mut job, i, StepStatus::Failed, Some(format!("{e:#}")));
            }
        }
    }
    let status = if cancelled {
        Status::Cancelled
    } else if failure.is_some() {
        Status::Failed
    } else {
        Status::Succeeded
    };
    finish(state, &mut job, status, failure);
}

fn mark(job: &mut Job, i: usize, status: StepStatus, message: Option<String>) {
    if let Some(s) = job.steps.get_mut(i) {
        match status {
            StepStatus::Running => {
                s.started_at = Some(now_ms());
                s.ended_at = None;
            }
            StepStatus::Pending => {}
            _ => s.ended_at = Some(now_ms()),
        }
        s.status = status;
        s.message = message.map(|m| redact(&m));
    }
}

fn finish(state: &Shared, job: &mut Job, status: Status, error: Option<String>) {
    job.status = status;
    job.error = error.map(|e| redact(&e));
    job.ended_at = Some(now_ms());
    log_line(&job.id, &format!("End: {}", status.as_str()));
    save(state, job);
}

async fn run_step(
    state: &Shared,
    job: &mut Job,
    env: Option<&Environment>,
    recipe: &Recipe,
    step: &Step,
    ctx: &mut Ctx,
    cancel: &mut watch::Receiver<bool>,
) -> Result<Option<String>> {
    let timeout = Duration::from_secs(step.timeout_secs.unwrap_or(600));
    if let Some(builtin) = &step.builtin {
        let res = builtin_step(state, job, env, builtin, ctx, timeout, cancel).await.map(Some);
        // Lo que dejó el paso queda disponible como `{out.x}` y sobrevive a un reintento.
        for (k, v) in &job.outputs {
            ctx.vars.insert(format!("out.{k}"), v.clone());
        }
        save(state, job);
        return res;
    }
    let argv = ctx.argv(step.run.as_ref().unwrap())?;
    let cwd = match step.cwd {
        Cwd::Repo => PathBuf::from(&ctx.vars["repo"]),
        Cwd::Worktree => PathBuf::from(&ctx.vars["worktree"]),
    };
    if step.convex && ctx.convex_env.is_empty() && ctx.target_args.is_empty() {
        bail!("{}", ctx.target_error.clone().unwrap_or_else(|| format!("environment {} has no credential for the Convex CLI", job.env)));
    }
    let vars: Vec<(&str, &str)> = if step.convex { ctx.convex_env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect() } else { vec![] };
    log_line(&job.id, &format!("$ {}", argv.join(" ")));
    let _ = recipe;
    exec(&job.id, &argv, &cwd, &vars, timeout, cancel).await.map(|_| None)
}

/// Corre un comando en su propio grupo de procesos, con el log línea a línea.
pub(crate) async fn exec(id: &str, argv: &[String], cwd: &Path, vars: &[(&str, &str)], timeout: Duration, cancel: &mut watch::Receiver<bool>) -> Result<()> {
    let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
    let mut cmd = command(&argv[0], &args, Some(cwd), vars);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
    let mut child = cmd.spawn().with_context(|| format!("couldn't run {}", argv[0]))?;
    let pid = child.id();
    let (out, err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let (id1, id2) = (id.to_string(), id.to_string());
    let t1 = tokio::spawn(async move {
        let mut lines = BufReader::new(out).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            log_line(&id1, &l);
        }
    });
    let t2 = tokio::spawn(async move {
        let mut lines = BufReader::new(err).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            log_line(&id2, &l);
        }
    });
    let kill_group = |pid: Option<u32>| {
        if let Some(pid) = pid {
            // SAFETY: señal al grupo de procesos que creó este mismo trabajo.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGTERM);
            }
        }
    };
    let result = tokio::select! {
        status = child.wait() => {
            let status = status?;
            if status.success() { Ok(()) } else { Err(anyhow!("exited with code {}", status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into()))) }
        }
        _ = tokio::time::sleep(timeout) => {
            kill_group(pid);
            Err(anyhow!("didn't finish in {} s", timeout.as_secs()))
        }
        _ = async { while cancel.changed().await.is_ok() { if *cancel.borrow() { break; } } } => {
            kill_group(pid);
            Err(anyhow!("cancelado"))
        }
    };
    if result.is_err() {
        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        if let Some(pid) = pid {
            // SAFETY: igual que arriba; si sigue vivo, SIGKILL.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), t1).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), t2).await;
    result
}

pub(crate) async fn quick(id: &str, program: &str, args: &[&str], cwd: Option<&Path>, timeout: Duration) -> Result<String> {
    log_line(id, &format!("$ {program} {}", args.join(" ")));
    let out = run(program, args, cwd, &[], timeout).await;
    for line in out.stdout.lines().chain(out.stderr.lines()) {
        log_line(id, line);
    }
    if !out.ok {
        bail!(out.error());
    }
    Ok(out.stdout)
}

pub(crate) fn container_name(env: &str) -> String {
    format!("heskala-throwaway-{env}")
}

/// Worktree desacoplado del commit, con el submódulo y los node_modules del repo.
pub(crate) async fn make_worktree(repo: &Path, dir: &Path, commit: &str, job_id: &str) -> Result<()> {
    let t = Duration::from_secs(120);
    if dir.exists() {
        let _ = quick(job_id, "git", &["worktree", "remove", "--force", &dir.display().to_string()], Some(repo), t).await;
        let _ = std::fs::remove_dir_all(dir);
    }
    std::fs::create_dir_all(dir.parent().unwrap())?;
    quick(job_id, "git", &["worktree", "add", "--detach", &dir.display().to_string(), commit], Some(repo), t).await?;
    quick(job_id, "git", &["submodule", "update", "--init"], Some(dir), Duration::from_secs(300)).await?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(repo.join("node_modules"), dir.join("node_modules"))?;
    Ok(())
}

pub(crate) async fn remove_worktree(repo: &Path, dir: &Path, job_id: &str) -> Result<bool> {
    if !dir.exists() {
        return Ok(false);
    }
    let _ = std::fs::remove_file(dir.join("node_modules"));
    quick(job_id, "git", &["worktree", "remove", "--force", &dir.display().to_string()], Some(repo), Duration::from_secs(120)).await?;
    Ok(true)
}

async fn builtin_step(
    state: &Shared,
    job: &mut Job,
    env: Option<&Environment>,
    name: &str,
    ctx: &mut Ctx,
    timeout: Duration,
    cancel: &mut watch::Receiver<bool>,
) -> Result<String> {
    let repo = crate::projects::repo_for(state, env);
    match name {
        "worktree" => {
            let dir = PathBuf::from(&ctx.vars["worktree"]);
            let commit = job.commit.clone().ok_or_else(|| anyhow!("the job has no commit"))?;
            make_worktree(&repo, &dir, &commit, &job.id).await?;
            Ok(format!("worktree at {} ({})", dir.display(), &commit[..8]))
        }
        "cleanup-worktree" => {
            let dir = PathBuf::from(&ctx.vars["worktree"]);
            Ok(if remove_worktree(&repo, &dir, &job.id).await? { "worktree removed" } else { "no worktree" }.into())
        }
        "record-deployment" => {
            let commit = job.commit.clone().ok_or_else(|| anyhow!("the job has no commit"))?;
            state.store.add_deployment(&job.env, &commit, &format!("{} ({})", job.requested_by, job.id))?;
            Ok(format!("{} deployed to {}", &commit[..8], job.env))
        }
        "throwaway-create" => {
            let port: u16 = job.args.get("port").and_then(|p| p.parse().ok()).unwrap_or(3310);
            let url = create_throwaway(state, &job.id, &job.env, port, timeout).await?;
            Ok(format!("{} at {url}", job.env))
        }
        "throwaway-destroy" => {
            let env = env.ok_or_else(|| anyhow!("the environment no longer exists"))?;
            destroy_throwaway(state, &job.id, env).await?;
            Ok(format!("{} removed", env.name))
        }
        n if n.starts_with("project-create-") => crate::create::builtin(state, job, n, cancel).await,
        // La aprobación de un flujo: ya se aprobó para llegar aquí.
        "workflow-gate" => Ok(format!("approved: {}", job.args.get("message").cloned().unwrap_or_default())),
        // Un release de Condx (`update.rs`): en esta Mac si se puede, si no en la nube.
        "condx-release" => {
            let version = job.args.get("version").cloned().unwrap_or_default();
            let repo = crate::update::condx_repo(state).ok_or_else(|| anyhow!("there's no Condx repo (set Mayer's repos)"))?;
            let r = repo.clone();
            let done = tokio::task::spawn_blocking(move || crate::update::release(&r, &version)).await.map_err(|e| anyhow!(e.to_string()))??;
            // Hecho aquí: el release ya está publicado; se ofrece enseguida como actualización (esa
            // versión, sin esperar a que GitHub la dé como la última).
            if done.starts_with("built on this Mac") {
                let released = std::fs::read_to_string(repo.join("Cargo.toml")).ok().and_then(|t| t.lines().find(|l| l.starts_with("version")).and_then(|l| l.split('"').nth(1)).map(str::to_string));
                let _ = crate::update::check_for(state, released).await;
            }
            Ok(done)
        }
        // Condx Cloud (`cloud/`) con la deploy key guardada: sin pegarla cada vez.
        "condx-cloud-deploy" => {
            let repo = crate::update::condx_repo(state).ok_or_else(|| anyhow!("there's no Condx repo (set Mayer's repos)"))?;
            tokio::task::spawn_blocking(move || crate::update::deploy_cloud(&repo)).await.map_err(|e| anyhow!(e.to_string()))?
        }
        // Una versión nueva de la app del teléfono: en esta Mac, publicada en Condx Cloud, con aviso.
        "mobile-release" => {
            let version = job.args.get("version").cloned().unwrap_or_default();
            let repo = crate::update::mobile_repo(state).ok_or_else(|| anyhow!("there's no condx-mobile repo (set Mayer's repos)"))?;
            let st = state.clone();
            tokio::task::spawn_blocking(move || crate::update::release_mobile(&st, &repo, &version)).await.map_err(|e| anyhow!(e.to_string()))?
        }
        // Una versión nueva de Condx (`update.rs`): instala y se reinicia cuando el trabajo ya terminó.
        "condx-update" => {
            let version = job.args.get("version").cloned().ok_or_else(|| anyhow!("which version?"))?;
            let done = tokio::task::spawn_blocking(move || crate::update::apply(&version)).await.map_err(|e| anyhow!(e.to_string()))??;
            crate::update::restart_soon();
            Ok(done)
        }
        other => crate::pipeline::builtin(state, job, env, other, ctx, timeout, cancel).await,
    }
}

pub(crate) async fn destroy_throwaway(state: &Shared, job_id: &str, env: &Environment) -> Result<()> {
    if env.kind != EnvKind::Throwaway {
        bail!("{} isn't a throwaway environment", env.name);
    }
    let native = matches!(&env.credential, Some(Credential::EnvFile { path, .. }) if crate::cmd::env_file_value(Path::new(path), "RUNTIME").as_deref() == Some("native"));
    if native {
        crate::native::destroy_throwaway(state, &env.name).await?;
    } else {
        let timeout = Duration::from_secs(60);
        let container = container_name(&env.name);
        let _ = quick(job_id, "docker", &["rm", "-f", &container], None, timeout).await;
        let _ = quick(job_id, "docker", &["volume", "rm", "-f", &format!("{container}-data")], None, timeout).await;
    }
    if let Some(Credential::EnvFile { path, .. }) = &env.credential {
        if Path::new(path).starts_with(home()) {
            let _ = std::fs::remove_file(path);
        }
    }
    state.store.remove_environment(&env.name)?;
    Ok(())
}

/// Levanta un convex-backend vacío y lo registra como entorno desechable.
pub(crate) async fn create_throwaway(state: &Shared, job_id: &str, name: &str, port: u16, timeout: Duration) -> Result<String> {
    let name = name.to_string();
    // Nativo si está el binario (sin Docker); si no, un contenedor.
    if crate::native::available(state) {
        let dir = home().join("throwaway");
        std::fs::create_dir_all(&dir)?;
        let file = dir.join(format!("{name}.env"));
        let _ = std::fs::remove_file(&file);
        let url = crate::native::create_throwaway(state, job_id, &name, port, &file).await?;
        state.store.upsert_environment(&Environment {
            name: name.clone(),
            kind: EnvKind::Throwaway,
            label: Some(format!("Native throwaway (port {port})")),
            convex_url: Some(url.clone()),
            convex_site_url: Some(format!("http://127.0.0.1:{}", port + 1)),
            convex_deployment: None,
            frontend_url: None,
            branch: Some("dev".into()),
            credential: Some(Credential::EnvFile { path: file.display().to_string(), key: "CONVEX_SELF_HOSTED_ADMIN_KEY".into() }),
            created_at: now_ms(),
            project: None,
        })?;
        return Ok(url);
    }
    let container = container_name(&name);
    let mut secret = [0u8; 32];
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(&mut secret)?;
    let secret: String = secret.iter().map(|b| format!("{b:02x}")).collect();
    let (api, site) = (port.to_string(), (port + 1).to_string());
    let publish_api = format!("127.0.0.1:{api}:3210");
    let publish_site = format!("127.0.0.1:{site}:3211");
    let volume = format!("{container}-data:/convex/data");
    let instance = format!("--instance-name={container}");
    let instance_secret = format!("--instance-secret={secret}");
    // El secreto no pasa por el log: se escribe el comando sin él.
    log_line(job_id, &format!("$ docker run -d --name {container} -p {publish_api} -p {publish_site} -v {volume} {BACKEND_IMAGE} {instance} --instance-secret=… /convex/data/db.sqlite3"));
    let out = run(
        "docker",
        &["run", "-d", "--name", &container, "-p", &publish_api, "-p", &publish_site, "-v", &volume, BACKEND_IMAGE, &instance, &instance_secret, "/convex/data/db.sqlite3"],
        None,
        &[],
        timeout,
    )
    .await;
    if !out.ok {
        bail!("docker run failed: {}", out.error());
    }
    let url = format!("http://127.0.0.1:{api}");
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(2)).build();
    let mut up = false;
    for _ in 0..60 {
        let probe = agent.clone();
        let u = format!("{url}/version");
        if tokio::task::spawn_blocking(move || probe.get(&u).call().is_ok()).await.unwrap_or(false) {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    if !up {
        bail!("the backend didn't respond at {url}");
    }
    log_line(job_id, &format!("Backend up at {url}; generating admin key"));
    // Sin pasar por `run`, que redacta: la clave no se escribe en el log.
    let raw = tokio::process::Command::new("docker")
        .args(["run", "--rm", BACKEND_IMAGE, "keygen", "admin-key", "--instance-name", &container, "--instance-secret", &secret])
        .env("PATH", std::env::var("PATH").unwrap_or_default() + ":/opt/homebrew/bin:/usr/local/bin")
        .output()
        .await?;
    if !raw.status.success() {
        bail!("keygen failed: {}", redact(&String::from_utf8_lossy(&raw.stderr)));
    }
    let admin_key = String::from_utf8_lossy(&raw.stdout).lines().last().unwrap_or_default().trim().to_string();
    if !admin_key.contains('|') {
        bail!("keygen didn't return a valid admin key");
    }
    let dir = home().join("throwaway");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{name}.env"));
    std::fs::write(&file, format!("CONVEX_SELF_HOSTED_URL={url}\nCONVEX_SELF_HOSTED_ADMIN_KEY=\"{admin_key}\"\nINSTANCE_SECRET={secret}\n"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
    }
    state.store.upsert_environment(&Environment {
        name: name.clone(),
        kind: EnvKind::Throwaway,
        label: Some(format!("Throwaway ({container})")),
        convex_url: Some(url.clone()),
        convex_site_url: Some(format!("http://127.0.0.1:{site}")),
        convex_deployment: None,
        frontend_url: None,
        branch: Some("dev".into()),
        credential: Some(Credential::EnvFile { path: file.display().to_string(), key: "CONVEX_SELF_HOSTED_ADMIN_KEY".into() }),
        created_at: now_ms(),
        project: None,
    })?;
    Ok(url)
}

/// Resumen para listas (sin pasos).
pub fn summary(job: &Value) -> Value {
    json!({
        "id": job["id"],
        "recipe": job["recipe"],
        "title": job["title"],
        "env": job["env"],
        "args": job["args"],
        "commit": job["commit"],
        "status": job["status"],
        "approval": job["approval"],
        "requestedBy": job["requestedBy"],
        "createdAt": job["createdAt"],
        "endedAt": job["endedAt"],
        "error": job["error"],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sello_de_fecha() {
        assert_eq!(stamp(0), "19700101-000000");
        assert_eq!(stamp(1_792_866_114_000), "20261024-182154");
    }

    #[test]
    fn nombres_de_entorno() {
        assert!(valid_env_name("ensayo-1"));
        assert!(!valid_env_name("Ensayo"));
        assert!(!valid_env_name("-x"));
        assert!(!valid_env_name("a"));
        assert!(!valid_env_name("x;rm"));
    }

    #[test]
    fn plantillas() {
        let ctx = Ctx {
            vars: HashMap::from([
                ("args.function".to_string(), "a:b".to_string()),
                ("args.payload".to_string(), "{}".to_string()),
                ("env".to_string(), "local".to_string()),
            ]),
            convex_bin: ("npx".into(), vec!["convex".into()]),
            convex_env: vec![],
            target_args: vec!["--deployment".into(), "x".into()],
            target_error: None,
        };
        let argv = ctx.argv(&["{convex}".into(), "run".into(), "{args.function}".into(), "{args.payload}".into(), "{env}-x.zip".into()]).unwrap();
        assert_eq!(argv, ["npx", "convex", "run", "a:b", "{}", "local-x.zip"]);
        assert!(ctx.argv(&["{nada}".into()]).is_err());
        assert_eq!(ctx.argv(&["logs".into(), "{target}".into()]).unwrap(), ["logs", "--deployment", "x"]);
    }
}
