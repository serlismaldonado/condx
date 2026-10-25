// Git del ERP y de sus submódulos: rama, adelanto/atraso, cambios sin
// commitear, ramas dev/main y el puntero de cada submódulo frente a su remoto.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};

use crate::cmd::run;
use crate::state::Shared;
use crate::store::now_ms;

const T: Duration = Duration::from_secs(10);
/// `git ls-remote` va a la red: cada 5 min alcanza.
const REMOTE_EVERY_MS: i64 = 5 * 60_000;

static REMOTE_CACHE: Mutex<Vec<(String, i64, Option<String>)>> = Mutex::new(Vec::new());

async fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = run("git", args, Some(repo), &[], T).await;
    out.ok.then(|| out.stdout.trim().to_string())
}

/// Lee `git status --porcelain=v2 --branch`.
pub fn parse_status(text: &str) -> Value {
    let mut branch = None;
    let mut upstream = None;
    let (mut ahead, mut behind, mut changed, mut untracked) = (0i64, 0i64, 0, 0);
    for line in text.lines() {
        if let Some(b) = line.strip_prefix("# branch.head ") {
            branch = Some(b.to_string());
        } else if let Some(u) = line.strip_prefix("# branch.upstream ") {
            upstream = Some(u.to_string());
        } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
            for part in ab.split_whitespace() {
                if let Some(n) = part.strip_prefix('+') {
                    ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix('-') {
                    behind = n.parse().unwrap_or(0);
                }
            }
        } else if line.starts_with("? ") {
            untracked += 1;
        } else if !line.starts_with('#') && !line.is_empty() {
            changed += 1;
        }
    }
    json!({ "branch": branch, "upstream": upstream, "ahead": ahead, "behind": behind, "changed": changed, "untracked": untracked })
}

async fn remote_head(repo: &Path, url: &str, branch: &str) -> Option<String> {
    let key = format!("{url}#{branch}");
    {
        let cache = REMOTE_CACHE.lock().unwrap();
        if let Some((_, at, sha)) = cache.iter().find(|(k, _, _)| *k == key) {
            if now_ms() - at < REMOTE_EVERY_MS {
                return sha.clone();
            }
        }
    }
    let sha = git(repo, &["ls-remote", url, &format!("refs/heads/{branch}")])
        .await
        .and_then(|o| o.split_whitespace().next().map(str::to_string));
    let mut cache = REMOTE_CACHE.lock().unwrap();
    cache.retain(|(k, _, _)| *k != key);
    cache.push((key, now_ms(), sha.clone()));
    sha
}

async fn repo_state(repo: &Path) -> Option<Value> {
    let status = git(repo, &["status", "--porcelain=v2", "--branch"]).await?;
    let mut value = parse_status(&status);
    let head = git(repo, &["log", "-1", "--format=%H%x09%s%x09%ct"]).await.unwrap_or_default();
    let mut parts = head.splitn(3, '\t');
    value["head"] = json!(parts.next());
    value["subject"] = json!(parts.next());
    value["committedAt"] = json!(parts.next().and_then(|t| t.parse::<i64>().ok()).map(|t| t * 1000));
    let mut refs = serde_json::Map::new();
    for r in ["dev", "main", "origin/dev", "origin/main"] {
        refs.insert(r.into(), json!(git(repo, &["rev-parse", "--verify", "--quiet", r]).await));
    }
    value["refs"] = Value::Object(refs);
    Some(value)
}

/// URL relativa de un submódulo (`../heskala-localfirst.git`) resuelta contra
/// el remoto del repo, como hace `git submodule`.
pub fn resolve_url(origin: &str, url: &str) -> String {
    if !url.starts_with("../") && !url.starts_with("./") {
        return url.to_string();
    }
    let origin = origin.trim().trim_end_matches('/');
    let (prefix, mut parts): (String, Vec<&str>) = match origin.split_once("://") {
        Some((scheme, rest)) => (format!("{scheme}://"), rest.split('/').collect()),
        None => match origin.split_once(':') {
            // scp: git@github.com:Org/repo.git
            Some((host, path)) => (format!("{host}:"), path.split('/').collect()),
            None => (String::new(), origin.split('/').collect()),
        },
    };
    for seg in url.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    format!("{prefix}{}", parts.join("/"))
}

