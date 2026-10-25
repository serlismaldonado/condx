// Reglas de datos por tipo de entorno, configurables desde la app (Configuración):
// - leer: si la pestaña Datos (y el MCP) pueden leer los registros del entorno;
// - cambiar: cuánto se exige a las recetas que modifican el entorno (`writes`): lo que diga la
//   receta, o como mínimo una aprobación, una aprobación escribiendo el nombre, Touch ID, o
//   bloqueadas.
// Endurecen lo que pide cada receta; la única que afloja es «sin aprobación», y nunca en
// producción. Las cambia únicamente la app (alcance de aprobar): un agente por el MCP no puede
// tocarlas.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use devtool_core::EnvKind;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::recipes::Approval;
use crate::state::Shared;

const KEY: &str = "meta:policy";
pub const KINDS: [&str; 4] = ["devbox", "throwaway", "cloud-dev", "cloud-prod"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WriteRule {
    /// Lo que pide cada receta.
    #[default]
    Recipe,
    /// Sin aprobación (no vale en producción): las recetas permitidas corren al pedirlas.
    None,
    /// Como mínimo, una aprobación en la app.
    Approval,
    /// Como mínimo, aprobar escribiendo el nombre del entorno.
    Typed,
    /// Como mínimo, Touch ID.
    Reinforced,
    /// Ninguna receta que modifique el entorno.
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Policy {
    pub read: BTreeMap<String, bool>,
    pub write: BTreeMap<String, WriteRule>,
}

impl Default for Policy {
    fn default() -> Self {
        // Como antes de que fuera configurable: producción no expone sus registros.
        let read = KINDS.iter().map(|k| (k.to_string(), *k != "cloud-prod")).collect();
        let write = KINDS.iter().map(|k| (k.to_string(), WriteRule::Recipe)).collect();
        Self { read, write }
    }
}

impl Policy {
    pub fn can_read(&self, kind: EnvKind) -> bool {
        self.read.get(kind.as_str()).copied().unwrap_or(kind != EnvKind::CloudProd)
    }

    /// La aprobación que se exige a una receta que modifica un entorno de este tipo.
    pub fn write_approval(&self, kind: &str, recipe: Approval) -> Approval {
        if recipe == Approval::Forbidden {
            return recipe;
        }
        let floor = match self.write.get(kind).copied().unwrap_or_default() {
            WriteRule::Recipe => return recipe,
            WriteRule::None => return Approval::None,
            WriteRule::Blocked => return Approval::Forbidden,
            WriteRule::Approval => Approval::Simple,
            WriteRule::Typed => Approval::Typed,
            WriteRule::Reinforced => Approval::Reinforced,
        };
        if rank(recipe) >= rank(floor) {
            recipe
        } else {
            floor
        }
    }

    pub fn is_blocked(&self, kind: &str) -> bool {
        self.write.get(kind) == Some(&WriteRule::Blocked)
    }
}

fn rank(a: Approval) -> u8 {
    match a {
        Approval::None => 0,
        Approval::Simple => 1,
        Approval::Typed => 2,
        Approval::Reinforced => 3,
        Approval::Forbidden => 4,
    }
}

pub fn load(state: &Shared) -> Policy {
    let stored = state.store.probes().ok().and_then(|l| l.into_iter().find(|(k, _)| k == KEY)).map(|(_, v)| v);
    let mut policy = Policy::default();
    if let Some(v) = stored.and_then(|v| serde_json::from_value::<Policy>(v).ok()) {
        // Lo guardado manda; los tipos que falten quedan con su valor por defecto.
        policy.read.extend(v.read);
        policy.write.extend(v.write);
    }
    policy
}

pub fn save(state: &Shared, body: &Value) -> Result<Policy> {
    let given: Policy = serde_json::from_value(body.clone()).map_err(|e| anyhow::anyhow!("invalid policy: {e}"))?;
    for k in given.read.keys().chain(given.write.keys()) {
        if !KINDS.contains(&k.as_str()) {
            bail!("unknown environment kind: {k}");
        }
    }
    if given.write.get("cloud-prod") == Some(&WriteRule::None) {
        bail!("production always needs an approval for changes");
    }
    let mut policy = Policy::default();
    policy.read.extend(given.read);
    policy.write.extend(given.write);
    state.store.save_probe(KEY, &serde_json::to_value(&policy)?)?;
    tracing::info!("data policy changed: {}", serde_json::to_string(&policy).unwrap_or_default());
    Ok(policy)
}

pub fn json(policy: &Policy) -> Value {
    json!({ "read": policy.read, "write": policy.write, "kinds": KINDS })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solo_endurece() {
        let mut p = Policy::default();
        assert!(!p.can_read(EnvKind::CloudProd));
        assert!(p.can_read(EnvKind::CloudDev));
        assert_eq!(p.write_approval("cloud-dev", Approval::None), Approval::None);
        p.write.insert("cloud-dev".into(), WriteRule::Typed);
        assert_eq!(p.write_approval("cloud-dev", Approval::Simple), Approval::Typed);
        assert_eq!(p.write_approval("cloud-dev", Approval::Reinforced), Approval::Reinforced);
        assert_eq!(p.write_approval("cloud-dev", Approval::Forbidden), Approval::Forbidden);
        p.write.insert("devbox".into(), WriteRule::Blocked);
        assert_eq!(p.write_approval("devbox", Approval::None), Approval::Forbidden);
        p.write.insert("cloud-dev".into(), WriteRule::None);
        assert_eq!(p.write_approval("cloud-dev", Approval::Simple), Approval::None);
        assert_eq!(p.write_approval("cloud-dev", Approval::Forbidden), Approval::Forbidden);
    }
}
