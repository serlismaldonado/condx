# Datos

La pestaña **Datos** de un entorno muestra lo que hay en su base de Convex, de **solo lectura**.

- **Tablas** (izquierda): todas las del proyecto con cuántos documentos tiene cada una; las que
  tienen documentos van primero. *Buscar tabla…* filtra la lista.
- **Documentos** (derecha): los de la tabla elegida, de los más nuevos a los más viejos, de a 200
  (*Cargar 200 más* trae los siguientes).
  - **Buscar** en todas las columnas, **Filter** abre las reglas de filtro (contiene, es, mayor
    que…) y de orden; un clic en un encabezado también ordena (Mayús agrega otro criterio).
  - **Columns** oculta columnas, **Density** cambia el alto de las filas y **Export** copia lo que
    se ve como CSV.
  - Las columnas de números se alinean, se tiñen según dónde cae cada valor y muestran su
    promedio al pie. Las fechas (`_creationTime`, `…At`, `…Time`) se leen como fecha y hora.
  - La tabla ocupa el alto de la ventana y se desplaza por dentro: el paginador siempre queda a
    la vista. Con una sola fila seleccionada, el documento entero se ve en JSON al costado.

Según el entorno:

- **Locales** (devbox y desechables): el daemon lee con su admin key, que nunca sale de él; se ve
  cuántos documentos tiene cada tabla.
- **Cloud dev**: de solo lectura. Si guardaste su deploy key en el Llavero, el daemon lee como en
  local (cuántos documentos tiene cada tabla, páginas con cursor):

  ```bash
  ~/.condx/daemon/bin/condxd secret set dev-deploy-key
  ```

  La key tiene que ser del deployment del entorno (si no, se rechaza sin conectarse). Sin key se
  lee con tu sesión de la CLI de Convex (`npx convex data`), que no da tamaños (la lista muestra
  «—») ni cursor: *Cargar más* vuelve a pedir los más nuevos, hasta 2000.
- **Producción**: no se muestra por defecto. Para mirar datos reales sin riesgo, restaura un
  respaldo de producción en un desechable (el ensayo lo hace) y abre su pestaña Datos.

## Reglas de datos y cambios

En **Configuración › Datos y cambios** se decide, por tipo de entorno (devbox, desechable, cloud
dev, producción):

- **Leer datos**: si la pestaña Datos y los agentes por el MCP pueden leer sus registros (siempre
  solo lectura). Producción viene desactivada.
- **Cambios**: cuánto se exige a las recetas que modifican el entorno: lo que diga la receta, como
  mínimo una aprobación, escribir el nombre del entorno, Touch ID, o bloqueadas.

Las guarda el daemon. Solo hacen más estrictas las recetas, nunca menos, y solo la app puede
cambiarlas: un agente por el MCP o la CLI no tiene ese permiso.
