//! El MCP remoto de Condx Cloud (`cloud/convex/mcp.ts`): Claude y otros agentes llegan desde cualquier
//! lugar. Cada llamada es una fila `_mcp-<id>` en el alcance de hilos con el mensaje JSON-RPC y el
//! permiso que firmó un teléfono. La Mac (o el sitio en la nube, si la Mac no late) verifica la firma
//! con sus teléfonos de confianza, corre el mensaje en el MCP local como esa conexión (su nombre y su
//! nivel: nunca aprobar, producción ni claves) y deja la respuesta en la fila.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use localfirst_core::Values;
use serde_json::{json, Value};

use crate::cloud::TrustedDevice;
use crate::state::Shared;

pub const ROW_PREFIX: &str = "_mcp-";

/// Lo que firmó el teléfono al crear el permiso (como `grantMessage` en mcp.ts).
pub fn grant_message(daemon_id: &str, g: &Value) -> String {
    format!(
        "condx-mcp-grant:v1|{daemon_id}|{}|{}|{}|{}|{}",
        g["grantId"].as_str().unwrap_or(""),
        g["name"].as_str().unwrap_or(""),
        g["level"].as_str().unwrap_or(""),
        g["codeHash"].as_str().unwrap_or(""),
        g["signedAt"].as_i64().unwrap_or(0)
    )
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    s.len().is_multiple_of(2).then(|| (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect::<Option<Vec<u8>>>()).flatten()
}

/// El permiso, si lo firmó un teléfono de confianza de este daemon (uno desvinculado deja de valer).
pub fn verify_grant(daemon_id: &str, g: &Value, devices: &[TrustedDevice]) -> Result<(), String> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let key = g["deviceKey"].as_str().ok_or("no key")?;
    devices.iter().find(|d| d.public_key == key).ok_or("the phone that created this connection isn't paired anymore")?;
    if !matches!(g["level"].as_str(), Some("read" | "reply" | "orchestrate")) {
        return Err("unknown level".into());
    }
    let sig = g["signature"].as_str().and_then(unhex).ok_or("no signature")?;
    let sig = Signature::from_slice(&sig).map_err(|_| "bad signature")?;
    let bytes: [u8; 32] = unhex(key).and_then(|b| b.try_into().ok()).ok_or("bad key")?;
    let vk = VerifyingKey::from_bytes(&bytes).map_err(|_| "bad key")?;
    vk.verify(grant_message(daemon_id, g).as_bytes(), &sig).map_err(|_| "the connection's signature doesn't match".to_string())
}

/// Las llamadas que esta sesión ya tomó y las respuestas que esperan escribirse (fila, llamada, JSON).
#[derive(Default)]
pub struct Calls {
    taken: HashSet<String>,
    done: Arc<Mutex<Vec<(String, String, String)>>>,
}

impl Calls {
    /// Las llamadas nuevas de las filas: cada una corre aparte (una herramienta puede tardar) y su
    /// respuesta queda para `drain` (y `wake` despierta a la sesión para escribirla enseguida).
    pub fn start(&mut self, state: &Shared, rt: &tokio::runtime::Handle, daemon_id: &str, rows: &[(String, Values)], devices: &[TrustedDevice], wake: Option<Arc<dyn Fn() + Send + Sync>>) {
        for (id, v) in rows {
            if !id.starts_with(ROW_PREFIX) {
                continue;
            }
            let Some(call_id) = v.get("mcpId").and_then(Value::as_str) else { continue };
            let replied = [v.get("resultFor"), v.get("siteFor")].iter().any(|f| f.and_then(|x| x.as_str()) == Some(call_id));
            if replied || !self.taken.insert(call_id.to_string()) {
                continue;
            }
            let grant: Value = v.get("mcpGrant").and_then(Value::as_str).and_then(|t| serde_json::from_str(t).ok()).unwrap_or(Value::Null);
            let msg: Value = v.get("mcpMsg").and_then(Value::as_str).and_then(|t| serde_json::from_str(t).ok()).unwrap_or(Value::Null);
            let (state, done, row, call, daemon_id, wake) = (state.clone(), self.done.clone(), id.clone(), call_id.to_string(), daemon_id.to_string(), wake.clone());
            let verified = verify_grant(&daemon_id, &grant, devices);
            rt.spawn(async move {
                let reply = match verified {
                    Err(e) => json!({ "error": { "code": -32002, "message": format!("Condx refused this connection: {e}") } }),
                    Ok(()) => {
                        let client = crate::connections::remote(grant["grantId"].as_str().unwrap_or(""), grant["name"].as_str().unwrap_or("Agent"), grant["level"].as_str().unwrap_or("read"));
                        tracing::info!("remote mcp: {} from {}", msg["method"].as_str().unwrap_or("?"), grant["name"].as_str().unwrap_or("?"));
                        match crate::mcp::handle(&state, &client, &msg).await {
                            Some(r) => json!({ "result": r.get("result").cloned(), "error": r.get("error").cloned() }),
                            None => json!({ "result": {} }),
                        }
                    }
                };
                let reply = if reply["error"].is_null() { json!({ "result": reply["result"] }) } else { json!({ "error": reply["error"] }) };
                done.lock().unwrap().push((row, call, reply.to_string()));
                if let Some(w) = wake {
                    w();
                }
            });
        }
        // Lo tomado que ya no está (contestado y borrado): se olvida.
        let live: HashSet<&str> = rows.iter().filter_map(|(_, v)| v.get("mcpId").and_then(Value::as_str)).collect();
        self.taken.retain(|c| live.contains(c.as_str()) || self.done.lock().unwrap().iter().any(|(_, d, _)| d == c));
    }

    /// Las respuestas listas: (fila, llamada, respuesta JSON).
    pub fn drain(&self) -> Vec<(String, String, String)> {
        std::mem::take(&mut *self.done.lock().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn verifica_el_permiso_firmado_por_un_telefono_de_confianza() {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let key: String = sk.verifying_key().as_bytes().iter().map(|b| format!("{b:02x}")).collect();
        let mut g = json!({ "grantId": "ab12", "name": "Claude", "level": "orchestrate", "codeHash": "c".repeat(64), "signedAt": 1000, "deviceKey": key });
        let sig: String = sk.sign(grant_message("daemon-x", &g).as_bytes()).to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        g["signature"] = json!(sig);
        let phone = TrustedDevice { device_id: "phone-1".into(), name: "Pixel".into(), public_key: key.clone(), paired_at: 0, encryption_key: None, kind: Default::default() };
        assert_eq!(verify_grant("daemon-x", &g, std::slice::from_ref(&phone)), Ok(()));
        // Otro daemon, otro nivel o un teléfono que ya no está: no vale.
        assert!(verify_grant("daemon-y", &g, std::slice::from_ref(&phone)).is_err());
        let mut more = g.clone();
        more["level"] = json!("read");
        assert!(verify_grant("daemon-x", &more, std::slice::from_ref(&phone)).is_err());
        assert!(verify_grant("daemon-x", &g, &[]).is_err());
    }
}
