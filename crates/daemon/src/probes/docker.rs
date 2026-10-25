// Contenedores de Docker y deployer del devbox (reemplaza a
// scripts/deployer-status.mjs, que el plugin de Vite invocaba cada segundo).

use std::time::Duration;

use devtool_core::{deployer_phase, DeployerPhase};
use serde_json::{json, Value};

use crate::cmd::run;
use crate::state::Shared;

const T: Duration = Duration::from_secs(5);
const DEPLOYER_SERVICE: &str = "deployer-dev";
const BACKEND_SERVICE: &str = "backend";
const COMPOSE_PROJECT: &str = "heskala";

fn label<'a>(labels: &'a str, key: &str) -> Option<&'a str> {
    labels.split(',').find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
}

pub async fn containers(state: Shared) {
    let out = run("docker", &["ps", "-a", "--format", "{{json .}}"], None, &[], T).await;
    if !out.ok {
        state.set_probe("docker", false, Value::Null, Some(out.error()));
        return;
    }
    let list: Vec<Value> = out
        .stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|c| {
            let labels = c["Labels"].as_str().unwrap_or("");
            json!({
                "name": c["Names"],
                "image": c["Image"],
                "state": c["State"],
                "status": c["Status"],
                "project": label(labels, "com.docker.compose.project"),
                "service": label(labels, "com.docker.compose.service"),
            })
        })
        .collect();
    state.set_probe("docker", true, json!({ "containers": list }), None);
}

/// Contenedor del servicio `service` del compose del devbox.
pub fn compose_container(state: &Shared, service: &str) -> Option<Value> {
    docker_container(state, COMPOSE_PROJECT, service)
}

/// Contenedor de un servicio de compose (según la última sonda de Docker).
pub fn docker_container(state: &Shared, project: &str, service: &str) -> Option<Value> {
    let data = state.probe_data("docker")?;
    data["containers"]
        .as_array()?
        .iter()
        .find(|c| c["project"] == project && c["service"] == service)
        .cloned()
}

pub fn backend_container(state: &Shared) -> Option<Value> {
    compose_container(state, BACKEND_SERVICE)
}

pub async fn deployer(state: Shared) {
    if crate::native::mode(&state) == crate::native::Mode::Native {
        let managed = state.procs.get(crate::native::DEPLOYER);
        let text = std::fs::read_to_string(crate::procs::log_file(crate::native::DEPLOYER)).unwrap_or_default();
        let lines: Vec<&str> = text.lines().rev().take(100).collect::<Vec<_>>().into_iter().rev().collect();
        let phase = if managed.is_some() { deployer_phase(lines.iter().copied()) } else { DeployerPhase::Starting };
        state.set_probe(
            "deployer",
            true,
            json!({
                "container": null,
                "running": managed.is_some(),
                "status": managed.map(|m| format!("pid {}", m.pid)),
                "phase": phase,
                "image": "convex dev (native)",
                "imageId": null,
                "imageCreatedAt": null,
                "containerCreatedAt": null,
                "recent": lines.iter().rev().take(8).rev().collect::<Vec<_>>(),
            }),
            None,
        );
        return;
    }
    let Some(container) = compose_container(&state, DEPLOYER_SERVICE) else {
        state.set_probe(
            "deployer",
            true,
            json!({ "container": null, "running": false, "phase": DeployerPhase::Unknown }),
            None,
        );
        return;
    };
    let name = container["name"].as_str().unwrap_or_default().to_string();
    let running = container["state"] == "running";
    let inspect = run("docker", &["inspect", "-f", "{{.Image}} {{.Created}}", &name], None, &[], T).await;
    let (image_id, created) = inspect
        .stdout
        .trim()
        .split_once(' ')
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .unwrap_or_default();
    let image_created = if image_id.is_empty() {
        None
    } else {
        let out = run("docker", &["image", "inspect", "-f", "{{.Created}}", &image_id], None, &[], T).await;
        out.ok.then(|| out.stdout.trim().to_string())
    };
    let (phase, recent) = if running {
        // `docker logs` reparte la salida del contenedor entre stdout y stderr.
        let logs = run("docker", &["logs", "--tail", "100", &name], None, &[], T).await;
        let lines: Vec<String> = format!("{}\n{}", logs.stdout, logs.stderr)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        let phase = deployer_phase(lines.iter().map(String::as_str));
        let recent: Vec<String> = lines.iter().rev().take(8).rev().cloned().collect();
        (phase, recent)
    } else {
        (DeployerPhase::Starting, vec![])
    };
    state.set_probe(
        "deployer",
        true,
        json!({
            "container": name,
            "running": running,
            "status": container["status"],
            "phase": phase,
            "image": container["image"],
            "imageId": image_id.get(..19).unwrap_or(&image_id),
            "imageCreatedAt": image_created,
            "containerCreatedAt": created,
            "recent": recent,
        }),
        None,
    );
}
