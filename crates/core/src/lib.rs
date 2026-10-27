//! Tipos y reglas compartidas por el daemon y la app del devtool
//! (docs/diseno-devtool-escritorio.md).

pub mod paths;

use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Tipo de entorno: define la política (§3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnvKind {
    Devbox,
    Throwaway,
    CloudDev,
    CloudProd,
}

impl EnvKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "devbox" => Some(Self::Devbox),
            "throwaway" => Some(Self::Throwaway),
            "cloud-dev" => Some(Self::CloudDev),
            "cloud-prod" => Some(Self::CloudProd),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Devbox => "devbox",
            Self::Throwaway => "throwaway",
            Self::CloudDev => "cloud-dev",
            Self::CloudProd => "cloud-prod",
        }
    }

    pub fn is_cloud(self) -> bool {
        matches!(self, Self::CloudDev | Self::CloudProd)
    }
}

/// De dónde sale la credencial de un entorno. Nunca el valor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "kebab-case")]
pub enum Credential {
    /// La sesión de `npx convex` del usuario (cloud).
    ConvexLogin,
    /// Una variable dentro de un archivo .env (devbox: `docker/onprem-dev/.env`).
    EnvFile { path: String, key: String },
    /// Entrada del Llavero de macOS (servicio `condx`).
    Keychain { account: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    pub name: String,
    pub kind: EnvKind,
    pub label: Option<String>,
    pub convex_url: Option<String>,
    pub convex_site_url: Option<String>,
    /// Nombre del deployment de Convex Cloud (`happy-otter-123`).
    pub convex_deployment: Option<String>,
    pub frontend_url: Option<String>,
    /// Rama que se espera desplegada (`dev`, `main`).
    pub branch: Option<String>,
    pub credential: Option<Credential>,
    pub created_at: i64,
    /// Proyecto al que pertenece (ver projects.rs del daemon); `None` = heskala-erp.
    #[serde(default)]
    pub project: Option<String>,
}

/// Lecturas que el daemon permite sobre un entorno (F0: todo es lectura).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Read {
    Status,
    EnvVarNames,
    FunctionLogs,
    Records,
}

/// Política de lectura (§3.2): prod no expone registros por ningún lado.
pub fn can_read(kind: EnvKind, what: Read) -> bool {
    !(kind == EnvKind::CloudProd && what == Read::Records)
}

/// Saca secretos de un texto antes de guardarlo o mandarlo (§9): admin keys
/// de Convex, JWT, bloques PEM, claves con prefijo conocido y valores de
/// variables que suenan a secreto.
pub fn redact(text: &str) -> String {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            (r"-----BEGIN [A-Z ]+-----[\s\S]*?-----END [A-Z ]+-----", "[pem redactado]"),
            (r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}", "[jwt redactado]"),
            // Admin key de Convex: `<instancia>|<hex>` o `prod:<nombre>|<base64>`.
            (r"[A-Za-z0-9:_-]+\|[A-Za-z0-9+/=]{20,}", "[admin key redactada]"),
            (r"\b(sk|pk|rk)_(live|test)_[A-Za-z0-9]{10,}", "[clave redactada]"),
            (r"\b(gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})", "[token redactado]"),
            (
                // La clave termina en la palabra o sigue con `_…` (`SECRET_KEY` sí; «Secretos:» de un texto,
                // no) y el valor parece un secreto (sin espacios ni asteriscos, 6+ caracteres).
                r#"(?i)\b([A-Z0-9_]*(SECRET|TOKEN|PASSWORD|PRIVATE_KEY|ADMIN_KEY|DEPLOY_KEY|API_KEY)(?:_[A-Z0-9_]*)?)\s*[=:]\s*["']?[^\s"'*]{6,}["']?"#,
                "$1=[redactado]",
            ),
        ]
        .into_iter()
        .map(|(p, r)| (Regex::new(p).expect("regex de redacción"), r))
        .collect()
    });
    let mut out = text.to_string();
    for (re, rep) in patterns {
        out = re.replace_all(&out, *rep).into_owned();
    }
    out
}

/// Fase del deployer del devbox a partir de sus últimas líneas de log
/// (misma heurística que `scripts/deployer-status.mjs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DeployerPhase {
    Idle,
    Syncing,
    Error,
    Starting,
    Unknown,
}

