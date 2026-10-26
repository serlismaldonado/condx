# Firma estable en esta Mac

macOS ata el «Permitir siempre» del Llavero a la firma del programa. Compilado a mano, `condxd`
queda con firma ad hoc: cada versión nueva es otro programa para macOS y vuelve a pedir la
contraseña para leer sus secretos (la clave de los agentes, las deploy keys, el token de ntfy).

## Una vez: crear la identidad «Condx Dev»

```bash
scripts/create-signing-cert.sh
```

Crea un certificado de firma de código autofirmado, solo para esta Mac, en el Llavero de inicio de
sesión, y pide tu contraseña para confiar en él para firmar código. No sirve para distribuir la app
a otras Macs (para eso hace falta un certificado de Apple Developer).

## En cada actualización

```bash
scripts/install-daemon.sh            # compila, firma con «Condx Dev», instala y reinicia
SKIP_BUILD=1 scripts/bundle-app.sh   # la app, también firmada con «Condx Dev»
```

`condxd install` también firma con esa identidad si existe. La primera vez después de firmarlo,
macOS pregunta una última vez: «Permitir siempre». Desde ahí, las versiones nuevas firmadas con la
misma identidad no preguntan. Otra identidad: `CONDX_SIGN_IDENTITY="Mi firma"`.

## Las pruebas no usan el Llavero

Un daemon con `CONDX_HOME` (los de prueba) guarda sus secretos en `$CONDX_HOME/test-secrets.json`
(0600), o donde diga `CONDX_KEYCHAIN=file:<ruta>`. Así un daemon de prueba recompilado no abre
ventanas del Llavero ni se queda esperando una contraseña.