async fn submodules(repo: &Path) -> Vec<Value> {
    let origin = git(repo, &["remote", "get-url", "origin"]).await.unwrap_or_default();
    let Some(config) = git(repo, &["config", "-f", ".gitmodules", "--get-regexp", r"^submodule\..*\.(path|url|branch)$"]).await
    else {
        return vec![];
    };
    let mut subs: Vec<(String, String, Option<String>, Option<String>)> = vec![];
    for line in config.lines() {
        let Some((key, value)) = line.split_once(' ') else { continue };
        let Some(rest) = key.strip_prefix("submodule.") else { continue };
        let Some((name, field)) = rest.rsplit_once('.') else { continue };
        let entry = match subs.iter_mut().find(|s| s.0 == name) {
            Some(e) => e,
            None => {
                subs.push((name.to_string(), String::new(), None, None));
                subs.last_mut().unwrap()
            }
        };
        match field {
            "path" => entry.1 = value.to_string(),
            "url" => entry.2 = Some(resolve_url(&origin, value)),
            "branch" => entry.3 = Some(value.to_string()),
            _ => {}
        }
    }
    let mut list = vec![];
    for (name, path, url, branch) in subs {
        let recorded = git(repo, &["ls-tree", "HEAD", &path])
            .await
            .and_then(|o| o.split_whitespace().nth(2).map(str::to_string));
        let dir = repo.join(&path);
        let checked_out = git(&dir, &["rev-parse", "HEAD"]).await;
        let dirty = git(&dir, &["status", "--porcelain"]).await.map(|s| s.lines().count());
        let branch = branch.unwrap_or_else(|| "main".into());
        let remote = match &url {
            Some(u) => remote_head(repo, u, &branch).await,
            None => None,
        };
        list.push(json!({
            "name": name,
            "path": path,
            "url": url,
            "branch": branch,
            "recorded": recorded,
            "checkedOut": checked_out,
            "remote": remote,
            "dirty": dirty,
            "matchesRemote": recorded.is_some() && recorded == remote,
        }));
    }
    list
}

pub async fn repos(state: Shared) {
    let Some(repo) = state.config.repo.clone() else { return };
    let Some(mut erp) = repo_state(&repo).await else {
        state.set_probe("git", false, Value::Null, Some(format!("{} isn't a readable git repo", repo.display())));
        return;
    };
    erp["path"] = json!(repo.display().to_string());
    erp["submodules"] = json!(submodules(&repo).await);
    state.set_probe("git", true, erp, None);
}

/// Commits de `from` a `to` (cuántos le faltan a `from` para llegar a `to`).
pub async fn commits_between(repo: &Path, from: &str, to: &str) -> Option<i64> {
    git(repo, &["rev-list", "--count", &format!("{from}..{to}")]).await?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lee_el_status() {
        let text = "# branch.oid abc\n# branch.head dev\n# branch.upstream origin/dev\n# branch.ab +3 -1\n1 .M N... 100644 100644 100644 a b docs/x.md\n? .archive/\n";
        let v = parse_status(text);
        assert_eq!(v["branch"], "dev");
        assert_eq!(v["ahead"], 3);
        assert_eq!(v["behind"], 1);
        assert_eq!(v["changed"], 1);
        assert_eq!(v["untracked"], 1);
    }

    #[test]
    fn resuelve_urls_relativas() {
        assert_eq!(resolve_url("git@github.com:Heskala/heskala-erp.git", "../heskala-localfirst.git"), "git@github.com:Heskala/heskala-localfirst.git");
        assert_eq!(resolve_url("https://github.com/Heskala/heskala-erp", "../lib"), "https://github.com/Heskala/lib");
        assert_eq!(resolve_url("x", "git@github.com:A/b.git"), "git@github.com:A/b.git");
    }
}
