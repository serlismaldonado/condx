// Touch ID (LocalAuthentication) para las aprobaciones reforzadas (§7.2):
// importar en producción o cambiar sus variables. Si la Mac no tiene Touch ID,
// macOS pide la contraseña del usuario. Bloquea: correr en el executor de fondo.

#[cfg(target_os = "macos")]
pub fn authenticate(reason: &str) -> Result<(), String> {
    use std::sync::mpsc;
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSError, NSString};
    use objc2_local_authentication::{LAContext, LAPolicy};

    // SAFETY: LAContext se crea y usa en este hilo; la respuesta llega por un
    // bloque que solo manda un bool por un canal.
    unsafe {
        let context = LAContext::new();
        let policy = LAPolicy::DeviceOwnerAuthentication;
        if let Err(e) = context.canEvaluatePolicy_error(policy) {
            return Err(t!("This Mac can't ask for Touch ID or a password: {a}", a = e.localizedDescription()));
        }
        let (tx, rx) = mpsc::channel::<Result<(), String>>();
        let reply = RcBlock::new(move |ok: Bool, error: *mut NSError| {
            let result = if ok.as_bool() {
                Ok(())
            } else if error.is_null() {
                Err(t!("Touch ID cancelled").to_string())
            } else {
                Err((*error).localizedDescription().to_string())
            };
            let _ = tx.send(result);
        });
        context.evaluatePolicy_localizedReason_reply(policy, &NSString::from_str(reason), &reply);
        rx.recv_timeout(Duration::from_secs(120)).unwrap_or_else(|_| Err(t!("Touch ID didn't respond").into()))
    }
}

#[cfg(not(target_os = "macos"))]
pub fn authenticate(_reason: &str) -> Result<(), String> {
    Err(t!("Touch ID is only available on macOS").into())
}
