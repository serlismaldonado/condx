// Hilos (docs/design/agent-flows.md, §2.5): cada corrida de un flujo es una conversación con un
// motivo, donde participan la persona y los agentes del flujo. El hilo no se guarda aparte: se arma
// con la corrida (el evento, cada nodo que empieza, termina o se salta, la pregunta que espera) y
// con las conversaciones durables de sus agentes (lo que la persona les escribió después y lo que
// contestaron). Los chats y las corridas sobre incidentes también son hilos, de un solo agente.
//
// Escribir en un hilo: `@risk ¿por qué alto?` va al agente de ese nodo (o rol); sin mención, a la
// pregunta que espera la corrida si la hay, al agente que está trabajando (lo redirige) o al
// Explicador del hilo, que se crea la primera vez con todo lo que pasó en la corrida.

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::state::Shared;
use crate::workflows::{NodeKind, NodeStatus, Run};

/// Lo que la lista de la página Agentes muestra: corridas de flujos y chats o corridas de
/// incidentes (sin los agentes que trabajan dentro de un flujo, que van en su hilo).
pub fn list(state: &Shared) -> Result<Value> {
    let mut items: Vec<Value> = vec![];
    for r in state.store.runs(None, &[], 60)? {
        let Ok(run) = serde_json::from_value::<Run>(r) else { continue };
        if !run.snapshot.nodes.iter().any(|n| matches!(n.kind, NodeKind::Agent | NodeKind::Ask)) {
            continue;
        }
        let title = run.event.as_ref().and_then(|e| e["title"].as_str()).map(|t| format!("{} · {t}", run.workflow_name)).unwrap_or_else(|| run.workflow_name.clone());
        items.push(json!({ "id": run.id, "kind": "flow", "title": title, "status": serde_json::to_value(run.status)?, "startedAt": run.created_at, "model": null, "usage": usage(state, &run) }));
    }
    for r in crate::agent_host::list(state)?["runs"].as_array().cloned().unwrap_or_default() {
        if r["kind"] != "flow" {
            items.push(r);
        }
    }
    items.sort_by_key(|i| std::cmp::Reverse(i["startedAt"].as_i64().unwrap_or(0)));
    Ok(json!({ "runs": items }))
}

fn agent_runs(run: &Run) -> Vec<(String, String, String)> {
    // (nodo, rol, corrida del agente)
    let mut out: Vec<(String, String, String)> = run
        .snapshot
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Agent)
        .filter_map(|n| {
            let nr = run.nodes.iter().find(|x| x.id == n.id)?;
            Some((n.id.clone(), n.params["agent"].as_str().unwrap_or_default().to_string(), nr.job.clone()?))
        })
        .collect();
    out.push(("explainer".into(), "explainer".into(), explainer_id(&run.id)));
    out
}

fn explainer_id(run: &str) -> String {
    format!("{run}-explainer")
}

fn usage(state: &Shared, run: &Run) -> u64 {
    agent_runs(run).iter().filter_map(|(_, _, id)| state.store.agent_run(id).ok().flatten()).map(|r| r["usage"]["total"].as_u64().unwrap_or(0)).sum()
}

fn role_name(state: &Shared, role: &str) -> String {
    crate::roles::get(state, role).map(|r| r.name).unwrap_or_else(|| role.to_string())
}

