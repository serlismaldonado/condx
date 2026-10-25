// Pasos internos de F3 (§8): respaldos (cifrados en cloud), import, rellenos,
// verificación de errores, ensayo con datos reales en un desechable y la
// promoción a producción con sus precondiciones.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use devtool_core::{Credential, EnvKind, Environment};
use serde_json::{json, Value};
use tokio::sync::watch;

use crate::cmd::run;
use crate::config::home;
use crate::jobs::{
    backups_dir, create_throwaway, destroy_throwaway, exec, log_line, make_worktree, remove_worktree, resolve_commit, stamp, Ctx, Job,
};
use crate::probes::ci::{github_slug, is_invalid_workflow};
use crate::probes::convex::target;
use crate::probes::function_logs;
use crate::probes::git::resolve_url;
use crate::recipes::{validate, ArgType};
use crate::state::Shared;
use crate::store::now_ms;

/// Un respaldo de producción sirve para promover durante 2 h.
pub const BACKUP_MAX_AGE_MS: i64 = 2 * 3_600_000;
/// Cloud dev sin errores de funciones en los últimos 30 min.
const QUIET_MS: i64 = 30 * 60_000;
/// Los respaldos cifrados se borran a los 14 días.
pub const BACKUP_RETENTION_MS: i64 = 14 * 24 * 3_600_000;

/// Corre `convex <sub…>` contra `env` (con el log del trabajo, cancelable).
#[allow(clippy::too_many_arguments)]
async fn convex_on(
    state: &Shared,
    job_id: &str,
    env: &Environment,
    write: bool,
    sub: &[&str],
    cwd: &Path,
    timeout: Duration,
    cancel: &mut watch::Receiver<bool>,
) -> Result<()> {
    let (bin, prefix) = crate::probes::convex::convex_bin_at(&crate::projects::repo_for(state, Some(env)));
    let (extra, vars) = target(env, write)?;
    let mut argv = vec![bin];
    argv.extend(prefix);
    argv.extend(sub.iter().map(|s| s.to_string()));
    argv.extend(extra);
    log_line(job_id, &format!("$ {} (→ {})", argv.join(" "), env.name));
    let vars: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    exec(job_id, &argv, cwd, &vars, timeout, cancel).await
}

fn env_named(state: &Shared, name: &str) -> Result<Environment> {
    state.store.environment(name)?.ok_or_else(|| anyhow!("environment “{name}” doesn't exist"))
}

fn is_simulated(env: &Environment) -> bool {
    matches!(env.credential, Some(Credential::EnvFile { .. }))
}

/// Errores propios del backend self-hosted bajo carga (límite de escritura,
/// conflictos de concurrencia): el ensayo del 2026-10-24 los vio igual y no
/// son del código. En cloud cuentan como cualquier otro.
fn transient(entry: &Value) -> bool {
    let msg = entry["error"].as_str().unwrap_or_default();
    msg.contains("Too many writes per second")
        || msg.contains("OptimisticConcurrencyControlFailure")
        || msg.contains("changed while this mutation was being run")
}

/// Errores de `env` desde `since`, sin los transitorios del self-hosted.
async fn errors_since(state: &Shared, env: &Environment, since: i64) -> Result<(Vec<Value>, usize)> {
    let all: Vec<Value> = function_logs(state, env, 300, true)
        .await?
        .into_iter()
        .filter(|e| e["timestamp"].as_f64().map(|t| (t * 1000.0) as i64 >= since).unwrap_or(false))
        .collect();
    if !is_simulated(env) {
        return Ok((all, 0));
    }
    let (skipped, real): (Vec<Value>, Vec<Value>) = all.into_iter().partition(transient);
    Ok((real, skipped.len()))
}

