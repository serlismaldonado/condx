// Crear un proyecto nuevo en Convex (receta project-create): genera la carpeta
// con una plantilla de create-convex, instala dependencias, crea el proyecto en
// la cuenta de Convex con la sesión de la CLI de esta Mac (convex dev
// --configure new, que deja su .env.local con el deployment de desarrollo) y lo
// registra en el devtool. «Agregar proyecto» es otra cosa: solo registra una
// carpeta que ya existe.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use regex::Regex;

use crate::jobs::{exec, log_line, Job};
use crate::state::Shared;

/// Nombre válido para la carpeta y el proyecto: minúsculas, números y guiones.
pub fn valid_name(name: &str) -> bool {
    Regex::new(r"^[a-z0-9][a-z0-9-]{1,60}$").map(|re| re.is_match(name)).unwrap_or(false)
}

fn arg<'a>(job: &'a Job, key: &str) -> Result<&'a str> {
    job.args.get(key).map(String::as_str).ok_or_else(|| anyhow!("missing argument {key}"))
}

/// Carpeta del proyecto nuevo: `parent/name`.
fn target(job: &Job) -> Result<(PathBuf, String)> {
    let name = arg(job, "name")?.to_string();
    if !valid_name(&name) {
        bail!("“{name}” isn't a valid name: lowercase letters, digits and hyphens (2 to 61)");
    }
    let parent = PathBuf::from(crate::projects::expand(arg(job, "parent")?));
    if !parent.is_dir() {
        bail!("folder {} doesn't exist", parent.display());
    }
    Ok((parent.join(&name), name))
}

/// Las plantillas que salen de un ejemplo de un monorepo (como los de Better Auth) traen
/// dependencias locales (`file:../..`, `workspace:`) que fuera del repo no existen. Se cambian por
/// versiones de npm: el rango que pide una dependencia que las declara como «peer» (así
/// `better-auth` queda en el que exige `@convex-dev/better-auth`) o, si no, `^` la última. Se
/// reemplaza el texto para no reordenar el package.json.
async fn unlink_local_deps(id: &str, dir: &std::path::Path) -> Result<String> {
    let path = dir.join("package.json");
    let mut text = std::fs::read_to_string(&path)?;
    let pkg: serde_json::Value = serde_json::from_str(&text)?;
    let local = |v: &str| v.starts_with("file:") || v.starts_with("workspace:") || v.starts_with("link:");
    let mut found: Vec<(String, String)> = vec![];
    for section in ["dependencies", "devDependencies"] {
        for (name, v) in pkg[section].as_object().into_iter().flatten() {
            if let Some(v) = v.as_str().filter(|v| local(v)) {
                found.push((name.clone(), v.to_string()));
            }
        }
    }
    if found.is_empty() {
        return Ok(String::new());
    }
    let npm = |args: Vec<String>| async move {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        crate::cmd::run("npm", &args, None, &[], Duration::from_secs(60)).await
    };
    // Rangos «peer» de las dependencias locales (las que traen las demás).
    let mut peers: std::collections::HashMap<String, String> = Default::default();
    for (name, _) in &found {
        let out = npm(vec!["view".into(), name.clone(), "peerDependencies".into(), "--json".into()]).await;
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(out.stdout.trim()) {
            for (k, v) in map {
                if let Some(v) = v.as_str() {
                    peers.entry(k).or_insert_with(|| v.to_string());
                }
            }
        }
    }
    let mut changed = vec![];
    for (name, old) in &found {
        let version = match peers.get(name) {
            Some(range) => range.clone(),
            None => {
                let out = npm(vec!["view".into(), name.clone(), "version".into()]).await;
                let v = out.stdout.trim().to_string();
                if !out.ok || v.is_empty() {
                    bail!("couldn't find {name} on npm to replace “{old}”");
                }
                format!("^{v}")
            }
        };
        text = text.replace(&format!("\"{name}\": \"{old}\""), &format!("\"{name}\": \"{version}\""));
        log_line(id, &format!("{name}: {old} → {version}"));
        changed.push(name.clone());
    }
    std::fs::write(&path, text)?;
    // El lockfile del ejemplo apunta a las rutas locales: npm install lo rehace.
    let _ = std::fs::remove_file(dir.join("package-lock.json"));
    Ok(format!(" (local dependencies replaced with npm: {})", changed.join(", ")))
}

pub async fn builtin(state: &Shared, job: &mut Job, step: &str, cancel: &mut tokio::sync::watch::Receiver<bool>) -> Result<String> {
    let id = job.id.clone();
    let (dir, name) = target(job)?;
    let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
    match step {
        "project-create-scaffold" => {
            if dir.exists() {
                bail!("{} already exists: pick another name or add it with “Add project”", dir.display());
            }
            let template = arg(job, "template")?.to_string();
            let parent = dir.parent().unwrap_or(&dir).to_path_buf();
            let cmd = argv(&["npm", "create", "--yes", "convex@latest", &name, "--", "-t", &template]);
            log_line(&id, &format!("$ {}", cmd.join(" ")));
            exec(&id, &cmd, &parent, &[("CI", "1")], Duration::from_secs(600), cancel).await?;
            if !dir.join("package.json").exists() {
                bail!("create-convex didn't leave {}/package.json", dir.display());
            }
            let fixed = unlink_local_deps(&id, &dir).await?;
            Ok(format!("{} with template {template}{fixed}", dir.display()))
        }
        "project-create-install" => {
            let cmd = argv(&["npm", "install"]);
            log_line(&id, "$ npm install");
            exec(&id, &cmd, &dir, &[], Duration::from_secs(900), cancel).await?;
            Ok("dependencies installed".into())
        }
        "project-create-convex" => {
            // Sin sesión de la CLI, convex dev pediría iniciar sesión en una terminal: mejor avisar.
            let config = dirs::home_dir().unwrap_or_default().join(".convex/config.json");
            if !config.exists() {
                bail!("the Convex CLI isn't logged in on this Mac: run “npx convex login” in a terminal and retry");
            }
            let mut cmd = argv(&["npx", "convex", "dev", "--once", "--configure", "new", "--project", &name]);
            let team = arg(job, "team")?.to_string();
            if team != "auto" {
                cmd.extend(argv(&["--team", &team]));
            }
            log_line(&id, &format!("$ {}", cmd.join(" ")));
            exec(&id, &cmd, &dir, &[("CI", "1")], Duration::from_secs(600), cancel).await?;
            let env = std::fs::read_to_string(dir.join(".env.local")).unwrap_or_default();
            let deployment = env.lines().find_map(|l| l.strip_prefix("CONVEX_DEPLOYMENT=")).map(|v| v.split('#').next().unwrap_or(v).trim().to_string());
            match deployment {
                Some(d) => Ok(format!("project {name} on Convex ({d})")),
                None => bail!("convex dev finished but didn't leave CONVEX_DEPLOYMENT in .env.local"),
            }
        }
        "project-create-register" => {
            let p = crate::projects::register(state, &dir, None)?;
            job.outputs.insert("project".into(), p.name.clone());
            Ok(format!("added to Condx as {}", p.name))
        }
        other => bail!("unknown step: {other}"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn nombres_validos() {
        assert!(super::valid_name("mi-app"));
        assert!(super::valid_name("crm2"));
        assert!(!super::valid_name("Mi App"));
        assert!(!super::valid_name("-raro"));
        assert!(!super::valid_name("a"));
        assert!(!super::valid_name("../fuera"));
    }
}
