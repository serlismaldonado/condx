// Preferencias de la app y su centro de notificaciones, en SQLite
// (~/.condx/app/app.sqlite3). Son de esta Mac y de la interfaz: el
// estado del devtool sigue en el daemon.

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS prefs (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS notifications (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  title TEXT NOT NULL,
  body TEXT NOT NULL,
  at INTEGER NOT NULL,
  read INTEGER NOT NULL DEFAULT 0
);
";

/// Cuántas notificaciones se guardan como máximo.
const KEEP: usize = 200;

pub struct Prefs {
    conn: Mutex<Connection>,
}

fn path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    if let Err(e) = devtool_core::paths::migrate(&home, devtool_core::paths::APP) {
        eprintln!("couldn't move ~/.heskala/devtool to ~/.condx/app: {e}");
    }
    devtool_core::paths::resolve(&home, devtool_core::paths::APP).join("app.sqlite3")
}

impl Prefs {
    pub fn open() -> Result<Self> {
        let path = path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Self::at(Connection::open(path)?)
    }

    /// En memoria: si el archivo no abre, la app sigue sin recordar nada.
    pub fn memory() -> Self {
        Self::at(Connection::open_in_memory().expect("in-memory sqlite")).expect("in-memory schema")
    }

    fn at(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn get(&self, key: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT value FROM prefs WHERE key = ?1", [key], |r| r.get(0)).optional().ok().flatten()
    }

    pub fn set(&self, key: &str, value: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute("INSERT INTO prefs (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2", params![key, value]);
    }

    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        self.get(key).map(|v| v == "1").unwrap_or(default)
    }

    pub fn set_bool(&self, key: &str, value: bool) {
        self.set(key, if value { "1" } else { "0" });
    }

    /// Agrega una notificación; `false` si ese id ya estaba (no se repite).
    pub fn notify(&self, id: &str, kind: &str, title: &str, body: &str, at: i64) -> bool {
        let conn = self.conn.lock().unwrap();
        let added = conn
            .execute("INSERT OR IGNORE INTO notifications (id, kind, title, body, at) VALUES (?1, ?2, ?3, ?4, ?5)", params![id, kind, title, body, at])
            .map(|n| n > 0)
            .unwrap_or(false);
        if added {
            let _ = conn.execute(
                "DELETE FROM notifications WHERE id NOT IN (SELECT id FROM notifications ORDER BY at DESC LIMIT ?1)",
                [KEEP as i64],
            );
        }
        added
    }

    pub fn notifications(&self) -> Vec<Value> {
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) = conn.prepare("SELECT id, kind, title, body, at, read FROM notifications ORDER BY at DESC LIMIT 100") else {
            return vec![];
        };
        stmt.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "kind": r.get::<_, String>(1)?,
                "title": r.get::<_, String>(2)?,
                "body": r.get::<_, String>(3)?,
                "at": r.get::<_, i64>(4)?,
                "read": r.get::<_, i64>(5)? != 0,
            }))
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    pub fn unread(&self) -> usize {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM notifications WHERE read = 0", [], |r| r.get::<_, i64>(0)).unwrap_or(0) as usize
    }

    pub fn mark_all_read(&self) {
        let _ = self.conn.lock().unwrap().execute("UPDATE notifications SET read = 1", []);
    }

    pub fn dismiss(&self, id: &str) {
        let _ = self.conn.lock().unwrap().execute("DELETE FROM notifications WHERE id = ?1", [id]);
    }

    pub fn clear(&self) {
        let _ = self.conn.lock().unwrap().execute("DELETE FROM notifications", []);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn guarda_preferencias_y_notificaciones() {
        let p = super::Prefs::memory();
        assert_eq!(p.get("x"), None);
        p.set_bool("sidebar.collapsed", true);
        assert!(p.get_bool("sidebar.collapsed", false));
        assert!(p.notify("a", "error", "Falló", "detalle", 1));
        assert!(!p.notify("a", "error", "Falló", "detalle", 2));
        assert_eq!(p.unread(), 1);
        p.mark_all_read();
        assert_eq!(p.unread(), 0);
        p.dismiss("a");
        assert!(p.notifications().is_empty());
    }
}
