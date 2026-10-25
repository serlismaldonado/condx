// Plantillas de agente por tipo de incidente (docs/design/agents.md, fase A5: la "fábrica" de
// agentes). Una plantilla decide cómo trabaja el agente en los incidentes que le tocan:
// - cuándo aplica: fuentes (`job`, `vercel`, `ci`, `convex`) y, opcional, un texto que debe
//   aparecer en el título, la huella o el error;
// - hasta qué modo puede llegar (`diagnose` o `fix`): la autonomía no lo supera;
// - proveedor y modelo (si no, los de por defecto), instrucciones que se suman al prompt, qué
//   herramientas puede usar y los límites de cada corrida (tokens y minutos).
// Gana la primera encendida que coincide; la genérica (sin fuentes) va al final. Las de fábrica se
// pueden cambiar, apagar o restaurar; solo la app las cambia.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents::{Level, SOURCES};
use crate::state::Shared;

const KEY: &str = "meta:agent-templates";

/// Las herramientas que una plantilla puede dar (las de escritura solo existen en modo arreglo).
pub const TOOLS: &[&str] = &["read_file", "list_files", "search_code", "git_log", "git_show", "condx", "edit_file", "write_file", "run_check"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Template {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// Fuentes a las que aplica; vacío: a todas (la genérica).
    pub sources: Vec<String>,
    /// Texto que debe aparecer (sin distinguir mayúsculas) en el título, la huella o el error.
    #[serde(rename = "match")]
    pub match_text: Option<String>,
    /// Hasta dónde puede llegar: `diagnose` o `fix`.
    pub max_mode: Level,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub instructions: String,
    /// Herramientas permitidas; vacío: todas las del modo.
    pub tools: Vec<String>,
    /// Tokens por corrida (0: sin límite propio; el presupuesto diario sigue).
    pub max_tokens: u64,
    /// Minutos por corrida (0: sin límite propio).
    pub max_minutes: u64,
    pub builtin: bool,
}

impl Default for Template {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            enabled: true,
            sources: vec![],
            match_text: None,
            max_mode: Level::Fix,
            provider: None,
            model: None,
            instructions: String::new(),
            tools: vec![],
            max_tokens: 150_000,
            max_minutes: 15,
            builtin: false,
        }
    }
}

fn builtin(id: &str, name: &str, sources: &[&str], match_text: Option<&str>, max_mode: Level, instructions: &str) -> Template {
    Template {
        id: id.into(),
        name: name.into(),
        sources: sources.iter().map(|s| s.to_string()).collect(),
        match_text: match_text.map(str::to_string),
        max_mode,
        instructions: instructions.into(),
        builtin: true,
        ..Default::default()
    }
}

/// Las de fábrica, en orden (la genérica al final).
pub fn defaults() -> Vec<Template> {
    vec![
        builtin(
            "frontend-build",
            "Frontend build (Vercel)",
            &["vercel"],
            None,
            Level::Fix,
            "This is a frontend build that failed on Vercel. Start from the end of the build log in the evidence: the first error usually names the file and line. Check recent changes to package.json and imports (renamed or removed exports after a dependency upgrade are common). In fix mode, run the build or typecheck script to confirm.",
        ),
        builtin(
            "failing-tests",
            "Failing tests",
            &["ci", "job"],
            Some("test"),
            Level::Fix,
            "Tests are failing. Reproduce with the test script first. Fix the code under test, not the test, unless the test itself is clearly wrong; if you change a test, say why in Change.",
        ),
        builtin(
            "convex-deploy",
            "Convex deploy or function error",
            &["job"],
            Some("deploy"),
            Level::Fix,
            "A Convex deploy or function failed. Look in convex/: schema.ts (validators, indexes), the function named in the error, and its args and return validators. Schema changes that existing documents don't satisfy are a common cause: say so instead of loosening the schema silently.",
        ),
        Template {
            tools: vec!["condx".into(), "git_log".into()],
            max_tokens: 50_000,
            ..builtin(
            "convex-down",
            "Convex not responding",
            &["convex"],
            None,
            Level::Diagnose,
            "A Convex deployment isn't responding. This is usually not a code problem: check the environment and recent deploys through condx (get_environment, get_overview, list_jobs) and say what the person should check. Don't propose code changes unless the evidence points to them.",
        )
        },
        builtin("generic", "Any other incident", &[], None, Level::Fix, ""),
    ]
}

pub fn list(state: &Shared) -> Vec<Template> {
    let stored = state.store.probes().ok().and_then(|l| l.into_iter().find(|(k, _)| k == KEY)).map(|(_, v)| v);
    stored.and_then(|v| serde_json::from_value::<Vec<Template>>(v).ok()).unwrap_or_else(defaults)
}

fn save_all(state: &Shared, list: &[Template]) -> Result<()> {
    state.store.save_probe(KEY, &serde_json::to_value(list)?)?;
    Ok(())
}

/// La plantilla para un incidente: la primera encendida que coincide.
pub fn for_incident(list: &[Template], incident: &Value) -> Option<Template> {
    let source = incident["source"].as_str().unwrap_or_default();
    let haystack = format!(
        "{} {} {} {}",
        incident["title"].as_str().unwrap_or_default(),
        incident["fingerprint"].as_str().unwrap_or_default(),
        incident["evidence"]["error"].as_str().unwrap_or_default(),
        incident["evidence"]["recipe"].as_str().unwrap_or_default()
    )
    .to_lowercase();
    list.iter()
        .filter(|t| t.enabled)
        .find(|t| (t.sources.is_empty() || t.sources.iter().any(|s| s == source)) && t.match_text.as_deref().is_none_or(|m| m.trim().is_empty() || haystack.contains(&m.trim().to_lowercase())))
        .cloned()
}

