# Aprobar

Lo que espera tu visto bueno aparece en **Aprobaciones** (y un aviso, si lo pidió un agente). Cada tarjeta muestra quién lo pidió, el commit, los argumentos y, en las promociones, las precondiciones con su estado.

## Niveles

| Nivel | Qué tienes que hacer | Se usa para |
| --- | --- | --- |
| Simple | **Aprobar** | Desplegar o rellenar en cloud dev; restaurar en el devbox |
| Escribir el nombre | Escribir el nombre del entorno, **Aprobar** y confirmar | Promover y revertir producción; restaurar en cloud dev |
| Touch ID | Escribir el nombre + **Aprobar con Touch ID** | Restaurar un respaldo en producción |

- Una aprobación **vence a los 30 minutos**.
- **Rechazar** la descarta.
- Solo esta app puede aprobar: los agentes y el CLI usan un token que no tiene ese permiso.