/// Respaldo del entorno; en cloud se cifra con age (clave en el Llavero).
pub(crate) async fn export(state: &Shared, job_id: &str, env: &Environment, cancel: &mut watch::Receiver<bool>) -> Result<(i64, PathBuf)> {
    std::fs::create_dir_all(backups_dir())?;
    let path = backups_dir().join(format!("{}-{}.zip", env.name, stamp(now_ms())));
    let path_s = path.display().to_string();
    let repo = crate::projects::repo_for(state, Some(env));
    convex_on(state, job_id, env, false, &["export", "--path", &path_s], &repo, Duration::from_secs(1800), cancel).await?;
    if !path.exists() {
        bail!("convex export didn't leave {path_s}");
    }
    let (path, encrypted) = if env.kind.is_cloud() {
        log_line(job_id, "Encrypting the backup (age, key in the Keychain)");
        (crate::secrets::encrypt_file(&path)?, true)
    } else {
        (path, false)
    };
    let size = std::fs::metadata(&path)?.len() as i64;
    let commit = state.store.last_deployment(&env.name)?.map(|d| d.commit);
    let id = state.store.add_backup(&env.name, &path.display().to_string(), encrypted, size, commit.as_deref(), job_id)?;
    log_line(job_id, &format!("Backup #{id}: {} ({} KB{})", path.display(), size / 1024, if encrypted { ", encrypted" } else { "" }));
    Ok((id, path))
}

