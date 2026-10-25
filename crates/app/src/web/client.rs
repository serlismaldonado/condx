// El "daemon" de la demo de la web: las mismas llamadas que el cliente HTTP del escritorio, pero
// responden con datos de ejemplo (demo.rs). No sale nada del navegador.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::Value;

#[path = "demo.rs"]
mod demo;

#[derive(Clone)]
pub struct Daemon {
    pub base: String,
    pub repo: Option<PathBuf>,
}

pub fn home() -> PathBuf {
    PathBuf::from("~/.condx/daemon")
}

impl Daemon {
    pub fn from_home() -> Result<Self> {
        Ok(Self { base: "http://127.0.0.1:4790".into(), repo: None })
    }

    pub fn get(&self, path: &str) -> Result<Value> {
        demo::get(path)
    }

    pub fn post(&self, path: &str, body: Value) -> Result<Value> {
        demo::post(path, body)
    }

    pub fn delete(&self, path: &str) -> Result<Value> {
        demo::delete(path)
    }

    pub fn tool(&self, name: &str, args: Value) -> Result<Value> {
        demo::tool(name, args)
    }
}
