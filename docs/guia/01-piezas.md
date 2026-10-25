# Las piezas y los entornos

| Pieza | Qué es | Dónde vive |
| --- | --- | --- |
| **Daemon** (`condxd`) | El cerebro: corre siempre en segundo plano, vigila los entornos y ejecuta los trabajos. | Servicio de macOS que arranca solo; datos en `~/.condx/daemon/` |
| **App** (Condx) | Esta ventana: mirar, pedir trabajos y aprobar. Si la cierras, el daemon sigue. | `~/Applications/Condx.app` y el ícono «Condx ✓» de la barra de menú |
| **MCP** | La puerta para los agentes de IA. Pueden mirar y pedir trabajos, pero **nunca aprobar**. | Conectado a Claude Code en el proyecto |

## Entornos

| Entorno | Tipo | Qué es |
| --- | --- | --- |
| **local** | devbox | El backend Convex en Docker de esta Mac |
| **dev** | cloud dev | `happy-otter-123` |
| **prod** | producción | `brave-falcon-456`, más el frontend en Vercel |
| desechables | desechable | Backends temporales en Docker para probar; se crean y se borran |

## La regla general

Cuanto más cerca de producción, más protección:

| Entorno | Qué se puede hacer |
| --- | --- |
| devbox, desechables | Casi todo, directo |
| cloud dev | Desplegar y rellenos, con tu aprobación |
| producción | Solo promover, revertir o restaurar, con aprobación fuerte |
