# Datos

La pestaña **Datos** de un entorno muestra lo que hay en su base de Convex, de **solo lectura**.

- **Tablas** (izquierda): todas las del proyecto con cuántos documentos tiene cada una; las que
  tienen documentos van primero. *Buscar tabla…* encuentra cualquier palabra en cualquier parte del
  nombre. Las flechas arriba y abajo recorren la lista.
- **Documentos** (derecha): los de la tabla elegida, de los más nuevos a los más viejos, de a 50
  por página. El paginador cuenta las páginas del total de la tabla («1–50 de 240») y al ir a una
  que no se leyó, la lee.
  - **Buscar** en todas las columnas, **Filter** abre las reglas de filtro (contiene, es, mayor
    que…) y de orden; un clic en un encabezado también ordena (Mayús agrega otro criterio). Con
    búsqueda, filtros u orden, la tabla trabaja sobre lo leído, con su propio paginador.
  - **Columns** oculta columnas, **Density** cambia el alto de las filas y **Export** copia lo que
    se ve como CSV.
  - Las fechas se reconocen por sus valores y se leen como fecha y hora. Los campos que apuntan a
    otra tabla lo dicen en el encabezado (`userId → users`) y sus ids se ven como etiquetas.
  - Los números se alinean y muestran su promedio al pie. La casilla **Colores** los tiñe según
    dónde cae cada valor entre el menor (rojo) y el mayor (verde) de la columna.
  - Con una sola fila seleccionada, el documento entero se ve en JSON al costado, con las líneas
    largas cortadas. **Doble clic** en una fila copia su `_id`.

Según el entorno:

- **Locales** (devbox y desechables): el daemon lee con su admin key, que nunca sale de él; se ve
  cuántos documentos tiene cada tabla.
- **Cloud dev**: de solo lectura. Si guardaste su deploy key en la pestaña **Claves**, el daemon lee
  como en local (cuántos documentos tiene cada tabla, páginas con cursor). La key tiene que ser del
  deployment del entorno (si no, se rechaza sin conectarse). Sin key se lee con tu sesión de la CLI
  de Convex (`npx convex data`), que no da tamaños (la lista muestra «—») ni cursor.
- **Producción**: no se muestra por defecto. Para mirar datos reales sin riesgo, restaura un
  respaldo de producción en un desechable (el ensayo lo hace) y abre su pestaña Datos.

## Reglas de datos y cambios

En **Configuración › Datos y cambios** se decide, por tipo de entorno (devbox, desechable, cloud
dev, producción):

- **Leer datos**: si la pestaña Datos y los agentes por el MCP pueden leer sus registros (siempre
  solo lectura). Producción viene desactivada.
- **Cambios**: cuánto se exige a las recetas que modifican el entorno: lo que diga la receta, como
  mínimo una aprobación, escribir el nombre del entorno, Touch ID, bloqueadas o, salvo en
  producción, sin aprobación.

Las queries, que solo leen, no piden aprobación si leer está permitido. Las reglas las guarda el
daemon y solo la app puede cambiarlas: un agente por el MCP o la CLI no tiene ese permiso.
