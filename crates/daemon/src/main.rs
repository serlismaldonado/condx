// condxd: daemon local de Condx
// (docs/diseno-devtool-escritorio.md). F0: entornos, sondas, telemetría de la
// app y MCP de lectura; nada despliega ni escribe datos.

mod agent_host;
mod agents;
mod attachments;
mod auth;
mod cloud;
mod cmd;
mod connections;
mod config;
mod create;
mod data;
mod doctor;
mod http;
mod incidents;
mod mcp;
mod jobs;
mod overview;
mod pipeline;
mod policy;
mod probes;
mod recipes;
mod native;
mod notify;
mod outcomes;
mod procs;
mod projects;
mod frontend;
mod functions;
mod gitflow;
mod gitinfo;
mod mail;
mod schema;
mod seed;
mod secrets;
mod services;
mod state;
mod store;
mod conditions;
mod events;
mod roles;
mod templates;
mod threads;
mod telemetry;
mod workflows;
#[cfg(test)]
mod security_tests;

use std::io::{BufRead, Write};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use devtool_core::{Credential, EnvKind, Environment};

use crate::config::{home, Config};
use crate::state::State;
use crate::store::{now_ms, Store};

const HELP: &str = "condxd — local devtool for Convex

Usage:
  condxd [serve]              starts the daemon (127.0.0.1, port from config.json; 4790 by default)
  condxd mcp                  MCP over stdio (bridge to the daemon) for Claude Code, Codex, Cursor…
  condxd status               prints the overview
  condxd env list
  condxd env add <name> --kind <devbox|throwaway|cloud-dev|cloud-prod>
                       [--url U] [--site-url S] [--deployment D] [--label L] [--branch B] [--frontend F]
  condxd env remove <name>
  condxd project list         Convex projects
  condxd project add <folder> [--name N]    adds a Convex repo as a project
  condxd project remove <name>              removes it (keeps its data)
  condxd project discover [folder]          Convex repos in a folder
  condxd install | uninstall  launchd service (dev.condx.daemon)
  condxd secret set prod-deploy-key   stores the production deploy key in the Keychain (reads it from stdin)
  condxd secret set dev-deploy-key    stores the cloud dev deploy key (optional: record counts in Data)
  condxd secret status                which secrets exist (never their values)
  condxd cloud link --url <URL> [--name N] [--site URL]   links to Condx Cloud (reads an admin or deploy key from stdin, once)
  condxd cloud site <https://…> | --clear     custom domain for the QR, /pair, /ingest and /files
  condxd cloud status | unlink

Files: ~/.condx/daemon/{config.json,token,devd.sqlite3,devd.log}
";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let sub = args.first().map(String::as_str).unwrap_or("serve");
    match sub {
        "serve" => serve(),
        "mcp" => mcp_stdio(),
        "status" => status(),
        "env" => env_cmd(&args[1..]),
        "project" => project_cmd(&args[1..]),
        "install" => launchd(true),
        "secret" => secret_cmd(&args[1..]),
        "cloud" => cloud_cmd(&args[1..]),
        "uninstall" => launchd(false),
        "-h" | "--help" | "help" => {
            print!("{HELP}");
            Ok(())
        }
        other => bail!("unknown subcommand: {other}\n\n{HELP}"),
    }
}

fn open_store() -> Result<Store> {
    std::fs::create_dir_all(home())?;
    Store::open(&home().join("devd.sqlite3"))
}

