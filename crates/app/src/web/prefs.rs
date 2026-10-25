// Preferencias y notificaciones de la demo de la web: en memoria (se pierden al recargar).

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::Result;
use serde_json::{json, Value};

pub struct Prefs {
    values: Mutex<HashMap<String, String>>,
    notes: Mutex<Vec<Value>>,
}

impl Prefs {
    pub fn open() -> Result<Self> {
        Ok(Self::memory())
    }

    /// La demo arranca en el proyecto de ejemplo, en inglés y en oscuro.
    pub fn memory() -> Self {
        let values = HashMap::from([("project".to_string(), "my-app".to_string()), ("theme".to_string(), "dark".to_string())]);
        Self { values: Mutex::new(values), notes: Mutex::new(vec![]) }
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.values.lock().ok()?.get(key).cloned()
    }

    pub fn set(&self, key: &str, value: &str) {
        if let Ok(mut v) = self.values.lock() {
            v.insert(key.into(), value.into());
        }
    }

    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        self.get(key).map(|v| v == "1").unwrap_or(default)
    }

    pub fn set_bool(&self, key: &str, value: bool) {
        self.set(key, if value { "1" } else { "0" });
    }

    pub fn notify(&self, id: &str, kind: &str, title: &str, body: &str, at: i64) -> bool {
        let Ok(mut notes) = self.notes.lock() else { return false };
        if notes.iter().any(|n| n["id"] == id) {
            return false;
        }
        notes.insert(0, json!({ "id": id, "kind": kind, "title": title, "body": body, "at": at, "read": false }));
        true
    }

    pub fn notifications(&self) -> Vec<Value> {
        self.notes.lock().map(|n| n.clone()).unwrap_or_default()
    }

    pub fn unread(&self) -> usize {
        self.notes.lock().map(|n| n.iter().filter(|x| x["read"] == false).count()).unwrap_or(0)
    }

    pub fn mark_all_read(&self) {
        if let Ok(mut n) = self.notes.lock() {
            for x in n.iter_mut() {
                x["read"] = json!(true);
            }
        }
    }

    pub fn dismiss(&self, id: &str) {
        if let Ok(mut n) = self.notes.lock() {
            n.retain(|x| x["id"] != id);
        }
    }

    pub fn clear(&self) {
        if let Ok(mut n) = self.notes.lock() {
            n.clear();
        }
    }
}
