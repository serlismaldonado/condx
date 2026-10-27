// Flujos de despliegue configurables (como n8n): cada proyecto tiene uno o más
// flujos, grafos de nodos unidos por aristas. Un nodo pide una receta (un
// trabajo normal, con su aprobación si la receta la pide), espera unos minutos,
// pide una aprobación (un trabajo de la receta workflow-approval: se aprueba
// solo desde la app), verifica algo (un respaldo reciente, el CI en verde, que
// el entorno responda) o deja un aviso. Cada nodo se puede apagar (se salta) y
// dice qué hacer si falla (parar o seguir).
//
// Una corrida recorre los nodos en orden topológico, uno por vez, y se guarda
// en SQLite: se puede cancelar, reintentar desde el nodo que falló y sigue sola
// si el daemon se reinicia. Los flujos se exportan e importan como JSON (para
// tenerlos en el repo del proyecto).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::watch;

use crate::jobs::{self, Status};
use crate::state::Shared;
use crate::store::now_ms;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeKind {
    /// Pide un trabajo de una receta: `{recipe, env, args}`.
    Recipe,
    /// Espera: `{minutes}`.
    Wait,
    /// Aprobación en la app: `{env, level: simple|strict, message}`.
    Approval,
    /// Verificación: `{check: recent-backup|ci-green|env-reachable|schema-safe, env, hours}`.
    Check,
    /// Aviso: `{message}`.
    Notify,
    /// Un agente del catálogo (roles.rs): `{agent, input: [ids de nodos], instructions}`. Su
    /// resultado (validado) son las salidas del nodo.
    Agent,
    /// Pregunta a la persona en el hilo de la corrida y espera su respuesta: `{question}`. La
    /// respuesta queda en `{node.<id>.answer}`.
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum OnFailure {
    #[default]
    Stop,
    Continue,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    pub title: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub on_failure: OnFailure,
    #[serde(default)]
    pub params: Value,
    /// Lugar en el editor.
    #[serde(default)]
    pub x: f32,
    #[serde(default)]
    pub y: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
    pub id: String,
    pub project: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub nodes: Vec<Node>,
    /// Apagado: se conserva (con sus corridas) pero no se puede correr, ni desde la app ni por
    /// el MCP.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Aristas `[desde, hasta]`.
    #[serde(default)]
    pub edges: Vec<(String, String)>,
    /// Condición de una arista (`"desde>hasta"` → `risk.risk >= high`; conditions.rs). Sin
    /// condición, la arista se cumple cuando el nodo de origen terminó.
    #[serde(default)]
    pub conditions: std::collections::BTreeMap<String, String>,
    /// Qué eventos lo empiezan solo (events.rs): un error de Convex, del frontend, un reporte de un
    /// usuario, un agente externo, un incidente. Sin disparadores, se corre a mano.
    #[serde(default)]
    pub triggers: Vec<crate::events::Trigger>,
    #[serde(default)]
    pub updated_at: i64,
}

pub fn edge_key(from: &str, to: &str) -> String {
    format!("{from}>{to}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeStatus {
    Pending,
    Running,
    WaitingApproval,
    WaitingAnswer,
    Succeeded,
    Failed,
    Skipped,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeRun {
    pub id: String,
    pub status: NodeStatus,
    #[serde(default)]
    pub job: Option<String>,
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub ended_at: Option<i64>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub outputs: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus {
    Running,
    WaitingApproval,
    WaitingAnswer,
    Succeeded,
    Failed,
    Cancelled,
}

impl RunStatus {
    fn as_str(self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::WaitingApproval => "waiting-approval",
            RunStatus::WaitingAnswer => "waiting-answer",
            RunStatus::Succeeded => "succeeded",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    pub workflow_id: String,
    pub workflow_name: String,
    pub project: String,
    /// Commit o rama que se lleva (`{run.commit}` en los argumentos).
    pub commit: String,
    pub status: RunStatus,
    /// El flujo tal como estaba al empezar: editarlo después no cambia la corrida.
    pub snapshot: Workflow,
    pub order: Vec<String>,
    pub nodes: Vec<NodeRun>,
    pub requested_by: String,
    pub created_at: i64,
    #[serde(default)]
    pub ended_at: Option<i64>,
    #[serde(default)]
    pub error: Option<String>,
    /// El evento que la empezó (un error, un reporte…); `None` si se corrió a mano.
    #[serde(default)]
    pub event: Option<Value>,
}

fn new_id(prefix: &str) -> String {
    let mut bytes = [0u8; 3];
    use std::io::Read;
    let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes));
    format!("{prefix}{}{:02x}{:02x}{:02x}", now_ms(), bytes[0], bytes[1], bytes[2])
}

// ---------- Validación y orden ----------

/// Orden topológico de los nodos (Kahn, estable por el orden de la lista). Error si hay un ciclo.
pub fn topo_order(w: &Workflow) -> Result<Vec<String>> {
    let ids: Vec<&str> = w.nodes.iter().map(|n| n.id.as_str()).collect();
    let mut indeg: HashMap<&str, usize> = ids.iter().map(|i| (*i, 0)).collect();
    let mut out: HashMap<&str, Vec<&str>> = HashMap::new();
    for (a, b) in &w.edges {
        if !indeg.contains_key(a.as_str()) || !indeg.contains_key(b.as_str()) {
            bail!("connection {a} → {b} uses a node that doesn't exist");
        }
        out.entry(a.as_str()).or_default().push(b.as_str());
        *indeg.get_mut(b.as_str()).unwrap() += 1;
    }
    let mut ready: VecDeque<&str> = ids.iter().copied().filter(|i| indeg[i] == 0).collect();
    let mut order = vec![];
    while let Some(n) = ready.pop_front() {
        order.push(n.to_string());
        for m in out.get(n).cloned().unwrap_or_default() {
            let d = indeg.get_mut(m).unwrap();
            *d -= 1;
            if *d == 0 {
                ready.push_back(m);
            }
        }
    }
    if order.len() != ids.len() {
        bail!("the workflow has a cycle: a node can't depend on itself");
    }
    Ok(order)
}

/// Problemas del flujo (vacío = se puede correr). `envs` y `recipe_exists` vienen del daemon.
pub fn problems(w: &Workflow, envs: &HashSet<String>, recipe_exists: impl Fn(&str) -> bool) -> Vec<String> {
    let mut out = vec![];
    if w.name.trim().is_empty() {
        out.push("the workflow has no name".into());
    }
    if w.nodes.is_empty() {
        out.push("the workflow has no nodes".into());
    }
    let mut seen = HashSet::new();
    for n in &w.nodes {
        if !seen.insert(n.id.as_str()) {
            out.push(format!("id {} is duplicated", n.id));
        }
    }
    if let Err(e) = topo_order(w) {
        out.push(e.to_string());
    }
    for t in &w.triggers {
        if let Err(e) = crate::events::check_trigger(t) {
            out.push(e.to_string());
        }
    }
    for (key, cond) in &w.conditions {
        if cond.trim().is_empty() {
            continue;
        }
        let Some((a, b)) = key.split_once('>') else { continue };
        if !w.edges.iter().any(|(x, y)| x == a && y == b) {
            out.push(format!("the condition on {a} → {b} has no connection"));
        } else if let Err(e) = crate::conditions::check(cond) {
            out.push(format!("condition on {a} → {b}: {e}"));
        }
    }
    let s = |n: &Node, k: &str| n.params[k].as_str().unwrap_or_default().trim().to_string();
    for n in w.nodes.iter().filter(|n| n.enabled) {
        let t = &n.title;
        let env_ok = |out: &mut Vec<String>| {
            let e = s(n, "env");
            if e.is_empty() {
                out.push(format!("“{t}”: missing environment"));
            } else if !envs.contains(&e) {
                out.push(format!("“{t}”: environment {e} doesn't exist"));
            }
        };
        match n.kind {
            NodeKind::Recipe => {
                let r = s(n, "recipe");
                if r.is_empty() {
                    out.push(format!("“{t}”: missing recipe"));
                } else if !recipe_exists(&r) {
                    out.push(format!("“{t}”: recipe {r} doesn't exist"));
                }
                env_ok(&mut out);
            }
            NodeKind::Wait => {
                if n.params["minutes"].as_f64().is_none_or(|m| m <= 0.) {
                    out.push(format!("“{t}”: minutes must be more than 0"));
                }
            }
            NodeKind::Approval => env_ok(&mut out),
            NodeKind::Check => {
                match s(n, "check").as_str() {
                    "recent-backup" | "env-reachable" | "schema-safe" => env_ok(&mut out),
                    "ci-green" => {}
                    "fix-checks" => {
                        if s(n, "node").is_empty() {
                            out.push(format!("“{t}”: which agent's change to check"));
                        }
                    }
                    other => out.push(format!("“{t}”: unknown check “{other}”")),
                }
            }
            NodeKind::Notify => {}
            NodeKind::Agent => {
                if s(n, "agent").is_empty() {
                    out.push(format!("“{t}”: choose an agent"));
                }
            }
            NodeKind::Ask => {
                if s(n, "question").is_empty() {
                    out.push(format!("“{t}”: write the question"));
                }
            }
        }
    }
    out
}

/// Los problemas del flujo, también los que necesitan el estado (que cada agente exista y esté encendido).
pub fn problems_for(state: &Shared, w: &Workflow, envs: &HashSet<String>) -> Vec<String> {
    let mut out = problems(w, envs, |r| crate::recipes::get(r).is_some());
    for n in w.nodes.iter().filter(|n| n.enabled && n.kind == NodeKind::Agent) {
        let id = n.params["agent"].as_str().unwrap_or_default();
        match crate::roles::get(state, id) {
            None if !id.is_empty() => out.push(format!("“{}”: there's no agent “{id}”", n.title)),
            Some(r) if !r.enabled => out.push(format!("“{}”: the agent “{}” is turned off", n.title, r.name)),
            _ => {}
        }
    }
    out
}

fn check_daemon(state: &Shared, w: &Workflow) -> Result<()> {
    let envs: HashSet<String> = state.store.environments()?.into_iter().map(|e| e.name).collect();
    let p = problems_for(state, w, &envs);
    if p.is_empty() {
        Ok(())
    } else {
        bail!("{}", p.join("; "))
    }
}

/// `{run.commit}` y `{node.<id>.<salida>}` en los argumentos de un nodo.
pub fn substitute(text: &str, run: &Run) -> String {
    let mut out = text.replace("{run.commit}", &run.commit);
    for nr in &run.nodes {
        for (k, v) in &nr.outputs {
            out = out.replace(&format!("{{node.{}.{k}}}", nr.id), v);
        }
    }
    out
}

// ---------- Plantillas ----------

fn node(id: &str, kind: NodeKind, title: &str, x: f32, y: f32, params: Value) -> Node {
    Node { id: id.into(), kind, title: title.into(), enabled: true, on_failure: OnFailure::Stop, params, x, y }
}

fn chain(nodes: &[Node]) -> Vec<(String, String)> {
    nodes.windows(2).map(|w| (w[0].id.clone(), w[1].id.clone())).collect()
}

/// Plantillas para un proyecto: la promoción de heskala-erp (la cadena de siempre) solo en su
/// proyecto; las demás, genéricas. `envs` = entornos del proyecto por tipo.
pub fn templates(project: &str, is_default: bool, envs: &[(String, String)]) -> Vec<Workflow> {
    let of = |kind: &str| envs.iter().find(|(_, k)| k == kind).map(|(n, _)| n.clone()).unwrap_or_default();
    let (local, dev, prod) = (of("devbox"), of("cloud-dev"), of("cloud-prod"));
    let mut out = vec![];
    if is_default {
        let nodes = vec![
            node("backup", NodeKind::Recipe, "Production backup", 0., 0., json!({ "recipe": "export", "env": prod, "args": {} })),
            node("rehearsal", NodeKind::Recipe, "Rehearsal with real data", 300., 0., json!({ "recipe": "rehearsal", "env": prod, "args": { "commit": "{run.commit}", "backup": "{node.backup.backup_id}" } })),
            node("deploy-dev", NodeKind::Recipe, "Deploy to cloud dev", 600., 0., json!({ "recipe": "deploy-cloud", "env": dev, "args": { "commit": "{run.commit}" } })),
            node("quiet", NodeKind::Wait, "Wait 30 minutes", 900., 0., json!({ "minutes": 30 })),
            node("promote", NodeKind::Recipe, "Promote to production", 1200., 0., json!({ "recipe": "promote-prod", "env": prod, "args": { "commit": "{run.commit}" } })),
        ];
        let edges = chain(&nodes);
        out.push(Workflow { id: String::new(), project: project.into(), name: "Promotion to production".into(), description: "The usual chain: backup, rehearsal, cloud dev, a 30-minute wait and production.".into(), nodes, edges, enabled: true, conditions: Default::default(), triggers: vec![], updated_at: 0 });
    }
    // Los sugeridos (`suggested`): se crean apagados con «Crear los sugeridos».
    let flow = |name: &str, description: &str, nodes: Vec<Node>| {
        let edges = chain(&nodes);
        Workflow { id: String::new(), project: project.into(), name: name.into(), description: description.into(), nodes, edges, enabled: true, conditions: Default::default(), triggers: vec![], updated_at: 0 }
    };
    if !local.is_empty() {
        out.push(flow(
            "Deploy to local",
            "The local backend responds, the commit is deployed to it and you get a notice.",
            vec![
                node("check", NodeKind::Check, "Local responds", 0., 0., json!({ "check": "env-reachable", "env": local })),
                node("deploy", NodeKind::Recipe, "Deploy to local", 300., 0., json!({ "recipe": "deploy", "env": local, "args": { "commit": "{run.commit}" } })),
                node("notify", NodeKind::Notify, "Notice", 600., 0., json!({ "message": "{run.commit} deployed to local" })),
            ],
        ));
    }
    if !dev.is_empty() {
        out.push(flow(
            "Deploy to cloud dev",
            "CI green, cloud dev responds, your approval in the app, deploy the commit and a notice. Safe for agents: they can start it but only you approve.",
            vec![
                node("ci", NodeKind::Check, "CI is green", 0., 0., json!({ "check": "ci-green" })),
                node("check", NodeKind::Check, "Cloud dev responds", 300., 0., json!({ "check": "env-reachable", "env": dev })),
                node("approve", NodeKind::Approval, "Approve the deploy", 600., 0., json!({ "env": dev, "level": "simple", "message": "Deploy to cloud dev?" })),
                node("deploy", NodeKind::Recipe, "Deploy to cloud dev", 900., 0., json!({ "recipe": "deploy-cloud", "env": dev, "args": { "commit": "{run.commit}" } })),
                node("notify", NodeKind::Notify, "Notice", 1200., 0., json!({ "message": "{run.commit} deployed to cloud dev" })),
            ],
        ));
    }
    if is_default && !dev.is_empty() {
        out.push(flow(
            "Rehearsal with cloud dev data",
            "Backs up cloud dev and tries the commit on a copy of that data in a throwaway backend; cloud dev is only read.",
            vec![
                node("backup", NodeKind::Recipe, "Cloud dev backup", 0., 0., json!({ "recipe": "export", "env": dev, "args": {} })),
                node("rehearsal", NodeKind::Recipe, "Rehearsal", 300., 0., json!({ "recipe": "rehearsal", "env": dev, "args": { "commit": "{run.commit}", "backup": "{node.backup.backup_id}" } })),
                node("notify", NodeKind::Notify, "Notice", 600., 0., json!({ "message": "Rehearsal of {run.commit} passed" })),
            ],
        ));
    }
    if is_default && !prod.is_empty() {
        out.push(flow(
            "Roll back production",
            "Reinforced approval, a production backup and back to the previous deploy.",
            vec![
                node("approve", NodeKind::Approval, "Approve the rollback", 0., 0., json!({ "env": prod, "level": "strict", "message": "Roll production back to the previous deploy?" })),
                node("backup", NodeKind::Recipe, "Production backup", 300., 0., json!({ "recipe": "export", "env": prod, "args": {} })),
                node("rollback", NodeKind::Recipe, "Roll back", 600., 0., json!({ "recipe": "rollback-prod", "env": prod, "args": { "commit": "previous" } })),
                node("notify", NodeKind::Notify, "Notice", 900., 0., json!({ "message": "Production rolled back" })),
            ],
        ));
    }
    let clouds: Vec<&String> = [&dev, &prod].into_iter().filter(|e| !e.is_empty()).collect();
    if !clouds.is_empty() {
        let mut nodes: Vec<Node> = clouds
            .iter()
            .enumerate()
            .map(|(i, e)| node(&format!("backup-{i}"), NodeKind::Recipe, &format!("Backup of {e}"), 300. * i as f32, 0., json!({ "recipe": "export", "env": e, "args": {} })))
            .collect();
        let x = 300. * nodes.len() as f32;
        nodes.push(node("notify", NodeKind::Notify, "Notice", x, 0., json!({ "message": "Cloud backups done" })));
        out.push(flow("Cloud backups", "An encrypted backup of each cloud environment (kept 14 days).", nodes));
    }

    let target = if !dev.is_empty() { dev.clone() } else { local.clone() };
    let nodes = vec![
        node("check", NodeKind::Check, "The environment responds", 0., 0., json!({ "check": "env-reachable", "env": target })),
        node("approve", NodeKind::Approval, "Approve", 300., 0., json!({ "env": target, "level": "simple", "message": "Continue?" })),
        node("run", NodeKind::Recipe, "Run a function", 600., 0., json!({ "recipe": "run-function", "env": target, "args": { "function": "", "payload": "{}" } })),
        node("notify", NodeKind::Notify, "Notify", 900., 0., json!({ "message": "Done" })),
    ];
    let edges = chain(&nodes);
    // Con agentes: uno revisa el error, otro mide el riesgo, y un aviso con el resumen.
    out.push(flow(
        "Triage an error",
        "An agent looks at the error and writes a quick report; another rates the risk (low to critical), says what to check and whether it needs a fix now; then a notice with the summary.",
        vec![
            node("triage", NodeKind::Agent, "Triage", 0., 0., json!({ "agent": "triage" })),
            node("risk", NodeKind::Agent, "Risk", 300., 0., json!({ "agent": "risk" })),
            node("notify", NodeKind::Notify, "Notice", 600., 0., json!({ "message": "Reviewed: {node.triage.summary} · risk {node.risk.risk} · fix now: {node.risk.immediate_fix}" })),
        ],
    ));
    // Lo mismo, pero solo avisa (con prioridad alta) si el riesgo es alto o crítico.
    let mut urgent = flow(
        "Triage an error, notify if risky",
        "Like “Triage an error”, but the notice only goes out when the risk is high or critical, and with high priority.",
        vec![
            node("triage", NodeKind::Agent, "Triage", 0., 0., json!({ "agent": "triage" })),
            node("risk", NodeKind::Agent, "Risk", 380., 0., json!({ "agent": "risk" })),
            node("notify", NodeKind::Notify, "Urgent notice", 760., 0., json!({ "message": "Risk {node.risk.risk}: {node.triage.summary}. Check: {node.risk.check}", "priority": "high" })),
        ],
    );
    urgent.conditions.insert(edge_key("risk", "notify"), "risk.risk >= high".into());
    // Empieza solo con los errores que manda el SDK de la app (se crea apagado, como los sugeridos).
    urgent.triggers = vec![
        crate::events::Trigger { source: "convex-error".into(), ..Default::default() },
        crate::events::Trigger { source: "frontend-error".into(), ..Default::default() },
    ];
    out.push(urgent);
    // El ciclo completo: revisa, mide el riesgo y avisa; si hace falta arreglarlo ya, un agente lo
    // arregla en su rama, otro lo revisa, corren los chequeos y, con tu aprobación, va a cloud dev.
    // (Para dejarlo en automático, quita el nodo de aprobación.)
    let mut fix = flow(
        "Fix an error",
        "Triage, risk and a notice; if it needs a fix now, an agent fixes it on its own branch, another reviews it, the project's checks run, and after your approval it's deployed to cloud dev.",
        {
            let mut n = vec![
                node("triage", NodeKind::Agent, "Triage", 0., 0., json!({ "agent": "triage" })),
                node("risk", NodeKind::Agent, "Risk", 380., 0., json!({ "agent": "risk" })),
                node("notify", NodeKind::Notify, "Reviewed", 760., -140., json!({ "message": "Reviewed: {node.triage.summary} · risk {node.risk.risk} · fix now: {node.risk.immediate_fix}" })),
                node("fixer", NodeKind::Agent, "Fixer", 760., 140., json!({ "agent": "fixer" })),
                node("review", NodeKind::Agent, "Reviewer", 1140., 140., json!({ "agent": "reviewer" })),
                node("checks", NodeKind::Check, "Project checks", 1520., 140., json!({ "check": "fix-checks", "node": "fixer" })),
            ];
            if dev.is_empty() {
                n.push(node("ready", NodeKind::Notify, "Fix ready", 1900., 140., json!({ "message": "Fix ready on {node.fixer.branch} ({node.checks.summary}). Review it and deploy it from Condx.", "priority": "high" })));
            } else {
                n.push(node("approve", NodeKind::Approval, "Approve the deploy", 1900., 140., json!({ "env": dev, "level": "simple", "message": "Deploy the fix {node.fixer.commit} to cloud dev?" })));
                n.push(node("deploy", NodeKind::Recipe, "Deploy to cloud dev", 2280., 140., json!({ "recipe": "deploy-cloud", "env": dev, "args": { "commit": "{node.fixer.commit}" } })));
                n.push(node("done", NodeKind::Notify, "Deployed", 2660., 140., json!({ "message": "Fix {node.fixer.branch} deployed to cloud dev" })));
            }
            n
        },
    );
    fix.edges = vec![("triage".into(), "risk".into()), ("risk".into(), "notify".into()), ("risk".into(), "fixer".into()), ("fixer".into(), "review".into()), ("review".into(), "checks".into())];
    if dev.is_empty() {
        fix.edges.push(("checks".into(), "ready".into()));
    } else {
        fix.edges.extend([("checks".into(), "approve".into()), ("approve".into(), "deploy".into()), ("deploy".into(), "done".into())]);
    }
    fix.conditions.insert(edge_key("risk", "fixer"), "risk.immediate_fix == true".into());
    fix.conditions.insert(edge_key("fixer", "review"), "fixer.changes == true".into());
    fix.conditions.insert(edge_key("review", "checks"), "review.verdict == approve".into());
    fix.triggers = vec![
        crate::events::Trigger { source: "convex-error".into(), ..Default::default() },
        crate::events::Trigger { source: "frontend-error".into(), ..Default::default() },
    ];
    out.push(fix);
    // Una tarea pedida desde el chat (o desde Claude por el MCP): un agente la hace en su rama, otro la
    // revisa, los chequeos; con cloud dev, además, la vista previa de esa rama (en el sitio en la nube:
    // convex dev y el frontend con su URL) y un aviso con dónde probarla.
    let mut task = flow(
        "Do a task",
        if dev.is_empty() {
            "A task you ask for in Agents → New task: an agent does it on its own branch, another reviews it, the project's checks run, and you get the branch to merge."
        } else {
            "A task you ask for (in Agents, from the phone or from Claude): an agent does it on its own branch, another reviews it, the project's checks run and, on the cloud site, the branch runs on cloud dev with a URL to try it. Reply in the thread to keep iterating."
        },
        {
            let mut n = vec![
                node("fixer", NodeKind::Agent, "Do it", 0., 0., json!({ "agent": "fixer", "instructions": "The event is a task the developer asked for: do it." })),
                node("review", NodeKind::Agent, "Reviewer", 380., 0., json!({ "agent": "reviewer" })),
                node("checks", NodeKind::Check, "Project checks", 760., 0., json!({ "check": "fix-checks", "node": "fixer" })),
            ];
            if dev.is_empty() {
                n.push(node("ready", NodeKind::Notify, "Ready", 1140., 0., json!({ "message": "Task ready on {node.fixer.branch}: {node.fixer.change} ({node.checks.summary})" })));
            } else {
                n.push(node("preview", NodeKind::Recipe, "Try it on cloud dev", 1140., 0., json!({ "recipe": "preview", "env": dev, "args": { "commit": "{node.fixer.commit}" } })));
                n.push(node("ready", NodeKind::Notify, "Ready to try", 1520., 0., json!({ "message": "Ready to try on cloud dev (Condx → Environments → Open, or ask Claude for the URL): {node.fixer.change} ({node.checks.summary}, branch {node.fixer.branch})", "priority": "high" })));
            }
            n
        },
    );
    task.conditions.insert(edge_key("fixer", "review"), "fixer.changes == true".into());
    task.triggers = vec![crate::events::Trigger { source: "task".into(), ..Default::default() }];
    out.push(task);
    out.push(Workflow { id: String::new(), project: project.into(), name: "Example: check, approve and run".into(), description: "A starting point for building your own.".into(), nodes, edges, enabled: true, conditions: Default::default(), triggers: vec![], updated_at: 0 });
    out.push(Workflow { id: String::new(), project: project.into(), name: "Blank".into(), description: String::new(), nodes: vec![], edges: vec![], enabled: true, conditions: Default::default(), triggers: vec![], updated_at: 0 });
    out
}

/// Los sugeridos: las plantillas sin el ejemplo ni el flujo en blanco.
fn is_suggested(w: &Workflow) -> bool {
    !w.nodes.is_empty() && !w.name.starts_with("Example")
}

/// Crea, apagados, los flujos sugeridos que el proyecto todavía no tiene (por nombre); devuelve
/// los que creó. Se encienden desde la app cuando se quieran usar.
pub fn seed_suggested(state: &Shared, project: &str) -> Result<Vec<String>> {
    let is_default = crate::projects::get(state, Some(project)).map(|p| p.is_default()).unwrap_or(false);
    let existing: HashSet<String> = list(state, project)?.into_iter().map(|w| w.name).collect();
    let envs = project_envs(state, project);
    let mut created = vec![];
    for mut w in templates(project, is_default, &envs).into_iter().filter(is_suggested) {
        if existing.contains(&w.name) {
            continue;
        }
        w.enabled = false;
        let w = save(state, w)?;
        created.push(w.name);
    }
    Ok(created)
}

// ---------- Guardar, exportar, importar ----------

pub fn list(state: &Shared, project: &str) -> Result<Vec<Workflow>> {
    let mut list: Vec<Workflow> = state.store.workflows(project)?.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect();
    // heskala-erp empieza con su promoción ya armada (lo de siempre no se pierde).
    if list.is_empty() {
        if let Ok(p) = crate::projects::get(state, Some(project)) {
            if p.is_default() {
                let envs = project_envs(state, project);
                if let Some(mut w) = templates(project, true, &envs).into_iter().next() {
                    w.id = new_id("w");
                    w.updated_at = now_ms();
                    state.store.save_workflow(&w.id, project, &serde_json::to_value(&w)?)?;
                    list.push(w);
                }
            }
        }
    }
    Ok(list)
}

pub fn project_envs(state: &Shared, project: &str) -> Vec<(String, String)> {
    let default = crate::projects::get(state, Some(project)).map(|p| p.is_default()).unwrap_or(false);
    state
        .store
        .environments()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| match &e.project {
            Some(p) => p == project,
            None => default,
        })
        .map(|e| (e.name.clone(), e.kind.as_str().to_string()))
        .collect()
}

/// Guarda (crea si no tiene id). Se puede guardar con problemas; correrlo, no.
pub fn save(state: &Shared, mut w: Workflow) -> Result<Workflow> {
    if w.id.is_empty() {
        w.id = new_id("w");
    }
    w.updated_at = now_ms();
    state.store.save_workflow(&w.id, &w.project, &serde_json::to_value(&w)?)?;
    Ok(w)
}

pub fn get(state: &Shared, id: &str) -> Result<Workflow> {
    let v = state.store.workflow(id)?.ok_or_else(|| anyhow!("workflow {id} doesn't exist"))?;
    Ok(serde_json::from_value(v)?)
}

/// JSON para el repo: sin id ni proyecto (se asignan al importar).
pub fn export(w: &Workflow) -> Value {
    json!({ "condxWorkflow": 1, "name": w.name, "description": w.description, "enabled": w.enabled, "nodes": w.nodes, "edges": w.edges, "conditions": w.conditions, "triggers": w.triggers })
}

pub fn import(project: &str, raw: &Value) -> Result<Workflow> {
    if raw["condxWorkflow"].as_i64() != Some(1) {
        bail!("not an exported workflow (missing \"condxWorkflow\": 1)");
    }
    Ok(Workflow {
        id: String::new(),
        project: project.into(),
        name: raw["name"].as_str().unwrap_or("Imported workflow").to_string(),
        description: raw["description"].as_str().unwrap_or_default().to_string(),
        nodes: serde_json::from_value(raw["nodes"].clone())?,
        edges: serde_json::from_value(raw["edges"].clone()).unwrap_or_default(),
        enabled: raw["enabled"].as_bool().unwrap_or(true),
        conditions: serde_json::from_value(raw["conditions"].clone()).unwrap_or_default(),
        triggers: serde_json::from_value(raw["triggers"].clone()).unwrap_or_default(),
        updated_at: 0,
    })
}

// ---------- Corridas ----------

fn cancels() -> &'static Mutex<HashMap<String, watch::Sender<bool>>> {
    static C: OnceLock<Mutex<HashMap<String, watch::Sender<bool>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn persist(state: &Shared, run: &Run) {
    if let Ok(v) = serde_json::to_value(run) {
        let _ = state.store.save_run(&run.id, &run.workflow_id, run.created_at, run.status.as_str(), &v);
    }
}

pub fn load_run(state: &Shared, id: &str) -> Result<Run> {
    let v = state.store.run(id)?.ok_or_else(|| anyhow!("run {id} doesn't exist"))?;
    Ok(serde_json::from_value(v)?)
}

pub fn start(state: &Shared, workflow_id: &str, commit: &str, by: &str) -> Result<Run> {
    start_with(state, workflow_id, commit, by, None)
}

/// Como `start`, con el evento que la dispara (lo leen los agentes del flujo).
pub fn start_with(state: &Shared, workflow_id: &str, commit: &str, by: &str, event: Option<Value>) -> Result<Run> {
    let w = get(state, workflow_id)?;
    if !w.enabled {
        bail!("“{}” is turned off: turn it on in Condx (Workflows) to run it", w.name);
    }
    check_daemon(state, &w)?;
    if state.store.runs(Some(workflow_id), &["running", "waiting-approval", "waiting-answer"], 1)?.into_iter().next().is_some() {
        bail!("“{}” already has a run in progress", w.name);
    }
    let order = topo_order(&w)?;
    let nodes = order.iter().map(|id| NodeRun { id: id.clone(), status: NodeStatus::Pending, job: None, started_at: None, ended_at: None, message: String::new(), outputs: HashMap::new() }).collect();
    let run = Run {
        id: new_id("r"),
        workflow_id: w.id.clone(),
        workflow_name: w.name.clone(),
        project: w.project.clone(),
        commit: if commit.trim().is_empty() { "dev".into() } else { commit.trim().into() },
        status: RunStatus::Running,
        snapshot: w,
        order,
        nodes,
        requested_by: by.into(),
        created_at: now_ms(),
        ended_at: None,
        error: None,
        event,
    };
    persist(state, &run);
    spawn(state, run.id.clone());
    Ok(run)
}

pub fn cancel(state: &Shared, run_id: &str, by: &str) -> Result<Run> {
    let mut run = load_run(state, run_id)?;
    if !matches!(run.status, RunStatus::Running | RunStatus::WaitingApproval | RunStatus::WaitingAnswer) {
        bail!("the run already finished");
    }
    if let Some(tx) = cancels().lock().unwrap().get(run_id) {
        let _ = tx.send(true);
    }
    // El trabajo en curso también se cancela (o se rechaza si esperaba aprobación).
    for nr in run.nodes.iter_mut().filter(|n| matches!(n.status, NodeStatus::Running | NodeStatus::WaitingApproval | NodeStatus::WaitingAnswer)) {
        if let Some(job) = &nr.job {
            let _ = jobs::cancel(state, job, by);
        }
        nr.status = NodeStatus::Cancelled;
        nr.ended_at = Some(now_ms());
    }
    run.status = RunStatus::Cancelled;
    run.ended_at = Some(now_ms());
    run.error = Some(format!("cancelled by {by}"));
    persist(state, &run);
    Ok(run)
}

/// Reintenta desde el primer nodo que falló o se canceló: ese y los siguientes vuelven a pendiente.
pub fn retry(state: &Shared, run_id: &str) -> Result<Run> {
    let mut run = load_run(state, run_id)?;
    if !matches!(run.status, RunStatus::Failed | RunStatus::Cancelled) {
        bail!("only a failed or cancelled run can be retried");
    }
    let from = run.nodes.iter().position(|n| matches!(n.status, NodeStatus::Failed | NodeStatus::Cancelled)).ok_or_else(|| anyhow!("there's no node to retry"))?;
    for nr in run.nodes.iter_mut().skip(from) {
        if nr.status != NodeStatus::Skipped {
            *nr = NodeRun { id: nr.id.clone(), status: NodeStatus::Pending, job: None, started_at: None, ended_at: None, message: String::new(), outputs: HashMap::new() };
        }
    }
    run.status = RunStatus::Running;
    run.ended_at = None;
    run.error = None;
    persist(state, &run);
    spawn(state, run.id.clone());
    Ok(run)
}

/// Al arrancar el daemon: las corridas que quedaron a medias siguen solas.
pub fn recover(state: &Shared) {
    for v in state.store.runs(None, &["running", "waiting-approval", "waiting-answer"], 50).unwrap_or_default() {
        if let Some(id) = v["id"].as_str() {
            spawn(state, id.to_string());
        }
    }
}

fn spawn(state: &Shared, run_id: String) {
    let (tx, rx) = watch::channel(false);
    cancels().lock().unwrap().insert(run_id.clone(), tx);
    let state = state.clone();
    tokio::spawn(async move {
        execute(&state, &run_id, rx).await;
        cancels().lock().unwrap().remove(&run_id);
    });
}

async fn execute(state: &Shared, run_id: &str, mut cancel: watch::Receiver<bool>) {
    loop {
        let Ok(mut run) = load_run(state, run_id) else { return };
        if !matches!(run.status, RunStatus::Running | RunStatus::WaitingApproval | RunStatus::WaitingAnswer) || *cancel.borrow() {
            return;
        }
        let Some(i) = run.nodes.iter().position(|n| !matches!(n.status, NodeStatus::Succeeded | NodeStatus::Skipped | NodeStatus::Failed)) else {
            // Todo terminó: ok, salvo que un nodo «seguir» haya fallado (queda anotado).
            run.status = RunStatus::Succeeded;
            run.ended_at = Some(now_ms());
            persist(state, &run);
            return;
        };
        let node = run.snapshot.nodes.iter().find(|n| n.id == run.nodes[i].id).cloned();
        let Some(node) = node else {
            run.nodes[i].status = NodeStatus::Skipped;
            persist(state, &run);
            continue;
        };
        if !node.enabled {
            run.nodes[i].status = NodeStatus::Skipped;
            run.nodes[i].message = "disabled in the workflow".into();
            persist(state, &run);
            continue;
        }
        // Por qué camino se llega: si ninguna arista de entrada se cumple, se salta.
        if let Some(why) = blocked(state, &run, &node.id) {
            run.nodes[i].status = NodeStatus::Skipped;
            run.nodes[i].message = why;
            persist(state, &run);
            continue;
        }
        if run.nodes[i].started_at.is_none() {
            run.nodes[i].started_at = Some(now_ms());
        }
        run.nodes[i].status = NodeStatus::Running;
        persist(state, &run);
        let result = step(state, &mut run, i, &node, &mut cancel).await;
        if *cancel.borrow() {
            return;
        }
        // La corrida pudo cambiar afuera (cancelada desde la app): no pisarla.
        if load_run(state, run_id).map(|r| r.status == RunStatus::Cancelled).unwrap_or(true) {
            return;
        }
        run.nodes[i].ended_at = Some(now_ms());
        match result {
            Ok(msg) => {
                run.nodes[i].status = NodeStatus::Succeeded;
                run.nodes[i].message = msg;
                run.status = RunStatus::Running;
            }
            Err(e) => {
                run.nodes[i].status = NodeStatus::Failed;
                run.nodes[i].message = format!("{e:#}");
                if node.on_failure == OnFailure::Stop {
                    run.status = RunStatus::Failed;
                    run.error = Some(format!("“{}”: {e:#}", node.title));
                    run.ended_at = Some(now_ms());
                    persist(state, &run);
                    return;
                }
            }
        }
        persist(state, &run);
    }
}

/// Espera a que el trabajo termine, reflejando su estado en el nodo (y en la corrida si espera
/// aprobación). Devuelve el resumen o el error.
async fn follow_job(state: &Shared, run: &mut Run, i: usize, job_id: &str, cancel: &mut watch::Receiver<bool>) -> Result<String> {
    loop {
        let job = jobs::load(state, job_id)?;
        let (ns, rs) = match job.status {
            Status::WaitingApproval => (NodeStatus::WaitingApproval, RunStatus::WaitingApproval),
            Status::Queued | Status::Running => (NodeStatus::Running, RunStatus::Running),
            Status::Succeeded => {
                run.nodes[i].outputs = job.outputs.clone();
                return Ok(format!("{} finished", job.title));
            }
            Status::Failed => bail!("{} failed: {}", job.title, job.error.unwrap_or_default()),
            Status::Rejected => bail!("{}: approval rejected", job.title),
            Status::Expired => bail!("{}: the approval expired", job.title),
            Status::Cancelled => bail!("{}: cancelled", job.title),
        };
        if run.nodes[i].status != ns || run.status != rs {
            run.nodes[i].status = ns;
            run.status = rs;
            persist(state, run);
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            _ = cancel.changed() => return Err(anyhow!("cancelada")),
        }
    }
}

/// Lo que las condiciones leen de una corrida: las salidas de sus nodos, su estado y el orden de
/// las opciones de cada agente (low < medium < high < critical).
struct RunFacts<'a> {
    state: &'a Shared,
    run: &'a Run,
}

impl crate::conditions::Facts for RunFacts<'_> {
    fn value(&self, node: &str, field: &str) -> Option<String> {
        let nr = self.run.nodes.iter().find(|n| n.id == node)?;
        if field == "status" {
            return serde_json::to_value(nr.status).ok().and_then(|v| v.as_str().map(str::to_string));
        }
        nr.outputs.get(field).cloned()
    }
    fn order(&self, node: &str, field: &str) -> Option<Vec<String>> {
        let n = self.run.snapshot.nodes.iter().find(|n| n.id == node)?;
        let role = crate::roles::get(self.state, n.params["agent"].as_str()?)?;
        let t = role.output.get(field)?;
        t.contains('|').then(|| t.split('|').map(|v| v.trim().to_string()).collect())
    }
}