fn switch_registry() -> std::path::PathBuf {
    std::env::var_os("HESKALA_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap().join(".config/heskala"))
        .join("deployments.json")
}

fn serve() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        // Bajo launchd la salida va a devd.log: sin colores ANSI.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();
    let config = Config::load_or_init()?;
    // El puerto antes que nada: si hay otro daemon (p. ej. el viejo de launchd), no se
    // mueven sus carpetas.
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], config.port));
    let listener = std::net::TcpListener::bind(addr).with_context(|| format!("couldn't listen on {addr} (is another condxd running?)"))?;
    listener.set_nonblocking(true)?;
    config::migrate_legacy();
    let store = open_store()?;
    let imported = match &config.repo {
        Some(repo) => store.import_switch_registry(&switch_registry(), repo)?,
        None => 0,
    };
    if imported > 0 {
        tracing::info!("imported {imported} environments from {}", switch_registry().display());
    }
    let tokens = auth::Tokens::load()?;
    let state = Arc::new(State::new(config, store, tokens));
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        probes::spawn_all(&state);
        incidents::spawn(&state);
        agent_host::spawn_poller(&state);
        notify::spawn(&state);
        mail::spawn(&state);
        events::spawn(&state);
        jobs::recover(&state);
        workflows::recover(&state);
        if let Err(e) = projects::ensure_default(&state) {
            tracing::warn!("couldn't register the default project: {e:#}");
        }
        // Fuera del arranque: lee archivos de los repos y no debe demorar el puerto.
        let st = state.clone();
        tokio::task::spawn_blocking(move || projects::ensure_all_envs(&st));
        procs::adopt_all(&state);
        jobs::spawn_scheduler(&state);
        cloud::spawn(state.clone());
        let listener = tokio::net::TcpListener::from_std(listener)?;
        match &state.config.repo {
            Some(repo) => tracing::info!("condxd at http://{addr} · heskala-erp at {}", repo.display()),
            None => tracing::info!("condxd at http://{addr}"),
        }
        axum::serve(listener, http::router(state)).await?;
        Ok(())
    })
}

fn daemon_url(path: &str) -> Result<String> {
    let config = Config::load_or_init()?;
    Ok(format!("http://127.0.0.1:{}{path}", config.port))
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(150)).build()
}

/// MCP por stdio: una línea JSON-RPC por mensaje, reenviada a POST /mcp.
fn mcp_stdio() -> Result<()> {
    let url = daemon_url("/mcp")?;
    let token = config::token()?;
    let agent = agent();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: serde_json::Value = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(_) => {
                writeln!(stdout, r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":-32700,"message":"invalid JSON"}}}}"#)?;
                stdout.flush()?;
                continue;
            }
        };
        let res = agent.post(&url).set("Authorization", &format!("Bearer {token}"))
            .set("Content-Type", "application/json")
            .send_string(&msg.to_string());
        let reply = match res {
            Ok(r) if r.status() == 202 => None,
            Ok(r) => r.into_string().ok(),
            Err(e) => msg.get("id").filter(|id| !id.is_null()).map(|id| {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32000, "message": format!("condxd isn't responding at {url} ({e}). Start it with `condxd serve` or `condxd install`.") }
                })
                .to_string()
            }),
        };
        if let Some(r) = reply {
            writeln!(stdout, "{}", r.trim())?;
            stdout.flush()?;
        }
    }
    Ok(())
}