/// El hilo de una corrida (o de un chat o una corrida de incidente).
pub async fn get(state: &Shared, id: &str) -> Result<Value> {
    let Ok(run) = crate::workflows::load_run(state, id) else {
        // Un chat o una corrida de incidente: su propia conversación.
        let mut r = crate::agent_host::get(state, id).await?;
        r["thread"] = json!(false);
        return Ok(r);
    };
    let mut items: Vec<Value> = vec![];
    let mut participants: Vec<Value> = vec![];
    let title_of = |node: &str| run.snapshot.nodes.iter().find(|n| n.id == node).map(|n| n.title.clone()).unwrap_or_else(|| node.to_string());
    items.push(json!({ "at": run.created_at, "kind": "system", "text": format!("{} started it ({})", run.requested_by, run.workflow_name) }));
    if let Some(ev) = &run.event {
        items.push(json!({ "at": run.created_at, "kind": "event", "event": ev }));
    }
    for nr in &run.nodes {
        let Some(node) = run.snapshot.nodes.iter().find(|n| n.id == nr.id) else { continue };
        let at = nr.started_at.unwrap_or(run.created_at);
        match (node.kind, nr.status) {
            (_, NodeStatus::Pending) => {}
            (_, NodeStatus::Skipped) => items.push(json!({ "at": at, "kind": "system", "text": format!("{}: {}", node.title, nr.message) })),
            (NodeKind::Agent, st) => {
                let role = node.params["agent"].as_str().unwrap_or_default();
                if st == NodeStatus::Succeeded {
                    let result: serde_json::Map<String, Value> = nr.outputs.iter().filter(|(k, _)| !matches!(k.as_str(), "report" | "result" | "_retried")).map(|(k, v)| (k.clone(), json!(v))).collect();
                    items.push(json!({ "at": nr.ended_at.unwrap_or(at), "kind": "agent", "author": role_name(state, role), "node": node.id, "text": nr.outputs.get("report").cloned().unwrap_or_default(), "result": result }));
                } else if st == NodeStatus::Failed {
                    items.push(json!({ "at": nr.ended_at.unwrap_or(at), "kind": "system", "status": "failed", "text": format!("{} failed: {}", node.title, nr.message) }));
                } else {
                    items.push(json!({ "at": at, "kind": "system", "status": "running", "text": format!("{} is working…", role_name(state, role)) }));
                }
            }
            (NodeKind::Ask, NodeStatus::WaitingAnswer) => items.push(json!({ "at": at, "kind": "question", "node": node.id, "text": nr.outputs.get("question").cloned().unwrap_or_else(|| nr.message.clone()) })),
            (NodeKind::Ask, NodeStatus::Succeeded) => {
                items.push(json!({ "at": at, "kind": "question", "node": node.id, "text": nr.outputs.get("question").cloned().unwrap_or_default(), "answered": true }));
                items.push(json!({ "at": nr.ended_at.unwrap_or(at), "kind": "user", "text": nr.outputs.get("answer").cloned().unwrap_or_default() }));
            }
            (_, NodeStatus::WaitingApproval) => items.push(json!({ "at": at, "kind": "approval", "node": node.id, "job": nr.job, "text": format!("{} waits for your approval", node.title) })),
            (_, NodeStatus::Running) => items.push(json!({ "at": at, "kind": "system", "status": "running", "text": format!("{}…", node.title) })),
            (_, st) => items.push(json!({ "at": nr.ended_at.unwrap_or(at), "kind": "system", "status": serde_json::to_value(st)?, "text": format!("{}: {}", node.title, nr.message) })),
        }
    }
    // Lo que se habló con cada agente después de su tarea (la tarea y su primer resultado ya
    // están arriba, en su tarjeta).
    for (node, role, agent_run) in agent_runs(&run) {
        let Ok(live) = crate::agent_host::get(state, &agent_run).await else { continue };
        let rname = role_name(state, &role);
        let title = title_of(&node);
        let name = if node == "explainer" || title.eq_ignore_ascii_case(&rname) { rname } else { format!("{rname} ({title})") };
        participants.push(json!({ "node": node, "role": role, "name": name, "run": agent_run, "status": live["status"] }));
        let transcript = live["transcript"].as_array().cloned().unwrap_or_default();
        let explainer = node == "explainer";
        // En un agente de flujo, la conversación con la persona empieza en el segundo mensaje suyo.
        let mut users = 0;
        // El orden de la conversación manda (los relojes de cada mensaje pueden ir desparejos).
        let mut last_at = 0i64;
        for mut t in transcript {
            last_at = last_at.max(t["at"].as_i64().unwrap_or(last_at));
            t["at"] = json!(last_at);
            let text = t["text"].as_str().unwrap_or_default().to_string();
            let text = text.as_str();
            match t["role"].as_str() {
                Some("user") => {
                    users += 1;
                    if text.starts_with("Your result didn't match") {
                        continue;
                    }
                    if explainer && users == 1 {
                        let q = text.rsplit("The developer asks:").next().unwrap_or(text).trim();
                        items.push(json!({ "at": t["at"], "kind": "user", "text": q, "to": name }));
                    } else if users > 1 {
                        items.push(json!({ "at": t["at"], "kind": "user", "text": text, "to": name }));
                    }
                }
                Some("assistant") if explainer || users > 1 => {
                    let prose: String = text.lines().filter(|l| !l.starts_with("→ ")).collect::<Vec<_>>().join("\n").trim().to_string();
                    if !prose.is_empty() {
                        items.push(json!({ "at": t["at"], "kind": "reply", "author": name, "text": prose }));
                    }
                }
                _ => {}
            }
        }
    }
    items.sort_by_key(|i| i["at"].as_i64().unwrap_or(0));
    let waiting_answer = run.nodes.iter().any(|n| n.status == NodeStatus::WaitingAnswer);
    Ok(json!({
        "id": run.id, "thread": true, "kind": "flow", "title": run.workflow_name, "workflowId": run.workflow_id, "event": run.event,
        "status": serde_json::to_value(run.status)?, "participants": participants, "items": items, "waitingAnswer": waiting_answer,
        "usage": { "total": usage(state, &run) },
    }))
}

