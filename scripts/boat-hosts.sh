#!/usr/bin/env bash
# Publica en boat el frontend de cada proyecto del sitio en la nube (privado, con token: el teléfono
# lo abre con «Abrir») y le pasa al sitio las URLs (/data/condx/hosts.json). En la nube no hay backend
# local: el frontend usa el cloud dev del proyecto. Las URLs de `boat host` son fijas: se corre una
# vez, o al agregar un proyecto al sitio.
#
#   scripts/boat-hosts.sh [sandbox]
# Los puertos salen de `condxd project list` dentro del contenedor del sitio.
set -euo pipefail
SANDBOX="${1:-bx_wn3z77sm}"
sh_out() { boat exec "$SANDBOX" -- bash -lc "$1" | python3 -c "import sys,json;print(json.loads(sys.stdin.readline()).get('stdout',''))"; }
LIST=$(sh_out 'docker exec condx-site condxd project list')
HOSTS=$(LIST="$LIST" SANDBOX="$SANDBOX" python3 - <<'PY'
import json, os, subprocess
hosts = {}
for line in os.environ["LIST"].splitlines():
    parts = line.split()
    if "backend" not in parts or "frontend" not in parts:
        continue
    name = parts[0]
    frontend = int(parts[parts.index("frontend") + 1])
    for port, title in [(frontend, f"{name} frontend")]:
        args = ["boat", "host", os.environ["SANDBOX"], str(port), "--localhost", "--title", title, "--json"]
        out = subprocess.run(args, capture_output=True, text=True, check=True).stdout.strip().splitlines()[-1]
        url = json.loads(out)["url"]
        hosts[str(port)] = url
        print(f"{port:>5}  private  {url.split('?')[0]}", file=os.sys.stderr)
print(json.dumps(hosts))
PY
)
# Al contenedor sin que las URLs con token queden en la salida.
B64=$(printf '%s' "$HOSTS" | base64 | tr -d '\n')
sh_out "echo $B64 | base64 -d | docker exec -i condx-site sh -c 'cat > /data/condx/hosts.json && chmod 600 /data/condx/hosts.json' && echo saved" >/dev/null
echo "Listo: el sitio tiene las URLs (/data/condx/hosts.json)."