fn status() -> Result<()> {
    let url = daemon_url("/api/overview")?;
    let token = config::token()?;
    let body = agent()
        .get(&url)
        .set("Authorization", &format!("Bearer {token}"))
        .call()
        .with_context(|| format!("condxd isn't responding at {url}"))?
        .into_string()?;
    let value: serde_json::Value = serde_json::from_str(&body)?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

/// Proyectos: pasan por el daemon (asigna puertos y detiene procesos).
fn project_cmd(args: &[String]) -> Result<()> {
    let token = config::token()?;
    let auth = format!("Bearer {token}");
    let call = |req: ureq::Request, body: Option<serde_json::Value>| -> Result<serde_json::Value> {
        let req = req.set("Authorization", &auth);
        let res = match body {
            Some(b) => req.set("Content-Type", "application/json").send_string(&b.to_string()),
            None => req.call(),
        };
        match res {
            Ok(r) => Ok(serde_json::from_str(&r.into_string()?)?),
            Err(ureq::Error::Status(_, r)) => {
                let v: serde_json::Value = r.into_string().ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
                bail!("{}", v["error"].as_str().unwrap_or("daemon error"))
            }
            Err(e) => bail!("condxd isn't responding ({e})"),
        }
    };
    match args.first().map(String::as_str) {
        Some("list") | None => {
            let list = call(agent().get(&daemon_url("/api/projects")?), None)?;
            for p in list.as_array().into_iter().flatten() {
                println!(
                    "{:<24} backend {:<5} frontend {:<5} {:<24} {}",
                    p["name"].as_str().unwrap_or(""),
                    p["backendPort"],
                    if p["frontendUrl"].is_null() { "-".to_string() } else { p["frontendPort"].to_string() },
                    p["convexProject"].as_str().unwrap_or("-"),
                    p["repo"].as_str().unwrap_or("")
                );
            }
        }
        Some("add") => {
            let path = args.get(1).filter(|p| !p.starts_with("--")).context("missing repo folder")?;
            let path = std::fs::canonicalize(path).with_context(|| format!("{path} doesn't exist"))?;
            let p = call(
                agent().post(&daemon_url("/api/projects")?),
                Some(serde_json::json!({ "path": path.display().to_string(), "name": flag(args, "--name") })),
            )?;
            println!(
                "Project “{}” added: backend on {}, frontend on {}.",
                p["name"].as_str().unwrap_or(""),
                p["backendPort"],
                if p["frontend"].as_array().map(|a| a.is_empty()).unwrap_or(true) { "(not detected)".to_string() } else { p["frontendPort"].to_string() }
            );
        }
        Some("remove") => {
            let name = args.get(1).context("missing name")?;
            let v = call(agent().delete(&daemon_url(&format!("/api/projects/{name}"))?), None)?;
            println!("Removed. Its data is still in {}", v["dataKept"].as_str().unwrap_or("?"));
        }
        Some("discover") => {
            let mut url = daemon_url("/api/projects/discover")?;
            if let Some(root) = args.get(1) {
                url = format!("{url}?root={}", urlencode(root));
            }
            let v = call(agent().get(&url), None)?;
            println!("In {}:", v["root"].as_str().unwrap_or(""));
            for c in v["candidates"].as_array().into_iter().flatten() {
                println!(
                    "  {:<60} {:<26} {}",
                    c["path"].as_str().unwrap_or(""),
                    c["convexProject"].as_str().unwrap_or("-"),
                    c["project"].as_str().map(|n| format!("(already “{n}”)")).unwrap_or_default()
                );
            }
        }
        Some(other) => bail!("project {other}: use list, add, remove or discover"),
    }
    Ok(())
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn env_cmd(args: &[String]) -> Result<()> {
    let store = open_store()?;
    match args.first().map(String::as_str) {
        Some("list") | None => {
            for env in store.environments()? {
                println!(
                    "{:<16} {:<11} {:<46} {}",
                    env.name,
                    env.kind.as_str(),
                    env.convex_url.as_deref().unwrap_or("-"),
                    env.label.as_deref().unwrap_or("")
                );
            }
        }
        Some("add") => {
            let name = args.get(1).filter(|n| !n.starts_with("--")).context("missing name")?;
            let kind = flag(args, "--kind").and_then(|k| EnvKind::parse(&k)).context("--kind devbox|throwaway|cloud-dev|cloud-prod")?;
            let deployment = flag(args, "--deployment").map(|d| d.trim_start_matches("prod:").trim_start_matches("dev:").to_string());
            let url = flag(args, "--url").or_else(|| deployment.as_ref().map(|d| format!("https://{d}.convex.cloud")));
            let site_url = flag(args, "--site-url").or_else(|| deployment.as_ref().map(|d| format!("https://{d}.convex.site")));
            let config = Config::load_or_init()?;
            let credential = match kind {
                EnvKind::Devbox | EnvKind::Throwaway => Some(Credential::EnvFile {
                    path: config.erp()?.join("docker/onprem-dev/.env").display().to_string(),
                    key: "CONVEX_SELF_HOSTED_ADMIN_KEY".into(),
                }),
                _ => Some(Credential::ConvexLogin),
            };
            let previous = store.environment(name)?;
            store.upsert_environment(&Environment {
                name: name.clone(),
                kind,
                label: flag(args, "--label"),
                convex_url: url,
                convex_site_url: site_url,
                convex_deployment: deployment,
                frontend_url: flag(args, "--frontend"),
                branch: flag(args, "--branch").or_else(|| Some(if kind == EnvKind::CloudProd { "main" } else { "dev" }.into())),
                credential,
                created_at: previous.map(|p| p.created_at).unwrap_or_else(now_ms),
                project: None,
            })?;
            println!("Environment “{name}” saved ({}).", kind.as_str());
        }
        Some("remove") => {
            let name = args.get(1).context("missing name")?;
            println!("{}", if store.remove_environment(name)? { "Eliminado." } else { "It didn't exist." });
        }
        Some(other) => bail!("env {other}: use list, add or remove"),
    }
    Ok(())
}

/// Secretos en el Llavero. Correrlo con el binario instalado
/// (~/.condx/daemon/bin/condxd) para que el daemon lo lea sin pedir permiso.
fn secret_cmd(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("set") => {
            let account = args.get(1).context("missing account (prod-deploy-key, dev-deploy-key, github-token or agentmail-key)")?;
            secrets::writable_account(account)?;
            if account == mail::KEY_ACCOUNT {
                eprintln!("Paste the AgentMail API key and press Enter (it isn't shown):");
                let mut value = String::new();
                std::io::stdin().read_line(&mut value)?;
                let value = value.trim();
                if value.len() < 16 || value.contains(char::is_whitespace) {
                    bail!("that doesn't look like an AgentMail API key");
                }
                secrets::set(account, value)?;
                println!("Saved ({account}). Turn email on in Condx (or POST /api/mail).");
                return Ok(());
            }
            if account == gitflow::TOKEN_ACCOUNT {
                // Para el respaldo y los PR de los agentes: mejor uno fine-grained, solo los repos de
                // los proyectos, con Contents y Pull requests de escritura.
                eprintln!("Paste a GitHub token (fine-grained: Contents + Pull requests, read and write) and press Enter (it isn't shown):");
                let mut value = String::new();
                std::io::stdin().read_line(&mut value)?;
                let value = value.trim();
                if value.len() < 20 || value.contains(char::is_whitespace) {
                    bail!("that doesn't look like a GitHub token");
                }
                secrets::set(account, value)?;
                println!("Saved ({account}). Agents push their branches and open PRs with it.");
                return Ok(());
            }
            let (what, prefix) = if account == secrets::DEV_DEPLOY_KEY { ("cloud dev", "dev:") } else { ("production", "prod:") };
            eprintln!("Paste the {what} deploy key and press Enter (it isn't shown or kept in history):");
            let mut value = String::new();
            std::io::stdin().read_line(&mut value)?;
            let value = value.trim();
            if !value.starts_with(prefix) || !value.contains('|') {
                bail!("that doesn't look like a Convex {what} deploy key (“{prefix}<deployment>|…”)");
            }
            secrets::set(account, value)?;
            println!("Saved ({account}, deployment {}).", value.split('|').next().unwrap_or_default());
        }
        Some("delete") => {
            let account = args.get(1).context("missing account")?;
            secrets::writable_account(account)?;
            secrets::delete(account)?;
            println!("Borrada.");
        }
        Some("status") | None => {
            for account in [secrets::PROD_DEPLOY_KEY, secrets::DEV_DEPLOY_KEY, secrets::BACKUP_KEY, gitflow::TOKEN_ACCOUNT, mail::KEY_ACCOUNT] {
                println!("{account:<16} {}", if secrets::exists(account) { "stored" } else { "missing" });
            }
        }
        Some(other) => bail!("secret {other}: use set, delete or status"),
    }
    Ok(())
}

const LABEL: &str = "dev.condx.daemon";
/// Servicio de antes del renombre: se detiene y se borra al instalar o desinstalar.
const LEGACY_LABEL: &str = "com.heskala.devd";

/// Firma el binario con la identidad estable (`CONDX_SIGN_IDENTITY`, por defecto «Condx Dev»; la
/// crea `scripts/create-signing-cert.sh`): así el «Permitir siempre» del Llavero sigue valiendo
/// después de cada actualización. Sin esa identidad queda la firma ad hoc del compilador.
fn sign(path: &std::path::Path) {
    let identity = std::env::var("CONDX_SIGN_IDENTITY").unwrap_or_else(|_| "Condx Dev".into());
    let found = std::process::Command::new("security").args(["find-identity", "-v", "-p", "codesigning"]).output().map(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{identity}\""))).unwrap_or(false);
    if !found {
        println!("No “{identity}” signing identity: macOS will ask again for the Keychain after each update (see docs/signing.md).");
        return;
    }
    let ok = std::process::Command::new("codesign").args(["--force", "--sign", &identity, "--identifier", "dev.condx.daemon"]).arg(path).status().map(|s| s.success()).unwrap_or(false);
    println!("{}", if ok { format!("Signed with “{identity}”.") } else { format!("Couldn't sign with “{identity}”: it keeps the ad hoc signature.") });
}

