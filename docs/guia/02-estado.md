# Mirar el estado

## El ícono de la barra de menú

| Ícono | Significa |
| --- | --- |
| **Condx ✓** | Todo bien |
| **Condx ⚠ N** | Hay N cosas que mirar (por ejemplo, una aprobación pendiente o un trabajo fallido) |
| **Condx ✕** | Un entorno no responde o el daemon está caído |

Su menú muestra una línea por entorno y tiene **Abrir Condx** y **Salir**.

## La ventana

Tiene la estructura de Heskala ERP con un estilo plano y compacto, como Zed o Cursor: grises
neutros, bordes finos y rectos, sin sombras, la tipografía de Zed (IBM Plex Sans; JetBrains Mono
para código y cifras) y el color solo donde importa.

**Barra de título** (integrada, con los botones de macOS):

- el botón para plegar la barra lateral a un riel de íconos (con su nombre al pasar el mouse);
- el **selector de proyecto**, como el selector de sucursal del ERP (desde ahí también se agrega
  uno);
- la rama y el commit del repositorio y, en un entorno, su nombre y su tipo (devbox, cloud dev,
  producción);
- a la derecha, lo que está corriendo y lo que espera aprobación (clic para ir).

**Barra lateral:**

- **Parte de los entornos:** casi todo pertenece a uno (sus servicios, sus trabajos, su
  promoción, las pestañas de la app conectadas a él), así que cada entorno es la entrada a lo suyo,
  en pestañas.
- **Los grupos:**
  - **Vista general.**
  - **Entornos:** local, dev, prod… con su salud. El número dice los servicios arriba (en el
    devbox) o lo que espera aprobación en ese entorno.
  - **Proyecto:** Flujos (los de despliegue, ver [Flujos](12-flujos.md)), Git y CI (en otros
    proyectos, Git) y Terminal: sirven a todos los entornos.
  - **Guía.**

  Cada grupo se abre o se cierra con su flecha. En un proyecto sin entornos registrados, el grupo
  Entornos muestra «Entorno local».
- **Trabajos y aprobaciones de todos los entornos:** desde los avisos de la barra de título
  («N trabajos en curso», «N por aprobar»).
- **Abajo:** la campana de **notificaciones**, el **tema** (sistema → oscuro → claro) y el estado
  del daemon.

**Contenido:** sin título repetido; la barra lateral marca la página. Los logs y las listas con
scroll propio mueven primero su contenido; al llegar arriba o abajo del todo, la rueda sigue con la
página. El diagrama del esquema nunca mueve la página. Las listas de commits se muestran
completas, sin scroll propio.

