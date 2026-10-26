//! Timbre en vivo: una suscripción de Convex (WebSocket) a `sync:doorbell` en vez de preguntar cada
//! segundo. Convex vuelve a correr la consulta solo cuando cambia la versión de un alcance, así que con
//! la app abierta y sin novedades no hay llamadas. Si el socket no anda, quien lo usa vuelve al timbre
//! por HTTP (más espaciado).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use futures_util::StreamExt;

#[derive(Default)]
struct Inner {
    /// La última respuesta del timbre (sin leer) y si sonó desde la última vez.
    latest: Mutex<Option<serde_json::Value>>,
    rang: Mutex<bool>,
    ring: Condvar,
    live: AtomicBool,
    stop: AtomicBool,
}

pub struct Bell(Arc<Inner>);

impl Bell {
    pub fn start(url: &str, token: &str, scopes: &[String], protocol: u32) -> Bell {
        let inner = Arc::new(Inner::default());
        let (url, token, scopes, me) = (url.to_string(), token.to_string(), scopes.to_vec(), inner.clone());
        let _ = std::thread::Builder::new().name("bell".into()).spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
            while !me.stop.load(Ordering::Relaxed) {
                let _ = rt.block_on(listen(&me, &url, &token, &scopes, protocol));
                me.live.store(false, Ordering::Relaxed);
                // Otro intento en un rato (sin red, el teléfono sigue con el timbre por HTTP).
                for _ in 0..10 {
                    if me.stop.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        });
        Bell(inner)
    }

    /// Si la suscripción está andando (ya contestó al menos una vez).
    pub fn is_live(&self) -> bool {
        self.0.live.load(Ordering::Relaxed)
    }

    /// La respuesta nueva del timbre, si sonó.
    pub fn take(&self) -> Option<serde_json::Value> {
        *self.0.rang.lock().unwrap() = false;
        self.0.latest.lock().unwrap().take()
    }

    /// Espera a que suene, hasta `max`. Devuelve si sonó.
    pub fn wait(&self, max: Duration) -> bool {
        let rang = self.0.rang.lock().unwrap();
        let (rang, _) = self.0.ring.wait_timeout_while(rang, max, |r| !*r).unwrap();
        *rang
    }
}

impl Drop for Bell {
    fn drop(&mut self) {
        self.0.stop.store(true, Ordering::Relaxed);
    }
}

async fn listen(me: &Inner, url: &str, token: &str, scopes: &[String], protocol: u32) -> Result<(), String> {
    let mut client = convex::ConvexClient::new(url).await.map_err(|e| e.to_string())?;
    let mut args = BTreeMap::new();
    args.insert("token".to_string(), convex::Value::String(token.to_string()));
    args.insert("scopes".to_string(), convex::Value::Array(scopes.iter().map(|s| convex::Value::String(s.clone())).collect()));
    args.insert("protocolVersion".to_string(), convex::Value::Float64(protocol as f64));
    let mut sub = client.subscribe("sync:doorbell", args).await.map_err(|e| e.to_string())?;
    loop {
        let next = tokio::select! {
            n = sub.next() => n,
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                if me.stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
                continue;
            }
        };
        match next {
            Some(convex::FunctionResult::Value(v)) => {
                *me.latest.lock().unwrap() = Some(serde_json::Value::from(v));
                me.live.store(true, Ordering::Relaxed);
                *me.rang.lock().unwrap() = true;
                me.ring.notify_all();
            }
            Some(other) => return Err(format!("doorbell: {other:?}")),
            None => return Err("doorbell: the subscription ended".into()),
        }
    }
}