/// `import --replace-all` de un respaldo (descifrado en un temporal si hace falta).
pub async fn import(state: &Shared, job_id: &str, env: &Environment, backup: &Path, cancel: &mut watch::Receiver<bool>) -> Result<()> {
    let is_age = backup.extension().map(|e| e == "age").unwrap_or(false);
    let tmp = home().join("tmp").join(format!("{job_id}.zip"));
    let source = if is_age {
        std::fs::create_dir_all(tmp.parent().unwrap())?;
        log_line(job_id, "Decrypting the backup to a temporary file");
        crate::secrets::decrypt_file(backup, &tmp)?;
        tmp.clone()
    } else {
        backup.to_path_buf()
    };
    let path = source.display().to_string();
    let repo = crate::projects::repo_for(state, Some(env));
    let res = convex_on(state, job_id, env, true, &["import", "--replace-all", "-y", &path], &repo, Duration::from_secs(3600), cancel).await;
    if is_age {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Despliega `commit` en `env` desde un worktree propio y lo registra.
pub async fn deploy_commit(state: &Shared, job: &Job, env: &Environment, commit: &str, cancel: &mut watch::Receiver<bool>) -> Result<()> {
    let repo = crate::projects::repo_for(state, Some(env));
    let dir = home().join("worktrees").join(format!("{}-{}", job.id, &commit[..8]));
    make_worktree(&repo, &dir, commit, &job.id).await?;
    let message = format!("condx {} {}", job.id, &commit[..8]);
    // Producción con su deploy key: `deploy`. Lo demás, como los rollouts: `dev --once`.
    let sub: Vec<&str> =
        if env.kind == EnvKind::CloudProd && !is_simulated(env) { vec!["deploy", "--message", &message] } else { vec!["dev", "--once"] };
    let res = convex_on(state, &job.id, env, true, &sub, &dir, Duration::from_secs(1200), cancel).await;
    let _ = remove_worktree(&repo, &dir, &job.id).await;
    res?;
    state.store.add_deployment(&env.name, commit, &format!("{} ({})", job.requested_by, job.id))?;
    log_line(&job.id, &format!("{} deployed to {}", &commit[..8], env.name));
    Ok(())
}

pub async fn backfills(state: &Shared, job_id: &str, env: &Environment, list: &str, cancel: &mut watch::Receiver<bool>) -> Result<usize> {
    let functions: Vec<&str> = list.split(',').map(str::trim).filter(|f| !f.is_empty()).collect();
    for f in &functions {
        validate(ArgType::Function, f, &backups_dir())?;
        convex_on(state, job_id, env, true, &["run", f, "{}"], &crate::projects::repo_for(state, Some(env)), Duration::from_secs(1800), cancel).await?;
    }
    Ok(functions.len())
}

/// Errores de funciones en `env` desde `since`; con `minutes` > 0 sigue
/// mirando ese tiempo (cada 30 s) y falla al primero.
pub async fn verify(state: &Shared, job_id: &str, env: &Environment, since: i64, minutes: u64, cancel: &mut watch::Receiver<bool>) -> Result<String> {
    let until = now_ms() + (minutes as i64) * 60_000;
    loop {
        let (errors, skipped) = errors_since(state, env, since).await?;
        if skipped > 0 {
            log_line(job_id, &format!("{skipped} transient local backend error(s) (write limit / concurrency) don't count"));
        }
        if !errors.is_empty() {
            let sample: Vec<String> = errors
                .iter()
                .take(3)
                .map(|e| format!("{}: {}", e["identifier"].as_str().unwrap_or("?"), e["error"].as_str().unwrap_or("error").lines().next().unwrap_or("")))
                .collect();
            bail!("{} function error(s) on {}: {}", errors.len(), env.name, sample.join(" | "));
        }
        if now_ms() >= until {
            return Ok(format!("no function errors on {}{}", env.name, if skipped > 0 { format!(" ({skipped} transient from the local backend)") } else { String::new() }));
        }
        log_line(job_id, &format!("No errors so far; still watching {} ({} s left)", env.name, (until - now_ms()) / 1000));
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            _ = cancel.changed() => bail!("cancelado"),
        }
    }
}

async fn ci_check(state: &Shared, commit: &str) -> Value {
    let repo = state.config.workdir();
    let t = Duration::from_secs(20);
    let origin = run("git", &["remote", "get-url", "origin"], Some(&repo), &[], t).await.stdout;
    let mut targets: Vec<(String, String)> = vec![];
    if let Some(slug) = github_slug(&origin) {
        targets.push((slug, commit.to_string()));
    }
    // Submódulos: el commit al que apunta el candidato, en su propio repo.
    let modules = run("git", &["config", "-f", ".gitmodules", "--get-regexp", r"^submodule\..*\.(path|url)$"], Some(&repo), &[], t).await.stdout;
    let mut path: Option<String> = None;
    for line in modules.lines() {
        let Some((key, value)) = line.split_once(' ') else { continue };
        if key.ends_with(".path") {
            path = Some(value.to_string());
        } else if key.ends_with(".url") {
            if let (Some(p), Some(slug)) = (path.take(), github_slug(&resolve_url(&origin, value))) {
                let tree = run("git", &["ls-tree", commit, &p], Some(&repo), &[], t).await.stdout;
                if let Some(sha) = tree.split_whitespace().nth(2) {
                    targets.push((slug, sha.to_string()));
                }
            }
        }
    }
    let mut details = vec![];
    let mut ok = true;
    for (slug, sha) in targets {
        let out = run("gh", &["run", "list", "-R", &slug, "--commit", &sha, "--limit", "30", "--json", "status,conclusion,workflowName"], None, &[], t).await;
        if !out.ok {
            ok = false;
            details.push(format!("{slug}: couldn't query ({})", out.error()));
            continue;
        }
        let runs: Vec<Value> = serde_json::from_str::<Vec<Value>>(&out.stdout).unwrap_or_default().into_iter().filter(|r| !is_invalid_workflow(r)).collect();
        let short = &sha[..sha.len().min(8)];
        if runs.is_empty() {
            // ¿El repo tiene algún workflow que corra? Si no, el CI está apagado.
            let any = state
                .probe_data("ci")
                .and_then(|d| d["repos"].as_array().cloned())
                .unwrap_or_default()
                .iter()
                .any(|r| r["repo"] == slug.as_str() && r["runs"].as_array().map(|l| !l.is_empty()).unwrap_or(false));
            if any {
                ok = false;
                details.push(format!("{slug}: no CI run for {short}"));
            } else {
                details.push(format!("{slug}: no CI configured (workflows disabled)"));
            }
        } else if runs.iter().any(|r| r["status"] != "completed") {
            ok = false;
            details.push(format!("{slug}: CI running for {short}"));
        } else if runs.iter().any(|r| r["conclusion"] != "success") {
            ok = false;
            details.push(format!("{slug}: CI failed for {short}"));
        } else {
            details.push(format!("{slug}: green on {short}"));
        }
    }
    json!({ "key": "ci", "label": "CI green", "ok": ok, "detail": details.join(" · ") })
}

/// Dos precondiciones: el commit desplegado en cloud dev por el daemon, y
/// que lleve al menos 30 min ahí sin errores de funciones (cuenta desde el
/// despliegue: un despliegue recién hecho todavía no prueba nada).
async fn cloud_dev_checks(state: &Shared, commit: &str) -> Vec<Value> {
    let (label, quiet_label) = ("Same commit deployed on cloud dev", "30 min on cloud dev without errors");
    let envs = state.store.environments().unwrap_or_default();
    let Some(dev) = envs.iter().find(|e| e.kind == EnvKind::CloudDev) else {
        return vec![
            json!({ "key": "cloud-dev", "label": label, "ok": false, "detail": "no cloud dev environment registered" }),
            json!({ "key": "cloud-dev-quiet", "label": quiet_label, "ok": false, "detail": "—" }),
        ];
    };
    let deployed = state.store.last_deployment(&dev.name).ok().flatten();
    let first = match &deployed {
        Some(d) if d.commit == commit => {
            json!({ "key": "cloud-dev", "label": label, "ok": true, "env": dev.name, "detail": format!("{} on {} for {} min", &commit[..8], dev.name, (now_ms() - d.at) / 60_000) })
        }
        Some(d) => json!({ "key": "cloud-dev", "label": label, "ok": false, "env": dev.name, "detail": format!("{} has {}, not {}", dev.name, &d.commit[..8], &commit[..8]) }),
        None => json!({ "key": "cloud-dev", "label": label, "ok": false, "env": dev.name, "detail": format!("{} has no deploys made by the daemon", dev.name) }),
    };
    let Some(d) = deployed.filter(|d| d.commit == commit) else {
        return vec![first, json!({ "key": "cloud-dev-quiet", "label": quiet_label, "ok": false, "detail": "the deploy is needed first" })];
    };
    let ready_at = d.at + QUIET_MS;
    let since = d.at.max(now_ms() - QUIET_MS);
    let quiet = match errors_since(state, dev, since).await {
        Ok((logs, _)) if !logs.is_empty() => {
            json!({ "key": "cloud-dev-quiet", "label": quiet_label, "ok": false, "readyAt": ready_at, "detail": format!("{} function error(s) on {} since the deploy", logs.len(), dev.name) })
        }
        Ok(_) if now_ms() < ready_at => json!({
            "key": "cloud-dev-quiet", "label": quiet_label, "ok": false, "readyAt": ready_at,
            "detail": format!("no errors so far; {} min to go", (ready_at - now_ms() + 59_999) / 60_000)
        }),
        Ok(_) => json!({ "key": "cloud-dev-quiet", "label": quiet_label, "ok": true, "readyAt": ready_at, "detail": "no function errors" }),
        Err(e) => json!({ "key": "cloud-dev-quiet", "label": quiet_label, "ok": false, "detail": format!("couldn't read the logs of {}: {e:#}", dev.name) }),
    };
    vec![first, quiet]
}

/// Precondiciones de la promoción (§8.3). Todas tienen que dar `ok`.
pub async fn preconditions(state: &Shared, env: &Environment, commit: &str) -> Vec<Value> {
    let mut list = vec![ci_check(state, commit).await];
    list.extend(cloud_dev_checks(state, commit).await);
    let backup = state.store.backups(Some(&env.name), 1).ok().and_then(|b| b.into_iter().next());
    let fresh = backup.as_ref().filter(|b| now_ms() - b["at"].as_i64().unwrap_or(0) < BACKUP_MAX_AGE_MS);
    list.push(json!({
        "key": "backup",
        "label": "Production backup less than 2 h old",
        "ok": fresh.is_some(),
        "detail": match (&backup, fresh) {
            (Some(b), Some(_)) => format!("#{} from {} min ago", b["id"], (now_ms() - b["at"].as_i64().unwrap_or(0)) / 60_000),
            (Some(b), None) => format!("the latest (#{}) is {} min old: request an export of {}", b["id"], (now_ms() - b["at"].as_i64().unwrap_or(0)) / 60_000, env.name),
            _ => format!("no backups of {}: request an export", env.name),
        },
    }));
    let rehearsal = state.store.rehearsal(&env.name, commit).ok().flatten();
    let matches = match (&rehearsal, fresh) {
        (Some(r), Some(b)) => r["backupId"] == b["id"],
        _ => false,
    };
    list.push(json!({
        "key": "rehearsal",
        "label": "Rehearsal passed with that backup and commit",
        "ok": matches,
        "detail": match (&rehearsal, fresh) {
            (_, None) => "a recent backup is needed first".to_string(),
            (Some(r), Some(b)) if r["backupId"] == b["id"] => format!("job {}", r["job"].as_str().unwrap_or("?")),
            (_, Some(b)) => format!("missing a rehearsal of {} with backup #{}", &commit[..8], b["id"]),
        },
    }));
    list
}

fn arg<'a>(job: &'a Job, name: &str) -> &'a str {
    job.args.get(name).map(String::as_str).unwrap_or("")
}