/// `None` si el nodo puede correr: no tiene aristas de entrada, o alguna se cumple (su origen
/// terminó —o está apagado— y su condición, si la tiene, es cierta). Si no, por qué se salta.
fn blocked(state: &Shared, run: &Run, node: &str) -> Option<String> {
    let incoming: Vec<&(String, String)> = run.snapshot.edges.iter().filter(|(_, b)| b == node).collect();
    if incoming.is_empty() {
        return None;
    }
    let facts = RunFacts { state, run };
    let mut reasons = vec![];
    for (from, _) in incoming {
        let Some(src) = run.nodes.iter().find(|n| &n.id == from) else { continue };
        let passed = match src.status {
            NodeStatus::Succeeded | NodeStatus::Failed => true,
            NodeStatus::Skipped => src.message == "disabled in the workflow",
            _ => false,
        };
        if !passed {
            reasons.push(format!("{from} didn't run"));
            continue;
        }
        match run.snapshot.conditions.get(&edge_key(from, node)).filter(|c| !c.trim().is_empty()) {
            None => return None,
            Some(cond) if crate::conditions::eval(cond, &facts) => return None,
            Some(cond) => reasons.push(format!("“{cond}” isn't true")),
        }
    }
    Some(format!("skipped: {}", reasons.join("; ")))
}

