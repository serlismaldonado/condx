// Agentes con responsabilidad (docs/design/agent-flows.md, §2.3): el catálogo de roles que los
// flujos usan en sus nodos «agente». Un rol dice qué hace el agente (sus instrucciones), si solo
// lee o escribe en su worktree, qué modelo usa, qué herramientas, sus límites y la forma de su
// resultado: campos que el daemon valida antes de pasarlos al nodo siguiente (y a las condiciones).
// Los de fábrica se pueden cambiar, apagar o restaurar; solo la app los cambia.

use anyhow::{bail, Result};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::state::Shared;

const KEY: &str = "meta:agent-roles";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Lee el código y Condx (por el MCP).
    #[default]
    Read,
    /// Escribe en su worktree y corre los chequeos en el sandbox (como el modo arreglo).
    Write,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Role {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// Qué hace: se suma al prompt del agente.
    pub instructions: String,
    pub mode: Mode,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// Herramientas permitidas; vacío: todas las del modo.
    pub tools: Vec<String>,
    pub max_tokens: u64,
    pub max_minutes: u64,
    /// La forma del resultado: campo → tipo (`string`, `boolean`, `number`, `list` o valores
    /// separados por `|`, como `low|medium|high|critical`). Vacío: texto libre.
    pub output: IndexMap<String, String>,
    pub builtin: bool,
}

impl Default for Role {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            enabled: true,
            instructions: String::new(),
            mode: Mode::Read,
            provider: None,
            model: None,
            tools: vec![],
            max_tokens: 80_000,
            max_minutes: 10,
            output: IndexMap::new(),
            builtin: false,
        }
    }
}

fn builtin(id: &str, name: &str, mode: Mode, instructions: &str, output: &[(&str, &str)]) -> Role {
    Role {
        id: id.into(),
        name: name.into(),
        mode,
        instructions: instructions.into(),
        output: output.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        builtin: true,
        ..Default::default()
    }
}

pub fn defaults() -> Vec<Role> {
    vec![
        builtin(
            "triage",
            "Triage",
            Mode::Read,
            "You triage an error. Find where it happens and the most likely cause, quickly: read the error and its context, then the code it points to. Don't propose a full fix; a reviewer and a fixer come after you.",
            &[("summary", "string"), ("where", "list"), ("likely_cause", "string"), ("confidence", "low|medium|high")],
        ),
        builtin(
            "risk",
            "Risk",
            Mode::Read,
            "You rate the risk of an error that another agent already triaged (its report is in your inputs). Consider who is affected (production users, data, payments, security), how often it happens and whether it gets worse. Say what the developer should check and whether it needs a fix right now. low: cosmetic or rare; medium: a feature degraded for some users; high: a main flow broken or data at risk; critical: production down, data loss or a security hole.",
            &[("risk", "low|medium|high|critical"), ("impact", "string"), ("check", "list"), ("immediate_fix", "boolean"), ("reason", "string")],
        ),
        Role {
            max_tokens: 150_000,
            max_minutes: 15,
            ..builtin(
                "fixer",
                "Fixer",
                Mode::Write,
                "You fix the error described in your inputs, in your own worktree: the smallest change that fixes the cause, then the project's checks.",
                &[("change", "string"), ("files", "list"), ("checks", "string")],
            )
        },
        builtin(
            "reviewer",
            "Reviewer",
            Mode::Read,
            "You review a fix another agent made (its branch, commit and summary are in your inputs; read the diff with git_show). Check that it fixes the cause, doesn't break anything else, touches nothing unrelated and has no secrets. Approve only if you'd merge it.",
            &[("verdict", "approve|changes"), ("findings", "list")],
        ),
        builtin("explainer", "Explainer", Mode::Read, "You answer the developer's questions about this thread's event and the project, briefly, with file:line references.", &[]),
    ]
}

pub fn list(state: &Shared) -> Vec<Role> {
    let stored = state.store.probes().ok().and_then(|l| l.into_iter().find(|(k, _)| k == KEY)).map(|(_, v)| v);
    stored.and_then(|v| serde_json::from_value::<Vec<Role>>(v).ok()).unwrap_or_else(defaults)
}

pub fn get(state: &Shared, id: &str) -> Option<Role> {
    list(state).into_iter().find(|r| r.id == id)
}

fn save_all(state: &Shared, list: &[Role]) -> Result<()> {
    state.store.save_probe(KEY, &serde_json::to_value(list)?)?;
    Ok(())
}

