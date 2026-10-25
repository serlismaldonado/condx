// Git de un repo para las vistas de la app (Git y CI, despliegues): archivos
// cambiados con sus líneas, ramas con su distancia al remoto y los últimos
// commits. Solo lectura.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use crate::cmd::run;

const T: Duration = Duration::from_secs(15);

async fn git(repo: &Path, args: &[&str]) -> String {
    run("git", args, Some(repo), &[], T).await.stdout
}

/// `+añadidas -quitadas` por archivo, de `git diff --numstat`.
fn numstat(text: &str) -> HashMap<String, (u64, u64)> {
    text.lines()
        .filter_map(|l| {
            let mut p = l.split('\t');
            let (a, d, path) = (p.next()?, p.next()?, p.next()?);
            Some((path.to_string(), (a.parse().unwrap_or(0), d.parse().unwrap_or(0))))
        })
        .collect()
}

/// Archivos cambiados, en sus secciones (preparados, sin preparar, sin seguimiento, en conflicto).
pub async fn changes(repo: &Path) -> Value {
    let status = git(repo, &["status", "--porcelain=v1", "--untracked-files=normal"]).await;
    let staged_n = numstat(&git(repo, &["diff", "--cached", "--numstat"]).await);
    let unstaged_n = numstat(&git(repo, &["diff", "--numstat"]).await);
    let (mut staged, mut unstaged, mut untracked, mut conflicted) = (vec![], vec![], vec![], vec![]);
    for line in status.lines() {
        if line.len() < 4 {
            continue;
        }
        let (x, y) = (line.as_bytes()[0] as char, line.as_bytes()[1] as char);
        let path = line[3..].rsplit(" -> ").next().unwrap_or(&line[3..]).trim_matches('"').to_string();
        let entry = |code: char, n: &HashMap<String, (u64, u64)>| {
            let lines = n.get(&path).map(|(a, d)| json!([a, d])).unwrap_or(Value::Null);
            json!({ "path": path, "status": code.to_string(), "lines": lines })
        };
        match (x, y) {
            ('?', '?') => untracked.push(json!({ "path": path, "status": "?", "lines": Value::Null })),
            ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D') => conflicted.push(entry('U', &unstaged_n)),
            _ => {
                if x != ' ' {
                    staged.push(entry(x, &staged_n));
                }
                if y != ' ' {
                    unstaged.push(entry(y, &unstaged_n));
                }
            }
        }
    }
    json!({ "staged": staged, "unstaged": unstaged, "untracked": untracked, "conflicted": conflicted })
}

/// Ramas locales con su upstream, adelanto/atraso, último commit y cuándo.
pub async fn branches(repo: &Path) -> Value {
    let fmt = "%(refname:short)%09%(upstream:short)%09%(upstream:track,nobracket)%09%(HEAD)%09%(subject)%09%(committerdate:relative)";
    let text = git(repo, &["for-each-ref", "--sort=-committerdate", &format!("--format={fmt}"), "refs/heads"]).await;
    let list: Vec<Value> = text
        .lines()
        .take(40)
        .map(|l| {
            let p: Vec<&str> = l.split('\t').collect();
            let track = p.get(2).copied().unwrap_or_default();
            let num = |k: &str| track.split(", ").find_map(|s| s.strip_prefix(k)).and_then(|n| n.trim().parse::<u64>().ok()).unwrap_or(0);
            json!({
                "name": p.first().copied().unwrap_or_default(),
                "upstream": p.get(1).copied().unwrap_or_default(),
                "ahead": num("ahead "),
                "behind": num("behind "),
                "current": p.get(3).copied() == Some("*"),
                "subject": p.get(4).copied().unwrap_or_default(),
                "when": p.get(5).copied().unwrap_or_default(),
            })
        })
        .collect();
    json!(list)
}

/// Últimos commits de HEAD, con padres y referencias.
pub async fn commits(repo: &Path, limit: usize) -> Value {
    let n = format!("-{limit}");
    let text = git(repo, &["log", &n, "--format=%H%x09%P%x09%s%x09%an%x09%cr%x09%D"]).await;
    let list: Vec<Value> = text
        .lines()
        .map(|l| {
            let p: Vec<&str> = l.split('\t').collect();
            let refs: Vec<&str> = p.get(5).copied().unwrap_or_default().split(", ").filter(|r| !r.is_empty()).collect();
            json!({
                "id": p.first().copied().unwrap_or_default(),
                "parents": p.get(1).copied().unwrap_or_default().split(' ').filter(|s| !s.is_empty()).collect::<Vec<_>>(),
                "subject": p.get(2).copied().unwrap_or_default(),
                "author": p.get(3).copied().unwrap_or_default(),
                "when": p.get(4).copied().unwrap_or_default(),
                "refs": refs,
            })
        })
        .collect();
    json!(list)
}

/// Asunto y autor de un commit (para los despliegues registrados).
pub async fn describe(repo: &Path, sha: &str) -> (String, String) {
    if sha.is_empty() || sha.starts_with('-') {
        return (String::new(), String::new());
    }
    let text = git(repo, &["log", "-1", "--format=%s%x09%an", sha, "--"]).await;
    let mut p = text.trim().splitn(2, '\t');
    (p.next().unwrap_or_default().to_string(), p.next().unwrap_or_default().to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn lee_numstat() {
        let n = super::numstat("3\t1\tsrc/a.rs\n-\t-\timg.png\n");
        assert_eq!(n["src/a.rs"], (3, 1));
        assert_eq!(n["img.png"], (0, 0));
    }
}
