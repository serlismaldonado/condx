# Entorno local (Servicios) y terminal

## Servicios del entorno local

El **Resumen** del entorno local (en Entornos, **local**) empieza por sus servicios: levanta y detiene lo necesario para probar en esta Mac. Arriba se elige
el proyecto: heskala-erp u otro repo de Convex (ver «Otros proyectos de Convex»). Lo que sigue
describe heskala-erp.

### Modo Docker o modo nativo

Arriba, la tarjeta **Modo del entorno local** dice cómo corren el backend y el deployer:

| Modo | Backend | Deployer | ¿Necesita Docker/OrbStack? |
| --- | --- | --- | --- |
| **Docker** | contenedor `convex-backend` | contenedor `deployer-dev` | sí |
| **Nativo** | binario `convex-local-backend` (el de la edición de escritorio), datos en `~/.condx/daemon/native/local` | `convex dev` como proceso del daemon | **no** |

- **Migrar a modo nativo** pide un trabajo (con aprobación) que: respalda el backend Docker, lee sus variables de entorno, lo detiene, levanta el nativo, importa los datos, copia las variables y levanta el deployer nativo. El volumen de Docker no se toca.
- **Volver a Docker / Usar modo nativo** cambia de modo después (receta «Cambiar el modo del entorno local»). Cada modo conserva sus datos: volver a Docker vuelve a los datos de antes de migrar.
- El backend sigue en `localhost:3210`, así que Vite y `.env.local` no cambian. El deployer nativo corre desde una carpeta espejo del repo y no toca tu `.env.local`.
- Los **backends desechables** (ensayos) también son nativos cuando está el binario: no usan Docker.

> Si el binario no está, se baja con `bun scripts/fetch-backend-binaries.ts`.

### Servicios

Arriba, el **monitor de recursos**: la CPU y la memoria de los servicios que corren (sumadas, y la
memoria de cada uno), con su minigráfica del último minuto y medio.

Debajo, los servicios en filas. Cada fila trae:

- el puerto, la CPU y la memoria (o la fase, en el deployer);
- su estado;
- los botones **levantar/detener**, **reiniciar**, **ver sus logs** y, en el frontend, **abrir**.

| Servicio | Qué es |
| --- | --- |
| **Backend Convex** | En Docker o nativo, según el modo (localhost:3210). |
| **Deployer (convex dev)** | Despliega las funciones del checkout al backend cada vez que guardas un archivo. |
| **Frontend (Vite)** | La app web en http://localhost:3000, apuntando al Convex de `.env.local` (la tarjeta muestra a cuál). |

- **Levantar todo** arranca en orden backend → deployer → Vite; **Detener todo**, al revés.
- Los maneja el daemon: **siguen corriendo aunque cierres la app**. Si reinstalas el daemon, vuelve a vigilar el Vite que estaba corriendo.
- Si el puerto 3000 lo usa otro proceso, la fila lo dice (con su pid) y no arranca otro encima.
- **El proyecto y el modo** (Docker o nativo) quedan plegados al final de la página.

### Logs

El servicio se elige en el desplegable (o con el botón de logs de su fila). Se ven las últimas 400
líneas, sin secretos ni colores, actualizadas cada 2 segundos.

- Cada línea con su hora, nivel y origen (la función de Convex o el módulo).
- **Nivel mínimo** (TRACE…ERROR) y **búsqueda** filtran sin volver a pedir.
- El botón de seguir baja solo con cada línea nueva; al subir con la rueda, se suelta.
- En Backend y Deployer, al final aparecen también las acciones del devtool (quién levantó o detuvo qué).

## Terminal

La página **Terminal** abre un shell (tu `$SHELL`, con login) **en el repo del proyecto elegido** (el
ERP, por defecto), dentro de la app.

- **Nueva** abre otra pestaña; **Cerrar** cierra la actual (el shell termina).
- Funciona como una terminal normal: colores, `Tab` para completar, flechas para el historial, `ctrl-c`, `ctrl-l`, programas a pantalla completa. **cmd-v** pega.
- La **rueda del mouse** sube por el historial de la pantalla; al escribir, vuelve abajo.
- **Atajos** escribe y ejecuta comandos frecuentes en la pestaña activa: `git status`, tipos, lint, tests, logs de Convex y el estado del devtool.
- Las terminales viven mientras la app esté abierta (no en el daemon): si cierras la app, se cierran.

> Para lo que tiene que sobrevivir a cerrar la app (Vite, el backend), usa el Resumen del entorno local.
