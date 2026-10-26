#!/usr/bin/env bash
# Crea una identidad de firma de código propia, «Condx Dev» (autofirmada, solo para esta Mac), en el
# Llavero de inicio de sesión. Con ella se firman el daemon y la app en cada instalación: macOS ve
# siempre el mismo firmante, y el «Permitir siempre» del Llavero sigue valiendo tras actualizar.
# Se corre una vez; macOS pide la contraseña para confiar en el certificado. Ver docs/signing.md.
set -euo pipefail
NAME="${CONDX_SIGN_IDENTITY:-Condx Dev}"
if security find-identity -v -p codesigning | grep -q "\"$NAME\""; then
  echo "Ya existe la identidad “$NAME”."
  exit 0
fi
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
cat > "$TMP/cert.cnf" <<CNF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $NAME
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
CNF
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -keyout "$TMP/key.pem" -out "$TMP/cert.pem" -config "$TMP/cert.cnf" 2>/dev/null
PASS=$(openssl rand -hex 16)
openssl pkcs12 -export -inkey "$TMP/key.pem" -in "$TMP/cert.pem" -name "$NAME" -out "$TMP/id.p12" -passout "pass:$PASS" 2>/dev/null \
  || openssl pkcs12 -legacy -export -inkey "$TMP/key.pem" -in "$TMP/cert.pem" -name "$NAME" -out "$TMP/id.p12" -passout "pass:$PASS"
KC="$HOME/Library/Keychains/login.keychain-db"
security import "$TMP/id.p12" -k "$KC" -P "$PASS" -T /usr/bin/codesign
echo "Ahora macOS pide tu contraseña para confiar en el certificado para firmar código…"
security add-trusted-cert -r trustRoot -p codeSign -k "$KC" "$TMP/cert.pem"
security find-identity -v -p codesigning | grep "\"$NAME\"" && echo "Listo: identidad “$NAME” creada. Reinstala el daemon con scripts/install-daemon.sh."
