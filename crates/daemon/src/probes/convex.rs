// Backends de Convex de cada entorno: si responde y qué versión corre; y, a
// pedido, nombres de variables y logs de funciones por la CLI de Convex.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use devtool_core::{can_read, Credential, EnvKind, Environment, Read};
use serde_json::{json, Value};

use crate::cmd::{capture_for, env_file_value, run, Out};
use crate::state::Shared;

const T: Duration = Duration::from_secs(5);

fn version(url: &str) -> (bool, Option<String>, u128, Option<String>) {
    let started = Instant::now();
    let agent = ureq::AgentBuilder::new().timeout(T).build();
    match agent.get(&format!("{}/version", url.trim_end_matches('/'))).call() {
        Ok(res) => {
            let body = res.into_string().unwrap_or_default();
            (true, Some(body.trim().chars().take(80).collect()), started.elapsed().as_millis(), None)
        }
        Err(ureq::Error::Status(code, _)) => (true, None, started.elapsed().as_millis(), Some(format!("HTTP {code}"))),
        Err(e) => (false, None, started.elapsed().as_millis(), Some(e.to_string())),
    }
}

pub async fn environments(state: Shared) {
    let envs = match state.store.environments() {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("couldn't read the environments: {e}");
            return;
        }
    };
    for env in envs {
        let key = format!("convex:{}", env.name);
        let Some(url) = env.convex_url.clone() else {
            state.set_probe(&key, false, Value::Null, Some("no Convex URL registered".into()));
            continue;
        };
        let (reachable, version, latency, error) =
            tokio::task::spawn_blocking(move || version(&url)).await.unwrap_or((false, None, 0, Some("probe down".into())));
        let mut data = json!({ "reachable": reachable, "version": version, "latencyMs": latency });
        if env.kind == EnvKind::Devbox && env.project.is_none() {
            data["container"] = json!(super::docker::backend_container(&state));
        }
        state.set_probe(&key, reachable, data, if reachable { None } else { error });
    }
}

pub fn convex_bin(state: &Shared) -> (String, Vec<String>) {
    convex_bin_at(&state.config.workdir())
}

/// La CLI de Convex del repo (su versión), o `npx convex`.
pub fn convex_bin_at(repo: &std::path::Path) -> (String, Vec<String>) {
    let local: PathBuf = repo.join("node_modules/.bin/convex");
    if local.exists() {
        (local.display().to_string(), vec![])
    } else {
        ("npx".into(), vec!["convex".into()])
    }
}

/// Argumentos extra y variables de entorno para la CLI.
pub type Target = (Vec<String>, Vec<(String, String)>);