fn out<'a>(job: &'a Job, name: &str) -> Result<&'a str> {
    job.outputs.get(name).map(String::as_str).ok_or_else(|| anyhow!("missing “{name}” from a previous step"))
}

fn rehearsal_name(job: &Job) -> String {
    format!("reh-{}", &job.id[job.id.len() - 6..])
}

pub async fn builtin(
    state: &Shared,
    job: &mut Job,
    env: Option<&Environment>,
    name: &str,
    ctx: &mut Ctx,
    _timeout: Duration,
    cancel: &mut watch::Receiver<bool>,
) -> Result<String> {
    let env = env.ok_or_else(|| anyhow!("environment {} no longer exists", job.env))?;
    let commit = job.commit.clone().unwrap_or_default();
    match name {
        "export" => {
            let (id, path) = export(state, &job.id, env, cancel).await?;
            job.outputs.insert("backup_id".into(), id.to_string());
            job.outputs.insert("backup_path".into(), path.display().to_string());
            Ok(format!("backup #{id}"))
        }
        "import" => {
            import(state, &job.id, env, Path::new(arg(job, "path")), cancel).await?;
            Ok(format!("{} imported into {}", arg(job, "path"), env.name))
        }
        "backfills" => {
            let n = backfills(state, &job.id, env, arg(job, "backfills"), cancel).await?;
            Ok(format!("{n} backfill(s)"))
        }
        "verify-logs" => {
            let minutes = arg(job, "verify_minutes").parse().unwrap_or(5);
            verify(state, &job.id, env, job.started_at.unwrap_or_else(now_ms), minutes, cancel).await
        }
        "check-preconditions" => {
            let list = preconditions(state, env, &commit).await;
            job.preconditions = list.clone();
            let failed: Vec<String> = list.iter().filter(|p| p["ok"] != true).map(|p| format!("{}: {}", p["label"].as_str().unwrap_or("?"), p["detail"].as_str().unwrap_or(""))).collect();
            if !failed.is_empty() {
                bail!("no longer met: {}", failed.join(" | "));
            }
            Ok("all met".into())
        }
        "deploy-commit" => {
            deploy_commit(state, job, env, &commit, cancel).await?;
            Ok(format!("{} on {}", &commit[..8], env.name))
        }
        "vercel-deploy" => {
            let mode = arg(job, "frontend");
            if mode == "none" || (mode != "vercel" && (env.frontend_url.is_none() || is_simulated(env))) {
                return Ok("frontend: no changes (environment without Vercel)".into());
            }
            let dir = home().join("worktrees").join(format!("{}-front", job.id));
            let repo = state.config.erp()?.to_path_buf();
            make_worktree(&repo, &dir, &commit, &job.id).await?;
            let res = async {
                // El worktree no trae el vínculo con el proyecto: se copia.
                std::fs::create_dir_all(dir.join(".vercel"))?;
                std::fs::copy(repo.join(".vercel/project.json"), dir.join(".vercel/project.json"))?;
                let argv: Vec<String> = ["vercel", "deploy", "--prod", "--yes"].iter().map(|s| s.to_string()).collect();
                exec(&job.id, &argv, &dir, &[], Duration::from_secs(1800), cancel).await
            }
            .await;
            let _ = remove_worktree(&repo, &dir, &job.id).await;
            res?;
            state.store.add_deployment(&format!("{}:frontend", env.name), &commit, &format!("{} ({})", job.requested_by, job.id))?;
            Ok(format!("frontend {} deployed to Vercel", &commit[..8]))
        }
        // --- Ensayo (§8.2): todo pasa en un desechable; el origen solo se lee.
        "rehearsal-backup" => {
            let (id, path) = match arg(job, "backup") {
                "" | "new" => export(state, &job.id, env, cancel).await?,
                id => {
                    let id: i64 = id.parse().map_err(|_| anyhow!("backup: “new” or a backup number"))?;
                    let b = state.store.backup(id)?.ok_or_else(|| anyhow!("backup #{id} doesn't exist"))?;
                    if b["env"] != env.name.as_str() {
                        bail!("backup #{id} is from {}, not {}", b["env"], env.name);
                    }
                    (id, PathBuf::from(b["path"].as_str().unwrap_or_default()))
                }
            };
            job.outputs.insert("backup_id".into(), id.to_string());
            job.outputs.insert("backup_path".into(), path.display().to_string());
            Ok(format!("backup #{id}"))
        }
        "rehearsal-create" => {
            let name = rehearsal_name(job);
            if let Some(old) = state.store.environment(&name)? {
                destroy_throwaway(state, &job.id, &old).await?;
            }
            let port: u16 = arg(job, "port").parse().unwrap_or(3330);
            let url = create_throwaway(state, &job.id, &name, port, Duration::from_secs(180)).await?;
            job.outputs.insert("throwaway".into(), name.clone());
            Ok(format!("{name} at {url}"))
        }
        "rehearsal-base" => {
            let base = arg(job, "base_commit").to_string();
            if base.is_empty() || base == "none" {
                return Ok("no base commit: importing into the empty backend".into());
            }
            let sha = resolve_commit(state.config.erp()?, &base).await?;
            let target_env = env_named(state, out(job, "throwaway")?)?;
            deploy_commit(state, job, &target_env, &sha, cancel).await?;
            Ok(format!("base {} deployed", &sha[..8]))
        }
        "rehearsal-import" => {
            let target_env = env_named(state, out(job, "throwaway")?)?;
            let path = PathBuf::from(out(job, "backup_path")?);
            import(state, &job.id, &target_env, &path, cancel).await?;
            Ok(format!("backup #{} imported", out(job, "backup_id")?))
        }
        "rehearsal-deploy" => {
            let target_env = env_named(state, out(job, "throwaway")?)?;
            deploy_commit(state, job, &target_env, &commit, cancel).await?;
            Ok(format!("{} deployed to the throwaway", &commit[..8]))
        }
        "rehearsal-backfills" => {
            let target_env = env_named(state, out(job, "throwaway")?)?;
            let n = backfills(state, &job.id, &target_env, arg(job, "backfills"), cancel).await?;
            Ok(format!("{n} backfill(s)"))
        }
        "rehearsal-verify" => {
            let target_env = env_named(state, out(job, "throwaway")?)?;
            verify(state, &job.id, &target_env, job.started_at.unwrap_or_else(now_ms), 0, cancel).await
        }
        "rehearsal-record" => {
            let backup: i64 = out(job, "backup_id")?.parse()?;
            state.store.add_rehearsal(&env.name, backup, &commit, true, &job.id)?;
            Ok(format!("rehearsal of {} with backup #{backup} recorded", &commit[..8]))
        }
        "rehearsal-destroy" => {
            let Some(name) = job.outputs.get("throwaway").cloned() else { return Ok("no throwaway was created".into()) };
            if arg(job, "keep") == "yes" {
                return Ok(format!("{name} kept (keep=yes)"));
            }
            match state.store.environment(&name)? {
                Some(t) => {
                    destroy_throwaway(state, &job.id, &t).await?;
                    Ok(format!("{name} removed"))
                }
                None => Ok("the throwaway was already gone".into()),
            }
        }
        other if other.starts_with("project-") => crate::seed::builtin(state, job, other, cancel).await,
        other if other.starts_with("native-") || other == "local-mode" => {
            let _ = ctx;
            crate::native::builtin(state, job, other, cancel).await
        }
        other => bail!("unknown internal step: {other}"),
    }
}

/// Borra los respaldos cifrados que pasaron la retención.
pub fn prune_backups(state: &Shared) {
    for (id, path) in state.store.expired_backups(now_ms() - BACKUP_RETENTION_MS).unwrap_or_default() {
        let _ = std::fs::remove_file(&path);
        let _ = state.store.forget_backup(id);
        tracing::info!("backup #{id} expired and deleted ({path})");
    }
}
