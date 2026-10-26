// Almacén del daemon (SQLite en ~/.condx/daemon/devd.sqlite3): entornos,
// último resultado de cada sonda e historial de despliegues (§2, §4.3).

use std::path::Path;
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use devtool_core::{Credential, EnvKind, Environment};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

pub struct Store {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Deployment {
    pub env: String,
    pub commit: String,
    pub by: String,
    pub at: i64,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS environments (
  name TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  label TEXT,
  convex_url TEXT,
  convex_site_url TEXT,
  convex_deployment TEXT,
  frontend_url TEXT,
  branch TEXT,
  credential TEXT,
  created_at INTEGER NOT NULL,
  project TEXT
);
CREATE TABLE IF NOT EXISTS probes (
  key TEXT PRIMARY KEY,
  data TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS deployments (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  env TEXT NOT NULL,
  commit_sha TEXT NOT NULL,
  by TEXT NOT NULL,
  at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS deployments_by_env ON deployments (env, at);
CREATE TABLE IF NOT EXISTS jobs (
  id TEXT PRIMARY KEY,
  created_at INTEGER NOT NULL,
  status TEXT NOT NULL,
  data TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS jobs_by_status ON jobs (status, created_at);
CREATE TABLE IF NOT EXISTS backups (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  env TEXT NOT NULL,
  path TEXT NOT NULL,
  encrypted INTEGER NOT NULL,
  size INTEGER NOT NULL,
  commit_sha TEXT,
  job TEXT,
  at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS projects (
  name TEXT PRIMARY KEY,
  data TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS rehearsals (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_env TEXT NOT NULL,
  backup_id INTEGER NOT NULL,
  commit_sha TEXT NOT NULL,
  ok INTEGER NOT NULL,
  job TEXT NOT NULL,
  at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS workflows (
  id TEXT PRIMARY KEY,
  project TEXT NOT NULL,
  data TEXT NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS workflow_runs (
  id TEXT PRIMARY KEY,
  workflow TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  status TEXT NOT NULL,
  data TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS workflow_runs_by_workflow ON workflow_runs (workflow, created_at);
CREATE TABLE IF NOT EXISTS incidents (
  id TEXT PRIMARY KEY,
  fingerprint TEXT NOT NULL UNIQUE,
  status TEXT NOT NULL,
  last_seen INTEGER NOT NULL,
  data TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS incidents_by_status ON incidents (status, last_seen);
CREATE TABLE IF NOT EXISTS agent_providers (
  id TEXT PRIMARY KEY,
  data TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS agent_runs (
  id TEXT PRIMARY KEY,
  incident TEXT NOT NULL,
  status TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  data TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS agent_runs_by_incident ON agent_runs (incident, started_at);
CREATE TABLE IF NOT EXISTS events (
  id TEXT PRIMARY KEY,
  fingerprint TEXT NOT NULL,
  status TEXT NOT NULL,
  last_at INTEGER NOT NULL,
  data TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_by_fingerprint ON events (fingerprint, last_at);
CREATE TABLE IF NOT EXISTS thread_notes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  thread TEXT NOT NULL,
  at INTEGER NOT NULL,
  data TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS thread_notes_by_thread ON thread_notes (thread, id);
CREATE INDEX IF NOT EXISTS events_by_status ON events (status, last_at);
";

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("couldn't open {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        // Bases de antes de los proyectos: la columna falta (el error de «ya existe» se ignora).
        let _ = conn.execute("ALTER TABLE environments ADD COLUMN project TEXT", []);
        Ok(Self { conn: Mutex::new(conn) })
    }

    #[cfg(test)]
    pub fn memory() -> Self {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        Self { conn: Mutex::new(conn) }
    }

    fn row_to_env(row: &rusqlite::Row) -> rusqlite::Result<Environment> {
        let kind: String = row.get("kind")?;
        let credential: Option<String> = row.get("credential")?;
        Ok(Environment {
            name: row.get("name")?,
            kind: EnvKind::parse(&kind).unwrap_or(EnvKind::Devbox),
            label: row.get("label")?,
            convex_url: row.get("convex_url")?,
            convex_site_url: row.get("convex_site_url")?,
            convex_deployment: row.get("convex_deployment")?,
            frontend_url: row.get("frontend_url")?,
            branch: row.get("branch")?,
            credential: credential.and_then(|c| serde_json::from_str::<Credential>(&c).ok()),
            created_at: row.get("created_at")?,
            project: row.get("project")?,
        })
    }

    pub fn environments(&self) -> Result<Vec<Environment>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT * FROM environments ORDER BY CASE kind WHEN 'devbox' THEN 0 WHEN 'throwaway' THEN 1 WHEN 'cloud-dev' THEN 2 ELSE 3 END, name",
        )?;
        let rows = stmt.query_map([], Self::row_to_env)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn environment(&self, name: &str) -> Result<Option<Environment>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row("SELECT * FROM environments WHERE name = ?1", [name], Self::row_to_env)
            .optional()?)
    }

    pub fn upsert_environment(&self, env: &Environment) -> Result<()> {
        if env.name.trim().is_empty() {
            bail!("el entorno necesita nombre");
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO environments (name, kind, label, convex_url, convex_site_url, convex_deployment, frontend_url, branch, credential, created_at, project)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(name) DO UPDATE SET kind = ?2, label = ?3, convex_url = ?4, convex_site_url = ?5,
               convex_deployment = ?6, frontend_url = ?7, branch = ?8, credential = ?9, project = ?11",
            params![
                env.name,
                env.kind.as_str(),
                env.label,
                env.convex_url,
                env.convex_site_url,
                env.convex_deployment,
                env.frontend_url,
                env.branch,
                env.credential.as_ref().map(|c| serde_json::to_string(c).unwrap()),
                env.created_at,
                env.project,
            ],
        )?;
        Ok(())
    }

    pub fn remove_environment(&self, name: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute("DELETE FROM environments WHERE name = ?1", [name])? > 0)
    }

    pub fn save_probe(&self, key: &str, data: &Value) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO probes (key, data) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET data = ?2",
            params![key, data.to_string()],
        )?;
        Ok(())
    }

    pub fn probes(&self) -> Result<Vec<(String, Value)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT key, data FROM probes")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .filter_map(|r| r.ok())
            .filter_map(|(k, d)| serde_json::from_str(&d).ok().map(|v| (k, v)))
            .collect();
        Ok(rows)
    }

    pub fn last_deployment(&self, env: &str) -> Result<Option<Deployment>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT env, commit_sha, by, at FROM deployments WHERE env = ?1 ORDER BY at DESC LIMIT 1",
                [env],
                |r| Ok(Deployment { env: r.get(0)?, commit: r.get(1)?, by: r.get(2)?, at: r.get(3)? }),
            )
            .optional()?)
    }

    pub fn add_deployment(&self, env: &str, commit: &str, by: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO deployments (env, commit_sha, by, at) VALUES (?1, ?2, ?3, ?4)",
            params![env, commit, by, now_ms()],
        )?;
        Ok(())
    }

    pub fn save_job(&self, id: &str, created_at: i64, status: &str, data: &Value) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO jobs (id, created_at, status, data) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET status = ?3, data = ?4",
            params![id, created_at, status, data.to_string()],
        )?;
        Ok(())
    }