async fn step(state: &Shared, run: &mut Run, i: usize, node: &Node, cancel: &mut watch::Receiver<bool>) -> Result<String> {
    let p = &node.params;
    let s = |k: &str| p[k].as_str().unwrap_or_default().to_string();
    let by = format!("workflow “{}”", run.workflow_name);
    match node.kind {
        NodeKind::Recipe | NodeKind::Approval => {
            // Un nodo que ya pidió su trabajo (el daemon se reinició) lo sigue en vez de pedir otro.
            let job_id = match run.nodes[i].job.clone() {
                Some(j) => j,
                None => {
                    let (recipe, args) = if node.kind == NodeKind::Recipe {
                        let mut args = serde_json::Map::new();
                        for (k, v) in p["args"].as_object().cloned().unwrap_or_default() {
                            let text = v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
                            args.insert(k, json!(substitute(&text, run)));
                        }
                        (s("recipe"), Value::Object(args))
                    } else {
                        let recipe = if s("level") == "strict" { "workflow-approval-strict" } else { "workflow-approval" };
                        (recipe.to_string(), json!({ "message": if s("message").is_empty() { node.title.clone() } else { s("message") } }))
                    };
                    let job = jobs::request(state, &recipe, &s("env"), &args, &by).await?;
                    run.nodes[i].job = Some(job.id.clone());
                    persist(state, run);
                    job.id
                }
            };
            follow_job(state, run, i, &job_id, cancel).await
        }
        NodeKind::Wait => {
            let minutes = p["minutes"].as_f64().unwrap_or(1.);
            let started = run.nodes[i].started_at.unwrap_or_else(now_ms);
            let until = started + (minutes * 60_000.) as i64;
            run.nodes[i].outputs.insert("until".into(), until.to_string());
            run.nodes[i].message = format!("waiting {minutes} min");
            persist(state, run);
            while now_ms() < until {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(((until - now_ms()).max(1) as u64).min(2000))) => {}
                    _ = cancel.changed() => return Err(anyhow!("cancelada")),
                }
            }
            Ok(format!("waited {minutes} min"))
        }
        NodeKind::Check if s("check") == "fix-checks" => {
            // Los chequeos del proyecto en el worktree del agente que escribió (sin red, en el sandbox).
            let fixer = s("node");
            let worktree = run.nodes.iter().find(|n| n.id == fixer).and_then(|n| n.outputs.get("worktree").cloned()).filter(|w| !w.is_empty()).ok_or_else(|| anyhow!("“{fixer}” left no worktree to check"))?;
            let st = state.clone();
            let cwd = worktree.clone();
            let res = tokio::task::spawn_blocking(move || st.agent_host.call(&st, "POST", "/checks", Some(json!({ "cwd": cwd })))).await.map_err(|e| anyhow!(e.to_string()))??;
            let mut results = res["results"].as_array().cloned().unwrap_or_default();
            // Lo que falla, ¿ya fallaba antes del cambio? Se corre otra vez en el commit de partida: si
            // también falla ahí, no lo rompió el agente (un repo con errores de lint viejos no bloquea
            // ningún cambio); solo cuenta lo que el cambio rompe.
            let failed: Vec<String> = results.iter().filter(|r| r["ok"] != true).filter_map(|r| r["script"].as_str().map(str::to_string)).collect();
            let base = run.nodes.iter().find(|n| n.id == fixer).and_then(|n| n.outputs.get("base").cloned()).filter(|b| !b.is_empty());
            if let (false, Some(base)) = (failed.is_empty(), base) {
                let st = state.clone();
                let wt = worktree.clone();
                let before = tokio::task::spawn_blocking(move || checks_at_base(&st, &wt, &base, &failed)).await.map_err(|e| anyhow!(e.to_string()))?;
                for r in results.iter_mut().filter(|r| r["ok"] != true) {
                    if before.iter().any(|b| b["script"] == r["script"] && b["ok"] != true) {
                        r["ok"] = json!(true);
                        r["preexisting"] = json!(true);
                    }
                }
            }
            let summary = if results.is_empty() {
                "no typecheck, lint or test scripts".to_string()
            } else {
                results.iter().map(|r| if r["preexisting"] == true { format!("⚠ {} (already failing before the change)", r["script"].as_str().unwrap_or("?")) } else { format!("{} {}", ui_mark(r["ok"] == true), r["script"].as_str().unwrap_or("?")) }).collect::<Vec<_>>().join(" · ")
            };
            let passed = results.iter().all(|r| r["ok"] == true);
            run.nodes[i].outputs.insert("passed".into(), passed.to_string());
            run.nodes[i].outputs.insert("summary".into(), summary.clone());
            if let Some(failed) = results.iter().find(|r| r["ok"] != true) {
                run.nodes[i].outputs.insert("report".into(), failed["tail"].as_str().unwrap_or_default().to_string());
                bail!("checks failed: {summary}");
            }
            Ok(summary)
        }
        NodeKind::Check => check(state, &run.project, &s("check"), &s("env"), p["hours"].as_f64().unwrap_or(24.)).await,
        NodeKind::Notify => {
            let msg = substitute(&s("message"), run);
            run.nodes[i].outputs.insert("message".into(), msg.clone());
            // Al teléfono (ntfy, si está encendido el aviso de flujos); prioridad alta si el nodo lo pide.
            let high = s("priority") == "high";
            crate::notify::send(state, "flow", crate::notify::Message { title: run.workflow_name.clone(), body: msg.clone(), tags: if high { "rotating_light" } else { "robot" }, priority: if high { 4 } else { 3 } });
            Ok(msg)
        }
        NodeKind::Ask => {
            let question = substitute(&s("question"), run);
            run.nodes[i].outputs.insert("question".into(), question.clone());
            run.nodes[i].message = question.clone();
            run.nodes[i].status = NodeStatus::WaitingAnswer;
            run.status = RunStatus::WaitingAnswer;
            persist(state, run);
            crate::notify::send(state, "flow", crate::notify::Message { title: format!("{} asks", run.workflow_name), body: question.clone(), tags: "question", priority: 4 });
            let key = answer_key(&run.id, &node.id);
            loop {
                if let Some(answer) = state.store.probes().ok().and_then(|l| l.into_iter().find(|(k, _)| *k == key)).and_then(|(_, v)| v["text"].as_str().map(str::to_string)) {
                    run.nodes[i].outputs.insert("answer".into(), answer.clone());
                    run.status = RunStatus::Running;
                    return Ok(format!("answered: {}", answer.chars().take(120).collect::<String>()));
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                    _ = cancel.changed() => return Err(anyhow!("cancelada")),
                }
            }
        }
        NodeKind::Agent => {
            let role = crate::roles::get(state, &s("agent")).ok_or_else(|| anyhow!("there's no agent “{}”", s("agent")))?;
            // La corrida del agente se llama como el nodo: si el daemon se reinicia, se sigue esa.
            let agent_run = format!("{}-{}", run.id, node.id);
            if run.nodes[i].job.is_none() {
                let text = agent_task(run, node, &role);
                loop {
                    // Desde qué commit trabaja un agente que escribe: el del evento, si lo trae, o el de la corrida.
                    let base = run.event.as_ref().and_then(|e| e["evidence"]["commit"].as_str().or(e["commit"].as_str())).map(str::to_string).unwrap_or_else(|| run.commit.clone());
                    // Uno que solo lee después de uno que escribe (el Reviewer): mira el commit de ese, no la copia del proyecto.
                    let reviewed = run.snapshot.edges.iter().filter(|(_, b)| *b == node.id).filter_map(|(a, _)| run.nodes.iter().find(|n| &n.id == a)).filter_map(|n| n.outputs.get("commit").filter(|c| !c.is_empty()).cloned()).last();
                    let base = match reviewed { Some(c) if role.mode != crate::roles::Mode::Write => c, _ => base };
                    match crate::agent_host::start_role(state, &role, &agent_run, &text, Some(&run.project), &run.workflow_name, Some(&base)).await {
                        Ok(_) => break,
                        // Otros agentes ocupan los lugares: espera su turno.
                        Err(e) if e.to_string().contains("already running") => {
                            run.nodes[i].message = format!("waiting for a free agent slot ({e})");
                            persist(state, run);
                            tokio::select! {
                                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                                _ = cancel.changed() => return Err(anyhow!("cancelada")),
                            }
                        }
                        Err(e) => return Err(e),
                    }
                }
                run.nodes[i].job = Some(agent_run.clone());
                run.nodes[i].message = format!("{} is working", role.name);
                persist(state, run);
            }
            follow_agent(state, run, i, &agent_run, &role, cancel).await
        }
    }
}