/// Argumentos y variables para que la CLI de Convex apunte al entorno.
/// `write`: para escribir en producción hace falta su deploy key (Llavero);
/// leer alcanza con la sesión de `npx convex`.
pub fn target(env: &Environment, write: bool) -> Result<Target> {
    match (&env.kind, &env.credential) {
        // Self-hosted (devbox, desechables o un entorno cloud simulado en pruebas).
        (_, Some(Credential::EnvFile { path, key })) => {
            let Some(admin) = env_file_value(std::path::Path::new(path), key) else {
                bail!("couldn't find {key} in {path}");
            };
            // Un backend nativo guarda su URL en el archivo (el de Docker trae la
            // de la red interna del compose, que no sirve desde la Mac).
            let file = std::path::Path::new(path);
            let url = match env_file_value(file, "RUNTIME").as_deref() {
                Some("native") => env_file_value(file, "CONVEX_SELF_HOSTED_URL"),
                _ => None,
            }
            .or_else(|| env.convex_url.clone())
            .unwrap_or_default();
            // El .env.local del repo trae CONVEX_DEPLOYMENT (cloud): se anula
            // para que la CLI use el self-hosted.
            Ok((
                vec![],
                vec![
                    ("CONVEX_SELF_HOSTED_URL".into(), url),
                    ("CONVEX_SELF_HOSTED_ADMIN_KEY".into(), admin),
                    ("CONVEX_DEPLOYMENT".into(), String::new()),
                ],
            ))
        }
        (EnvKind::CloudProd, _) if write => {
            let (_, key) = crate::secrets::deploy_key(env)?.ok_or_else(|| {
                anyhow::anyhow!("the production deploy key is missing: save it in the environment's Keys tab in Condx")
            })?;
            // La key dice a qué deployment apunta («prod:<nombre>|…»): que sea el registrado.
            if let Some(name) = &env.convex_deployment {
                let target_name = key.split('|').next().unwrap_or_default().trim_start_matches("prod:");
                if target_name != name {
                    bail!("the Keychain deploy key is for “{target_name}”, not “{name}”");
                }
            }
            Ok((vec![], vec![("CONVEX_DEPLOY_KEY".into(), key), ("CONVEX_DEPLOYMENT".into(), String::new())]))
        }
        (EnvKind::CloudDev, _) if write => match &env.convex_deployment {
            // Igual que los scripts de rollout: `convex dev --once` con el deployment de dev.
            Some(name) => Ok((vec![], vec![("CONVEX_DEPLOYMENT".into(), format!("dev:{name}"))])),
            None => bail!("environment {} has no Convex deployment registered", env.name),
        },
        (EnvKind::CloudDev | EnvKind::CloudProd, _) => match &env.convex_deployment {
            Some(name) => Ok((vec!["--deployment".into(), name.clone()], vec![])),
            None if env.kind == EnvKind::CloudProd => Ok((vec!["--prod".into()], vec![])),
            None => bail!("environment {} has no Convex deployment registered", env.name),
        },
        _ => bail!("environment {} has no credential for the Convex CLI", env.name),
    }
}

/// Como `target`, pero en cloud dev usa la deploy key del entorno si está guardada y es suya.
pub fn target_with_key(env: &Environment, write: bool) -> Result<Target> {
    if env.kind == EnvKind::CloudDev {
        if let Some((_, key)) = crate::secrets::deploy_key(env)? {
            if crate::secrets::check_deploy_key(env, &key).is_ok() {
                return Ok((vec![], vec![("CONVEX_DEPLOY_KEY".into(), key), ("CONVEX_DEPLOYMENT".into(), String::new())]));
            }
        }
    }
    target(env, write)
}

/// Con qué credencial corre la CLI en `env`: la admin key del .env (local), la deploy key
/// (cloud, si la receta la prefiere y está guardada) o la sesión de `npx convex`.
pub fn credential_kind(env: &Environment, key_first: bool) -> &'static str {
    match env.kind {
        EnvKind::Devbox | EnvKind::Throwaway => "admin-key",
        EnvKind::CloudProd => "cli",
        EnvKind::CloudDev => {
            let usable = key_first && crate::secrets::deploy_key(env).ok().flatten().is_some_and(|(_, k)| crate::secrets::check_deploy_key(env, &k).is_ok());
            if usable {
                "deploy-key"
            } else {
                "cli"
            }
        }
    }
}

pub(crate) async fn convex(state: &Shared, env: &Environment, sub: &[&str], window: Option<Duration>) -> Result<Out> {
    let repo = crate::projects::repo_for(state, Some(env));
    let (bin, mut args) = convex_bin_at(&repo);
    args.extend(sub.iter().map(|s| s.to_string()));
    let (extra, vars) = target(env, false)?;
    args.extend(extra);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let vars: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    Ok(match window {
        Some(w) => capture_for(&bin, &args, Some(&repo), &vars, w).await,
        None => run(&bin, &args, Some(&repo), &vars, Duration::from_secs(60)).await,
    })
}

