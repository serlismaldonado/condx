# Si algo sale mal en producción

| Situación | Qué hacer |
| --- | --- |
| Las funciones nuevas fallan | Receta **Revertir producción (funciones)**: por defecto vuelve al commit anterior registrado. Aprobación escribiendo «prod». Los datos no se tocan. |
| El frontend falla | En la terminal: `vercel rollback` (la receta no lo hace). |
| Los datos se dañaron | **Restaurar respaldo** en prod con un `.zip.age`. Aprobación con nombre + Touch ID. **Pierde todo lo escrito después del respaldo**: es el último recurso. |

## Un trabajo falló

1. Abre el trabajo en **Trabajos**: el paso en rojo y el log dicen qué pasó.
2. Si fue algo pasajero (red, un servicio caído), **Reintentar desde el paso que falló**.
3. Si el problema es del código, corrígelo, sube el commit y vuelve a empezar la cadena.