fn ui_mark(ok: bool) -> &'static str {
    if ok { "✓" } else { "✗" }
}

fn answer_key(run: &str, node: &str) -> String {
    format!("answer:{run}:{node}")
}

/// La respuesta de la persona a la pregunta que espera la corrida (nodo «preguntar»). `Ok(false)`
/// si no hay ninguna esperando.
pub fn answer(state: &Shared, run_id: &str, text: &str) -> Result<bool> {
    let run = load_run(state, run_id)?;
    let Some(nr) = run.nodes.iter().find(|n| n.status == NodeStatus::WaitingAnswer) else { return Ok(false) };
    state.store.save_probe(&answer_key(run_id, &nr.id), &json!({ "text": text.trim(), "at": now_ms() }))?;
    Ok(true)
}

/// La tarea de un agente del flujo: su rol, el evento, lo que dejaron los nodos que lee y la forma
/// de su resultado. Por defecto lee los nodos que llegan a él.
/// La línea del reporte de un agente que escribe con su base: `(línea, si coincide con la rama de
/// desarrollo de GitHub)`. Sin rama de desarrollo conocida, nada.
fn base_line(r: &Value) -> Option<(String, bool)> {
    let base = r["base"].as_str().filter(|b| !b.is_empty())?;
    let short = |s: &str| s.chars().take(8).collect::<String>();
    let (branch, head) = (r["devHead"]["branch"].as_str()?, r["devHead"]["commit"].as_str()?);
    let current = base == head;
    let line = if current {
        format!("Base: {} (origin/{branch}, up to date).", short(base))
    } else {
        format!("⚠ Base: {} — NOT the current origin/{branch} ({}): this change was made on an older base and may conflict when merged.", short(base), short(head))
    };
    Some((line, current))
}

