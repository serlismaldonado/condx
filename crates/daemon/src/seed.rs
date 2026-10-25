// Instancia local de un proyecto a partir de su cloud: recetas `project-seed`
// (cloud dev) y `project-seed-prod` (producción, con aprobación). El cloud
// solo se lee con la sesión de la CLI (`npx convex login`); los datos y las
// variables van al backend nativo del proyecto.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;
use tokio::io::AsyncWriteExt;

use crate::cmd::env_file_value;
use crate::config::home;
use crate::jobs::{exec, log_line, Job};
use crate::projects::Project;
use crate::state::Shared;

/// Deployment de cloud dev del proyecto: `CONVEX_DEPLOYMENT=dev:<nombre>` de su `.env.local`.
pub fn cloud_dev_of(repo: &Path) -> Option<String> {
    let value = env_file_value(&repo.join(".env.local"), "CONVEX_DEPLOYMENT")?;
    let value = value.split('#').next().unwrap_or_default().trim().trim_matches('"');
    value.strip_prefix("dev:").map(str::to_string).filter(|n| !n.is_empty())
}

/// La CLI de Convex del repo del proyecto (su versión), o `npx convex`.
fn cli(project: &Project) -> (String, Vec<String>) {
    let local = project.repo.join("node_modules/.bin/convex");
    if local.exists() {
        (local.display().to_string(), vec![])
    } else {
        ("npx".into(), vec!["convex".into()])
    }
}

/// Origen del job: cloud dev o producción del proyecto.
struct Source {
    prod: bool,
    deployment: String,
}

impl Source {
    fn label(&self) -> String {
        if self.prod {
            format!("production of the dev:{} project", self.deployment)
        } else {
            format!("cloud dev ({})", self.deployment)
        }
    }

    /// Variables y argumentos para leer del cloud: la sesión de la CLI elige el
    /// proyecto por su deployment de dev; `--prod` pasa a su producción.
    fn target(&self) -> (Vec<String>, Vec<(String, String)>) {
        let vars = vec![("CONVEX_DEPLOYMENT".to_string(), format!("dev:{}", self.deployment))];
        (if self.prod { vec!["--prod".into()] } else { vec![] }, vars)
    }
}

fn local_target(project: &Project) -> Result<Vec<(String, String)>> {
    let file = project.env_file();
    let get = |k: &str| env_file_value(&file, k).ok_or_else(|| anyhow!("missing {k} in {}: start its backend first", file.display()));
    Ok(vec![
        ("CONVEX_SELF_HOSTED_URL".into(), get("CONVEX_SELF_HOSTED_URL")?),
        ("CONVEX_SELF_HOSTED_ADMIN_KEY".into(), get("CONVEX_SELF_HOSTED_ADMIN_KEY")?),
        ("CONVEX_DEPLOYMENT".into(), String::new()),
    ])
}

fn project_of(state: &Shared, job: &Job) -> Result<Project> {
    let name = job.args.get("project").ok_or_else(|| anyhow!("missing argument project"))?;
    let project = crate::projects::get(state, Some(name))?;
    if project.is_default() {
        bail!("heskala-erp has its own recipes (migrate-native, import): use those");
    }
    Ok(project)
}

fn source_of(job: &Job, project: &Project) -> Result<Source> {
    let deployment = cloud_dev_of(&project.repo)
        .ok_or_else(|| anyhow!("{}'s .env.local has no CONVEX_DEPLOYMENT=dev:<name>: run `npx convex dev` once in the repo", project.repo.display()))?;
    Ok(Source { prod: job.recipe == "project-seed-prod", deployment })
}

fn tmp(job_id: &str, ext: &str) -> PathBuf {
    home().join("tmp").join(format!("{job_id}.{ext}"))
}

