// Adjuntos de los hilos (fase 3 de los chats): el teléfono sube cada archivo cifrado con age para la
// Mac a Condx Cloud (`/files`), y manda en su mensaje firmado qué subió. Aquí se bajan, se borran de
// la nube, se guardan tal como llegaron (cifrados) en `~/.condx/daemon/attachments/<hilo>/` y se
// prepara lo que recibe el agente: las imágenes como imágenes (los modelos que ven), el texto
// adentro del mensaje, el audio transcrito con un proveedor que lo haga (OpenAI o Groq) y, lo demás,
// dónde quedó guardado.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::state::Shared;

/// Lo que el teléfono dice de un archivo que subió.
#[derive(Debug, Clone, Deserialize)]
pub struct FileRef {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub mime: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub seconds: Option<f64>,
}

/// Lo que va al agente: el texto que se agrega al mensaje y las imágenes.
#[derive(Debug, Default)]
pub struct Prepared {
    pub text: String,
    pub images: Vec<Value>,
}

const MAX_FILES: usize = 6;
/// Texto que entra entero en el mensaje; más grande, se guarda y se dice dónde.
const MAX_INLINE: usize = 200_000;
const MAX_IMAGE: usize = 6 * 1024 * 1024;

pub fn parse(raw: Option<&str>) -> Vec<FileRef> {
    raw.and_then(|r| serde_json::from_str::<Vec<FileRef>>(r).ok()).unwrap_or_default().into_iter().take(MAX_FILES).collect()
}

fn safe_name(name: &str) -> String {
    let n: String = name.chars().map(|c| if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).take(80).collect();
    if n.trim_matches('.').is_empty() { "file".into() } else { n }
}

/// Baja, guarda y borra de la nube cada archivo; arma lo que recibe el agente.
pub fn receive(state: &Shared, site: &str, token: &str, thread_id: &str, refs: &[FileRef]) -> Result<Prepared> {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(60)).build();
    let dir = crate::config::home().join("attachments").join(safe_name(thread_id));
    std::fs::create_dir_all(&dir)?;
    let mut out = Prepared::default();
    for f in refs {
        let resp = agent.get(&format!("{site}/files/{}", f.id)).set("Authorization", &format!("Bearer {token}")).call().with_context(|| format!("couldn't download {}", f.name))?;
        let mut sealed = vec![];
        std::io::Read::read_to_end(&mut resp.into_reader(), &mut sealed)?;
        let path = dir.join(format!("{}-{}.age", crate::store::now_ms(), safe_name(&f.name)));
        std::fs::write(&path, &sealed)?;
        let _ = agent.delete(&format!("{site}/files/{}", f.id)).set("Authorization", &format!("Bearer {token}")).call();
        let plain = crate::agents::open_bytes(&sealed)?;
        let kb = plain.len().div_ceil(1024);
        let is_text = f.mime.starts_with("text/") || matches!(f.mime.as_str(), "application/json" | "application/xml" | "application/x-yaml") || [".md", ".txt", ".json", ".ts", ".tsx", ".js", ".rs", ".py", ".log", ".csv", ".yml", ".yaml", ".toml", ".sql"].iter().any(|e| f.name.ends_with(e));
        let line = if f.kind == "audio" || f.mime.starts_with("audio/") {
            let secs = f.seconds.map(|s| format!(" {}:{:02}", s as u64 / 60, s as u64 % 60)).unwrap_or_default();
            match transcribe(state, &plain, &f.mime, &f.name) {
                Ok(t) if !t.trim().is_empty() => format!("[voice{secs}] {}", t.trim()),
                Ok(_) => format!("[voice{secs}] (nothing was said)"),
                Err(e) => format!("[voice{secs}] (not transcribed: {e:#})"),
            }
        } else if f.mime.starts_with("image/") && plain.len() <= MAX_IMAGE {
            out.images.push(json!({ "data": base64::engine::general_purpose::STANDARD.encode(&plain), "mimeType": f.mime }));
            format!("[image: {}]", f.name)
        } else if is_text && plain.len() <= MAX_INLINE {
            format!("[file: {}]\n```\n{}\n```", f.name, devtool_core::redact(&String::from_utf8_lossy(&plain)))
        } else {
            format!("[file: {}, {kb} KB, saved on the Mac at {}]", f.name, path.display())
        };
        if !out.text.is_empty() {
            out.text.push('\n');
        }
        out.text.push_str(&line);
    }
    Ok(out)
}

