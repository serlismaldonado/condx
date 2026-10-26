#!/usr/bin/env bash
# Publica en boat los puertos de los servicios locales del sitio en la nube (el «local» con la Mac
# apagada) y le pasa al sitio las URLs (/data/condx/hosts.json). Las URLs de `boat host` son fijas:
# se corre una vez, o al agregar un proyecto al sitio.
#   - backend y su puerto de HTTP actions: públicos (el navegador del teléfono se conecta a ellos;
#     los datos los protege la autenticación de la app)
#   - frontend: privado, con token (el teléfono lo abre con «Abrir»)
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
    backend = int(parts[parts.index("backend") + 1])
    frontend = int(parts[parts.index("frontend") + 1])
    for port, public, title in [(backend, True, f"{name} backend"), (backend + 1, True, f"{name} site"), (frontend, False, f"{name} frontend")]:
        args = ["boat", "host", os.environ["SANDBOX"], str(port), "--localhost", "--title", title, "--json"] + (["--public"] if public else [])
        out = subprocess.run(args, capture_output=True, text=True, check=True).stdout.strip().splitlines()[-1]
        url = json.loads(out)["url"]
        hosts[str(port)] = url
        print(f"{port:>5}  {'public ' if public else 'private'}  {url.split('?')[0]}", file=os.sys.stderr)
print(json.dumps(hosts))
PY
)
# Al contenedor sin que las URLs con token queden en la salida.
B64=$(printf '%s' "$HOSTS" | base64 | tr -d '\n')
sh_out "echo $B64 | base64 -d | docker exec -i condx-site sh -c 'cat > /data/condx/hosts.json && chmod 600 /data/condx/hosts.json' && echo saved" >/dev/null
echo "Listo: el sitio tiene las URLs (/data/condx/hosts.json)."
