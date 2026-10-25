// Estado compartido del daemon: configuración, almacén, resultados de las
// sondas y el hub de telemetría de las pestañas.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde_json::{json, Value};

use crate::auth::Tokens;
use crate::config::Config;
use crate::jobs::Runner;
use crate::procs::Procs;
use crate::services::Services;
use crate::store::{now_ms, Store};
use crate::telemetry::Hub;

pub struct State {
    pub config: Config,
    pub store: Store,
    pub tokens: Tokens,
    pub started_at: i64,
    probes: RwLock<HashMap<String, Value>>,
    pub hub: Hub,
    pub runner: Runner,
    pub services: Services,
    pub procs: Procs,
    pub agent_host: crate::agent_host::AgentHost,
}

pub type Shared = Arc<State>;

impl State {
    pub fn new(config: Config, store: Store, tokens: Tokens) -> Self {
        let probes = store.probes().unwrap_or_default().into_iter().collect();
        Self {
            config,
            store,
            tokens,
            started_at: now_ms(),
            probes: RwLock::new(probes),
            hub: Hub::default(),
            runner: Runner::default(),
            services: Services::default(),
            procs: Procs::default(),
            agent_host: Default::default(),
        }
    }

    /// Guarda el resultado de una sonda: `{ key, at, ok, error, data }`.
    pub fn set_probe(&self, key: &str, ok: bool, data: Value, error: Option<String>) {
        let value = json!({ "key": key, "at": now_ms(), "ok": ok, "error": error, "data": data });
        if let Err(e) = self.store.save_probe(key, &value) {
            tracing::warn!("couldn't save probe {key}: {e}");
        }
        self.probes.write().unwrap().insert(key.to_string(), value);
    }

    pub fn probe(&self, key: &str) -> Option<Value> {
        self.probes.read().unwrap().get(key).cloned()
    }

    /// Datos de una sonda solo si la última corrida salió bien.
    pub fn probe_data(&self, key: &str) -> Option<Value> {
        self.probe(key).filter(|p| p["ok"] == true).map(|p| p["data"].clone())
    }

    pub fn probes(&self) -> Vec<Value> {
        // Las claves `meta:` (modo local, importaciones) no son sondas.
        let mut list: Vec<Value> =
            self.probes.read().unwrap().values().filter(|p| !p["key"].as_str().unwrap_or("").starts_with("meta:")).cloned().collect();
        list.sort_by(|a, b| a["key"].as_str().cmp(&b["key"].as_str()));
        list
    }
}

/// Un estado con el almacén en memoria, para las pruebas.
#[cfg(test)]
pub fn test_state() -> Shared {
    let config = Config { repo: None, port: 4790, telemetry_origins: vec![] };
    Arc::new(State::new(config, Store::memory(), Tokens::fixed("cli-token-for-tests", "app-token-for-tests")))
}