/// El audio a texto, con el primer proveedor que transcribe (OpenAI o Groq). `CONDX_TRANSCRIBE_URL`
/// apunta a otro servidor compatible (las pruebas).
pub fn transcribe(state: &Shared, audio: &[u8], mime: &str, name: &str) -> Result<String> {
    let (url, key, model) = if let Ok(url) = std::env::var("CONDX_TRANSCRIBE_URL") {
        (url, None, "whisper-1".to_string())
    } else {
        let providers = state.store.agent_providers()?;
        let pick = providers.iter().find(|p| p["kind"] == "groq" || p["baseUrl"].as_str().is_some_and(|u| u.contains("groq.com"))).map(|p| (p, "https://api.groq.com/openai/v1", "whisper-large-v3-turbo")).or_else(|| providers.iter().find(|p| p["kind"] == "openai").map(|p| (p, "https://api.openai.com/v1", "gpt-4o-mini-transcribe")));
        // Sin OpenAI ni Groq: en esta máquina, con whisper.cpp (sin claves; el audio no sale de aquí).
        let Some((p, default_base, model)) = pick else { return transcribe_local(audio) };
        let id = p["id"].as_str().unwrap_or_default();
        let base = p["baseUrl"].as_str().filter(|b| !b.is_empty()).unwrap_or(default_base).trim_end_matches('/').to_string();
        (format!("{base}/audio/transcriptions"), crate::agents::api_key(state, id)?, model.to_string())
    };
    let boundary = format!("condx{}", crate::store::now_ms());
    let mut body = Vec::new();
    let mut field = |name: &str, value: &str| body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
    field("model", &model);
    field("response_format", "json");
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n", safe_name(name), if mime.is_empty() { "audio/mp4" } else { mime }).as_bytes());
    body.extend_from_slice(audio);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let mut req = ureq::post(&url).timeout(Duration::from_secs(90)).set("Content-Type", &format!("multipart/form-data; boundary={boundary}"));
    if let Some(k) = key {
        req = req.set("Authorization", &format!("Bearer {k}"));
    }
    let v: Value = match req.send_bytes(&body) {
        Ok(r) => r.into_json()?,
        Err(ureq::Error::Status(code, r)) => bail!("the transcription service answered {code}: {}", r.into_string().unwrap_or_default().chars().take(160).collect::<String>()),
        Err(e) => bail!("couldn't reach the transcription service: {e}"),
    };
    Ok(v["text"].as_str().unwrap_or_default().to_string())
}

/// Un ejecutable de whisper.cpp (Homebrew en la Mac, o el PATH) y su modelo multilingüe.
fn local_whisper() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let bin = ["/opt/homebrew/bin/whisper-cli", "/usr/local/bin/whisper-cli", "/usr/bin/whisper-cli"].iter().map(std::path::PathBuf::from).find(|p| p.exists())?;
    let dir = crate::config::home().join("models");
    let model = ["ggml-large-v3-turbo-q5_0.bin", "ggml-large-v3-turbo.bin", "ggml-small.bin", "ggml-base.bin"].iter().map(|m| dir.join(m)).find(|p| p.exists())?;
    Some((bin, model))
}

fn ffmpeg() -> Option<std::path::PathBuf> {
    ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"].iter().map(std::path::PathBuf::from).find(|p| p.exists())
}

/// Transcribe en esta máquina: ffmpeg pasa la nota (m4a) a WAV de 16 kHz y whisper.cpp la lee, con el
/// idioma detectado solo.
fn transcribe_local(audio: &[u8]) -> Result<String> {
    let Some((bin, model)) = local_whisper() else {
        bail!("add an OpenAI or Groq provider (Settings → Agents), or install whisper.cpp on the Mac (brew install whisper-cpp) and put a model in ~/.condx/daemon/models, to transcribe voice notes")
    };
    let ffmpeg = ffmpeg().ok_or_else(|| anyhow::anyhow!("ffmpeg is needed to transcribe voice notes on this Mac (brew install ffmpeg)"))?;
    let dir = std::env::temp_dir().join(format!("condx-voice-{}", crate::store::now_ms()));
    std::fs::create_dir_all(&dir)?;
    let res = (|| -> Result<String> {
        let (input, wav) = (dir.join("in.m4a"), dir.join("in.wav"));
        std::fs::write(&input, audio)?;
        let conv = std::process::Command::new(&ffmpeg).args(["-loglevel", "error", "-y", "-i"]).arg(&input).args(["-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le"]).arg(&wav).output()?;
        if !conv.status.success() {
            bail!("couldn't read the voice note: {}", String::from_utf8_lossy(&conv.stderr).trim().chars().take(160).collect::<String>());
        }
        let out = std::process::Command::new(&bin).arg("-m").arg(&model).args(["-l", "auto", "-nt", "-np", "-f"]).arg(&wav).output()?;
        if !out.status.success() {
            bail!("whisper.cpp failed: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("").chars().take(160).collect::<String>());
        }
        Ok(String::from_utf8_lossy(&out.stdout).lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" "))
    })();
    let _ = std::fs::remove_dir_all(&dir);
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nombres_seguros_y_lista_acotada() {
        assert_eq!(safe_name("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(safe_name("..."), "file");
        assert_eq!(safe_name("foto 1.jpg"), "foto_1.jpg");
        let many = serde_json::to_string(&(0..10).map(|i| json!({ "id": i.to_string(), "name": "a" })).collect::<Vec<_>>()).unwrap();
        assert_eq!(parse(Some(&many)).len(), MAX_FILES);
        assert!(parse(Some("not json")).is_empty());
    }
}