/// Crea o cambia una plantilla (por `id`; sin él, una nueva al final, antes de la genérica).
pub fn upsert(state: &Shared, body: &Value) -> Result<Template> {
    let mut t: Template = serde_json::from_value(body.clone()).map_err(|e| anyhow::anyhow!("invalid template: {e}"))?;
    t.name = t.name.trim().to_string();
    if t.name.is_empty() {
        bail!("give the template a name");
    }
    for s in &t.sources {
        if !SOURCES.contains(&s.as_str()) {
            bail!("unknown incident source: {s}");
        }
    }
    for tool in &t.tools {
        if !TOOLS.contains(&tool.as_str()) {
            bail!("unknown tool: {tool}");
        }
    }
    if !matches!(t.max_mode, Level::Diagnose | Level::Fix) {
        bail!("the highest mode is diagnose or fix");
    }
    if let Some(p) = t.provider.as_deref().filter(|p| !p.is_empty()) {
        if !state.store.agent_providers()?.iter().any(|v| v["id"] == p) {
            bail!("there's no provider “{p}”");
        }
    }
    if t.instructions.len() > 4_000 {
        bail!("instructions: at most 4000 characters");
    }
    let mut list = list(state);
    match list.iter_mut().find(|x| x.id == t.id && !t.id.is_empty()) {
        Some(existing) => {
            t.builtin = existing.builtin;
            *existing = t.clone();
        }
        None => {
            t.id = format!("tpl-{:x}", crate::store::now_ms());
            t.builtin = false;
            let at = list.iter().position(|x| x.sources.is_empty() && x.match_text.is_none()).unwrap_or(list.len());
            list.insert(at, t.clone());
        }
    }
    save_all(state, &list)?;
    Ok(t)
}

pub fn delete(state: &Shared, id: &str) -> Result<()> {
    let mut list = list(state);
    let Some(i) = list.iter().position(|t| t.id == id) else { bail!("there's no template “{id}”") };
    if list[i].builtin {
        bail!("built-in templates can be turned off, not deleted");
    }
    list.remove(i);
    save_all(state, &list)
}

/// Vuelve a las de fábrica (las propias se conservan, antes de la genérica).
pub fn reset(state: &Shared) -> Result<Vec<Template>> {
    let own: Vec<Template> = list(state).into_iter().filter(|t| !t.builtin).collect();
    let mut out = defaults();
    let at = out.len() - 1;
    for (i, t) in own.into_iter().enumerate() {
        out.insert(at + i, t);
    }
    save_all(state, &out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn inc(source: &str, title: &str, error: &str, recipe: &str) -> Value {
        json!({ "source": source, "title": title, "fingerprint": format!("{source}:x"), "evidence": { "error": error, "recipe": recipe } })
    }

    #[test]
    fn elige_la_primera_que_coincide() {
        let list = defaults();
        let pick = |i: &Value| for_incident(&list, i).unwrap().id;
        assert_eq!(pick(&inc("vercel", "Vercel deploy of main failed", "", "")), "frontend-build");
        assert_eq!(pick(&inc("job", "Unit tests failed on dev", "not ok 1", "unit-tests")), "failing-tests");
        assert_eq!(pick(&inc("ci", "CI “Tests” failed on main", "", "")), "failing-tests");
        assert_eq!(pick(&inc("job", "Deploy failed on dev", "Schema validation failed", "deploy")), "convex-deploy");
        assert_eq!(pick(&inc("convex", "Convex for dev isn't responding", "timeout", "")), "convex-down");
        assert_eq!(pick(&inc("job", "Backup failed", "disk full", "export")), "generic");
        // Una apagada no se elige.
        let mut off = defaults();
        off[0].enabled = false;
        assert_eq!(for_incident(&off, &inc("vercel", "x", "", "")).unwrap().id, "generic");
    }

    #[test]
    fn se_crean_cambian_y_restauran() {
        let st = crate::state::test_state();
        assert_eq!(list(&st).len(), defaults().len());
        let t = upsert(&st, &json!({ "name": "Payments", "sources": ["job"], "match": "stripe", "maxMode": "diagnose", "instructions": "Never touch keys.", "tools": ["read_file", "search_code"], "maxTokens": 20000, "maxMinutes": 5 })).unwrap();
        assert!(t.id.starts_with("tpl-"));
        let l = list(&st);
        // Antes de la genérica, que sigue al final.
        assert_eq!(l[l.len() - 2].name, "Payments");
        assert_eq!(l.last().unwrap().id, "generic");
        assert_eq!(for_incident(&l, &inc("job", "Charge failed", "Stripe: card declined", "charge")).unwrap().name, "Payments");
        // Cambiar una de fábrica la conserva como de fábrica; borrarla no se puede.
        let mut fb = serde_json::to_value(&l[0]).unwrap();
        fb["enabled"] = json!(false);
        assert!(upsert(&st, &fb).unwrap().builtin);
        assert!(delete(&st, "frontend-build").is_err());
        assert!(delete(&st, &t.id).is_ok());
        assert!(upsert(&st, &json!({ "name": "x", "sources": ["mail"] })).is_err());
        assert!(upsert(&st, &json!({ "name": "x", "tools": ["bash"] })).is_err());
        assert!(upsert(&st, &json!({ "name": "x", "maxMode": "notify" })).is_err());
        assert!(upsert(&st, &json!({ "name": "  " })).is_err());
        // Restaurar enciende otra vez la de fábrica.
        assert!(reset(&st).unwrap()[0].enabled);
    }
}
