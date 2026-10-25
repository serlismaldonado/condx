# Otros proyectos de Convex

El devtool no es solo para el ERP: cualquier repo de Convex de tu cuenta puede tener su instancia
local, con su backend, su deployer y su frontend, igual que heskala-erp.

## Elegir el proyecto

El **selector de proyecto** está en la barra de título, junto al logo. El que elijas filtra
toda la app (y se recuerda al volver a abrirla):

- **Vista general:** sus entornos, su git y su deployer local (sin Vercel ni CI, que son del ERP).
- **Entornos** de la barra lateral: solo los suyos.
- **Trabajos:** sus trabajos, y solo las recetas que corren en sus entornos.
- **Servicios** (en el Resumen del entorno local; «Entorno local» si no hay entornos) y **Terminal:** su instancia y su carpeta.
- Las pestañas **Promoción** y **En vivo** y el CI solo aparecen en heskala-erp.
- **Aprobaciones** muestra las de todos los proyectos: nada queda esperando sin verse.

## Agregar o crear: dos cosas distintas

- **Agregar proyecto existente:** registra en el devtool una carpeta que ya tiene `convex/` y
  `package.json`. No toca Convex: solo le asigna puertos y la conecta.
- **Crear proyecto en Convex:** crea uno desde cero en tu cuenta de Convex (ver más abajo).

Las dos están en el selector de proyecto de la barra de título y en el plegado «Proyecto» del
resumen del entorno local.

## Agregar un proyecto existente

**Agregar proyecto** (en el selector de proyecto de la barra de título, o en el plegado
«Proyecto» del Resumen del entorno local) abre un **panel lateral** a la derecha:

- Escribe la carpeta del repo (admite `~/…`) y toca **Agregar**, o
- toca **Agregar** en uno de los repos **encontrados** junto al ERP (los que tienen `convex/` y
  `package.json`). En cada fila se ve su proyecto de Convex, si el `.env.local` lo dice, y su
  frontend.

El panel se cierra al elegir y el proyecto nuevo queda seleccionado. Si algo falla, el error va a
las notificaciones.

Al agregarlo, el devtool:

1. le pone nombre: el proyecto de Convex del `.env.local` (`# team: …, project: …`) o la carpeta;
2. le asigna puertos propios: backend desde **3220** (de 10 en 10; el sitio es el siguiente) y
   frontend desde **3001**;
3. detecta el frontend: **Vite** o **Next** según el `package.json`. Si no hay ninguno, solo
   tendrá backend y deployer.

También le registra dos entornos:

- **`<proyecto>-local`** (devbox): su backend nativo.
- **`<proyecto>-dev`** (cloud dev): su deployment del `.env.local`, si lo tiene.

Se actualizan cada vez que arranca el daemon. No levanta nada todavía.

## Crear un proyecto en Convex

**Crear proyecto en Convex** abre un panel con el nombre (carpeta y proyecto: minúsculas,
números y guiones), la carpeta donde se crea, la **plantilla de frontend** en un desplegable (las
oficiales de `create-convex`: React + Vite, Next.js y TanStack Start, cada una con sus variantes de
shadcn/ui y de login —Convex Auth, Clerk, WorkOS AuthKit, **Better Auth**—, o «Solo Convex», sin
frontend) y el equipo de Convex. Las de Better Auth son los ejemplos oficiales del componente
(`convex/better-auth/examples/react|next|tanstack`): como vienen de un monorepo, el paso de
plantilla cambia sus dependencias locales por versiones de npm (`better-auth` en el rango que
pide `@convex-dev/better-auth`). Después hay que definir sus variables (`BETTER_AUTH_SECRET`,
`SITE_URL`), como dice su README (vacío: el de tu sesión, si tienes uno solo). Pide un
trabajo **project-create**, que espera tu aprobación porque crea recursos en tu cuenta:

1. genera la carpeta con `npm create convex@latest <nombre> -- -t <plantilla>`;
2. instala dependencias (`npm install`);
3. crea el proyecto y su deployment de desarrollo con `npx convex dev --once --configure new`,
   con la sesión de la CLI de Convex de esta Mac (si no hay, el paso falla y pide
   `npx convex login`);
4. lo agrega al devtool, como «Agregar proyecto existente».

## Quitar un proyecto del devtool

En el selector de proyecto, la **×** de su fila (o «Quitar del devtool» en su plegado). Pide
confirmación. Deja de aparecer, se detienen sus servicios y se quitan sus entornos, pero **no** se
borra su carpeta, ni sus datos locales, ni su proyecto en Convex: se puede volver a agregar (vuelve
a aparecer entre los encontrados). heskala-erp no se puede quitar.