fn valid_type(t: &str) -> bool {
    matches!(t, "string" | "boolean" | "number" | "list") || (t.contains('|') && t.split('|').all(|v| !v.trim().is_empty()))
}

pub fn upsert(state: &Shared, body: &Value) -> Result<Role> {
    let mut r: Role = serde_json::from_value(body.clone()).map_err(|e| anyhow::anyhow!("invalid agent: {e}"))?;
    r.name = r.name.trim().to_string();
    if r.name.is_empty() {
        bail!("give the agent a name");
    }
    for tool in &r.tools {
        if !crate::templates::TOOLS.contains(&tool.as_str()) {
            bail!("unknown tool: {tool}");
        }
    }
    for (field, t) in &r.output {
        if field.trim().is_empty() || !field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            bail!("output field “{field}”: letters, numbers and _ only");
        }
        if !valid_type(t) {
            bail!("output field “{field}”: the type is string, boolean, number, list or values like low|high");
        }
    }
    if let Some(p) = r.provider.as_deref().filter(|p| !p.is_empty()) {
        if !state.store.agent_providers()?.iter().any(|v| v["id"] == p) {
            bail!("there's no provider “{p}”");
        }
    }
    if r.instructions.len() > 6_000 {
        bail!("instructions: at most 6000 characters");
    }
    let mut list = list(state);
    match list.iter_mut().find(|x| x.id == r.id && !r.id.is_empty()) {
        Some(existing) => {
            r.builtin = existing.builtin;
            *existing = r.clone();
        }
        None => {
            let base: String = r.name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
            let base = base.trim_matches('-').to_string();
            let mut id = if base.is_empty() { "agent".to_string() } else { base };
            while list.iter().any(|x| x.id == id) {
                id = format!("{id}-{:x}", crate::store::now_ms() % 0xfff);
            }
            r.id = id;
            r.builtin = false;
            list.push(r.clone());
        }
    }
    save_all(state, &list)?;
    Ok(r)
}

pub fn delete(state: &Shared, id: &str) -> Result<()> {
    let mut list = list(state);
    let Some(i) = list.iter().position(|r| r.id == id) else { bail!("there's no agent “{id}”") };
    if list[i].builtin {
        bail!("built-in agents can be turned off, not deleted");
    }
    list.remove(i);
    save_all(state, &list)
}

/// Vuelve a los de fábrica (los propios se conservan).
pub fn reset(state: &Shared) -> Result<Vec<Role>> {
    let own: Vec<Role> = list(state).into_iter().filter(|r| !r.builtin).collect();
    let mut out = defaults();
    out.extend(own);
    save_all(state, &out)?;
    Ok(out)
}