**Color de la marca:** el amarillo **#F3B01C** reemplaza al azul en todo lo que no es un estado:
el botón principal de cada vista (levantar, pedir trabajo, aprobar, crear…), los conmutadores
activos, el subrayado de la pestaña elegida, los íconos activos de la barra lateral, las
relaciones de la tabla elegida en el esquema, enlaces, selección e interruptores. El texto sobre
el amarillo es oscuro (contraste 9,5:1). Como texto o línea sobre el fondo, en modo oscuro va el
mismo amarillo (9:1); en modo claro, un dorado oscuro (#A16207, 4,9:1), porque el amarillo sobre
blanco no llega a 2:1. Los estados (en línea, error, advertencia, información) siguen con sus
colores.

**Pantallas chicas:** con la ventana angosta (menos de 820 px) la barra lateral se pliega sola
al riel de íconos (sin cambiar la preferencia); con menos de 1000 px las filas de la vista general
esconden la latencia y el atraso. Las tarjetas de cifras se acomodan en varias filas, los
buscadores y desplegables se achican, y la ventana no baja de 640 × 480.

**Preferencias:** la app recuerda la barra plegada, los grupos cerrados, el proyecto y el tema
en `~/.condx/app/app.sqlite3`, solo en esta Mac.

## Notificaciones

Lo que antes eran banners en las páginas ahora llega a la campana:

- lo que necesita atención (un entorno que no responde, el CI en rojo…);
- los trabajos que terminan o fallan;
- lo que espera tu aprobación;
- los errores de una acción.

Lo nuevo aparece además como aviso flotante. El panel guarda las últimas 200: la **×** borra una,
**Marcar leídas** quita el número de la campana y **Limpiar** las borra todas. Al cerrar el panel,
lo visto queda leído.

## Las páginas

- **Vista general:**
  - Arriba, cifras: entornos en línea, trabajos en curso, por aprobar, atención y commits sin subir.
  - Debajo, los **entornos en filas**: salud, tipo, latencia de Convex, commit y atraso. Clic en
    una fila: su detalle. La fila del devbox tiene a la derecha el botón para levantarlo o
    detenerlo (el mismo que en su resumen).
  - Al final, **plegados** el frontend de producción, el CI, el deployer y el repositorio. El título
    de cada uno ya dice su estado («CI · 1 de 2 en verde»); se abren para ver el detalle.
- **Un entorno** (grupo «Entornos»): una sola fila arriba con las pestañas a la izquierda y, a la
  derecha, **Levantar todo** y **Detener todo** (en el resumen del devbox) o el enlace al
  dashboard de Convex. Las pestañas van de lo que más se mira a lo que menos, y cada
  tipo muestra las suyas:

  | Pestaña | devbox | cloud dev | producción |
  |---|---|---|---|
  | Resumen | ✓ (con servicios) | ✓ | ✓ |
  | Promoción | | | ✓ |
  | En vivo (heskala-erp) | ✓ | ✓ | ✓ |
  | Logs, Trabajos | ✓ | ✓ | ✓ |
  | Despliegues | | ✓ | ✓ |
  | Variables, Esquema | ✓ | ✓ | ✓ |

  - **Resumen:** en el devbox empieza por sus **servicios** (el backend, el deployer y el frontend
    de esta Mac, con su CPU y memoria; ver [Entorno local y terminal](10-local-terminal.md)) y
    debajo, plegados: **Logs** (abierto al entrar), **Convex y despliegue**, **Proyecto** y **Modo
    del entorno local**. En los cloud, solo la ficha de Convex y del despliegue.
  - **Levantar y detener:** con nada arriba se ve solo **Levantar todo**; con todo arriba, solo
    **Detener todo** (en rojo); a medias, **Levantar el resto** y **Detener todo**.
  - **Mientras levanta o detiene**, todo va en **naranja**: el botón («Levantando…» /
    «Deteniendo…»), la insignia de cada servicio, el punto del entorno en la barra lateral y en la
    vista general. El cambio se ve al instante (sin esperar el sondeo de 3 s) y se consulta cada
    medio segundo hasta que el daemon confirma el estado final.
  - **Variables:** solo nombres, nunca valores, en una tabla con búsqueda.
  - **Logs:** las funciones de Convex con su hora, función y mensaje. Se filtran por **nivel
    mínimo** (TRACE…ERROR) y por **búsqueda**, sin volver a pedirlos.
  - **Despliegues:** los que hizo el daemon, como lista de commits con su mensaje y autor.
  - **Esquema:** ver abajo.
  - **Trabajos:** pedir uno con las recetas que corren en ese entorno (sin elegirlo) y la lista de
    los suyos, con el detalle del elegido. Ver [Trabajos](03-trabajos.md).
  - **Promoción** (producción): los pasos de la promoción y, debajo, lo que espera aprobación en
    ese entorno, para aprobarlo ahí mismo. Ver [Promoción](06-promocion.md).
  - **En vivo:** las pestañas de la app web conectadas a la URL de Convex de ese entorno.
- **Git y CI** (en otros proyectos, **Git**): como los entornos, cada sección es un plegado
  (Cambios abierto al entrar) y el título ya dice lo importante:
  - **Cambios · N:** los archivos por sección (sin preparar, preparados, sin seguimiento, en
    conflicto), con su letra de estado y sus líneas;
  - **Ramas · N:** con su distancia al remoto;
  - **Últimos commits · N:** con sus referencias;
  - en heskala-erp, **Submódulos** («1 al día» o cuántos difieren del remoto) y un **CI** por
    repo con el estado de su última corrida (clic en una corrida: se abre en GitHub).
  Las listas de cambios y ramas tienen scroll propio y pasan la rueda a la página en el borde.
- **En vivo** (pestaña de cada entorno): si hay varias pestañas de la app conectadas, se elige en un
  desplegable.
  - Debajo, sus cifras, las funciones de Convex en una tabla ordenable por cualquier columna, los
    errores y, plegado, el snapshot completo como árbol JSON con búsqueda.

## Esquema de un entorno

La pestaña **Esquema** de un entorno muestra sus tablas de Convex.

- **Panel de tablas:** el botón del panel (al principio de la barra) lo oculta para dar todo el
  ancho al diagrama, y lo vuelve a mostrar («Tablas»). Se recuerda.
- **A la izquierda**, el árbol de carpetas de `convex/`, igual que en el repo: `companies` →
  `invoices` → `payments`… Cada tabla cuelga de la carpeta de donde la importa `convex/schema.ts`;
  las de `@convex-dev/auth` van aparte. Tiene buscador y cada carpeta dice cuántas tablas tiene.
  Clic en una tabla: la elige. Clic en una carpeta: pasa al modo **Carpeta** con esa carpeta.
- **A la derecha**, el diagrama: cada tabla es una caja con sus campos, y cada referencia es una
  línea hasta la tabla a la que apunta. Tres modos:
  - **Foco** (por defecto): la tabla elegida al centro, a la derecha las que apunta y a la
    izquierda las que la apuntan, en columnas cortas.
  - **Carpeta:** la carpeta elegida con sus subcarpetas, como árbol (ver Disposición). Se dibujan
    todas sus referencias, también las que van de una subcarpeta a otra.
  - **Todas:** todo `convex/`. Se dibujan las referencias dentro de cada carpeta y las de la tabla
    elegida.
- **Disposición** (en Carpeta y Todas):
  - **Por carpetas:** un árbol a lo ancho. Cada carpeta tiene su tabla principal (la que se llama
    como ella, como `companies` en `companies/`, o si no, la más apuntada desde dentro), con el
    título resaltado, a la izquierda de su marco; a su derecha va todo lo suyo: sus tablas y los
    bloques de sus subcarpetas, que repiten lo mismo (su principal a la izquierda y lo suyo a la
    derecha). Lo de la derecha se acomoda en filas para que el conjunto quede más ancho que alto.
    Unas líneas punteadas unen la principal con lo de su primera columna.
  - **Por relaciones:** capas de izquierda a derecha, cada tabla a la derecha de las que apunta, con el
  orden que deja menos cruces; las tablas sin relaciones van en una fila aparte, abajo.
- **Líneas:** en ángulo recto y rodeando las cajas, con un punto donde entran. La de una caja que
  se está arrastrando va directa hasta soltarla; ahí se recalcula.
- **Minimapa:** abajo a la derecha, cuando no se ve todo: las cajas en pequeño y un rectángulo
  con lo que se ve. Clic o arrastre sobre él: el diagrama va ahí.
- **Elegir y mover:** clic en cualquier parte de una caja la elige; arrastrarla la mueve (el
  marco de su carpeta la sigue) y el lugar se recuerda. En Foco, elegir otra tabla la pone al
  centro, así que se elige al soltar sin haber movido.
- **Relaciones de la elegida:** sus líneas van en otro color. Con **Solo las de la elegida**
  (en Carpeta y Todas) se ocultan las demás.
- **Moverse:** dos dedos en el trackpad (o la rueda), o arrastrar el fondo, mueven el diagrama;
  pellizcar o **⌘ + rueda** hacen zoom donde está el puntero (de 3 % a 400 %). También hay
  **−**, **+** y **Ajustar**, que encuadra todo lo que se ve. El diagrama no mueve la página.
- **De lejos:** el texto se achica pero no desaparece. Los campos se ven hasta que una fila mide
  6 px; después, una raya por campo. El nombre de la tabla no baja de 9 px (si no cabe en su
  franja va justo encima de la caja); ya muy lejos, la principal de cada carpeta y la elegida
  llevan su nombre sobre un fondo. Los nombres de carpeta siguen legibles.
- **Pantalla completa:** oculta la barra lateral y las pestañas y deja el esquema en toda la
  ventana. **Salir de pantalla completa** lo devuelve; cambiar de página también.
- **Alto:** arrastra el borde de abajo del diagrama para hacerlo más alto o más bajo (desde
  280 px). No baja más allá de la ventana, para que el borde siga a la vista; al achicar la
  ventana, el alto se recorta. Se recuerda.
- **Espaciado:** compacto, normal o amplio, para separar más las cajas.
- **Debajo**, la tabla elegida: sus campos, su carpeta, a qué tablas apunta y sus **índices**
  (normales, de búsqueda y vectoriales) con sus campos.
- **Se recuerda:** lo que muevas, el zoom, el espaciado y el alto quedan guardados, por entorno y por modo.
- **De dónde sale:** en los entornos locales es el esquema **desplegado**. En los cloud (dev,
  producción), la CLI de Convex no deja leer el esquema. Se muestra el del backend local del mismo
  proyecto, marcado como «esquema del checkout»; tiene que estar levantado.
