// Agentes externos conectados por el MCP (Claude Code, Codex, Cursor…), cada uno con su token y su
// nivel (docs/design/agent-flows.md, fase 5 de los chats):
//   read         leer los hilos
//   reply        además, contestar en los hilos donde lo mencionan (@su-nombre)
//   orchestrate  además, escribir en cualquier hilo, llamar a los agentes de Condx, arrancar chats y
//                tareas, y arrancar, cancelar y reintentar flujos
// Aparte: contestar las preguntas de los flujos (con orchestrate, encendido por defecto) y cambiar
// flujos y agentes (apagado). Nunca: aprobar, producción, claves. Se guarda el SHA-256 del token
// (el token se ve una vez); cuándo se lo vio por última vez dice si está conectado.

use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::auth::{Client, Scope};
use crate::store::now_ms;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub id: String,
    pub name: String,
    /// `claude-code`, `codex`, `cursor`, `gemini-cli` u `other` (de ahí el logo).
    pub kind: String,
    pub level: String,
    #[serde(default)]
    pub answer_questions: bool,
    #[serde(default)]
    pub edit_flows: bool,
    pub token_hash: String,
    pub created_at: i64,
    #[serde(default)]
    pub last_seen: i64,
    /// Hasta qué nota leyó sus menciones.
    #[serde(default)]
    pub cursor: i64,
}

impl Connection {
    /// Cómo se lo menciona: `@claude-code`.
    pub fn handle(&self) -> String {
        handle_of(&self.name)
    }

    pub fn logo(&self) -> &'static str {
        match self.kind.as_str() {
            "claude-code" => "claude",
            "codex" => "openai",
            "gemini-cli" => "gemini",
            _ => "other",
        }
    }

    pub fn online(&self) -> bool {
        now_ms() - self.last_seen < 120_000
    }

    pub fn json(&self) -> Value {
        json!({ "id": self.id, "name": self.name, "handle": self.handle(), "kind": self.kind, "logo": self.logo(), "level": self.level, "answerQuestions": self.answer_questions, "editFlows": self.edit_flows, "createdAt": self.created_at, "lastSeen": self.last_seen, "online": self.online() })
    }
}

pub fn handle_of(name: &str) -> String {
    let h: String = name.trim().to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    h.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
}

static CACHE: Mutex<Option<Vec<Connection>>> = Mutex::new(None);

fn file() -> std::path::PathBuf {
    crate::config::home().join("connections.json")
}

pub fn list() -> Vec<Connection> {
    let mut c = CACHE.lock().unwrap();
    if c.is_none() {
        *c = Some(std::fs::read_to_string(file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default());
    }
    c.clone().unwrap_or_default()
}

fn save(list: &[Connection]) -> Result<()> {
    let path = file();
    std::fs::write(&path, serde_json::to_string_pretty(list)?)?;
    crate::auth::restrict(&path)?;
    *CACHE.lock().unwrap() = Some(list.to_vec());
    Ok(())
}

fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Una conexión nueva: devuelve la conexión y su token (se ve una sola vez).
pub fn create(state: &crate::state::Shared, name: &str, kind: &str, level: &str, answer_questions: Option<bool>, edit_flows: bool) -> Result<(Connection, String)> {
    let name = name.trim();
    if name.is_empty() || name.len() > 40 {
        bail!("give it a name (at most 40 characters)");
    }
    if !matches!(level, "read" | "reply" | "orchestrate") {
        bail!("level must be read, reply or orchestrate");
    }
    let mut all = list();
    if all.iter().any(|c| c.handle() == handle_of(name)) {
        bail!("there's already an agent called @{}", handle_of(name));
    }
    if crate::roles::list(state).iter().any(|r| r.id == handle_of(name) || handle_of(&r.name) == handle_of(name)) {
        bail!("@{} is an agent of Condx's catalog: pick another name", handle_of(name));
    }
    let token = format!("cx_ext_{}", crate::auth::random_token()?);
    let c = Connection {
        id: format!("conn-{:x}", now_ms()),
        name: name.to_string(),
        kind: kind.to_string(),
        level: level.to_string(),
        answer_questions: answer_questions.unwrap_or(level == "orchestrate") && level == "orchestrate",
        edit_flows: edit_flows && level == "orchestrate",
        token_hash: hash(&token),
        created_at: now_ms(),
        last_seen: 0,
        cursor: 0,
    };
    all.push(c.clone());
    save(&all)?;
    Ok((c, token))
}

pub fn revoke(id: &str) -> Result<()> {
    let mut all = list();
    let before = all.len();
    all.retain(|c| c.id != id);
    if all.len() == before {
        bail!("there's no connection {id}");
    }
    save(&all)
}

pub fn by_handle(handle: &str) -> Option<Connection> {
    list().into_iter().find(|c| c.handle() == handle.to_lowercase())
}

fn update(id: &str, f: impl FnOnce(&mut Connection)) {
    let mut all = list();
    if let Some(c) = all.iter_mut().find(|c| c.id == id) {
        f(c);
        let _ = save(&all);
    }
}

/// Los alcances de su nivel (y sus opciones).
fn scopes(c: &Connection) -> &'static [Scope] {
    use Scope::*;
    match (c.level.as_str(), c.answer_questions, c.edit_flows) {
        ("read", _, _) => &[Read, ThreadsRead],
        ("reply", _, _) => &[Read, ThreadsRead, ThreadsWrite],
        (_, true, true) => &[Read, ThreadsRead, ThreadsWrite, Orchestrate, JobsRequest, AnswerQuestions, EditFlows],
        (_, true, false) => &[Read, ThreadsRead, ThreadsWrite, Orchestrate, JobsRequest, AnswerQuestions],
        (_, false, true) => &[Read, ThreadsRead, ThreadsWrite, Orchestrate, JobsRequest, EditFlows],
        (_, false, false) => &[Read, ThreadsRead, ThreadsWrite, Orchestrate, JobsRequest],
    }
}