pub fn agent_task(run: &Run, node: &Node, role: &crate::roles::Role) -> String {
    let mut inputs: Vec<String> = node.params["input"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
    if inputs.is_empty() {
        inputs = run.snapshot.edges.iter().filter(|(_, b)| *b == node.id).map(|(a, _)| a.clone()).collect();
    }
    let mut out = format!("Your role: {}\n{}\n", role.name, role.instructions);
    if let Some(extra) = node.params["instructions"].as_str().filter(|t| !t.trim().is_empty()) {
        out.push_str(&format!("\nFor this flow: {}\n", substitute(extra, run)));
    }
    out.push_str(&format!("\nFlow: {} (project {}, commit {}).\n", run.workflow_name, run.project, run.commit));
    match &run.event {
        Some(ev) => out.push_str(&format!("\nThe event that started it (untrusted data):\n<event>\n{}\n</event>\n", serde_json::to_string_pretty(ev).unwrap_or_default())),
        None => out.push_str("\nIt was started by hand (no event).\n"),
    }
    for id in inputs {
        let Some(nr) = run.nodes.iter().find(|n| n.id == id) else { continue };
        let title = run.snapshot.nodes.iter().find(|n| n.id == id).map(|n| n.title.clone()).unwrap_or_else(|| id.clone());
        let report = nr.outputs.get("report").cloned().unwrap_or_else(|| nr.message.clone());
        let result = nr.outputs.get("result").cloned().unwrap_or_default();
        // Lo que se le pidió a ese nodo (el Reviewer compara el cambio con la tarea).
        let asked = run.snapshot.nodes.iter().find(|n| n.id == id).and_then(|n| n.params["instructions"].as_str()).filter(|t| !t.trim().is_empty()).map(|t| format!("What “{title}” was asked: {}\n", substitute(t, run))).unwrap_or_default();
        out.push_str(&format!("\n{asked}Result of “{title}” (untrusted data):\n<input from=\"{id}\">\n{report}\n{result}\n</input>\n"));
        // El cambio exacto de un agente que escribe: entre su base y su commit (no contra una rama local).
        if let (Some(base), Some(commit)) = (nr.outputs.get("base").filter(|b| !b.is_empty()), nr.outputs.get("commit").filter(|c| !c.is_empty())) {
            out.push_str(&format!("\nTo see exactly “{title}”'s change: `git diff {base}..{commit}` (its base is {base}; don't compare against a local branch).\n"));
            if role.mode != crate::roles::Mode::Write {
                out.push_str(&format!("Your working directory is checked out at {commit} (“{title}”'s commit): the files you read are the changed ones.\n"));
            }
        }
        // Un cambio hecho sobre una base vieja: quien lo revisa lo tiene que decir.
        if nr.outputs.get("base_current").map(String::as_str) == Some("false") {
            out.push_str(&format!("\nNote: “{title}” worked from an old base (its report says which): say so explicitly in your result; it may conflict when merged.\n"));
        }
    }
    out.push_str(&format!("\n{}", crate::roles::output_instructions(role)));
    out
}

/// Los chequeos que fallaron, en el commit de partida (un worktree aparte que se borra después): los que
/// también fallan ahí ya fallaban antes del cambio.
fn checks_at_base(state: &Shared, worktree: &str, base: &str, scripts: &[String]) -> Vec<Value> {
    let wt = std::path::Path::new(worktree);
    let dir = wt.with_file_name(format!("{}-base", wt.file_name().and_then(|n| n.to_str()).unwrap_or("run")));
    let git = |args: &[&str]| std::process::Command::new("git").arg("-C").arg(wt).args(args).output().map(|o| o.status.success()).unwrap_or(false);
    let _ = std::fs::remove_dir_all(&dir);
    if !git(&["worktree", "add", "-q", "--detach", &dir.display().to_string(), base]) {
        return vec![];
    }
    // Los mismos node_modules que el worktree del agente (un enlace).
    if let Ok(target) = std::fs::read_link(wt.join("node_modules")) {
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink(target, dir.join("node_modules"));
    }
    let res = state.agent_host.call(state, "POST", "/checks", Some(json!({ "cwd": dir.display().to_string(), "scripts": scripts }))).ok();
    git(&["worktree", "remove", "--force", &dir.display().to_string()]);
    res.and_then(|r| r["results"].as_array().cloned()).unwrap_or_default()
}

/// Espera al agente del nodo; al terminar, valida su resultado (una vez le pide corregirlo) y lo
/// deja como salidas del nodo.
async fn follow_agent(state: &Shared, run: &mut Run, i: usize, agent_run: &str, role: &crate::roles::Role, cancel: &mut watch::Receiver<bool>) -> Result<String> {
    loop {
        let r = state.store.agent_run(agent_run)?.ok_or_else(|| anyhow!("the agent run {agent_run} is gone"))?;
        match r["status"].as_str().unwrap_or("running") {
            "running" => {}
            "done" => {
                let answer = r["answer"].as_str().unwrap_or_default().to_string();
                let mut report = answer.split("```json").next().unwrap_or(&answer).trim().to_string();
                // Un agente que escribe: desde qué commit trabajó y si era lo último de la rama de desarrollo.
                let base_line = (role.mode == crate::roles::Mode::Write).then(|| base_line(&r)).flatten();
                if let Some((line, _)) = &base_line {
                    report = format!("{line}\n\n{report}");
                }
                // Lo que dejó un agente que escribe: su rama y el commit que hizo el daemon.
                let fix = r["fix"].clone();
                if role.mode == crate::roles::Mode::Write {
                    if let Some(e) = fix["error"].as_str() {
                        bail!("{}: couldn't save the change: {e}", role.name);
                    }
                }
                let add_fix = |out: &mut HashMap<String, String>| {
                    if role.mode == crate::roles::Mode::Write {
                        out.insert("changes".into(), (fix["changes"] == true).to_string());
                        out.insert("branch".into(), r["branch"].as_str().unwrap_or_default().to_string());
                        out.insert("commit".into(), fix["commit"].as_str().unwrap_or_default().to_string());
                        out.insert("stat".into(), fix["stat"].as_str().unwrap_or_default().to_string());
                        out.insert("worktree".into(), r["worktree"].as_str().unwrap_or_default().to_string());
                        out.insert("base".into(), r["base"].as_str().unwrap_or_default().to_string());
                        out.insert("dev_head".into(), r["devHead"]["commit"].as_str().unwrap_or_default().to_string());
                        if let Some((_, current)) = &base_line {
                            out.insert("base_current".into(), current.to_string());
                        }
                    }
                };
                if role.output.is_empty() {
                    run.nodes[i].outputs = crate::roles::outputs(&json!({}), &report);
                    add_fix(&mut run.nodes[i].outputs);
                    return Ok(format!("{}: {}", role.name, first_line(&report)));
                }
                let checked = crate::roles::extract_json(&answer).ok_or_else(|| anyhow!("no JSON result")).and_then(|v| crate::roles::validate(role, &v));
                match checked {
                    Ok(result) => {
                        run.nodes[i].outputs = crate::roles::outputs(&result, &report);
                        add_fix(&mut run.nodes[i].outputs);
                        let first = role.output.keys().next().and_then(|k| run.nodes[i].outputs.get(k)).cloned().unwrap_or_default();
                        return Ok(format!("{}: {}", role.name, first_line(&first)));
                    }
                    Err(e) if !run.nodes[i].outputs.contains_key("_retried") => {
                        // Una segunda oportunidad, en la misma conversación (durable).
                        run.nodes[i].outputs.insert("_retried".into(), "1".into());
                        run.nodes[i].message = format!("asked {} to fix its result ({e})", role.name);
                        persist(state, run);
                        let ask = format!("Your result didn't match the format ({e}). Reply again with only the ```json block.\n{}", crate::roles::output_instructions(role));
                        crate::agent_host::message(state, agent_run, &ask, &[]).await?;
                    }
                    Err(e) => bail!("{}'s result didn't match the format: {e}", role.name),
                }
            }
            other => bail!("{} {other}: {}", role.name, r["error"].as_str().unwrap_or("no answer")),
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            _ = cancel.changed() => {
                let _ = crate::agent_host::abort(state, agent_run).await;
                return Err(anyhow!("cancelada"));
            }
        }
    }
}

pub(crate) fn first_line(text: &str) -> String {
    text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).unwrap_or("").chars().take(160).collect()
}

