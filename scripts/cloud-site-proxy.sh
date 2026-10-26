#!/usr/bin/env bash
# El sitio de Condx Cloud en condx.heskala.com: un proyecto chico de Vercel que solo reescribe todo
# hacia el sitio del deployment. Uso: scripts/cloud-site-proxy.sh https://<deployment>.convex.site
set -euo pipefail
TARGET="${1:?usage: $0 https://<deployment>.convex.site}"
TARGET="${TARGET%/}"
case "$TARGET" in https://*) ;; *) echo "The target must be an https URL." >&2; exit 1 ;; esac
# Los archivos (la APK) salen del almacenamiento del deployment (.convex.cloud) con su nombre en la ruta.
FILES="${TARGET%.convex.site}.convex.cloud"
DOMAIN="${CONDX_SITE_DOMAIN:-condx.heskala.com}"
PROJECT="${CONDX_SITE_PROJECT:-condx-cloud-site}"
cd "$(dirname "$0")/../cloud/site-proxy"
cat > vercel.json <<JSON
{
  "\$schema": "https://openapi.vercel.sh/vercel.json",
  "rewrites": [
    { "source": "/apk/:id/:name", "destination": "$FILES/api/storage/:id" },
    { "source": "/(.*)", "destination": "$TARGET/\$1" }
  ]
}
JSON
[ -d .vercel ] || vercel link --yes --project "$PROJECT"
vercel deploy --prod --yes
vercel domains add "$DOMAIN" "$PROJECT" 2>/dev/null || true
echo
echo "Rewrite: https://$DOMAIN → $TARGET"
vercel domains inspect "$DOMAIN" 2>&1 | sed -n '/Recommended\|CNAME\|configured/p'
