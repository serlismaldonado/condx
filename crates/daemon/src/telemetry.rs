// Ingesta de telemetría (§6): las pestañas de la app se conectan por
// WebSocket a /telemetry, mandan su snapshot (contrato devtools/schema.json
// v1) y reciben comandos de la lista cerrada del DevTools.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use axum::extract::ws::{Message, WebSocket};
use devtool_core::preferred_tab;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

use crate::store::now_ms;

/// Los mismos comandos que acepta la página (lib/devtools/commands.ts).
pub const COMMANDS: &[&str] =
    &["snapshot", "report", "top", "errors", "live", "offline", "reset", "simulateHidden", "simulateOffline", "navigate", "reload"];
const STALE_TAB_MS: i64 = 20_000;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

struct Tab {
    conn: u64,
    at: i64,
    origin: Option<String>,
    snapshot: Value,
    tx: mpsc::UnboundedSender<String>,
}

#[derive(Default)]
pub struct Hub {
    tabs: Mutex<HashMap<String, Tab>>,
    pending: Mutex<HashMap<String, oneshot::Sender<Value>>>,
    next: AtomicU64,
}

impl Hub {
    pub fn tabs(&self) -> Vec<Value> {
        let tabs = self.tabs.lock().unwrap();
        let preferred = self.preferred_id_locked(&tabs);
        let mut list: Vec<Value> = tabs
            .iter()
            .map(|(id, t)| {
                json!({
                    "tabId": id,
                    "at": t.at,
                    "origin": t.origin,
                    "route": t.snapshot["page"]["route"],
                    "visibility": t.snapshot["page"]["visibility"],
                    "convexUrl": t.snapshot["session"]["convexUrl"],
                    "perMinute": t.snapshot["convex"]["totals"]["perMinute"],
                    "preferred": Some(id) == preferred.as_ref(),
                })
            })
            .collect();
        list.sort_by_key(|t| -t["at"].as_i64().unwrap_or(0));
        list
    }

    fn preferred_id_locked(&self, tabs: &HashMap<String, Tab>) -> Option<String> {
        preferred_tab(
            tabs.iter().map(|(id, t)| (id.as_str(), t.at, t.snapshot["page"]["visibility"] == "visible", ())),
            now_ms(),
            STALE_TAB_MS,
        )
        .map(|(id, _)| id.to_string())
    }

    fn resolve(&self, tab_id: Option<&str>) -> Result<String> {
        let tabs = self.tabs.lock().unwrap();
        match tab_id {
            Some(id) if tabs.contains_key(id) => Ok(id.to_string()),
            Some(id) => bail!("Tab {id} isn't connected (see list_tabs)"),
            None => self
                .preferred_id_locked(&tabs)
                .ok_or_else(|| anyhow!("No app tab connected to the daemon. Open the app (npm run dev or with the daemon running).")),
        }
    }

    /// Último snapshot reportado por la pestaña (sin ida y vuelta).
    pub fn snapshot(&self, tab_id: Option<&str>) -> Result<Value> {
        let id = self.resolve(tab_id)?;
        let tabs = self.tabs.lock().unwrap();
        let tab = tabs.get(&id).ok_or_else(|| anyhow!("tab disconnected"))?;
        let mut snap = tab.snapshot.clone();
        if let Some(obj) = snap.as_object_mut() {
            obj.insert("tabId".into(), json!(id));
            obj.insert("receivedAt".into(), json!(tab.at));
        }
        Ok(snap)
    }

    /// Manda un comando a la pestaña y espera su resultado.
    pub async fn command(&self, command: &str, args: Value, tab_id: Option<&str>) -> Result<Value> {
        if !COMMANDS.contains(&command) {
            bail!("Unknown command: {command}. Valid: {}", COMMANDS.join(", "));
        }
        let tab = self.resolve(tab_id)?;
        let id = format!("d{}", self.next.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), tx);
        let msg = json!({ "type": "command", "id": id, "command": command, "args": args }).to_string();
        let sent = self.tabs.lock().unwrap().get(&tab).map(|t| t.tx.send(msg).is_ok()).unwrap_or(false);
        if !sent {
            self.pending.lock().unwrap().remove(&id);
            bail!("Tab {tab} disconnected");
        }
        let result = match tokio::time::timeout(COMMAND_TIMEOUT, rx).await {
            Ok(Ok(r)) => r,
            _ => {
                self.pending.lock().unwrap().remove(&id);
                bail!("Tab {tab} didn't respond in {}s", COMMAND_TIMEOUT.as_secs());
            }
        };
        if result["ok"] == true {
            Ok(result["result"].clone())
        } else {
            bail!(result["error"].as_str().unwrap_or("Command error").to_string())
        }
    }

    fn on_message(&self, conn: u64, origin: &Option<String>, tx: &mpsc::UnboundedSender<String>, text: &str) {
        let Ok(msg) = serde_json::from_str::<Value>(text) else { return };
        match msg["type"].as_str() {
            Some("snapshot") => {
                let Some(tab_id) = msg["tabId"].as_str().filter(|s| !s.is_empty() && s.len() <= 64) else { return };
                self.tabs.lock().unwrap().insert(
                    tab_id.to_string(),
                    Tab { conn, at: now_ms(), origin: origin.clone(), snapshot: msg["snapshot"].clone(), tx: tx.clone() },
                );
            }
            Some("result") => {
                if let Some(id) = msg["id"].as_str() {
                    if let Some(waiter) = self.pending.lock().unwrap().remove(id) {
                        let _ = waiter.send(msg);
                    }
                }
            }
            _ => {}
        }
    }

    pub async fn serve(&self, socket: WebSocket, origin: Option<String>) {
        let conn = self.next.fetch_add(1, Ordering::Relaxed);
        let (mut sink, mut stream) = socket.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let writer = tokio::spawn(async move {
            while let Some(text) = rx.recv().await {
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
        });
        while let Some(Ok(msg)) = stream.next().await {
            match msg {
                // Un snapshot pesa unos KB; más de 2 MB es un error del cliente.
                Message::Text(text) if text.len() <= 2_000_000 => self.on_message(conn, &origin, &tx, text.as_str()),
                Message::Close(_) => break,
                _ => {}
            }
        }
        writer.abort();
        self.tabs.lock().unwrap().retain(|_, t| t.conn != conn);
    }
}
