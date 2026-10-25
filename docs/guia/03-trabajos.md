# Pedir un trabajo

1. **Trabajos → Nuevo trabajo**.
2. Elige la **receta** en el desplegable. Al lado de cada una se ve en qué tipo de entornos corre.
3. Elige el **entorno** en el desplegable. Solo aparecen los entornos donde esa receta puede correr.
4. Mira la fila **Aprobación**: «sin aprobación» arranca solo; «pide aprobación» espera tu visto bueno.
5. Completa los **argumentos**. Traen valores por defecto razonables.
6. **Pedir trabajo**. Se abre su detalle.

## La lista y el detalle

- **Recientes** es una tabla: estado, receta, detalle (la función, el commit o el proyecto), entorno,
  quién lo pidió, cuándo y el error. Se ordena con un clic en el encabezado. Un clic en una fila
  abre su detalle debajo.
- **En el detalle, cada paso es un bloque** con su estado, su duración y su salida (el tramo del log
  de ese paso), que se pliega. Están abiertos el paso que corre y el que falló.
- **Argumentos:** se ven como árbol JSON con búsqueda.
- **Log completo:** plegado al final, con filtro por nivel mínimo.

## Mientras corre o después

- **Cancelar** corta el proceso y limpia lo que haya creado.
- **Reintentar desde el paso que falló** sigue donde quedó.
- Corre **un trabajo por entorno a la vez**, en el orden en que quedan listos. Entornos distintos corren en paralelo.
- Cuando un trabajo termina, falla o se cancela, aparece un aviso y queda en las notificaciones (la campana).

## Recetas

| Receta | Qué hace |
| --- | --- |
| Desplegar un commit | Devbox o desechable: worktree del commit + `convex dev --once` |
| Correr una función / Correr un relleno | `convex run` |
| Respaldo (export) | Exporta a `~/.condx/backups/`; en cloud, cifrado |
| Restaurar respaldo | `import --replace-all` (reemplaza todos los datos) |
| Reiniciar el backend / Reconstruir el deployer | Mantenimiento del devbox |
| Crear / Destruir backend desechable | Backends temporales en Docker |
| Desplegar en cloud dev | `convex dev --once` en cloud dev, con aprobación |
| Ensayo con datos reales | Ver «Promoción» (pestaña de producción) |
| Promover a producción / Revertir producción | Ver «Promoción» (pestaña de producción) y «Si algo sale mal» |
