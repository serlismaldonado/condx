// Resultados (docs/design/agent-flows.md, fase F7): cuánto rinde cada agente y cada flujo en los
// últimos 30 días, para decidir cuáles conservar, cambiar o apagar. Se calcula de lo que ya está
// guardado (corridas de agentes y de flujos): no hay contadores aparte.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::state::Shared;
use crate::store::now_ms;

const WINDOW_MS: i64 = 30 * 24 * 3600 * 1000;

#[derive(Default)]
struct Agg {
    runs: u64,
    ok: u64,
    failed: u64,
    stopped: u64,
    tokens: u64,
    cost: f64,
    ms: i64,
    timed: u64,
    // Solo los que escriben: cambios hechos, descartados.
    changes: u64,
    discarded: u64,
}

impl Agg {
    fn add_run(&mut self, r: &Value) {
        self.runs += 1;
        match r["status"].as_str() {
            Some("done") => self.ok += 1,
            Some("stopped" | "aborted") => self.stopped += 1,
            Some("running") => {}
            _ => self.failed += 1,
        }
        self.tokens += r["usage"]["total"].as_u64().unwrap_or(0);
        self.cost += r["usage"]["cost"].as_f64().unwrap_or(0.);
        if let (Some(a), Some(b)) = (r["startedAt"].as_i64(), r["endedAt"].as_i64()) {
            self.ms += (b - a).max(0);
            self.timed += 1;
        }
        if r["fix"]["changes"] == true {
            self.changes += 1;
        }
        if r["fix"]["discarded"] == true {
            self.discarded += 1;
        }
    }

    fn json(&self) -> Value {
        let rate = |n: u64| if self.runs == 0 { 0. } else { (n as f64 / self.runs as f64 * 100.).round() };
        json!({
            "runs": self.runs, "ok": self.ok, "failed": self.failed, "stopped": self.stopped, "okRate": rate(self.ok),
            "tokens": self.tokens, "avgTokens": self.tokens.checked_div(self.runs).unwrap_or(0),
            "cost": (self.cost * 100.).round() / 100., "avgMinutes": if self.timed == 0 { 0. } else { ((self.ms as f64 / self.timed as f64) / 6_000.).round() / 10. },
            "changes": self.changes, "discarded": self.discarded,
        })
    }
}