## Levantar su instancia local

Elige el proyecto arriba y usa **Levantar todo** (o cada tarjeta):

| Servicio | Qué hace |
| --- | --- |
| **Backend Convex (nativo)** | `convex-local-backend` en su puerto, con datos en `~/.condx/daemon/native/<proyecto>`. La primera vez arranca **vacío**. |
| **Deployer (convex dev)** | Despliega las funciones del repo a ese backend cada vez que guardas. |
| **Frontend** | `vite` o `next dev` en su puerto, apuntando a ese backend. |

- El frontend recibe la URL del backend local por variables de entorno (`VITE_CONVEX_URL`,
  `NEXT_PUBLIC_CONVEX_URL`, `CONVEX_URL` y sus `…_SITE_URL`). Esas variables ganan al
  `.env.local`, que **no se toca**: el repo sigue apuntando a su cloud dev cuando lo usas fuera del
  devtool.
- Como en el ERP, el deployer corre desde una carpeta espejo y no escribe en el `.env.local`.
- Todo es nativo: los otros proyectos no usan Docker.
- Los procesos siguen corriendo aunque cierres la app.

Los **logs** de abajo y la **Terminal** siguen al proyecto elegido: una terminal nueva abre en su
carpeta.

## Traer los datos del cloud

La primera vez el backend local de un proyecto está vacío. En su ficha (Servicios) hay dos
botones; los dos se activan si el `.env.local` del repo tiene `CONVEX_DEPLOYMENT=dev:<nombre>`
(la ficha lo muestra como **Cloud dev**):

| Botón | Receta | Aprobación |
| --- | --- | --- |
| **Traer datos y variables del cloud dev** | `project-seed` | ninguna: corre de inmediato |
| **Copiar producción (con aprobación)** | `project-seed-prod` | en **Aprobaciones**, escribiendo el nombre del proyecto |

Los dos hacen lo mismo, cada uno con su origen:

1. **Exportan** los datos del cloud (con `files = yes`, también el file storage) a un zip temporal
   0600.
2. **Leen las variables de entorno** a un archivo temporal 0600. Si el proyecto tiene frontend,
   `SITE_URL` pasa a su URL local para que las redirecciones de auth vuelvan a tu Mac.
3. **Importan** el zip en el backend local con `--replace-all`: **reemplazan sus datos locales**.
   El deployer se detiene mientras tanto. El zip se borra al terminar.
4. **Copian las variables** al backend local (el valor va por stdin, nunca por argumentos ni al
   log).
5. **Vuelven a levantar el deployer**, que despliega las funciones del repo.

El cloud solo se lee, con tu sesión de la CLI (`npx convex login`). Si la importación falla, el zip
queda en `~/.condx/daemon/tmp/` para el reintento; pide el trabajo de nuevo o bórralo.

> Las variables son las del cloud: claves de API de pruebas, webhooks, etc. Lo que la app haga en
> local con ellas (correos, pagos de prueba) sale de verdad hacia esos servicios.

## Quitar un proyecto

**Quitar del devtool** detiene sus servicios y lo saca de la lista. Sus datos quedan en
`~/.condx/daemon/native/<proyecto>`; si lo vuelves a agregar con el mismo nombre, los recupera.
heskala-erp no se puede quitar.

## Desde el CLI y los agentes

```bash
condxd project discover            # repos de Convex junto al ERP
condxd project add ~/Documents/Software/mi-proyecto
condxd project list
condxd project remove mi-proyecto
```

En el MCP: `list_projects`, `register_project`, y `list_services`, `service_action` y
`get_service_logs` con `project` (por defecto, heskala-erp).

Por agentes o CLI: `request_job` con `recipe = project-seed` (o `project-seed-prod`),
`env = local` y `args = { "project": "<nombre>" }`. La copia de producción siempre espera tu
aprobación en la app.

## Recetas en sus entornos

En los entornos de un proyecto corren las recetas marcadas `projects = true`: desplegar un commit,
desplegar en cloud dev, correr una función o un relleno, respaldo (export), restaurar (import) y
las dos de traer datos. Corren en el repo del proyecto y con su CLI de Convex. Las aprobaciones son
las mismas que en el ERP según el tipo de entorno (por ejemplo, desplegar en cloud dev pide
aprobación simple).

Las recetas del ERP (promoción, ensayo, modo nativo, Docker) se rechazan en esos entornos.

## Lo que todavía no hace

- Producción de otros proyectos como entorno: la promoción y su deploy key son solo del ERP.
- CI y Vercel de otros proyectos en la Vista general.