fn launchd(install: bool) -> Result<()> {
    let agents = dirs::home_dir().unwrap().join("Library/LaunchAgents");
    let plist = agents.join(format!("{LABEL}.plist"));
    let uid = std::process::Command::new("id").arg("-u").output()?;
    let domain = format!("gui/{}", String::from_utf8_lossy(&uid.stdout).trim());
    stop_service(&domain, LEGACY_LABEL);
    let _ = std::fs::remove_file(agents.join(format!("{LEGACY_LABEL}.plist")));
    stop_service(&domain, LABEL);
    if !install {
        let _ = std::fs::remove_file(&plist);
        println!("Service {LABEL} stopped and removed.");
        return Ok(());
    }
    // Con los dos servicios abajo ya se pueden mover las carpetas viejas.
    config::migrate_legacy();
    let config = Config::load_or_init()?;
    // El binario se copia fuera del repo: macOS protege ~/Documents y un
    // binario recompilado ahí queda trabado en dyld esperando permiso. La
    // copia solo cambia al reinstalar.
    let bin_dir = home().join("bin");
    std::fs::create_dir_all(&bin_dir)?;
    let exe = bin_dir.join("condxd");
    let tmp = bin_dir.join("condxd.new");
    std::fs::copy(std::env::current_exe()?, &tmp)?;
    sign(&tmp);
    std::fs::rename(&tmp, &exe)?;
    // Nombre viejo como enlace: los MCP ya registrados lo llaman así.
    let legacy = bin_dir.join("heskala-devd");
    let _ = std::fs::remove_file(&legacy);
    #[cfg(unix)]
    let _ = std::os::unix::fs::symlink("condxd", &legacy);
    let log = home().join("devd.log");
    let path = std::env::var("PATH").unwrap_or_default();
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array><string>{exe}</string><string>serve</string></array>
  <key>WorkingDirectory</key><string>{workdir}</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
  <key>EnvironmentVariables</key><dict><key>PATH</key><string>{path}</string></dict>
</dict>
</plist>
"#,
        exe = exe.display(),
        workdir = home().display(),
        log = log.display(),
    );
    std::fs::create_dir_all(plist.parent().unwrap())?;
    std::fs::write(&plist, xml)?;
    let ok = std::process::Command::new("launchctl").args(["bootstrap", &domain, &plist.display().to_string()]).status()?;
    if !ok.success() {
        bail!("launchctl bootstrap failed");
    }
    println!("Service {LABEL} installed: {} (log at {})", plist.display(), log.display());
    println!("If macOS asks to let condxd access your folders (e.g. Documents), accept it: it needs that to read git and run the Convex CLI in your projects.");
    let _ = config;
    Ok(())
}