async fn check(state: &Shared, project: &str, kind: &str, env: &str, hours: f64) -> Result<String> {
    match kind {
        "recent-backup" => {
            let last = state.store.backups(Some(env), 1)?.into_iter().next();
            let at = last.as_ref().and_then(|b| b["at"].as_i64()).unwrap_or(0);
            let age_h = (now_ms() - at) as f64 / 3_600_000.;
            if last.is_some() && age_h <= hours {
                Ok(format!("backup of {env} from {age_h:.1} h ago"))
            } else {
                bail!("no backup of {env} in the last {hours} h")
            }
        }
        "ci-green" => {
            let key = crate::probes::ci::probe_key(project);
            let ci = state.probe(&key).or_else(|| state.store.probes().ok()?.into_iter().find(|(k, _)| *k == key).map(|(_, v)| v)).unwrap_or(Value::Null);
            let repos = ci["data"]["repos"].as_array().cloned().unwrap_or_default();
            if repos.is_empty() {
                bail!("no CI data for this project (does it have a GitHub origin, and is gh logged in?)");
            }
            for r in &repos {
                let last = &r["runs"][0];
                if last["conclusion"] != "success" {
                    bail!("CI of {} isn't green", r["repo"].as_str().unwrap_or("?"));
                }
            }
            Ok("CI green".into())
        }
        "schema-safe" => {
            // Lo del backend local del proyecto contra lo desplegado en `env`: sin riesgos, pasa.
            let target = state.store.environment(env)?.ok_or_else(|| anyhow!("environment {env} doesn't exist"))?;
            let local = crate::schema::local_of(state, &target).ok_or_else(|| anyhow!("project {project} has no local backend to compare with"))?;
            let d = crate::schema::diff_envs(state, &local, &target).await?;
            let risks: Vec<String> = d["risks"].as_array().into_iter().flatten().filter_map(|r| r.as_str().map(str::to_string)).collect();
            if !risks.is_empty() {
                bail!("schema risks for {env}: {}", risks.join("; "));
            }
            let missing: Vec<&str> = d["envVars"]["missing"].as_array().into_iter().flatten().filter_map(|v| v.as_str()).collect();
            if !missing.is_empty() {
                bail!("{env} is missing environment variables the local backend has: {}", missing.join(", "));
            }
            Ok(if d["same"] == true { format!("{env} already has this schema") } else { format!("schema changes for {env} are safe") })
        }
        "env-reachable" => {
            let e = state.store.environment(env)?.ok_or_else(|| anyhow!("environment {env} doesn't exist"))?;
            let base = e.convex_url.clone().ok_or_else(|| anyhow!("{env} has no Convex URL"))?;
            let url = format!("{}/version", base.trim_end_matches('/'));
            let ok = tokio::task::spawn_blocking(move || ureq::AgentBuilder::new().timeout(Duration::from_secs(5)).build().get(&url).call().is_ok()).await.unwrap_or(false);
            if ok {
                Ok(format!("{env} responds"))
            } else {
                bail!("{env} isn't responding")
            }
        }
        other => bail!("unknown check: {other}"),
    }

}