pub fn deployer_phase<'a>(lines: impl IntoIterator<Item = &'a str>) -> DeployerPhase {
    static RE: OnceLock<[Regex; 3]> = OnceLock::new();
    let [error, ok, syncing] = RE.get_or_init(|| {
        [
            Regex::new(r"(?i)error|ENOENT|TypeError|SyntaxError|Build failed").unwrap(),
            Regex::new(r"(?i)Convex functions ready|Functions deployed|Synced [^:]*\d|table .* synced|Successfully deployed|✔").unwrap(),
            Regex::new(r"(?i)Bundling|Building|Compiling|Detected change|syncing|Watching for file changes").unwrap(),
        ]
    });
    // La última línea que se puede clasificar manda: un error viejo ya
    // corregido por un deploy posterior no deja el deployer en error.
    let mut phase = DeployerPhase::Idle;
    for line in lines {
        // Con `--tail-logs`, convex dev también imprime los logs de las funciones
        // («… [CONVEX A(mod:fn)] [ERROR] …»): son de la app, no del deployer.
        if line.contains("[CONVEX ") {
            continue;
        }
        if error.is_match(line) {
            phase = DeployerPhase::Error;
        } else if ok.is_match(line) {
            phase = DeployerPhase::Idle;
        } else if syncing.is_match(line) {
            phase = DeployerPhase::Syncing;
        }
    }
    phase
}

/// Pestaña preferida: entre las que reportaron hace menos de `stale_ms`, la
/// visible más reciente; si no hay visibles, la más reciente.
pub fn preferred_tab<'a, T: Clone>(
    tabs: impl IntoIterator<Item = (&'a str, i64, bool, T)>,
    now: i64,
    stale_ms: i64,
) -> Option<(&'a str, T)> {
    let all: Vec<_> = tabs.into_iter().collect();
    let fresh: Vec<_> = all.iter().filter(|t| now - t.1 < stale_ms).collect();
    let pool: Vec<_> = if fresh.is_empty() { all.iter().collect() } else { fresh };
    let visible: Vec<_> = pool.iter().copied().filter(|t| t.2).collect();
    let list = if visible.is_empty() { pool } else { visible };
    list.into_iter()
        .max_by_key(|t| t.1)
        .map(|t| (t.0, t.3.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacta_secretos() {
        let admin = "heskala-local|01a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4";
        assert!(!redact(admin).contains("01a2b3"));
        let jwt = "Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        assert!(!redact(jwt).contains("dozjg"));
        let pem = "JWT_PRIVATE_KEY=-----BEGIN PRIVATE KEY-----\nMIIEv\nabc\n-----END PRIVATE KEY-----";
        let out = redact(pem);
        assert!(!out.contains("MIIEv"), "{out}");
        assert_eq!(redact("RESEND_API_KEY=re_123456"), "RESEND_API_KEY=[redactado]");
        assert_eq!(redact("Convex functions ready! (2.1s)"), "Convex functions ready! (2.1s)");
    }

    #[test]
    fn fase_del_deployer() {
        assert_eq!(deployer_phase(["Bundling modules", "✔ Convex functions ready! (3s)"]), DeployerPhase::Idle);
        assert_eq!(deployer_phase(["✔ Convex functions ready!", "Detected change in convex/foo.ts"]), DeployerPhase::Syncing);
        assert_eq!(deployer_phase(["✖ Error: TypeError: x is undefined"]), DeployerPhase::Error);
        assert_eq!(deployer_phase(["✖ Error: x", "✔ Convex functions ready!"]), DeployerPhase::Idle);
        assert_eq!(
            deployer_phase(["✔ Convex functions ready!", "10/24/2026, 5:07:43 PM [CONVEX A(x:y)] [ERROR] 'Failed to send'"]),
            DeployerPhase::Idle
        );
    }

    #[test]
    fn pestana_preferida() {
        let tabs = [("a", 100, false, 1), ("b", 90, true, 2), ("c", 10, true, 3)];
        assert_eq!(preferred_tab(tabs, 105, 20).map(|t| t.0), Some("b"));
        let hidden = [("a", 100, false, 1), ("b", 95, false, 2)];
        assert_eq!(preferred_tab(hidden, 105, 20).map(|t| t.0), Some("a"));
        let stale = [("a", 10, false, 1), ("b", 5, true, 2)];
        assert_eq!(preferred_tab(stale, 1000, 20).map(|t| t.0), Some("b"));
        assert!(!can_read(EnvKind::CloudProd, Read::Records));
        assert!(can_read(EnvKind::CloudProd, Read::EnvVarNames));
    }
}

#[cfg(test)]
mod redact_prose_tests {
    use super::redact;

    #[test]
    fn no_redacta_prosa_que_nombra_secretos() {
        assert_eq!(redact("**Secretos:** no hay ninguno"), "**Secretos:** no hay ninguno");
        assert_eq!(redact("Tokens: 1200 usados"), "Tokens: 1200 usados");
        assert_eq!(redact("API_KEY=abcdef123456"), "API_KEY=[redactado]");
        assert_eq!(redact("client_secret: \"s3cr3tvalue\""), "client_secret=[redactado]");
        assert_eq!(redact("RESEND_API_KEY = re_123456789"), "RESEND_API_KEY=[redactado]");
    }
}