fn private(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Corre la CLI sin redactar ni loguear la salida (lee valores de variables).
async fn raw(project: &Project, cwd: &Path, sub: &[&str], extra: &[String], vars: &[(String, String)], stdin: Option<&str>) -> Result<String> {
    let (bin, prefix) = cli(project);
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.args(&prefix).args(sub).args(extra).current_dir(cwd).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    for (k, v) in vars {
        cmd.env(k, v);
    }
    let path = std::env::var("PATH").unwrap_or_default();
    cmd.env("PATH", format!("/opt/homebrew/bin:/usr/local/bin:{path}"));
    let mut child = cmd.spawn()?;
    if let (Some(text), Some(mut input)) = (stdin, child.stdin.take()) {
        input.write_all(text.as_bytes()).await?;
        drop(input);
    }
    let out = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output()).await.context("the Convex CLI didn't respond")??;
    if !out.status.success() {
        bail!("{}", devtool_core::redact(String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Corre la CLI con la salida al log del trabajo (redactada).
#[allow(clippy::too_many_arguments)]
async fn logged(job_id: &str, project: &Project, cwd: &Path, sub: &[&str], extra: Vec<String>, vars: Vec<(String, String)>, timeout: Duration, cancel: &mut tokio::sync::watch::Receiver<bool>) -> Result<()> {
    let (bin, prefix) = cli(project);
    let mut argv = vec![bin];
    argv.extend(prefix);
    argv.extend(sub.iter().map(|s| s.to_string()));
    argv.extend(extra);
    log_line(job_id, &format!("$ {}", argv.join(" ")));
    let vars: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    exec(job_id, &argv, cwd, &vars, timeout, cancel).await
}

pub async fn builtin(state: &Shared, job: &mut Job, name: &str, cancel: &mut tokio::sync::watch::Receiver<bool>) -> Result<String> {
    let id = job.id.clone();
    let project = project_of(state, job)?;
    match name {
        "project-export" => {
            let source = source_of(job, &project)?;
            let zip = tmp(&id, "zip");
            std::fs::create_dir_all(zip.parent().unwrap())?;
            let _ = std::fs::remove_file(&zip);
            let (mut extra, vars) = source.target();
            if job.args.get("files").map(String::as_str) == Some("yes") {
                extra.push("--include-file-storage".into());
            }
            log_line(&id, &format!("Exporting {} from {}", source.label(), project.name));
            let zip_s = zip.display().to_string();
            logged(&id, &project, &project.repo, &["export", "--path", &zip_s], extra, vars, Duration::from_secs(3600), cancel).await?;
            if !zip.exists() {
                bail!("convex export didn't leave {zip_s}");
            }
            private(&zip)?;
            let kb = std::fs::metadata(&zip)?.len() / 1024;
            job.outputs.insert("export_path".into(), zip_s);
            Ok(format!("{} exported ({kb} KB)", source.label()))
        }
        "project-read-env" => {
            let source = source_of(job, &project)?;
            let (extra, vars) = source.target();
            let names: Vec<String> = raw(&project, &project.repo, &["env", "list", "--names-only"], &extra, &vars, None)
                .await?
                .lines()
                .map(|l| l.split('=').next().unwrap_or_default().trim().to_string())
                .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
                .collect();
            let mut values = serde_json::Map::new();
            for n in &names {
                let mut v = raw(&project, &project.repo, &["env", "get", n], &extra, &vars, None).await?;
                if v.ends_with('\n') {
                    v.pop();
                }
                values.insert(n.clone(), json!(v));
            }
            // Las redirecciones de auth (Convex Auth, Better Auth) tienen que volver al frontend local.
            if let (true, Some(_)) = (!project.frontend.is_empty(), values.get("SITE_URL")) {
                values.insert("SITE_URL".into(), json!(format!("http://localhost:{}", project.frontend_port)));
                log_line(&id, &format!("SITE_URL becomes http://localhost:{} (the local frontend)", project.frontend_port));
            }
            let file = tmp(&id, "env.json");
            std::fs::write(&file, serde_json::to_string(&values)?)?;
            private(&file)?;
            log_line(&id, &format!("{} variables read: {}", names.len(), names.join(", ")));
            Ok(format!("{} variables from {}", names.len(), source.label()))
        }
        "project-import" => {
            let zip = PathBuf::from(job.outputs.get("export_path").ok_or_else(|| anyhow!("missing the export (previous step)"))?);
            if !zip.exists() {
                bail!("{} is gone (deleted?): request the job again", zip.display());
            }
            // El deployer se detiene: que no despliegue a medio importar.
            crate::procs::stop(state, &project.proc("deployer")).await?;
            crate::native::start_backend(state, &project).await?;
            let cwd = crate::native::deployer_root(&project)?;
            let zip_s = zip.display().to_string();
            logged(&id, &project, &cwd, &["import", "--replace-all", "-y", &zip_s], vec![], local_target(&project)?, Duration::from_secs(3600), cancel).await?;
            let _ = std::fs::remove_file(&zip);
            log_line(&id, "Temporary export deleted");
            Ok(format!("data imported at http://127.0.0.1:{}", project.backend_port))
        }
        "project-write-env" => {
            let file = tmp(&id, "env.json");
            let raw_values = std::fs::read_to_string(&file).context("missing the variables read (previous step)")?;
            let values: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&raw_values)?;
            crate::native::start_backend(state, &project).await?;
            let cwd = crate::native::deployer_root(&project)?;
            let vars = local_target(&project)?;
            for (n, v) in &values {
                raw(&project, &cwd, &["env", "set", n], &[], &vars, Some(v.as_str().unwrap_or_default())).await?;
            }
            let _ = std::fs::remove_file(&file);
            log_line(&id, &format!("{} variables copied to the local backend", values.len()));
            Ok(format!("{} variables copied", values.len()))
        }
        "project-start-deployer" => {
            crate::native::start_deployer(state, &project).await?;
            Ok("deployer running: deploys the repo's functions".into())
        }
        other => bail!("unknown internal step: {other}"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn lee_el_deployment_de_dev() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env.local"), "CONVEX_DEPLOYMENT=dev:beaming-clownfish-557 # team: heskala, project: x\n").unwrap();
        assert_eq!(super::cloud_dev_of(dir.path()).as_deref(), Some("beaming-clownfish-557"));
        std::fs::write(dir.path().join(".env.local"), "CONVEX_DEPLOYMENT=anonymous:anonymous-x\n").unwrap();
        assert_eq!(super::cloud_dev_of(dir.path()), None);
    }
}
