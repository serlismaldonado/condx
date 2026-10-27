// Recetas (§5.2): definiciones TOML versionadas en tools/devtool/recipes y
// compiladas dentro del binario (el servicio corre desde una copia fuera del
// repo). Una receta dice en qué tipos de entorno corre, qué aprobación pide
// en cada uno, qué argumentos acepta y qué pasos ejecuta.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

const SOURCES: &[(&str, &str)] = &[
    ("deploy", include_str!("../../../recipes/deploy.toml")),
    ("run-function", include_str!("../../../recipes/run-function.toml")),
    ("backfill", include_str!("../../../recipes/backfill.toml")),
    ("export", include_str!("../../../recipes/export.toml")),
    ("import", include_str!("../../../recipes/import.toml")),
    ("restart-backend", include_str!("../../../recipes/restart-backend.toml")),
    ("deployer-rebuild", include_str!("../../../recipes/deployer-rebuild.toml")),
    ("throwaway-create", include_str!("../../../recipes/throwaway-create.toml")),
    ("throwaway-destroy", include_str!("../../../recipes/throwaway-destroy.toml")),
    ("deploy-cloud", include_str!("../../../recipes/deploy-cloud.toml")),
    ("rehearsal", include_str!("../../../recipes/rehearsal.toml")),
    ("promote-prod", include_str!("../../../recipes/promote-prod.toml")),
    ("rollback-prod", include_str!("../../../recipes/rollback-prod.toml")),
    ("migrate-native", include_str!("../../../recipes/migrate-native.toml")),
    ("local-mode", include_str!("../../../recipes/local-mode.toml")),
    ("project-seed", include_str!("../../../recipes/project-seed.toml")),
    ("project-seed-prod", include_str!("../../../recipes/project-seed-prod.toml")),
    ("project-create", include_str!("../../../recipes/project-create.toml")),
    ("workflow-approval", include_str!("../../../recipes/workflow-approval.toml")),
    ("workflow-approval-strict", include_str!("../../../recipes/workflow-approval-strict.toml")),
    ("update-condx", include_str!("../../../recipes/update-condx.toml")),
    ("release-condx", include_str!("../../../recipes/release-condx.toml")),
    ("release-mobile", include_str!("../../../recipes/release-mobile.toml")),
    ("deploy-condx-cloud", include_str!("../../../recipes/deploy-condx-cloud.toml")),
    ("preview", include_str!("../../../recipes/preview.toml")),
];