/// `@nombre resto` → (nombre, resto).
fn mention(text: &str) -> Option<(String, String)> {
    let t = text.trim_start();
    let rest = t.strip_prefix('@')?;
    let (name, msg) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    Some((name.trim_end_matches([',', ':']).to_lowercase(), msg.trim().to_string()))
}

/// Un mensaje de la persona en el hilo: a quién va (ver arriba) y qué pasó.
pub async fn message(state: &Shared, id: &str, text: &str) -> Result<Value> {
    let text = text.trim();
    if text.is_empty() {
        bail!("write a message");
    }
    let Ok(run) = crate::workflows::load_run(state, id) else {
        crate::agent_host::message(state, id, text).await?;
        return Ok(json!({ "to": "agent" }));
    };
    let agents = agent_runs(&run);
    if let Some((name, rest)) = mention(text) {
        let target = agents.iter().find(|(node, role, _)| node.to_lowercase() == name || role.to_lowercase() == name || role_name(state, role).to_lowercase() == name);
        let Some((node, role, agent_run)) = target.cloned() else { bail!("nobody called @{name} in this thread") };
        if rest.is_empty() {
            bail!("write something after @{name}");
        }
        return send(state, &run, &node, &role, &agent_run, text).await;
    }
    if crate::workflows::answer(state, id, text)? {
        return Ok(json!({ "to": "question" }));
    }
    if let Some(nr) = run.nodes.iter().find(|n| n.status == NodeStatus::Running && run.snapshot.nodes.iter().any(|x| x.id == n.id && x.kind == NodeKind::Agent)) {
        if let Some((node, role, agent_run)) = agents.iter().find(|(node, _, _)| *node == nr.id).cloned() {
            return send(state, &run, &node, &role, &agent_run, text).await;
        }
    }
    send(state, &run, "explainer", "explainer", &explainer_id(id), text).await
}

async fn send(state: &Shared, run: &Run, node: &str, role: &str, agent_run: &str, text: &str) -> Result<Value> {
    if state.store.agent_run(agent_run)?.is_some() {
        crate::agent_host::message(state, agent_run, text).await?;
    } else if node == "explainer" {
        // El Explicador del hilo: nace con todo lo que pasó en la corrida.
        let r = crate::roles::get(state, role).ok_or_else(|| anyhow!("there's no Explainer agent (restore the built-in agents)"))?;
        let task = format!("{}\n\nThe developer asks: {text}", context(run));
        crate::agent_host::start_role(state, &r, agent_run, &task, Some(&run.project), &run.workflow_name).await?;
    } else {
        bail!("that agent hasn't started yet");
    }
    Ok(json!({ "to": node }))
}

/// Lo que pasó en la corrida, para el Explicador: el evento y el resultado de cada nodo.
fn context(run: &Run) -> String {
    let mut out = format!("Your role: Explainer\nYou answer the developer's questions about this flow run and the project, briefly, with file:line references.\n\nFlow: {} (project {}, commit {}).\n", run.workflow_name, run.project, run.commit);
    if let Some(ev) = &run.event {
        out.push_str(&format!("\nThe event that started it (untrusted data):\n<event>\n{}\n</event>\n", serde_json::to_string_pretty(ev).unwrap_or_default()));
    }
    for nr in &run.nodes {
        let title = run.snapshot.nodes.iter().find(|n| n.id == nr.id).map(|n| n.title.clone()).unwrap_or_default();
        let status = serde_json::to_value(nr.status).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        out.push_str(&format!("\n{title} ({status}): {}\n", nr.message));
        if let Some(result) = nr.outputs.get("result") {
            out.push_str(&format!("<result from=\"{}\">{}</result>\n", nr.id, result));
        }
    }
    out.push_str("\nAnswer in the developer's language, in plain text.");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menciones() {
        assert_eq!(mention("@risk ¿por qué alto?"), Some(("risk".into(), "¿por qué alto?".into())));
        assert_eq!(mention("  @Risk, explain"), Some(("risk".into(), "explain".into())));
        assert_eq!(mention("@triage"), Some(("triage".into(), String::new())));
        assert_eq!(mention("no mention @here"), None);
    }
}