/// El cliente de un token de una conexión (y anota que se lo vio).
pub fn resolve(token: &str) -> Option<Client> {
    if !token.starts_with("cx_ext_") {
        return None;
    }
    let h = hash(token);
    let c = list().into_iter().find(|c| c.token_hash == h)?;
    if now_ms() - c.last_seen > 15_000 {
        update(&c.id, |x| x.last_seen = now_ms());
    }
    Some(Client { name: "external", scopes: scopes(&c), conn: Some(Arc::new(c)) })
}

/// Un agente que llega por el MCP remoto de Condx Cloud (`remote_mcp.rs`), con un permiso que firmó un
/// teléfono: queda como una conexión más (`mcp-<permiso>`, sin token local que la abra), así lo que
/// escribe sale con su nombre y sus menciones le llegan. Con el mismo nombre que otra, se distingue.
pub fn remote(grant_id: &str, name: &str, level: &str) -> Client {
    let id = format!("mcp-{grant_id}");
    let mut all = list();
    let c = match all.iter_mut().find(|c| c.id == id) {
        Some(c) => {
            if now_ms() - c.last_seen > 15_000 {
                c.last_seen = now_ms();
                let snapshot = c.clone();
                let _ = save(&all);
                snapshot
            } else {
                c.clone()
            }
        }
        None => {
            let mut name = name.trim().to_string();
            if all.iter().any(|c| c.handle() == handle_of(&name)) {
                name = format!("{name} (cloud)");
            }
            let kind = if name.to_lowercase().contains("claude") { "claude-code" } else { "other" };
            let c = Connection { id: id.clone(), name, kind: kind.into(), level: level.into(), answer_questions: level == "orchestrate", edit_flows: false, token_hash: "remote".into(), created_at: now_ms(), last_seen: now_ms(), cursor: 0 };
            all.push(c.clone());
            let _ = save(&all);
            c
        }
    };
    Client { name: "external", scopes: scopes(&c), conn: Some(Arc::new(c)) }
}

/// Alguien lo mencionó en un hilo: queda para cuando pregunte (`wait_for_mention`).
pub fn mention(state: &crate::state::Shared, c: &Connection, thread: &str, text: &str, by: &str) -> Result<i64> {
    state.store.save_note(thread, now_ms(), &json!({ "kind": "user", "to": c.name, "toConnection": c.id, "text": text, "by": by }))
}

/// Las menciones nuevas para esta conexión (y avanza su cursor).
pub fn take_mentions(state: &crate::state::Shared, c: &Connection) -> Result<Vec<Value>> {
    let fresh = list().into_iter().find(|x| x.id == c.id).context("the connection was revoked")?;
    let notes = state.store.notes_after(fresh.cursor)?;
    let last = notes.last().map(|(id, _, _)| *id).unwrap_or(fresh.cursor);
    let mine: Vec<Value> = notes.into_iter().filter(|(_, _, v)| v["toConnection"] == c.id.as_str()).map(|(id, t, v)| json!({ "note": id, "thread": t, "text": v["text"], "by": v["by"] })).collect();
    if last != fresh.cursor {
        update(&c.id, |x| x.cursor = last);
    }
    Ok(mine)
}

/// Si lo mencionaron en ese hilo (lo que permite contestar con el nivel «reply»).
pub fn mentioned_in(state: &crate::state::Shared, c: &Connection, thread: &str) -> bool {
    state.store.notes(thread).unwrap_or_default().iter().any(|n| n["toConnection"] == c.id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nombres_y_alcances() {
        assert_eq!(handle_of("Claude Code"), "claude-code");
        assert_eq!(handle_of("  Mi Agente!! 2 "), "mi-agente-2");
        let mut c = Connection { id: "c".into(), name: "Claude Code".into(), kind: "claude-code".into(), level: "reply".into(), answer_questions: false, edit_flows: false, token_hash: String::new(), created_at: 0, last_seen: 0, cursor: 0 };
        assert!(!scopes(&c).contains(&Scope::Orchestrate) && scopes(&c).contains(&Scope::ThreadsWrite));
        c.level = "orchestrate".into();
        c.answer_questions = true;
        assert!(scopes(&c).contains(&Scope::AnswerQuestions) && !scopes(&c).contains(&Scope::EditFlows));
        assert!(!scopes(&c).contains(&Scope::JobsApprove));
        assert_eq!(c.logo(), "claude");
    }
}