pub fn json(state: &Shared) -> Value {
    let since = now_ms() - WINDOW_MS;
    let runs: Vec<Value> = state.store.agent_runs(&[], None).unwrap_or_default().into_iter().filter(|r| r["startedAt"].as_i64().unwrap_or(0) >= since).collect();
    // Por agente del catálogo (los de los flujos), por modo (diagnóstico, arreglo, chat) para los demás.
    let mut by_agent: BTreeMap<String, Agg> = BTreeMap::new();
    for r in &runs {
        let key = r["role"].as_str().map(str::to_string).unwrap_or_else(|| format!("mode:{}", r["mode"].as_str().unwrap_or("?")));
        by_agent.entry(key).or_default().add_run(r);
    }
    let names: BTreeMap<String, String> = crate::roles::list(state).into_iter().map(|r| (r.id, r.name)).collect();
    let agents: Vec<Value> = by_agent
        .iter()
        .map(|(k, a)| {
            let name = names.get(k).cloned().unwrap_or_else(|| match k.as_str() {
                "mode:diagnose" => "Incident diagnosis".to_string(),
                "mode:fix" => "Incident fix".to_string(),
                "mode:chat" => "Chats".to_string(),
                other => other.trim_start_matches("mode:").to_string(),
            });
            let mut v = a.json();
            v["id"] = json!(k);
            v["name"] = json!(name);
            v
        })
        .collect();
    // Por flujo: corridas, cómo terminaron, cuántos tokens gastaron sus agentes, y hasta dónde llegaron.
    let mut flows: BTreeMap<String, (String, Value)> = BTreeMap::new();
    for r in state.store.runs(None, &[], 500).unwrap_or_default() {
        let Ok(run) = serde_json::from_value::<crate::workflows::Run>(r) else { continue };
        if run.created_at < since || !run.snapshot.nodes.iter().any(|n| matches!(n.kind, crate::workflows::NodeKind::Agent | crate::workflows::NodeKind::Ask)) {
            continue;
        }
        let entry = flows.entry(run.workflow_id.clone()).or_insert_with(|| (run.workflow_name.clone(), json!({ "runs": 0, "succeeded": 0, "failed": 0, "cancelled": 0, "tokens": 0, "cost": 0.0, "deployed": 0, "fixes": 0, "fromEvents": 0 })));
        let f = &mut entry.1;
        f["runs"] = json!(f["runs"].as_u64().unwrap_or(0) + 1);
        let st = serde_json::to_value(run.status).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        if matches!(st.as_str(), "succeeded" | "failed" | "cancelled") {
            f[st.as_str()] = json!(f[st.as_str()].as_u64().unwrap_or(0) + 1);
        }
        if run.event.is_some() {
            f["fromEvents"] = json!(f["fromEvents"].as_u64().unwrap_or(0) + 1);
        }
        for nr in &run.nodes {
            if let Some(agent_run) = nr.job.as_deref().and_then(|j| runs.iter().find(|r| r["id"] == j)) {
                f["tokens"] = json!(f["tokens"].as_u64().unwrap_or(0) + agent_run["usage"]["total"].as_u64().unwrap_or(0));
                f["cost"] = json!(f["cost"].as_f64().unwrap_or(0.) + agent_run["usage"]["cost"].as_f64().unwrap_or(0.));
            }
            if nr.outputs.get("changes").map(String::as_str) == Some("true") {
                f["fixes"] = json!(f["fixes"].as_u64().unwrap_or(0) + 1);
            }
            let node = run.snapshot.nodes.iter().find(|n| n.id == nr.id);
            let deploy = node.is_some_and(|n| n.kind == crate::workflows::NodeKind::Recipe && n.params["recipe"].as_str().is_some_and(|r| r.starts_with("deploy") || r.starts_with("promote")));
            if deploy && nr.status == crate::workflows::NodeStatus::Succeeded {
                f["deployed"] = json!(f["deployed"].as_u64().unwrap_or(0) + 1);
            }
        }
    }
    let flows: Vec<Value> = flows
        .into_iter()
        .map(|(id, (name, mut f))| {
            let runs = f["runs"].as_u64().unwrap_or(0).max(1);
            f["okRate"] = json!(((f["succeeded"].as_u64().unwrap_or(0) as f64 / runs as f64) * 100.).round());
            f["cost"] = json!((f["cost"].as_f64().unwrap_or(0.) * 100.).round() / 100.);
            f["id"] = json!(id);
            f["name"] = json!(name);
            f
        })
        .collect();
    json!({ "days": 30, "agents": agents, "flows": flows })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suma_por_agente() {
        let st = crate::state::test_state();
        let now = now_ms();
        let run = |id: &str, role: Option<&str>, mode: &str, status: &str, tokens: u64, fix: Value| json!({ "id": id, "incident": null, "role": role, "mode": mode, "status": status, "startedAt": now - 60_000, "endedAt": now, "usage": { "total": tokens, "cost": 0.01 }, "fix": fix });
        st.store.save_agent_run(&run("a", Some("triage"), "task", "done", 1000, Value::Null)).unwrap();
        st.store.save_agent_run(&run("b", Some("triage"), "task", "failed", 500, Value::Null)).unwrap();
        st.store.save_agent_run(&run("c", Some("fixer"), "fix", "done", 9000, json!({ "changes": true, "discarded": true }))).unwrap();
        st.store.save_agent_run(&run("d", None, "chat", "done", 300, Value::Null)).unwrap();
        let v = json(&st);
        let agent = |id: &str| v["agents"].as_array().unwrap().iter().find(|a| a["id"] == id).cloned().unwrap();
        let t = agent("triage");
        assert_eq!((t["runs"].as_u64(), t["ok"].as_u64(), t["failed"].as_u64(), t["okRate"].as_f64(), t["avgTokens"].as_u64()), (Some(2), Some(1), Some(1), Some(50.), Some(750)));
        assert_eq!(t["name"], "Triage");
        let f = agent("fixer");
        assert_eq!((f["changes"].as_u64(), f["discarded"].as_u64()), (Some(1), Some(1)));
        assert_eq!(agent("mode:chat")["name"], "Chats");
        assert_eq!(t["avgMinutes"].as_f64(), Some(1.0));
    }
}