    pub fn job(&self, id: &str) -> Result<Option<Value>> {
        let conn = self.conn.lock().unwrap();
        let raw: Option<String> =
            conn.query_row("SELECT data FROM jobs WHERE id = ?1", [id], |r| r.get(0)).optional()?;
        Ok(raw.and_then(|r| serde_json::from_str(&r).ok()))
    }

    /// Trabajos más recientes primero; `statuses` vacío = todos.
    pub fn jobs(&self, statuses: &[&str], limit: usize) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT status, data FROM jobs ORDER BY created_at DESC LIMIT ?1")?;
        let rows = stmt
            .query_map([if statuses.is_empty() { limit as i64 } else { 1000 }], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .filter(|(s, _)| statuses.is_empty() || statuses.contains(&s.as_str()))
            .filter_map(|(_, d)| serde_json::from_str(&d).ok())
            .take(limit)
            .collect();
        Ok(rows)
    }

    /// Incidentes (incidents.rs): cada uno es un JSON con `id`, `fingerprint`, `status`, `lastSeen`.
    pub fn save_incident(&self, incident: &Value) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO incidents (id, fingerprint, status, last_seen, data) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET fingerprint = ?2, status = ?3, last_seen = ?4, data = ?5",
            params![
                incident["id"].as_str().unwrap_or_default(),
                incident["fingerprint"].as_str().unwrap_or_default(),
                incident["status"].as_str().unwrap_or_default(),
                incident["lastSeen"].as_i64().unwrap_or(0),
                incident.to_string()
            ],
        )?;
        Ok(())
    }

    pub fn incident(&self, id: &str) -> Result<Option<Value>> {
        self.incident_where("id", id)
    }

    pub fn incident_by_fingerprint(&self, fingerprint: &str) -> Result<Option<Value>> {
        self.incident_where("fingerprint", fingerprint)
    }

    fn incident_where(&self, column: &str, value: &str) -> Result<Option<Value>> {
        let conn = self.conn.lock().unwrap();
        let data: Option<String> =
            conn.query_row(&format!("SELECT data FROM incidents WHERE {column} = ?1"), [value], |r| r.get(0)).optional()?;
        Ok(data.and_then(|d| serde_json::from_str(&d).ok()))
    }

    /// Los incidentes con alguno de esos estados (todos si está vacío), el más reciente primero.
    pub fn incidents(&self, statuses: &[&str], limit: usize) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT status, data FROM incidents ORDER BY last_seen DESC")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .filter_map(|r| r.ok())
            .filter(|(s, _)| statuses.is_empty() || statuses.contains(&s.as_str()))
            .filter_map(|(_, d)| serde_json::from_str(&d).ok())
            .take(limit)
            .collect();
        Ok(rows)
    }

    /// Proveedores de modelos de los agentes (agents.rs), con la clave cifrada.
    pub fn save_agent_provider(&self, provider: &Value) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO agent_providers (id, data) VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET data = ?2",
            params![provider["id"].as_str().unwrap_or_default(), provider.to_string()],
        )?;
        Ok(())
    }

    pub fn agent_providers(&self) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT data FROM agent_providers ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?.filter_map(|r| r.ok()).filter_map(|d| serde_json::from_str(&d).ok()).collect();
        Ok(rows)
    }

    /// Eventos que disparan flujos (events.rs).
    /// Una nota de un hilo (lo que escribe un agente externo, o una mención a él); devuelve su id.
    pub fn save_note(&self, thread: &str, at: i64, data: &Value) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute("INSERT INTO thread_notes (thread, at, data) VALUES (?1, ?2, ?3)", params![thread, at, data.to_string()])?;
        Ok(conn.last_insert_rowid())
    }

    /// Las notas de un hilo, en orden.
    pub fn notes(&self, thread: &str) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare("SELECT id, at, data FROM thread_notes WHERE thread = ?1 ORDER BY id")?;
        let rows = st.query_map(params![thread], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))?;
        Ok(rows.filter_map(|r| r.ok()).filter_map(|(id, at, d)| serde_json::from_str::<Value>(&d).ok().map(|mut v| { v["id"] = serde_json::json!(id); v["at"] = serde_json::json!(at); v })).collect())
    }

    /// Las notas después de `after` (todas los hilos), para esperar menciones.
    pub fn notes_after(&self, after: i64) -> Result<Vec<(i64, String, Value)>> {
        let conn = self.conn.lock().unwrap();
        let mut st = conn.prepare("SELECT id, thread, data FROM thread_notes WHERE id > ?1 ORDER BY id LIMIT 200")?;
        let rows = st.query_map(params![after], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
        Ok(rows.filter_map(|r| r.ok()).filter_map(|(id, t, d)| serde_json::from_str::<Value>(&d).ok().map(|v| (id, t, v))).collect())
    }

    pub fn save_event(&self, ev: &Value) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO events (id, fingerprint, status, last_at, data) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET fingerprint = ?2, status = ?3, last_at = ?4, data = ?5",
            params![ev["id"].as_str().unwrap_or_default(), ev["fingerprint"].as_str().unwrap_or_default(), ev["status"].as_str().unwrap_or_default(), ev["lastAt"].as_i64().unwrap_or(0), ev.to_string()],
        )?;
        Ok(())
    }

    pub fn events(&self, statuses: &[&str], limit: usize) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT status, data FROM events ORDER BY last_at DESC LIMIT 1000")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .filter_map(|r| r.ok())
            .filter(|(st, _)| statuses.is_empty() || statuses.contains(&st.as_str()))
            .filter_map(|(_, d)| serde_json::from_str(&d).ok())
            .take(limit)
            .collect();
        Ok(rows)
    }

    pub fn event(&self, id: &str) -> Result<Option<Value>> {
        let conn = self.conn.lock().unwrap();
        let data: Option<String> = conn.query_row("SELECT data FROM events WHERE id = ?1", [id], |r| r.get(0)).optional()?;
        Ok(data.and_then(|d| serde_json::from_str(&d).ok()))
    }

    /// El evento más reciente con esa huella.
    pub fn event_by_fingerprint(&self, fingerprint: &str) -> Result<Option<Value>> {
        let conn = self.conn.lock().unwrap();
        let data: Option<String> = conn.query_row("SELECT data FROM events WHERE fingerprint = ?1 ORDER BY last_at DESC LIMIT 1", [fingerprint], |r| r.get(0)).optional()?;
        Ok(data.and_then(|d| serde_json::from_str(&d).ok()))
    }

    /// Corridas de agentes (agent_host.rs).
    pub fn save_agent_run(&self, run: &Value) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO agent_runs (id, incident, status, started_at, data) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET status = ?3, data = ?5",
            params![
                run["id"].as_str().unwrap_or_default(),
                run["incident"].as_str().unwrap_or_default(),
                run["status"].as_str().unwrap_or_default(),
                run["startedAt"].as_i64().unwrap_or(0),
                run.to_string()
            ],
        )?;
        Ok(())
    }

    pub fn agent_run(&self, id: &str) -> Result<Option<Value>> {
        let conn = self.conn.lock().unwrap();
        let data: Option<String> = conn.query_row("SELECT data FROM agent_runs WHERE id = ?1", [id], |r| r.get(0)).optional()?;
        Ok(data.and_then(|d| serde_json::from_str(&d).ok()))
    }

    /// Corridas con alguno de esos estados (todas si está vacío), la más reciente primero.
    pub fn agent_runs(&self, statuses: &[&str], incident: Option<&str>) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT status, incident, data FROM agent_runs ORDER BY started_at DESC LIMIT 500")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
            .filter_map(|r| r.ok())
            .filter(|(s, i, _)| (statuses.is_empty() || statuses.contains(&s.as_str())) && incident.is_none_or(|x| x == i))
            .filter_map(|(_, _, d)| serde_json::from_str(&d).ok())
            .collect();
        Ok(rows)
    }

    pub fn delete_agent_provider(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute("DELETE FROM agent_providers WHERE id = ?1", [id])? > 0)
    }

    pub fn add_backup(&self, env: &str, path: &str, encrypted: bool, size: i64, commit: Option<&str>, job: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO backups (env, path, encrypted, size, commit_sha, job, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![env, path, encrypted, size, commit, job, now_ms()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    fn row_to_backup(r: &rusqlite::Row) -> rusqlite::Result<Value> {
        Ok(serde_json::json!({
            "id": r.get::<_, i64>(0)?,
            "env": r.get::<_, String>(1)?,
            "path": r.get::<_, String>(2)?,
            "encrypted": r.get::<_, bool>(3)?,
            "size": r.get::<_, i64>(4)?,
            "commit": r.get::<_, Option<String>>(5)?,
            "job": r.get::<_, Option<String>>(6)?,
            "at": r.get::<_, i64>(7)?,
        }))
    }

    /// Respaldos que siguen en disco, más recientes primero.
    pub fn backups(&self, env: Option<&str>, limit: usize) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, env, path, encrypted, size, commit_sha, job, at FROM backups WHERE (?1 IS NULL OR env = ?1) ORDER BY at DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![env, limit as i64], Self::row_to_backup)?
            .filter_map(|r| r.ok())
            .filter(|b| std::path::Path::new(b["path"].as_str().unwrap_or_default()).exists())
            .collect();
        Ok(rows)
    }

    pub fn backup(&self, id: i64) -> Result<Option<Value>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row("SELECT id, env, path, encrypted, size, commit_sha, job, at FROM backups WHERE id = ?1", [id], Self::row_to_backup)
            .optional()?)
    }

    /// Respaldos cifrados más viejos que `older_than` (para la retención).
    pub fn expired_backups(&self, older_than: i64) -> Result<Vec<(i64, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id, path FROM backups WHERE encrypted = 1 AND at < ?1")?;
        let rows = stmt.query_map([older_than], |r| Ok((r.get(0)?, r.get(1)?)))?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    pub fn forget_backup(&self, id: i64) -> Result<()> {
        self.conn.lock().unwrap().execute("DELETE FROM backups WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn add_rehearsal(&self, source: &str, backup_id: i64, commit: &str, ok: bool, job: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO rehearsals (source_env, backup_id, commit_sha, ok, job, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![source, backup_id, commit, ok, job, now_ms()],
        )?;
        Ok(())
    }

    /// Último ensayo exitoso de `commit` con datos de `source`.
    pub fn rehearsal(&self, source: &str, commit: &str) -> Result<Option<Value>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT backup_id, job, at FROM rehearsals WHERE source_env = ?1 AND commit_sha = ?2 AND ok = 1 ORDER BY at DESC LIMIT 1",
                params![source, commit],
                |r| Ok(serde_json::json!({ "backupId": r.get::<_, i64>(0)?, "job": r.get::<_, String>(1)?, "at": r.get::<_, i64>(2)? })),
            )
            .optional()?)
    }

    /// Despliegues de un entorno, más recientes primero.
    pub fn deployments(&self, env: &str, limit: usize) -> Result<Vec<Deployment>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT env, commit_sha, by, at FROM deployments WHERE env = ?1 ORDER BY at DESC LIMIT ?2")?;
        let rows = stmt
            .query_map(params![env, limit as i64], |r| Ok(Deployment { env: r.get(0)?, commit: r.get(1)?, by: r.get(2)?, at: r.get(3)? }))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Flujos de despliegue (ver workflows.rs), guardados como JSON.
    pub fn save_workflow(&self, id: &str, project: &str, data: &Value) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO workflows (id, project, data, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET project = ?2, data = ?3, updated_at = ?4",
            params![id, project, data.to_string(), now_ms()],
        )?;
        Ok(())
    }

    pub fn workflows(&self, project: &str) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT data FROM workflows WHERE project = ?1 ORDER BY updated_at")?;
        let rows = stmt.query_map([project], |r| r.get::<_, String>(0))?.filter_map(|r| r.ok()).filter_map(|d| serde_json::from_str(&d).ok()).collect();
        Ok(rows)
    }

    pub fn workflow(&self, id: &str) -> Result<Option<Value>> {
        let conn = self.conn.lock().unwrap();
        let raw: Option<String> = conn.query_row("SELECT data FROM workflows WHERE id = ?1", [id], |r| r.get(0)).optional()?;
        Ok(raw.and_then(|r| serde_json::from_str(&r).ok()))
    }

    pub fn remove_workflow(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.execute("DELETE FROM workflows WHERE id = ?1", [id])? > 0)
    }

    pub fn save_run(&self, id: &str, workflow: &str, created_at: i64, status: &str, data: &Value) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO workflow_runs (id, workflow, created_at, status, data) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET status = ?4, data = ?5",
            params![id, workflow, created_at, status, data.to_string()],
        )?;
        Ok(())
    }

    pub fn run(&self, id: &str) -> Result<Option<Value>> {
        let conn = self.conn.lock().unwrap();
        let raw: Option<String> = conn.query_row("SELECT data FROM workflow_runs WHERE id = ?1", [id], |r| r.get(0)).optional()?;
        Ok(raw.and_then(|r| serde_json::from_str(&r).ok()))
    }

    /// Corridas de un flujo (o de todos con `None`), más recientes primero.
    pub fn runs(&self, workflow: Option<&str>, statuses: &[&str], limit: usize) -> Result<Vec<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT workflow, status, data FROM workflow_runs ORDER BY created_at DESC LIMIT 500")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
            .filter_map(|r| r.ok())
            .filter(|(w, s, _)| workflow.is_none_or(|x| x == w) && (statuses.is_empty() || statuses.contains(&s.as_str())))
            .filter_map(|(_, _, d)| serde_json::from_str(&d).ok())
            .take(limit)
            .collect();
        Ok(rows)
    }

    pub fn save_project(&self, p: &crate::projects::Project) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO projects (name, data) VALUES (?1, ?2) ON CONFLICT(name) DO UPDATE SET data = ?2",
            params![p.name, serde_json::to_string(p)?],
        )?;
        Ok(())
    }

    pub fn project(&self, name: &str) -> Result<Option<crate::projects::Project>> {
        let conn = self.conn.lock().unwrap();
        let raw: Option<String> = conn.query_row("SELECT data FROM projects WHERE name = ?1", [name], |r| r.get(0)).optional()?;
        Ok(raw.and_then(|r| serde_json::from_str(&r).ok()))
    }

    /// Proyectos; el por defecto primero.
    pub fn projects(&self) -> Result<Vec<crate::projects::Project>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT data FROM projects")?;
        let mut list: Vec<crate::projects::Project> = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .filter_map(|d| serde_json::from_str(&d).ok())
            .collect();
        list.sort_by_key(|p| (!p.is_default(), p.created_at));
        Ok(list)
    }

    pub fn remove_project(&self, name: &str) -> Result<bool> {
        Ok(self.conn.lock().unwrap().execute("DELETE FROM projects WHERE name = ?1", [name])? > 0)
    }

    /// Importa `~/.config/heskala/deployments.json` (scripts/convex-switch.mjs)
    /// la primera vez: `local-dev`/`self-hosted` → devbox, `cloud` → cloud-dev.
    /// Producción no está en ese archivo: se registra con `condxd env add`.
    pub fn import_switch_registry(&self, file: &Path, repo: &Path) -> Result<usize> {
        // Solo la primera vez: después, lo que el usuario borra no vuelve.
        let done = self.conn.lock().unwrap().query_row("SELECT 1 FROM probes WHERE key = 'meta:switch-imported'", [], |_| Ok(())).optional()?.is_some();
        if done {
            return Ok(0);
        }
        self.save_probe("meta:switch-imported", &serde_json::json!({ "at": now_ms() }))?;
        let Ok(raw) = std::fs::read_to_string(file) else { return Ok(0) };
        let store: Value = serde_json::from_str(&raw).with_context(|| format!("{} ilegible", file.display()))?;
        let Some(deployments) = store.get("deployments").and_then(Value::as_object) else { return Ok(0) };
        let mut added = 0;
        for (name, d) in deployments {
            if self.environment(name)?.is_some() {
                continue;
            }
            let url = d.get("url").and_then(Value::as_str).map(str::to_string);
            let kind = match d.get("kind").and_then(Value::as_str) {
                Some("cloud") => EnvKind::CloudDev,
                _ => EnvKind::Devbox,
            };
            let deployment = url
                .as_deref()
                .and_then(|u| u.strip_prefix("https://"))
                .and_then(|h| h.strip_suffix(".convex.cloud"))
                .map(str::to_string);
            let credential = match kind {
                EnvKind::Devbox => Some(Credential::EnvFile {
                    path: repo.join("docker/onprem-dev/.env").display().to_string(),
                    key: "CONVEX_SELF_HOSTED_ADMIN_KEY".into(),
                }),
                _ => Some(Credential::ConvexLogin),
            };
            self.upsert_environment(&Environment {
                name: name.clone(),
                kind,
                label: d.get("label").and_then(Value::as_str).map(str::to_string),
                convex_url: url,
                convex_site_url: d.get("siteUrl").and_then(Value::as_str).map(str::to_string),
                convex_deployment: deployment,
                frontend_url: (kind == EnvKind::Devbox).then(|| "http://localhost:3000".to_string()),
                branch: Some("dev".into()),
                credential,
                created_at: now_ms(),
                project: None,
            })?;
            added += 1;
        }
        Ok(added)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn importa_el_registro_del_switch() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("deployments.json");
        std::fs::write(
            &file,
            r#"{"current":"local","deployments":{
              "dev":{"url":"https://happy-otter-123.convex.cloud","siteUrl":"https://happy-otter-123.convex.site","kind":"cloud","label":"Cloud dev"},
              "local":{"url":"http://localhost:3210","siteUrl":"http://localhost:3211","kind":"local-dev"}}}"#,
        )
        .unwrap();
        let store = Store::memory();
        assert_eq!(store.import_switch_registry(&file, Path::new("/repo")).unwrap(), 2);
        let dev = store.environment("dev").unwrap().unwrap();
        assert_eq!(dev.kind, EnvKind::CloudDev);
        assert_eq!(dev.convex_deployment.as_deref(), Some("happy-otter-123"));
        assert_eq!(dev.credential, Some(Credential::ConvexLogin));
        // Una sola vez: lo que se borra después no vuelve.
        store.remove_environment("dev").unwrap();
        assert_eq!(store.import_switch_registry(&file, Path::new("/repo")).unwrap(), 0);
        assert!(store.environment("dev").unwrap().is_none());
        let envs = store.environments().unwrap();
        assert_eq!(envs[0].name, "local");
        assert_eq!(envs[0].kind, EnvKind::Devbox);
    }
}