/// Nombres de las variables del deployment, nunca sus valores (§9).
pub async fn env_var_names(state: &Shared, env: &Environment) -> Result<Vec<String>> {
    if !can_read(env.kind, Read::EnvVarNames) {
        bail!("the policy doesn't allow reading variables of {}", env.name);
    }
    let out = convex(state, env, &["env", "list", "--names-only"], None).await?;
    if !out.ok {
        bail!(out.error());
    }
    // Defensa extra: aunque la CLI cambie de formato, solo pasan nombres.
    Ok(out
        .stdout
        .lines()
        .filter_map(|l| {
            let name = l.split('=').next()?.trim();
            (!name.is_empty() && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
                .then(|| name.to_string())
        })
        .collect())
}

/// Últimos logs de funciones (JSONL de `convex logs --history`), redactados.
pub async fn function_logs(state: &Shared, env: &Environment, limit: usize, errors_only: bool) -> Result<Vec<Value>> {
    if !can_read(env.kind, Read::FunctionLogs) {
        bail!("the policy doesn't allow reading logs of {}", env.name);
    }
    let history = limit.clamp(1, 500).to_string();
    let out = convex(state, env, &["logs", "--history", &history, "--jsonl"], Some(Duration::from_secs(8))).await?;
    if !out.ok && out.stdout.trim().is_empty() {
        bail!(out.error());
    }
    let mut list: Vec<Value> = out
        .stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| !errors_only || is_error(e))
        .collect();
    let skip = list.len().saturating_sub(limit);
    list.drain(..skip);
    Ok(list)
}

fn is_error(entry: &Value) -> bool {
    entry["error"].is_string()
        || entry["status"] == "failure"
        || entry["level"].as_str().map(|l| l.eq_ignore_ascii_case("error")).unwrap_or(false)
        || entry["logLines"]
            .as_array()
            .map(|lines| lines.iter().any(|l| l.as_str().map(|s| s.contains("[ERROR]")).unwrap_or(false)))
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prod(credential: Option<Credential>) -> Environment {
        Environment {
            name: "prod".into(),
            kind: EnvKind::CloudProd,
            label: None,
            convex_url: Some("https://brave-falcon-456.convex.cloud".into()),
            convex_site_url: None,
            convex_deployment: Some("brave-falcon-456".into()),
            frontend_url: None,
            branch: Some("main".into()),
            credential,
            created_at: 0,
            project: None,
        }
    }

    #[test]
    fn produccion_lee_con_la_sesion_y_escribe_con_la_deploy_key() {
        // Usa el Llavero de macOS: en los runners de CI puede estar bloqueado y colgar la prueba.
        if std::env::var_os("CI").is_some() {
            return;
        }
        // Llavero propio de la prueba; se borra al final.
        std::env::set_var("CONDX_KEYCHAIN", "condx-unit-test");
        let account = Credential::Keychain { account: "prod-key-test".into() };
        let env = prod(Some(account));
        let _ = crate::secrets::delete("prod-key-test");

        let (args, vars) = target(&env, false).unwrap();
        assert_eq!(args, ["--deployment", "brave-falcon-456"]);
        assert!(vars.is_empty());

        let err = target(&env, true).unwrap_err().to_string();
        assert!(err.contains("deploy key is missing"), "{err}");

        crate::secrets::set("prod-key-test", "prod:otro-deployment-123|abcdef0123456789abcdef").unwrap();
        let err = target(&env, true).unwrap_err().to_string();
        assert!(err.contains("is for “otro-deployment-123”"), "{err}");

        crate::secrets::set("prod-key-test", "prod:brave-falcon-456|abcdef0123456789abcdef").unwrap();
        let (args, vars) = target(&env, true).unwrap();
        assert!(args.is_empty());
        assert!(vars.iter().any(|(k, v)| k == "CONVEX_DEPLOY_KEY" && v.starts_with("prod:brave-falcon-456|")));
        assert!(vars.iter().any(|(k, v)| k == "CONVEX_DEPLOYMENT" && v.is_empty()));
        crate::secrets::delete("prod-key-test").unwrap();
    }

    #[test]
    fn cloud_dev_escribe_como_los_rollouts() {
        let mut env = prod(Some(Credential::ConvexLogin));
        env.kind = EnvKind::CloudDev;
        env.convex_deployment = Some("happy-otter-123".into());
        let (args, vars) = target(&env, true).unwrap();
        assert!(args.is_empty());
        assert_eq!(vars, [("CONVEX_DEPLOYMENT".to_string(), "dev:happy-otter-123".to_string())]);
    }
}