/// Lo que se le pide al agente sobre la forma de su resultado (va al final de su tarea).
pub fn output_instructions(role: &Role) -> String {
    if role.output.is_empty() {
        return "Answer in plain text.".into();
    }
    let fields = role
        .output
        .iter()
        .map(|(k, t)| {
            let desc = match t.as_str() {
                "string" => "text".to_string(),
                "boolean" => "true or false".to_string(),
                "number" => "a number".to_string(),
                "list" => "a list of short strings".to_string(),
                values => format!("one of: {}", values.split('|').map(str::trim).collect::<Vec<_>>().join(", ")),
            };
            format!("  \"{k}\": {desc}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("When you're done, write a short explanation, then end with your result as a JSON object in a ```json block, with exactly these fields:\n{fields}")
}

/// El último bloque JSON de la respuesta (en ```json … ``` o el último objeto `{…}`).
pub fn extract_json(answer: &str) -> Option<Value> {
    if let Some(start) = answer.rfind("```json") {
        let rest = &answer[start + 7..];
        let end = rest.find("```").unwrap_or(rest.len());
        if let Ok(v) = serde_json::from_str::<Value>(rest[..end].trim()) {
            return Some(v);
        }
    }
    let end = answer.rfind('}')?;
    let mut depth = 0i32;
    for (i, c) in answer[..=end].char_indices().rev() {
        match c {
            '}' => depth += 1,
            '{' => {
                depth -= 1;
                if depth == 0 {
                    return serde_json::from_str(&answer[i..=end]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

/// Valida el resultado contra la forma del rol: cada campo presente y de su tipo. Devuelve el
/// objeto con solo esos campos (y los valores de las opciones normalizados a minúsculas).
pub fn validate(role: &Role, value: &Value) -> Result<Value> {
    let Some(obj) = value.as_object() else { bail!("the result isn't a JSON object") };
    let mut out = serde_json::Map::new();
    let mut problems = vec![];
    for (field, t) in &role.output {
        let v = obj.get(field).cloned().unwrap_or(Value::Null);
        let ok = match t.as_str() {
            "string" => v.as_str().map(|s| json!(s)),
            "boolean" => v.as_bool().map(|b| json!(b)).or_else(|| v.as_str().and_then(|s| s.parse::<bool>().ok()).map(|b| json!(b))),
            "number" => v.as_f64().map(|n| json!(n)),
            "list" => v.as_array().map(|a| json!(a.iter().map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string())).collect::<Vec<_>>())).or_else(|| v.as_str().map(|s| json!([s]))),
            values => {
                let options: Vec<String> = values.split('|').map(|s| s.trim().to_lowercase()).collect();
                v.as_str().map(|s| s.trim().to_lowercase()).filter(|s| options.contains(s)).map(|s| json!(s))
            }
        };
        match ok {
            Some(v) => {
                out.insert(field.clone(), v);
            }
            None => problems.push(format!("“{field}” should be {t}")),
        }
    }
    if !problems.is_empty() {
        bail!("{}", problems.join(", "));
    }
    Ok(Value::Object(out))
}

/// Las salidas de un nodo a partir del resultado: cada campo como texto (las listas unidas por
/// «; »), más `report` (la explicación) y `result` (el JSON entero).
pub fn outputs(result: &Value, report: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    if let Some(obj) = result.as_object() {
        for (k, v) in obj {
            let text = match v {
                Value::String(s) => s.clone(),
                Value::Array(a) => a.iter().map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string())).collect::<Vec<_>>().join("; "),
                other => other.to_string(),
            };
            out.insert(k.clone(), text);
        }
    }
    out.insert("result".into(), result.to_string());
    out.insert("report".into(), report.to_string());
    out
}

pub fn json(state: &Shared) -> Value {
    json!({ "roles": list(state), "tools": crate::templates::TOOLS })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn risk() -> Role {
        defaults().into_iter().find(|r| r.id == "risk").unwrap()
    }

    #[test]
    fn extrae_y_valida_el_resultado() {
        let answer = "It breaks checkout for everyone.\n\n```json\n{\"risk\": \"High\", \"impact\": \"checkout down\", \"check\": [\"payments.ts:40\"], \"immediate_fix\": \"true\", \"reason\": \"main flow\", \"extra\": 1}\n```";
        let v = validate(&risk(), &extract_json(answer).unwrap()).unwrap();
        assert_eq!(v, json!({ "risk": "high", "impact": "checkout down", "check": ["payments.ts:40"], "immediate_fix": true, "reason": "main flow" }));
        // Sin bloque: el último objeto.
        assert_eq!(extract_json("ok {\"a\": {\"b\": 1}} done").unwrap(), json!({ "a": { "b": 1 } }));
        assert!(extract_json("no json here").is_none());
        // Un valor fuera de las opciones o un campo que falta.
        let err = validate(&risk(), &json!({ "risk": "extreme", "impact": "x", "check": [], "immediate_fix": true })).unwrap_err().to_string();
        assert!(err.contains("risk") && err.contains("reason"), "{err}");
        let out = outputs(&v, "It breaks checkout");
        assert_eq!(out["risk"], "high");
        assert_eq!(out["check"], "payments.ts:40");
        assert_eq!(out["immediate_fix"], "true");
        assert!(output_instructions(&risk()).contains("one of: low, medium, high, critical"));
    }

    #[test]
    fn se_crean_cambian_y_restauran() {
        let st = crate::state::test_state();
        assert_eq!(list(&st).len(), defaults().len());
        let r = upsert(&st, &json!({ "name": "Security auditor", "instructions": "Look for secrets.", "output": { "severity": "low|high", "files": "list" } })).unwrap();
        assert_eq!(r.id, "security-auditor");
        assert!(get(&st, "security-auditor").is_some());
        assert!(upsert(&st, &json!({ "name": "x", "output": { "bad field": "string" } })).is_err());
        assert!(upsert(&st, &json!({ "name": "x", "output": { "a": "date" } })).is_err());
        assert!(upsert(&st, &json!({ "name": "x", "tools": ["bash"] })).is_err());
        let mut tri = serde_json::to_value(get(&st, "triage").unwrap()).unwrap();
        tri["enabled"] = json!(false);
        assert!(upsert(&st, &tri).unwrap().builtin);
        assert!(delete(&st, "triage").is_err());
        assert!(delete(&st, "security-auditor").is_ok());
        assert!(reset(&st).unwrap().iter().find(|r| r.id == "triage").unwrap().enabled);
    }
}