#[cfg(test)]
mod tests {
    #[test]
    fn el_reporte_dice_si_la_base_es_la_ultima_de_dev() {
        let r = serde_json::json!({ "base": "aaaaaaaa11", "devHead": { "branch": "dev", "commit": "aaaaaaaa11" } });
        let (line, current) = super::base_line(&r).unwrap();
        assert!(current && line.contains("up to date"), "{line}");
        let old = serde_json::json!({ "base": "bbbbbbbb22", "devHead": { "branch": "dev", "commit": "aaaaaaaa11" } });
        let (line, current) = super::base_line(&old).unwrap();
        assert!(!current && line.contains("NOT the current origin/dev") && line.contains("aaaaaaaa"), "{line}");
        assert!(super::base_line(&serde_json::json!({ "base": "x" })).is_none());
    }

    use super::*;

    fn wf(nodes: Vec<Node>, edges: Vec<(&str, &str)>) -> Workflow {
        Workflow { id: "w1".into(), project: "p".into(), name: "Prueba".into(), description: String::new(), nodes, edges: edges.into_iter().map(|(a, b)| (a.into(), b.into())).collect(), enabled: true, conditions: Default::default(), triggers: vec![], updated_at: 0 }
    }

    #[test]
    fn orden_topologico_y_ciclos() {
        let n = |id: &str| node(id, NodeKind::Notify, id, 0., 0., json!({}));
        let w = wf(vec![n("c"), n("a"), n("b")], vec![("a", "b"), ("b", "c")]);
        assert_eq!(topo_order(&w).unwrap(), vec!["a", "b", "c"]);
        let ciclo = wf(vec![n("a"), n("b")], vec![("a", "b"), ("b", "a")]);
        assert!(topo_order(&ciclo).is_err());
        let fantasma = wf(vec![n("a")], vec![("a", "z")]);
        assert!(topo_order(&fantasma).is_err());
    }