fn stop_service(domain: &str, label: &str) {
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &format!("{domain}/{label}")])
        .stderr(std::process::Stdio::null())
        .status();
    // `bootout` vuelve antes de que el servicio termine de bajar; un
    // `bootstrap` inmediato falla con «Input/output error».
    for _ in 0..50 {
        let alive = std::process::Command::new("launchctl")
            .args(["print", &format!("{domain}/{label}")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !alive {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn cloud_cmd(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("link") => {
            let url = flag(args, "--url").context("missing --url (the Condx Cloud deployment URL)")?;
            let name = flag(args, "--name").unwrap_or_else(|| "Condx".into());
            // La key llega por stdin (no queda en el historial del shell) y no se guarda.
            let mut key = String::new();
            std::io::stdin().read_line(&mut key)?;
            let link = cloud::link(&url, key.trim(), &name)?;
            println!("Linked as {} to {}. The daemon syncs within a few seconds.", link.daemon_id, link.url);
            if let Some(site) = flag(args, "--site") {
                println!("Site: {}", cloud::set_site(Some(&site))?.site());
            }
            Ok(())
        }
        Some("site") => {
            // Un dominio propio para el QR, `/pair`, `/ingest` y `/files`; `--clear` vuelve al del deployment.
            let link = match args.get(1).map(String::as_str) {
                Some("--clear") => cloud::set_site(None)?,
                Some(site) => cloud::set_site(Some(site))?,
                None => cloud::load_link().context("not linked")?,
            };
            println!("Site: {}", link.site());
            Ok(())
        }
        Some("unlink") => {
            cloud::unlink()?;
            println!("Unlinked. Data already in the deployment stays there.");
            Ok(())
        }
        Some("status") | None => {
            match cloud::load_link() {
                Some(l) => println!("Linked as {} ({}) to {} since {} · site {}", l.daemon_id, l.name, l.url, l.linked_at, l.site()),
                None => println!("Not linked."),
            }
            Ok(())
        }
        Some(other) => bail!("unknown cloud subcommand: {other}"),
    }
}
