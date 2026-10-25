# Agentes de IA (MCP)

## Pueden

- Mirar todo: estado, logs, CI, telemetría de la app (`get_overview`, `get_report`, `get_function_logs`…).
- Pedir trabajos (`request_job`) y seguirlos (`get_job`, `get_job_log`).
- Revisar las condiciones de una promoción (`check_promotion`).
- Cancelar los trabajos que ellos mismos pidieron.

## No pueden

- **Aprobar.** Lo que pidan y necesite aprobación te espera en esta app, con un aviso.

## Ejemplo

Un agente pide «Ensayo» y «Desplegar en cloud dev»: el ensayo corre solo; el despliegue aparece en **Aprobaciones** hasta que lo apruebes.

## Conectar un agente

En **Configuración › Conectar un agente** están los comandos listos para copiar (Claude Code,
Cursor, Codex y HTTP) y cuándo se usó el MCP por última vez. En Claude Code:

```
claude mcp add condx -- ~/.condx/daemon/bin/condxd mcp
```

Cualquier cliente MCP por stdio usa `condxd mcp`; necesita el daemon corriendo
(`condxd install`). No hay cuentas: el puente usa el token local `~/.condx/daemon/token`, que no
puede aprobar.
