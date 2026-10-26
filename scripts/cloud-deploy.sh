#!/usr/bin/env bash
# Condx Cloud en tu propio proyecto de Convex (fase S0 de docs/design/cloud-site.md): despliega
# `cloud/` y vincula el daemon instalado. La deploy key de producción se pega sin mostrarse, se usa
# para desplegar y registrar este daemon una vez, y no se guarda.
#
#   scripts/cloud-deploy.sh            # pide la key
#   CONDX_DRY_RUN=1 scripts/cloud-deploy.sh   # solo muestra qué haría
#   CONDX_DEPLOY_ONLY=1 scripts/cloud-deploy.sh   # solo despliega (el daemon ya está vinculado: no lo
#                                                 # vuelve a registrar, el teléfono no se re-vincula)
#   CONDX_SITE=https://condx.example.com scripts/cloud-deploy.sh   # con un dominio propio para el sitio
#     (el QR, /pair, /ingest, /files); la API sigue en <deployment>.convex.cloud
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${CONDX_BIN:-$HOME/.condx/daemon/bin/condxd}"
# El nombre que ya tenía esta Mac en Condx Cloud (o el del equipo).
CURRENT="$(sed -n 's/.*"name": *"\([^"]*\)".*/\1/p' "$HOME/.condx/daemon/cloud.json" 2>/dev/null | head -1)"
NAME="${CONDX_NAME:-${CURRENT:-$(scutil --get ComputerName 2>/dev/null || hostname)}}"

read -rsp "Production deploy key of your Convex project (dashboard → Settings → Deploy keys; not shown): " KEY
echo
case "$KEY" in
  prod:*\|*) ;;
  *) echo "That doesn't look like a production deploy key (prod:<deployment>|…)." >&2; exit 1 ;;
esac
DEPLOYMENT="${KEY#prod:}"
DEPLOYMENT="${DEPLOYMENT%%|*}"
URL="https://$DEPLOYMENT.convex.cloud"
echo "Deployment: $DEPLOYMENT ($URL)"

if [ -n "${CONDX_DRY_RUN:-}" ]; then
  echo "Would deploy $ROOT/cloud to $URL and run: condxd cloud link --url $URL --name \"$NAME\""
  exit 0
fi

cd "$ROOT/cloud"
[ -d node_modules ] || bun install
# `cloud/.env.local` apunta al backend de prueba (self-hosted): el CLI lee en su lugar un archivo
# temporal (0600, se borra al salir) que solo tiene la key, y sin las variables del shell, así el
# despliegue va solo al proyecto de la key.
ENVF="$(umask 077 && mktemp)"
trap 'rm -f "$ENVF"' EXIT
printf 'CONVEX_DEPLOY_KEY="%s"\n' "$KEY" > "$ENVF"
env -u CONVEX_SELF_HOSTED_URL -u CONVEX_SELF_HOSTED_ADMIN_KEY -u CONVEX_DEPLOYMENT -u CONVEX_URL -u CONVEX_DEPLOY_KEY \
  npx convex deploy --env-file "$ENVF"

if [ -n "${CONDX_DEPLOY_ONLY:-}" ]; then
  unset KEY
  echo "Deployed to $URL (the daemon's link is unchanged)."
  exit 0
fi
printf '%s\n' "$KEY" | "$BIN" cloud link --url "$URL" --name "$NAME" ${CONDX_SITE:+--site "$CONDX_SITE"}
unset KEY

cat <<EOF

Condx Cloud runs on $URL (site: ${CONDX_SITE:-https://$DEPLOYMENT.convex.site}).
Next: re-pair your phone. In the phone app, tap "Unpair this phone"; on the Mac, Condx → Settings →
Condx Cloud → Pair a phone, and scan the QR (pairing from the old backend doesn't carry over).
EOF