    #[test]
    fn problemas_del_flujo() {
        let envs: HashSet<String> = ["local".to_string()].into();
        let w = wf(
            vec![
                node("r", NodeKind::Recipe, "Receta", 0., 0., json!({ "recipe": "export", "env": "local" })),
                node("x", NodeKind::Recipe, "Mala", 0., 0., json!({ "recipe": "nada", "env": "otro" })),
                node("w", NodeKind::Wait, "Espera", 0., 0., json!({ "minutes": 0 })),
                node("c", NodeKind::Check, "Ver", 0., 0., json!({ "check": "raro" })),
            ],
            vec![],
        );
        let p = problems(&w, &envs, |r| r == "export");
        assert!(p.iter().any(|m| m.contains("recipe nada doesn't exist")));
        assert!(p.iter().any(|m| m.contains("environment otro doesn't exist")));
        assert!(p.iter().any(|m| m.contains("minutes")));
        assert!(p.iter().any(|m| m.contains("unknown check")));
        // Un nodo apagado no se valida.
        let mut w2 = w.clone();
        w2.nodes.retain(|n| n.id == "r" || n.id == "x");
        w2.nodes[1].enabled = false;
        assert!(problems(&w2, &envs, |r| r == "export").is_empty());
    }

    #[test]
    fn exportar_e_importar() {
        let w = templates("heskala-erp", true, &[("prod".into(), "cloud-prod".into()), ("dev".into(), "cloud-dev".into())]).remove(0);
        let raw = export(&w);
        let back = import("otro", &raw).unwrap();
        assert_eq!(back.name, w.name);
        assert_eq!(back.nodes.len(), 5);
        assert_eq!(back.edges.len(), 4);
        assert_eq!(back.project, "otro");
        assert!(back.id.is_empty());
        assert!(import("otro", &json!({ "nodes": [] })).is_err());
        // La promoción de siempre: respaldo → ensayo → cloud dev → espera → producción.
        assert_eq!(topo_order(&back).unwrap(), vec!["backup", "rehearsal", "deploy-dev", "quiet", "promote"]);
    }

    #[test]
    fn sustituye_commit_y_salidas() {
        let w = wf(vec![], vec![]);
        let mut outs = HashMap::new();
        outs.insert("backup_id".to_string(), "42".to_string());
        let run = Run {
            id: "r".into(),
            workflow_id: "w1".into(),
            workflow_name: "Prueba".into(),
            project: "p".into(),
            commit: "abc123".into(),
            status: RunStatus::Running,
            snapshot: w,
            order: vec![],
            nodes: vec![NodeRun { id: "backup".into(), status: NodeStatus::Succeeded, job: None, started_at: None, ended_at: None, message: String::new(), outputs: outs }],
            requested_by: "x".into(),
            created_at: 0,
            ended_at: None,
            error: None, event: None,
        };
        assert_eq!(substitute("{run.commit}/{node.backup.backup_id}", &run), "abc123/42");
    }

    #[test]
    fn sugeridos_sin_problemas() {
        let envs = vec![("local".to_string(), "devbox".to_string()), ("dev".to_string(), "cloud-dev".to_string()), ("prod".to_string(), "cloud-prod".to_string())];
        let names: HashSet<String> = envs.iter().map(|(n, _)| n.clone()).collect();
        let list: Vec<Workflow> = templates("heskala-erp", true, &envs).into_iter().filter(is_suggested).collect();
        assert!(list.len() >= 6, "{}", list.len());
        for w in &list {
            let p = problems(w, &names, |r| crate::recipes::get(r).is_some());
            assert!(p.is_empty(), "{}: {p:?}", w.name);
        }
        // Otro proyecto: solo los genéricos (sin promoción, ensayo ni reversión).
        let other: Vec<String> = templates("otro", false, &envs).into_iter().filter(is_suggested).map(|w| w.name).collect();
        assert!(other.contains(&"Deploy to cloud dev".to_string()) && !other.iter().any(|n| n.contains("production") || n.contains("Rehearsal")));
    }

    #[test]
    fn la_tarea_de_un_agente_lleva_el_evento_y_lo_de_antes() {
        let envs = vec![("dev".to_string(), "cloud-dev".to_string())];
        let w = templates("otro", false, &envs).into_iter().find(|w| w.name == "Triage an error").unwrap();
        let names: HashSet<String> = envs.iter().map(|(n, _)| n.clone()).collect();
        assert!(problems(&w, &names, |_| true).is_empty(), "{:?}", problems(&w, &names, |_| true));
        let mut bad = w.clone();
        bad.nodes[0].params = json!({});
        assert!(problems(&bad, &names, |_| true).iter().any(|p| p.contains("choose an agent")));
        let order = topo_order(&w).unwrap();
        let mut run = Run {
            id: "r1".into(), workflow_id: "w".into(), workflow_name: w.name.clone(), project: "otro".into(), commit: "abc".into(), status: RunStatus::Running,
            snapshot: w.clone(), order: order.clone(),
            nodes: order.iter().map(|id| NodeRun { id: id.clone(), status: NodeStatus::Pending, job: None, started_at: None, ended_at: None, message: String::new(), outputs: HashMap::new() }).collect(),
            requested_by: "app".into(), created_at: 0, ended_at: None, error: None,
            event: Some(json!({ "source": "convex-error", "title": "Uncaught Error in payments:charge" })),
        };
        run.nodes[0].outputs = crate::roles::outputs(&json!({ "summary": "charge throws on null card", "where": ["convex/payments.ts:40"] }), "It throws.");
        let roles = crate::roles::defaults();
        let risk = roles.iter().find(|r| r.id == "risk").unwrap();
        let task = agent_task(&run, &w.nodes[1], risk);
        assert!(task.contains("Your role: Risk") && task.contains("payments:charge") && task.contains("charge throws on null card") && task.contains("one of: low, medium, high, critical"), "{task}");
        // El aviso usa las salidas de los agentes.
        run.nodes[1].outputs = crate::roles::outputs(&json!({ "risk": "high", "immediate_fix": true }), "");
        assert_eq!(substitute("{node.triage.summary} · {node.risk.risk} · {node.risk.immediate_fix}", &run), "charge throws on null card · high · true");
    }

    #[test]
    fn una_arista_con_condicion_salta_lo_que_no_toca() {
        let envs = vec![("dev".to_string(), "cloud-dev".to_string())];
        let w = templates("otro", false, &envs).into_iter().find(|w| w.name == "Triage an error, notify if risky").unwrap();
        let names: HashSet<String> = envs.iter().map(|(n, _)| n.clone()).collect();
        assert!(problems(&w, &names, |_| true).is_empty());
        let mut bad = w.clone();
        bad.conditions.insert(edge_key("risk", "notify"), "risk >= high".into());
        bad.conditions.insert(edge_key("triage", "notify"), "x.y == 1".into());
        let p = problems(&bad, &names, |_| true);
        assert!(p.iter().any(|x| x.contains("node.field")) && p.iter().any(|x| x.contains("no connection")), "{p:?}");
        let st = crate::state::test_state();
        let order = topo_order(&w).unwrap();
        let mut run = Run {
            id: "r1".into(), workflow_id: "w".into(), workflow_name: w.name.clone(), project: "otro".into(), commit: "abc".into(), status: RunStatus::Running,
            snapshot: w.clone(), order: order.clone(),
            nodes: order.iter().map(|id| NodeRun { id: id.clone(), status: NodeStatus::Succeeded, job: None, started_at: None, ended_at: None, message: String::new(), outputs: HashMap::new() }).collect(),
            requested_by: "app".into(), created_at: 0, ended_at: None, error: None, event: None,
        };
        let risk = run.nodes.iter_mut().find(|n| n.id == "risk").unwrap();
        risk.outputs = crate::roles::outputs(&json!({ "risk": "medium" }), "");
        let why = blocked(&st, &run, "notify").unwrap();
        assert!(why.contains("risk.risk >= high"), "{why}");
        run.nodes.iter_mut().find(|n| n.id == "risk").unwrap().outputs = crate::roles::outputs(&json!({ "risk": "critical" }), "");
        assert!(blocked(&st, &run, "notify").is_none(), "critical >= high, by the risk agent's order");
        // Sin aristas de entrada corre; si el anterior se saltó, este también.
        assert!(blocked(&st, &run, "triage").is_none());
        run.nodes.iter_mut().find(|n| n.id == "risk").unwrap().status = NodeStatus::Skipped;
        assert!(blocked(&st, &run, "notify").unwrap().contains("risk didn't run"));
        // Exportar e importar conserva las condiciones.
        let back = import("otro", &export(&w)).unwrap();
        assert_eq!(back.conditions.get(&edge_key("risk", "notify")).map(String::as_str), Some("risk.risk >= high"));
    }
}