/// Tipo de entorno al que apunta una receta; `new` = crea el entorno.
pub const NEW_ENV: &str = "new";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Approval {
    None,
    Simple,
    Typed,
    Reinforced,
    Forbidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArgType {
    String,
    Json,
    Commit,
    Function,
    Backup,
    Port,
    /// Lista de funciones separadas por coma (rellenos).
    Functions,
    Minutes,
    /// yes | no
    Flag,
    /// auto | vercel | none
    Frontend,
    /// native | docker
    LocalMode,
    /// Nombre de un proyecto (ver projects.rs).
    Project,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Arg {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: ArgType,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Cwd {
    #[default]
    Repo,
    Worktree,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub name: String,
    #[serde(default)]
    pub run: Option<Vec<String>>,
    #[serde(default)]
    pub builtin: Option<String>,
    #[serde(default)]
    pub cwd: Cwd,
    /// Inyecta las variables para que la CLI de Convex apunte al entorno.
    #[serde(default)]
    pub convex: bool,
    #[serde(default)]
    pub idempotent: bool,
    /// Se vuelve a correr al reintentar (prepara lo que necesitan los demás).
    #[serde(default)]
    pub setup: bool,
    /// Corre siempre al final, aunque algo falle o se cancele.
    #[serde(default)]
    pub finally: bool,
    /// Un reintento que falla después de este paso vuelve a empezar desde aquí
    /// (p. ej. el ensayo: el desechable se destruye al terminar).
    #[serde(default)]
    pub checkpoint: bool,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Una salida de una receta: su nombre y qué es.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Output {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipe {
    pub name: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub kinds: Vec<String>,
    #[serde(default = "yes")]
    pub writes: bool,
    #[serde(default)]
    pub approval: HashMap<String, Approval>,
    #[serde(default)]
    pub args: Vec<Arg>,
    /// Exige las precondiciones de la promoción (§8.3) al pedir y al empezar.
    #[serde(default)]
    pub preconditions: bool,
    /// También corre en los entornos de otros proyectos (no solo heskala-erp).
    #[serde(default)]
    pub projects: bool,
    /// Lo que deja el trabajo para los nodos siguientes de un flujo (`{node.<id>.<nombre>}`).
    #[serde(default)]
    pub outputs: Vec<Output>,
    /// En cloud dev, la CLI usa la deploy key del entorno si está guardada (pestaña Claves) en
    /// vez de la sesión de `npx convex`.
    #[serde(default)]
    pub deploy_key: bool,
    #[serde(rename = "step")]
    pub steps: Vec<Step>,
}

fn yes() -> bool {
    true
}

pub fn all() -> &'static [Recipe] {
    static RECIPES: OnceLock<Vec<Recipe>> = OnceLock::new();
    RECIPES.get_or_init(|| {
        SOURCES
            .iter()
            .map(|(name, src)| {
                let r: Recipe = toml::from_str(src).unwrap_or_else(|e| panic!("invalid recipe {name}: {e}"));
                assert_eq!(r.name, *name, "the recipe name doesn't match its file");
                for s in &r.steps {
                    assert!(s.run.is_some() != s.builtin.is_some(), "step “{}” of {name}: run or builtin", s.name);
                }
                r
            })
            .collect()
    })
}

pub fn get(name: &str) -> Option<&'static Recipe> {
    all().iter().find(|r| r.name == name)
}

impl Recipe {
    /// Aprobación que pide en un tipo de entorno; si no lo declara, no corre ahí.
    pub fn approval_for(&self, kind: &str) -> Approval {
        if !self.kinds.iter().any(|k| k == kind) {
            return Approval::Forbidden;
        }
        self.approval.get(kind).copied().unwrap_or(Approval::Simple)
    }

    /// Valida y completa los argumentos (valores por defecto incluidos).
    pub fn resolve_args(&self, given: &serde_json::Value, backups: &Path) -> Result<HashMap<String, String>> {
        let given = given.as_object().cloned().unwrap_or_default();
        for key in given.keys() {
            if !self.args.iter().any(|a| &a.name == key) {
                bail!("unknown argument for {}: {key}", self.name);
            }
        }
        let mut out = HashMap::new();
        for arg in &self.args {
            let raw = match given.get(&arg.name) {
                Some(serde_json::Value::String(s)) => Some(s.clone()),
                Some(serde_json::Value::Null) | None => None,
                Some(other) => Some(other.to_string()),
            };
            // Un vacío por defecto (p. ej. la versión de un release: «la siguiente») vale tal cual.
            if raw.is_none() && arg.default.as_deref() == Some("") {
                out.insert(arg.name.clone(), String::new());
                continue;
            }
            let value = raw.or_else(|| arg.default.clone()).ok_or_else(|| anyhow!("missing argument “{}”", arg.name))?;
            out.insert(arg.name.clone(), validate(arg.kind, &value, backups).with_context(|| format!("argument “{}”", arg.name))?);
        }
        Ok(out)
    }
}

/// Los pasos reciben los argumentos como elementos de argv, nunca por un
/// shell; aun así se limita cada tipo a lo que tiene sentido.
pub fn validate(kind: ArgType, value: &str, backups: &Path) -> Result<String> {
    static FUNCTION: OnceLock<Regex> = OnceLock::new();
    static REF: OnceLock<Regex> = OnceLock::new();
    let value = value.trim();
    match kind {
        ArgType::String => {
            if value.is_empty() || value.len() > 200 || value.starts_with('-') {
                bail!("empty text, too long, or starting with “-”");
            }
        }
        ArgType::Json => {
            serde_json::from_str::<serde_json::Value>(value).map_err(|e| anyhow!("invalid JSON: {e}"))?;
        }
        ArgType::Commit => {
            let re = REF.get_or_init(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._/-]{0,99}$").unwrap());
            if value == "previous" {
                return Ok(value.to_string());
            }
            if !re.is_match(value) || value.contains("..") {
                bail!("invalid git reference");
            }
        }
        ArgType::Function => {
            let re = FUNCTION.get_or_init(|| Regex::new(r"^[A-Za-z0-9_][A-Za-z0-9_/.-]*:[A-Za-z0-9_]+$").unwrap());
            if !re.is_match(value) {
                bail!("expected path:function, e.g. system/users/queries:getCurrentUser");
            }
        }
        ArgType::Backup => {
            let path = Path::new(value);
            let canonical = path.canonicalize().with_context(|| format!("{value} doesn't exist"))?;
            let root = backups.canonicalize().with_context(|| format!("{} doesn't exist", backups.display()))?;
            let name = canonical.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if !canonical.starts_with(&root) || !(name.ends_with(".zip") || name.ends_with(".zip.age")) {
                bail!("the backup must be a .zip (or .zip.age) inside {}", backups.display());
            }
            return Ok(canonical.display().to_string());
        }
        ArgType::Functions => {
            for f in value.split(',').map(str::trim).filter(|f| !f.is_empty()) {
                validate(ArgType::Function, f, backups)?;
            }
        }
        ArgType::Minutes => {
            let m: u32 = value.parse().map_err(|_| anyhow!("invalid minutes"))?;
            if m > 60 {
                bail!("at most 60 minutes");
            }
        }
        ArgType::Flag => {
            if value != "yes" && value != "no" {
                bail!("yes or no");
            }
        }
        ArgType::Project => {
            if value.is_empty() || value.len() > 40 || !value.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
                bail!("invalid project name");
            }
        }
        ArgType::LocalMode => {
            if !matches!(value, "native" | "docker") {
                bail!("native or docker");
            }
        }
        ArgType::Frontend => {
            if !matches!(value, "auto" | "vercel" | "none") {
                bail!("auto, vercel or none");
            }
        }
        ArgType::Port => {
            let port: u16 = value.parse().map_err(|_| anyhow!("invalid port"))?;
            if !(1024..=65_000).contains(&port) {
                bail!("port out of range (1024–65000)");
            }
        }
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn todas_las_recetas_cargan() {
        assert!(all().len() >= 13);
        assert_eq!(get("promote-prod").unwrap().approval_for("cloud-prod"), Approval::Typed);
        assert!(get("promote-prod").unwrap().preconditions);
        assert_eq!(get("deploy").unwrap().approval_for("cloud-dev"), Approval::Forbidden);
        assert_eq!(get("deploy-cloud").unwrap().approval_for("cloud-prod"), Approval::Forbidden);
        assert_eq!(get("import").unwrap().approval_for("cloud-prod"), Approval::Reinforced);
        assert_eq!(get("backfill").unwrap().approval_for("cloud-prod"), Approval::Forbidden);
        let deploy = get("deploy").unwrap();
        assert_eq!(deploy.approval_for("devbox"), Approval::None);
        assert_eq!(deploy.approval_for("cloud-prod"), Approval::Forbidden);
        assert_eq!(get("import").unwrap().approval_for("devbox"), Approval::Simple);
        assert!(deploy.steps.iter().any(|s| s.finally));
    }

    #[test]
    fn valida_argumentos() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path();
        let run = get("run-function").unwrap();
        let args = run.resolve_args(&json!({ "function": "system/users/queries:getCurrentUser" }), backups).unwrap();
        assert_eq!(args["payload"], "{}");
        assert!(run.resolve_args(&json!({ "function": "rm -rf /" }), backups).is_err());
        assert!(run.resolve_args(&json!({ "function": "a:b", "payload": "{no" }), backups).is_err());
        assert!(run.resolve_args(&json!({ "function": "a:b", "otro": 1 }), backups).is_err());
        assert!(validate(ArgType::Commit, "--upload-pack=x", backups).is_err());
        assert!(validate(ArgType::Commit, "dev", backups).is_ok());
        std::fs::write(backups.join("x.zip"), "").unwrap();
        assert!(validate(ArgType::Backup, &backups.join("x.zip").display().to_string(), backups).is_ok());
        assert!(validate(ArgType::Backup, "/etc/hosts", backups).is_err());
        assert!(validate(ArgType::Port, "80", backups).is_err());
    }
}
