# Primer arranque, la paleta y los chequeos antes de desplegar

## Primer arranque

Sin proyectos todavía, el **Resumen** pasa a ser una guía de tres pasos:

1. **Revisar esta Mac.** Node.js 18+, la sesión del CLI de Convex (`npx convex login`), el
   backend local de Convex, git y, opcionales, el CLI de GitHub (para el CI) y Docker (el modo
   nativo no lo necesita). Lo que falta muestra el comando que lo arregla, con botón de copiar.
   - **Descargar** baja `convex-local-backend` para esta Mac de los releases de Convex en GitHub
     a `~/.condx/daemon/bin`. Si `npx convex dev --local` ya bajó uno, Condx lo reutiliza.
   - Bajar un ejecutable es solo de la app: un agente por el MCP no puede.
2. **Agregar tu proyecto de Convex.** Una carpeta con `convex/` y `package.json`, o uno de los
   repos de Convex que Condx encontró en tu carpeta de proyectos. **Crear en Convex** hace uno
   nuevo.
3. **Levantarlo.** Entorno local → **Iniciar todo**: backend, deployer (`convex dev`) y frontend.

## Paleta de comandos

**⌘K** (Ctrl+K en la demo web) abre una paleta con búsqueda difusa:

- **Ir a** cualquier página (Resumen, Flujos, Git y CI, Trabajos, Aprobaciones, Configuración…).
- **Entornos**: un entorno, o directo a una de sus pestañas (`prod › Datos`, `local › Logs`…).
- **Tablas**: cualquier tabla de un entorno cuya pestaña Datos ya abriste, p. ej.
  `comments · local`.
- **Proyectos**: cambiar de proyecto.
- **Acciones**: levantar o detener los servicios locales, agregar o crear un proyecto, avisos,
  tema.

Lo último que corriste aparece primero.

## CI de cada proyecto

Condx lee las corridas de GitHub Actions del `origin` de cada proyecto (`gh run list`; `gh`
tiene que tener sesión). Se ven en **Git y CI** y en el resumen, y la verificación de flujos
**El CI está en verde** usa el CI del propio proyecto.

## Comparar esquemas antes de desplegar

En la pestaña **Esquema** de un entorno, **Comparar local → cloud** lee el esquema desplegado en
ambos (el de cloud con su deploy key, de la pestaña Claves) y lista:

- tablas, campos e índices que entran, salen o cambian;
- **riesgos** que harían fallar `convex deploy` o dejarían documentos inválidos: un campo nuevo
  obligatorio, un campo que cambia de tipo o pasa a ser obligatorio, un campo que sale de una
  tabla tipada.

No se despliega nada. La misma comparación es una verificación de flujos (**El esquema local es
seguro para desplegar aquí**) y una herramienta del MCP (`get_schema_diff`), para que un agente
mire antes de pedir.
